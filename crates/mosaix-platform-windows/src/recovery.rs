//! Verifying and restoring recorded native handles.
//!
//! The recovery ledger records a native handle together with the process
//! instance and window class that owned it. Windows reuses handle values
//! freely once a window is destroyed, so before anything here moves a
//! window it re-reads what the handle names *now* and hands that to the
//! platform-neutral verdict in `mosaix_domain::recovery`. This module
//! only answers the two questions the ledger cannot: what does this
//! handle name today, and how is a verified window put back.
//!
//! Every call is a documented public API: `IsWindow`,
//! `GetWindowThreadProcessId`, `OpenProcess`/`GetProcessTimes`,
//! `GetClassNameW`, `GetWindowPlacement`/`SetWindowPlacement`, and
//! `SetWindowPos`. Nothing here cloaks, hides, or minimises a window.

use mosaix_domain::recovery::{LiveHandleEvidence, ProcessInstance, RecoveryEntry, ShowState};
use mosaix_domain::Rect;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HWND, POINT, RECT};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowPlacement, IsWindow, SetWindowPlacement, SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER,
    SW_SHOWMAXIMIZED, SW_SHOWMINIMIZED, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, WINDOWPLACEMENT,
    WINDOWPLACEMENT_FLAGS,
};

use crate::events::WindowHandle;
use crate::win32_helpers;
use crate::{Result, WindowError};

/// The kernel's creation time for the process `pid`, as a 100-nanosecond
/// count since 1601, or `None` when the process cannot be opened. Paired
/// with the pid this names one process instance: a later process handed
/// the same id has a different creation time.
pub fn process_creation_time(pid: u32) -> Option<u64> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let read = GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user);
        let _ = CloseHandle(process);
        read.ok()?;
        Some(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
    }
}

/// What `handle` names right now: the owning process instance and the
/// window class, or `None` when it names no window at all.
pub fn probe_handle(handle: WindowHandle) -> Option<LiveHandleEvidence> {
    let hwnd = HWND::from(handle);
    if !unsafe { IsWindow(hwnd) }.as_bool() {
        return None;
    }
    let process_id = win32_helpers::get_process_id(hwnd);
    if process_id == 0 {
        return None;
    }
    let class = win32_helpers::get_class_name(hwnd);
    Some(LiveHandleEvidence {
        process: ProcessInstance {
            process_id,
            creation_time: process_creation_time(process_id).unwrap_or(0),
        },
        native_class: (!class.is_empty()).then_some(class),
    })
}

/// The window's restored (non-maximised, non-minimised) bounds and its
/// current show state, as `GetWindowPlacement` reports them. This is what
/// a recovery entry records as `normal_bounds` and `show_state`.
///
/// `rcNormalPosition` is in workspace coordinates, which differ from
/// screen coordinates by the work-area offset on a display whose taskbar
/// sits at the top or left; restoration compensates by placing a normal
/// window at its recorded visible bounds afterwards.
pub fn window_placement(handle: WindowHandle) -> Option<(Rect, ShowState)> {
    let hwnd = HWND::from(handle);
    if !unsafe { IsWindow(hwnd) }.as_bool() {
        return None;
    }
    let mut placement = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    unsafe { GetWindowPlacement(hwnd, &mut placement) }.ok()?;
    let normal = placement.rcNormalPosition;
    let bounds = Rect::new(
        normal.left,
        normal.top,
        normal.right - normal.left,
        normal.bottom - normal.top,
    );
    let show_state = match placement.showCmd {
        command if command == SW_SHOWMAXIMIZED.0 as u32 => ShowState::Maximized,
        command
            if command == SW_SHOWMINIMIZED.0 as u32 || command == SW_SHOWMINNOACTIVE.0 as u32 =>
        {
            ShowState::Minimized
        }
        _ => ShowState::Normal,
    };
    Some((bounds, show_state))
}

/// Puts a verified window back where `entry` recorded it, without
/// activating it where the public API allows that.
///
/// A normal window is placed at its recorded visible bounds with
/// `SWP_NOACTIVATE`. A maximised window is re-maximised through
/// `SetWindowPlacement`, which the documentation says activates the
/// window; that is a measured limitation of the public API, not a
/// choice, and the parking prototype records it. A minimised window is
/// left minimised with its normal bounds restored, so un-minimising it
/// later lands where it was.
pub fn restore_window(handle: WindowHandle, entry: &RecoveryEntry) -> Result<()> {
    let hwnd = HWND::from(handle);
    if !unsafe { IsWindow(hwnd) }.as_bool() {
        return Err(WindowError::InvalidWindow);
    }
    let normal = entry.draft.normal_bounds;
    let show = match entry.draft.show_state {
        ShowState::Normal => SW_SHOWNOACTIVATE,
        ShowState::Maximized => SW_SHOWMAXIMIZED,
        ShowState::Minimized => SW_SHOWMINNOACTIVE,
    };
    let placement = WINDOWPLACEMENT {
        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
        flags: WINDOWPLACEMENT_FLAGS(0),
        showCmd: show.0 as u32,
        ptMinPosition: POINT { x: -1, y: -1 },
        ptMaxPosition: POINT { x: -1, y: -1 },
        rcNormalPosition: RECT {
            left: normal.x,
            top: normal.y,
            right: normal.x + normal.width,
            bottom: normal.y + normal.height,
        },
    };
    unsafe { SetWindowPlacement(hwnd, &placement) }.map_err(WindowError::from)?;
    if entry.draft.show_state == ShowState::Normal {
        let visible = entry.draft.visible_bounds;
        unsafe {
            SetWindowPos(
                hwnd,
                None,
                visible.x,
                visible.y,
                visible.width,
                visible.height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        }
        .map_err(WindowError::from)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::create_test_window;
    use mosaix_domain::recovery::{RecoveryDraft, RecoveryEntryId};
    use mosaix_domain::ApplicationId;
    use windows::Win32::UI::WindowsAndMessaging::{DestroyWindow, ShowWindow, SW_SHOWNOACTIVATE};

    fn entry_for(handle: WindowHandle, visible: Rect, normal: Rect) -> RecoveryEntry {
        let live = probe_handle(handle).expect("the test window is live");
        RecoveryEntry {
            id: RecoveryEntryId(1),
            draft: RecoveryDraft {
                session_id: "test".to_owned(),
                native_handle: handle.0,
                process: live.process,
                application_id: ApplicationId("test.exe".to_owned()),
                executable_path: None,
                native_class: live.native_class,
                original_display_fingerprint: "D".to_owned(),
                visible_bounds: visible,
                normal_bounds: normal,
                show_state: ShowState::Normal,
                recorded_at_unix: 0,
            },
            parked: true,
            restored: false,
        }
    }

    #[test]
    fn a_live_test_window_probes_to_this_process_and_its_class() {
        let hwnd = create_test_window();
        let handle = WindowHandle::from(hwnd);

        let live = probe_handle(handle).expect("the window is live");

        assert_eq!(live.process.process_id, std::process::id());
        assert_eq!(
            live.process.creation_time,
            process_creation_time(std::process::id()).unwrap(),
            "the creation time read through the window matches the one read by pid"
        );
        assert_eq!(
            live.native_class.as_deref(),
            Some("MosaixPlatformWindowsSpikeTestWindow")
        );
        unsafe { DestroyWindow(hwnd) }.unwrap();
    }

    #[test]
    fn a_destroyed_window_probes_to_nothing() {
        let hwnd = create_test_window();
        let handle = WindowHandle::from(hwnd);
        unsafe { DestroyWindow(hwnd) }.unwrap();

        assert_eq!(probe_handle(handle), None);
    }

    #[test]
    fn a_verified_window_moved_off_screen_is_restored_to_its_recorded_bounds() {
        let hwnd = create_test_window();
        let handle = WindowHandle::from(hwnd);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        let visible = Rect::new(40, 60, 320, 240);
        crate::move_resize_window(hwnd, visible).unwrap();
        let (normal, state) = window_placement(handle).unwrap();
        assert_eq!(state, ShowState::Normal);
        let entry = entry_for(handle, visible, normal);

        // What a park does: the window leaves visible geometry.
        crate::move_resize_window(hwnd, Rect::new(-32000, -32000, 320, 240)).unwrap();
        assert_eq!(
            crate::window_bounds(hwnd).unwrap().x,
            -32000,
            "the window is parked"
        );

        restore_window(handle, &entry).unwrap();

        assert_eq!(crate::window_bounds(hwnd).unwrap(), visible);
        unsafe { DestroyWindow(hwnd) }.unwrap();
    }

    #[test]
    fn restoring_a_stale_handle_is_refused_rather_than_moving_anything() {
        let hwnd = create_test_window();
        let handle = WindowHandle::from(hwnd);
        let entry = entry_for(handle, Rect::new(0, 0, 200, 150), Rect::new(0, 0, 200, 150));
        unsafe { DestroyWindow(hwnd) }.unwrap();

        assert!(matches!(
            restore_window(handle, &entry),
            Err(WindowError::InvalidWindow)
        ));
    }
}
