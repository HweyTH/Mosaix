//! Experimental window parking through public APIs only (ADR 0023,
//! ADR 0029).
//!
//! A parked window is moved to a *parking site*: a position outside every
//! connected display, found beyond one edge of the virtual screen and
//! validated with `MonitorFromRect`, so that no monitor covers it. The
//! window keeps its show state, its style, its z-order slot, and its
//! taskbar and Alt-Tab presence; nothing here hides, cloaks, or
//! minimises it, and nothing here touches a private interface. That is
//! the whole point of the experiment: the only thing that changes is
//! where the window is, and where it was is in the recovery ledger.
//!
//! Site discovery is split in two so the choice is testable without a
//! monitor: [`plan_parking_sites`] is pure geometry over the displays the
//! adapter reported, and [`find_parking_site`] asks the live desktop
//! whether the planned block really is beyond every monitor. Both must
//! agree before a site is reported as verified.
//!
//! Every call is documented: `MonitorFromRect`, `MonitorFromWindow`,
//! `GetWindowPlacement`/`SetWindowPlacement`, `SetWindowPos`, and
//! `GetForegroundWindow`.

use mosaix_domain::recovery::ShowState;
use mosaix_domain::workspace::ParkingCapability;
use mosaix_domain::Display;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{MonitorFromRect, MonitorFromWindow, MONITOR_DEFAULTTONULL};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, IsIconic, IsWindow, IsZoomed, SetWindowPlacement, SetWindowPos,
    SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SW_SHOWNOACTIVATE,
    WINDOWPLACEMENT, WINDOWPLACEMENT_FLAGS,
};

use crate::events::WindowHandle;
use crate::{Result, WindowError};

// The platform-neutral half of parking, re-exported so `parking::` reads
// the same to callers as it did before the geometry moved to the shared
// crate.
pub use mosaix_platform_api::parking::{
    plan_parking_sites, virtual_screen_of, ParkedAs, ParkingEdge, ParkingSite, ParkingSiteRefusal,
    COORDINATE_LIMIT, PARKING_MARGIN, PROBE_EXTENT,
};

/// Whether the live desktop agrees that no monitor covers any part of
/// `site`'s probe block (`MonitorFromRect`). This is what catches a
/// display the enumeration missed.
pub fn verify_parking_site(site: &ParkingSite) -> bool {
    let probe = site.probe_rect();
    let rect = RECT {
        left: probe.x,
        top: probe.y,
        right: probe.x + probe.width,
        bottom: probe.y + probe.height,
    };
    unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONULL) }.is_invalid()
}

/// Finds a validated parking site for `displays`, or refuses.
///
/// Each geometric candidate is checked live before it is accepted. A
/// candidate the geometry allows but the desktop refuses is skipped, and
/// when none survives the refusal is that every edge is covered: by the
/// live desktop's account, which is the one that matters.
pub fn find_parking_site(
    displays: &[Display],
) -> std::result::Result<ParkingSite, ParkingSiteRefusal> {
    plan_parking_sites(displays)?
        .into_iter()
        .find(verify_parking_site)
        .ok_or(ParkingSiteRefusal::EveryEdgeCovered)
}

/// What the adapter reports to the engine about parking for `displays`,
/// with the site itself for the executor to park against. `Refused`
/// carries the reason a person reads.
pub fn parking_capability(displays: &[Display]) -> (ParkingCapability, Option<ParkingSite>) {
    match find_parking_site(displays) {
        Ok(site) => (ParkingCapability::Verified, Some(site)),
        Err(refusal) => (
            ParkingCapability::Refused {
                reason: refusal.to_string(),
            },
            None,
        ),
    }
}

/// Moves `handle` to `site` without activating it, and verifies that no
/// monitor covers it afterwards.
///
/// A maximised window cannot simply be moved: the shell keeps it filling
/// its monitor. It is taken to its normal size at the site through
/// `SetWindowPlacement` with `SW_SHOWNOACTIVATE`, which is the one public
/// way to un-maximise without activation; the recovery entry already
/// holds its maximised state for the way back.
pub fn park_window(handle: WindowHandle, site: &ParkingSite) -> Result<ParkedAs> {
    let hwnd = HWND::from(handle);
    if !unsafe { IsWindow(hwnd) }.as_bool() {
        return Err(WindowError::InvalidWindow);
    }
    if unsafe { IsIconic(hwnd) }.as_bool() {
        return Ok(ParkedAs::LeftMinimized);
    }
    let parked_as = if unsafe { IsZoomed(hwnd) }.as_bool() {
        // Two steps, because the shell adjusts a normal position handed
        // to `SetWindowPlacement` so the window stays reachable, which
        // would leave it on a monitor. First it is taken to its normal
        // size where it already was, without activation; then it is
        // moved like any other normal window.
        let (normal, _) = crate::window_placement(handle).ok_or(WindowError::InvalidWindow)?;
        let placement = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            flags: WINDOWPLACEMENT_FLAGS(0),
            showCmd: SW_SHOWNOACTIVATE.0 as u32,
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
        ParkedAs::Unmaximized
    } else {
        ParkedAs::Normal
    };
    let bounds = crate::window_bounds(hwnd)?;
    let (x, y) = site.position_for((bounds.width, bounds.height));
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        )
    }
    .map_err(WindowError::from)?;
    if !is_parked(handle) {
        return Err(WindowError::ParkingNotEffective);
    }
    Ok(parked_as)
}

/// Whether no monitor covers any part of `handle`'s window.
pub fn is_parked(handle: WindowHandle) -> bool {
    let hwnd = HWND::from(handle);
    unsafe { IsWindow(hwnd) }.as_bool()
        && unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL) }.is_invalid()
}

/// The window that owns the foreground right now, for measuring whether
/// a park or restore stole focus.
pub fn foreground() -> Option<WindowHandle> {
    let hwnd = unsafe { GetForegroundWindow() };
    (!hwnd.0.is_null()).then_some(WindowHandle::from(hwnd))
}

/// The show state of `handle` as the shell reports it right now.
pub fn show_state(handle: WindowHandle) -> Option<ShowState> {
    let hwnd = HWND::from(handle);
    if !unsafe { IsWindow(hwnd) }.as_bool() {
        return None;
    }
    Some(if unsafe { IsIconic(hwnd) }.as_bool() {
        ShowState::Minimized
    } else if unsafe { IsZoomed(hwnd) }.as_bool() {
        ShowState::Maximized
    } else {
        ShowState::Normal
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::create_test_window;
    use mosaix_domain::recovery::{RecoveryDraft, RecoveryEntry, RecoveryEntryId};
    use mosaix_domain::{topology_fingerprint, ApplicationId, Rect};
    use windows::Win32::UI::WindowsAndMessaging::{
        DestroyWindow, ShowWindow, SW_SHOWMAXIMIZED, SW_SHOWNOACTIVATE,
    };

    #[test]
    fn the_live_topology_yields_a_site_no_monitor_covers() {
        crate::test_support::ensure_dpi_awareness();
        let displays = crate::enumerate_displays().expect("displays enumerate");

        let site = find_parking_site(&displays).expect("this machine has a free edge");

        assert!(
            verify_parking_site(&site),
            "the site's probe block is beyond every monitor"
        );
        assert_eq!(site.topology_fingerprint, topology_fingerprint(&displays));
        let (capability, reported) = parking_capability(&displays);
        assert_eq!(capability, ParkingCapability::Verified);
        assert_eq!(reported, Some(site));
    }

    #[test]
    fn parking_moves_a_window_off_every_monitor_without_taking_the_foreground() {
        crate::test_support::ensure_dpi_awareness();
        let displays = crate::enumerate_displays().unwrap();
        let site = find_parking_site(&displays).unwrap();
        let hwnd = create_test_window();
        let handle = WindowHandle::from(hwnd);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        crate::move_resize_window(hwnd, Rect::new(100, 100, 320, 240)).unwrap();
        let foreground_before = foreground();

        let parked_as = park_window(handle, &site).unwrap();

        assert_eq!(parked_as, ParkedAs::Normal);
        assert!(is_parked(handle));
        assert_eq!(
            foreground(),
            foreground_before,
            "parking never activates the parked window"
        );
        assert_eq!(show_state(handle), Some(ShowState::Normal));
        let bounds = crate::window_bounds(hwnd).unwrap();
        assert_eq!(
            (bounds.width, bounds.height),
            (320, 240),
            "size is untouched"
        );
        unsafe { DestroyWindow(hwnd) }.unwrap();
    }

    #[test]
    fn a_parked_window_is_restored_to_its_recorded_geometry_by_the_recovery_path() {
        crate::test_support::ensure_dpi_awareness();
        let displays = crate::enumerate_displays().unwrap();
        let site = find_parking_site(&displays).unwrap();
        let hwnd = create_test_window();
        let handle = WindowHandle::from(hwnd);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        let visible = Rect::new(120, 80, 300, 200);
        crate::move_resize_window(hwnd, visible).unwrap();
        let (normal, state) = crate::window_placement(handle).unwrap();
        let live = crate::probe_handle(handle).unwrap();
        let entry = RecoveryEntry {
            id: RecoveryEntryId(1),
            draft: RecoveryDraft {
                session_id: "test".to_owned(),
                native_handle: handle.0,
                process: live.process,
                application_id: ApplicationId("test.exe".to_owned()),
                executable_path: None,
                native_class: live.native_class,
                original_display_fingerprint: displays[0].stable_fingerprint.clone(),
                visible_bounds: visible,
                normal_bounds: normal,
                show_state: state,
                recorded_at_unix: 0,
            },
            parked: true,
            restored: false,
        };

        park_window(handle, &site).unwrap();
        assert!(is_parked(handle));
        crate::restore_window(handle, &entry).unwrap();

        assert!(!is_parked(handle));
        assert_eq!(crate::window_bounds(hwnd).unwrap(), visible);
        unsafe { DestroyWindow(hwnd) }.unwrap();
    }

    #[test]
    fn a_maximized_window_is_parked_at_its_normal_size_and_re_maximizes_on_restore() {
        crate::test_support::ensure_dpi_awareness();
        let displays = crate::enumerate_displays().unwrap();
        let site = find_parking_site(&displays).unwrap();
        let hwnd = create_test_window();
        let handle = WindowHandle::from(hwnd);
        crate::move_resize_window(hwnd, Rect::new(200, 150, 400, 300)).unwrap();
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWMAXIMIZED);
        }
        assert_eq!(show_state(handle), Some(ShowState::Maximized));
        let (normal, state) = crate::window_placement(handle).unwrap();
        assert_eq!(state, ShowState::Maximized);
        let maximized_bounds = crate::window_bounds(hwnd).unwrap();
        let live = crate::probe_handle(handle).unwrap();
        let entry = RecoveryEntry {
            id: RecoveryEntryId(1),
            draft: RecoveryDraft {
                session_id: "test".to_owned(),
                native_handle: handle.0,
                process: live.process,
                application_id: ApplicationId("test.exe".to_owned()),
                executable_path: None,
                native_class: live.native_class,
                original_display_fingerprint: displays[0].stable_fingerprint.clone(),
                visible_bounds: maximized_bounds,
                normal_bounds: normal,
                show_state: ShowState::Maximized,
                recorded_at_unix: 0,
            },
            parked: true,
            restored: false,
        };

        let parked_as = park_window(handle, &site).unwrap();

        assert_eq!(parked_as, ParkedAs::Unmaximized);
        assert!(is_parked(handle));
        assert_eq!(show_state(handle), Some(ShowState::Normal));

        crate::restore_window(handle, &entry).unwrap();
        assert_eq!(show_state(handle), Some(ShowState::Maximized));
        assert!(!is_parked(handle));
        unsafe { DestroyWindow(hwnd) }.unwrap();
    }

    #[test]
    fn a_minimized_window_is_left_minimized_and_a_destroyed_one_is_refused() {
        crate::test_support::ensure_dpi_awareness();
        let displays = crate::enumerate_displays().unwrap();
        let site = find_parking_site(&displays).unwrap();
        let hwnd = create_test_window();
        let handle = WindowHandle::from(hwnd);
        unsafe {
            let _ = ShowWindow(
                hwnd,
                windows::Win32::UI::WindowsAndMessaging::SW_SHOWMINNOACTIVE,
            );
        }
        assert_eq!(show_state(handle), Some(ShowState::Minimized));

        assert_eq!(park_window(handle, &site).unwrap(), ParkedAs::LeftMinimized);
        assert_eq!(show_state(handle), Some(ShowState::Minimized));

        unsafe { DestroyWindow(hwnd) }.unwrap();
        assert!(matches!(
            park_window(handle, &site),
            Err(WindowError::InvalidWindow)
        ));
        assert!(!is_parked(handle));
        assert_eq!(show_state(handle), None);
    }
}
