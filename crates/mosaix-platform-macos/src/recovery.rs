//! Verifying recorded native handles on macOS (ADR 0023).
//!
//! The recovery ledger records a native handle together with the process
//! instance that owned it, so that a later session -- or the
//! out-of-process restore command after a crash -- can decide whether the
//! handle still means the window it did before moving anything. This
//! module answers the platform half of that question and hands the
//! answer to the platform-neutral verdict in `mosaix_domain::recovery`.
//!
//! Every call here is public API: `CGWindowListCopyWindowInfo` and
//! `proc_pidinfo(PROC_PIDTBSDINFO)`. Nothing reads another process's
//! memory, injects, or needs reduced System Integrity Protection.
//!
//! ## What identity means here, and how it differs from Windows
//!
//! On Windows a handle is an `HWND`, which the system reuses freely once
//! its window is destroyed; the class name is therefore load-bearing
//! evidence against a reused handle. On macOS the handle is a Core
//! Graphics window id, which the window server allocates monotonically
//! and does not hand out again within a session, and the strong evidence
//! is the owning process instance: a pid paired with the kernel's start
//! time for it.
//!
//! This module deliberately reports `native_class: None`. The class a
//! macOS window is recorded with is its Accessibility subrole, and there
//! is no public way to go from a Core Graphics window id back to the
//! Accessibility element that would report it. `verdict_for` treats an
//! unknown live class as "no disagreement" and decides on the process
//! instance alone, which is the honest reading of what this platform can
//! actually witness -- not a class check that quietly always passes.

use std::ffi::c_void;

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use mosaix_domain::recovery::{LiveHandleEvidence, ProcessInstance, RecoveryEntry, ShowState};
use mosaix_domain::Rect;

use crate::accessibility::{
    ApplicationElement, Element, K_AX_FULL_SCREEN_ATTRIBUTE, K_AX_MINIMIZED_ATTRIBUTE,
    K_AX_TITLE_ATTRIBUTE,
};
use crate::events::WindowHandle;
use crate::{MacosError, Result};

/// Ask only about the one window named by `relative_to_window`.
const K_CG_WINDOW_LIST_OPTION_INCLUDING_WINDOW: u32 = 1 << 3;
/// Ask about every window currently on screen, front to back.
const K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1 << 0;
/// The window server's "no window" id, used to mean "not relative to any
/// particular window".
const K_CG_NULL_WINDOW_ID: u32 = 0;
/// The layer ordinary application windows live on. The menu bar, the
/// Dock, and overlays sit on higher layers and are never the foreground
/// window in the sense the engine means.
const NORMAL_WINDOW_LAYER: i64 = 0;

extern "C" {
    fn CGWindowListCopyWindowInfo(
        option: u32,
        relative_to_window: u32,
    ) -> core_foundation::array::CFArrayRef;
}

/// The kernel's start time for the process `pid`, in microseconds since
/// the Unix epoch, or `None` when no such process exists.
///
/// Paired with the pid this names one process instance, exactly as
/// `GetProcessTimes` does on Windows: a later process handed the same id
/// has a different start time, so a recycled pid cannot pass for the one
/// the ledger recorded.
pub fn process_creation_time(pid: u32) -> Option<u64> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let read = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut libc::proc_bsdinfo as *mut c_void,
            size,
        )
    };
    // proc_pidinfo returns the number of bytes it wrote. A short read is
    // as untrustworthy as a failed one, and a process that is not running
    // reports nothing at all.
    if read != size {
        return None;
    }
    Some(info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
}

/// What the window server reports about one window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowServerInfo {
    pub owner_pid: u32,
    /// The window's frame in global top-left coordinates, which is the
    /// same space `AXPosition` reports in.
    pub bounds: Rect,
    /// The window's name. Reading it needs Screen Recording permission on
    /// macOS 10.15 and later, so `None` here means "not observable",
    /// never "the window is untitled".
    pub title: Option<String>,
}

fn number(info: &CFDictionary<CFString, CFType>, key: &str) -> Option<i64> {
    info.find(CFString::new(key))?
        .downcast::<CFNumber>()?
        .to_i64()
}

/// What the window server reports about `window_id`, or `None` when it
/// knows no such window.
pub fn window_server_info(window_id: u32) -> Option<WindowServerInfo> {
    let array =
        unsafe { CGWindowListCopyWindowInfo(K_CG_WINDOW_LIST_OPTION_INCLUDING_WINDOW, window_id) };
    if array.is_null() {
        return None;
    }
    let windows: CFArray<*const c_void> = unsafe { CFArray::wrap_under_create_rule(array) };
    let entry = windows.get(0)?;
    let info: CFDictionary<CFString, CFType> =
        unsafe { CFDictionary::wrap_under_get_rule(*entry as _) };

    let owner_pid = u32::try_from(number(&info, "kCGWindowOwnerPID")?).ok()?;
    let frame = info.find(CFString::new("kCGWindowBounds"))?;
    let frame: CFDictionary<CFString, CFType> =
        unsafe { CFDictionary::wrap_under_get_rule(frame.as_CFTypeRef() as _) };
    let bounds = Rect::new(
        number(&frame, "X")? as i32,
        number(&frame, "Y")? as i32,
        number(&frame, "Width")? as i32,
        number(&frame, "Height")? as i32,
    );
    let title = info
        .find(CFString::new("kCGWindowName"))
        .and_then(|name| name.downcast::<CFString>())
        .map(|name| name.to_string())
        .filter(|name| !name.is_empty());

    Some(WindowServerInfo {
        owner_pid,
        bounds,
        title,
    })
}

/// The pid that owns the Core Graphics window `window_id`, or `None`
/// when the window server knows no such window.
pub fn window_owner_pid(window_id: u32) -> Option<u32> {
    window_server_info(window_id).map(|info| info.owner_pid)
}

/// How far an Accessibility frame may sit from the window server's frame
/// and still be the same window.
///
/// The two report the same geometry, but they are sampled at different
/// instants and an animating window can be a pixel or two apart between
/// them. This is deliberately tight: it is a tolerance for sampling
/// skew, not a similarity score.
const FRAME_TOLERANCE: i32 = 2;

fn frames_agree(a: Rect, b: Rect) -> bool {
    (a.x - b.x).abs() <= FRAME_TOLERANCE
        && (a.y - b.y).abs() <= FRAME_TOLERANCE
        && (a.width - b.width).abs() <= FRAME_TOLERANCE
        && (a.height - b.height).abs() <= FRAME_TOLERANCE
}

/// Whether one Accessibility window is the one the window server calls
/// `window_id`. Pure, so the matching rule is testable without a desktop.
///
/// The frame must agree. A title is additional evidence only when both
/// sides can see one: `kCGWindowName` needs Screen Recording permission,
/// which Mosaix does not require, so an absent title must not be read as
/// a mismatch.
pub fn is_same_window(server: &WindowServerInfo, ax_frame: Rect, ax_title: Option<&str>) -> bool {
    if !frames_agree(server.bounds, ax_frame) {
        return false;
    }
    match (server.title.as_deref(), ax_title) {
        (Some(server_title), Some(ax_title)) => server_title == ax_title,
        _ => true,
    }
}

/// Resolves the Accessibility element that can actually move the window
/// the window server calls `window_id`.
///
/// macOS has no public mapping from a Core Graphics window id to an
/// `AXUIElement`; the private route through remote tokens is exactly what
/// ADR 0023 forbids. What is public is that both sides describe the same
/// window, so this asks the owning application for its windows and keeps
/// the ones whose frame -- and title, where both sides can see one --
/// agree with the window server's account.
///
/// Exactly one survivor is an identification. **Anything else is a
/// refusal**: no candidate means the window is gone or unaddressable, and
/// more than one means two windows of the same app are indistinguishable
/// through public API. Both refuse rather than guess, because the cost of
/// guessing is moving a window the user did not ask us to touch.
pub fn resolve_window(window_id: u32) -> Result<Element> {
    let server = window_server_info(window_id).ok_or(MacosError::WindowNotResolvable(window_id))?;
    let application = ApplicationElement::for_process(server.owner_pid)?;

    let mut matches: Vec<Element> = application
        .windows()
        .into_iter()
        .filter(|window| {
            let (Some((x, y)), Some((width, height))) = (window.position(), window.size()) else {
                return false;
            };
            is_same_window(
                &server,
                Rect::new(x, y, width, height),
                window.string_attribute(K_AX_TITLE_ATTRIBUTE).as_deref(),
            )
        })
        .collect();

    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(MacosError::WindowNotResolvable(window_id)),
        candidates => Err(MacosError::AmbiguousWindow {
            window_id,
            candidates,
        }),
    }
}

/// Whether the window `window_id` is minimized, as Accessibility reports
/// it right now.
pub fn is_minimized(window_id: u32) -> Option<bool> {
    resolve_window(window_id)
        .ok()?
        .bool_attribute(K_AX_MINIMIZED_ATTRIBUTE)
}

/// What `handle` names right now: the owning process instance, or `None`
/// when it names no window at all.
///
/// See the module note on why the live class is deliberately unknown.
pub fn probe_handle(handle: WindowHandle) -> Option<LiveHandleEvidence> {
    let window_id = u32::try_from(handle.0).ok()?;
    let process_id = window_owner_pid(window_id)?;
    Some(LiveHandleEvidence {
        process: ProcessInstance {
            process_id,
            creation_time: process_creation_time(process_id).unwrap_or(0),
        },
        native_class: None,
    })
}

/// The window's current bounds and show state, as Accessibility reports
/// them. This is what a recovery entry records as `normal_bounds` and
/// `show_state`.
///
/// Unlike Windows, macOS keeps no separate "restored" rectangle that can
/// be read while a window is full screen or minimized: `AXPosition` and
/// `AXSize` describe the window as it is now. For a normal window that is
/// the same answer `GetWindowPlacement` gives. For a full-screen one it
/// is the full-screen frame, and the pre-full-screen geometry is simply
/// not observable through public API -- a limitation the ledger records
/// rather than a number this guesses.
pub fn window_placement(handle: WindowHandle) -> Option<(Rect, ShowState)> {
    let window_id = u32::try_from(handle.0).ok()?;
    let element = resolve_window(window_id).ok()?;
    let (x, y) = element.position()?;
    let (width, height) = element.size()?;
    Some((Rect::new(x, y, width, height), show_state_of(&element)))
}

/// The show state Accessibility reports for `element`.
///
/// macOS full screen is mapped to [`ShowState::Maximized`]: it is the
/// state the window returns to, and the domain's three-way show state has
/// no separate full-screen case.
pub fn show_state_of(element: &Element) -> ShowState {
    if element.bool_attribute(K_AX_MINIMIZED_ATTRIBUTE) == Some(true) {
        ShowState::Minimized
    } else if element.bool_attribute(K_AX_FULL_SCREEN_ATTRIBUTE) == Some(true) {
        ShowState::Maximized
    } else {
        ShowState::Normal
    }
}

/// Puts a verified window back where `entry` recorded it.
///
/// A normal window is moved and sized through `AXPosition`/`AXSize`,
/// neither of which raises or activates it -- macOS is better than
/// Windows here, where re-maximising has to go through a call the
/// documentation says activates the window.
///
/// Re-entering full screen is the one exception: macOS animates the
/// transition and gives the window the foreground, and there is no public
/// way to ask for the state without the animation. That is a measured
/// limitation of the platform, not a choice.
///
/// A window recorded as minimized was never moved -- parking leaves a
/// minimized window alone -- so restoring it only asserts the state it
/// should already be in.
pub fn restore_window(handle: WindowHandle, entry: &RecoveryEntry) -> Result<()> {
    let window_id = u32::try_from(handle.0).map_err(|_| MacosError::WindowNotResolvable(0))?;
    let element = resolve_window(window_id)?;
    match entry.draft.show_state {
        ShowState::Minimized => element.set_bool_attribute(K_AX_MINIMIZED_ATTRIBUTE, true),
        ShowState::Maximized => {
            let normal = entry.draft.normal_bounds;
            element.set_position(normal.x, normal.y)?;
            element.set_size(normal.width, normal.height)?;
            element.set_bool_attribute(K_AX_FULL_SCREEN_ATTRIBUTE, true)
        }
        ShowState::Normal => {
            let visible = entry.draft.visible_bounds;
            element.set_position(visible.x, visible.y)?;
            element.set_size(visible.width, visible.height)
        }
    }
}

/// The Core Graphics id of the frontmost ordinary window, or `None` when
/// nothing ordinary is on screen.
///
/// The window server returns on-screen windows front to back, so this is
/// the first entry on the normal window layer. Reading the order is
/// public API and needs no AppKit main thread.
pub fn frontmost_window_id() -> Option<u32> {
    let array = unsafe {
        CGWindowListCopyWindowInfo(K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY, K_CG_NULL_WINDOW_ID)
    };
    if array.is_null() {
        return None;
    }
    let windows: CFArray<*const c_void> = unsafe { CFArray::wrap_under_create_rule(array) };
    windows.iter().find_map(|entry| {
        let info: CFDictionary<CFString, CFType> =
            unsafe { CFDictionary::wrap_under_get_rule(*entry as _) };
        (number(&info, "kCGWindowLayer")? == NORMAL_WINDOW_LAYER)
            .then(|| u32::try_from(number(&info, "kCGWindowNumber")?).ok())
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mosaix_domain::recovery::{
        verdict_for, HandleVerdict, RecoveryDraft, RecoveryEntry, RecoveryEntryId,
    };
    use mosaix_domain::{ApplicationId, Rect};

    fn entry_recording(process: ProcessInstance, class: Option<&str>) -> RecoveryEntry {
        RecoveryEntry {
            id: RecoveryEntryId(1),
            draft: RecoveryDraft {
                session_id: "test".to_owned(),
                native_handle: 1,
                process,
                application_id: ApplicationId("test.app".to_owned()),
                executable_path: None,
                native_class: class.map(str::to_owned),
                original_display_fingerprint: "D".to_owned(),
                visible_bounds: Rect::new(0, 0, 200, 150),
                normal_bounds: Rect::new(0, 0, 200, 150),
                show_state: mosaix_domain::recovery::ShowState::Normal,
                recorded_at_unix: 0,
            },
            parked: true,
            restored: false,
        }
    }

    #[test]
    fn this_process_has_a_start_time_and_it_is_stable_across_reads() {
        let pid = std::process::id();

        let first = process_creation_time(pid).expect("this process is running");
        let second = process_creation_time(pid).expect("this process is still running");

        assert_eq!(first, second, "a process instance has one start time");
        assert!(first > 0, "the kernel reports a real start time");
    }

    #[test]
    fn a_pid_that_is_not_running_has_no_start_time() {
        // pid 0 is the kernel and is never returned by KERN_PROC_PID as a
        // user process, so it stands in for "no such process instance".
        assert_eq!(process_creation_time(u32::MAX), None);
    }

    #[test]
    fn a_window_id_the_window_server_does_not_know_probes_to_nothing() {
        // CG window ids are allocated monotonically from a low base, so a
        // very high id names no window on any real session.
        assert_eq!(probe_handle(WindowHandle(u32::MAX as isize - 1)), None);
    }

    fn server_window(bounds: Rect, title: Option<&str>) -> WindowServerInfo {
        WindowServerInfo {
            owner_pid: 4242,
            bounds,
            title: title.map(str::to_owned),
        }
    }

    #[test]
    fn a_window_whose_frame_and_title_agree_is_the_same_window() {
        let server = server_window(Rect::new(100, 200, 800, 600), Some("Notes"));

        assert!(is_same_window(
            &server,
            Rect::new(100, 200, 800, 600),
            Some("Notes")
        ));
    }

    #[test]
    fn a_frame_that_disagrees_is_a_different_window_whatever_the_title() {
        let server = server_window(Rect::new(100, 200, 800, 600), Some("Notes"));

        assert!(!is_same_window(
            &server,
            Rect::new(900, 200, 800, 600),
            Some("Notes")
        ));
    }

    #[test]
    fn sampling_skew_within_the_tolerance_is_still_the_same_window() {
        let server = server_window(Rect::new(100, 200, 800, 600), None);

        assert!(
            is_same_window(&server, Rect::new(101, 199, 800, 601), None),
            "a pixel of drift between two samples is not a different window"
        );
        assert!(
            !is_same_window(&server, Rect::new(105, 200, 800, 600), None),
            "the tolerance is for skew, not for similarity"
        );
    }

    #[test]
    fn an_unreadable_title_is_not_treated_as_a_mismatch() {
        // kCGWindowName needs Screen Recording permission, which Mosaix
        // does not ask for. Absent evidence must not read as contrary
        // evidence, or every window would fail to resolve.
        let server = server_window(Rect::new(0, 0, 400, 300), None);

        assert!(is_same_window(
            &server,
            Rect::new(0, 0, 400, 300),
            Some("Untitled")
        ));
    }

    #[test]
    fn two_different_titles_at_the_same_frame_are_not_the_same_window() {
        let server = server_window(Rect::new(0, 0, 400, 300), Some("Inbox"));

        assert!(!is_same_window(
            &server,
            Rect::new(0, 0, 400, 300),
            Some("Drafts")
        ));
    }

    #[test]
    fn the_frontmost_window_is_one_this_machine_actually_reports() {
        // A live check against the window server on the host running the
        // suite: whatever it names as frontmost must be a window it can
        // then describe.
        if let Some(id) = frontmost_window_id() {
            assert!(
                window_server_info(id).is_some(),
                "the frontmost window id resolves to window server info"
            );
        }
    }

    #[test]
    fn a_recycled_pid_is_rejected_even_though_the_class_is_unknown() {
        let recorded = ProcessInstance {
            process_id: 4242,
            creation_time: 1_000,
        };
        let entry = entry_recording(recorded, Some("AXStandardWindow"));

        // The same pid, a different instance: this is what a reused handle
        // looks like on macOS, and the start time is what catches it.
        let live = LiveHandleEvidence {
            process: ProcessInstance {
                process_id: 4242,
                creation_time: 9_999,
            },
            native_class: None,
        };

        assert!(matches!(
            verdict_for(&entry, Some(live)),
            HandleVerdict::Reused { .. }
        ));
    }

    #[test]
    fn the_same_process_instance_verifies_despite_an_unknown_live_class() {
        let recorded = ProcessInstance {
            process_id: 4242,
            creation_time: 1_000,
        };
        let entry = entry_recording(recorded, Some("AXStandardWindow"));

        let live = LiveHandleEvidence {
            process: recorded,
            native_class: None,
        };

        assert_eq!(verdict_for(&entry, Some(live)), HandleVerdict::Verified);
    }
}
