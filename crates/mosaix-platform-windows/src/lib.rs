//! Windows platform adapter: Win32 window management, event hooks, and DPI handling.
//!
//! Two things currently live side by side here, matching the architecture
//! doc's phased approach (section 20, "Phase 0: Platform spikes"): the
//! merged window-enumeration pipeline (`adapter`, `enumeration`,
//! `win32_helpers`) behind the `PlatformAdapter` trait, and standalone
//! Win32 spikes -- move/resize, OS event hooks, display-topology watching,
//! and shutdown handling -- that haven't been wired into `PlatformAdapter`
//! yet. That wiring happens once the risky Win32 behavior in the spikes
//! (DPI handling, coordinate spaces, move/resize semantics, event
//! delivery) has been proven out.

#[cfg(windows)]
pub mod adapter;
#[cfg(windows)]
pub mod display;
#[cfg(windows)]
pub mod enumeration;
#[cfg(windows)]
pub mod events;
#[cfg(windows)]
pub mod focus_border;
#[cfg(windows)]
pub mod hotkeys;
#[cfg(windows)]
pub mod overlay;
#[cfg(windows)]
pub mod parking;
#[cfg(windows)]
pub mod recovery;
#[cfg(windows)]
pub mod shutdown;
#[cfg(all(windows, test))]
mod test_support;
#[cfg(windows)]
pub mod tray;
#[cfg(windows)]
pub mod win32_helpers;

#[cfg(windows)]
pub use adapter::WindowsPlatformAdapter;
#[cfg(windows)]
pub use display::{enumerate_displays, watch_display_topology, DisplayWatcher, TopologyEvent};
#[cfg(windows)]
pub use enumeration::enumerate_windows;
#[cfg(windows)]
pub use events::{start_event_hooks, EventHooks, RawEvent, WindowHandle};
#[cfg(windows)]
pub use focus_border::{start_focus_border, FocusBorder, FocusBorderStyle};
#[cfg(windows)]
pub use hotkeys::{
    probe_hotkey, start_hotkeys, HotkeyAvailability, HotkeyBinding, HotkeyFired,
    HotkeyRegistrationResult, HotkeyRegistrations,
};
#[cfg(windows)]
pub use overlay::{start_preview_overlay, PreviewOverlay};
#[cfg(windows)]
pub use parking::{
    find_parking_site, is_parked, park_window, parking_capability, plan_parking_sites,
    verify_parking_site, ParkedAs, ParkingEdge, ParkingSite, ParkingSiteRefusal,
};
#[cfg(windows)]
pub use recovery::{probe_handle, process_creation_time, restore_window, window_placement};
#[cfg(windows)]
pub use shutdown::register_shutdown_signal;
#[cfg(windows)]
pub use tray::{start_tray, TrayEvent, TrayHandle, TrayStatus};

#[cfg(windows)]
use mosaix_domain::{DisplayId, Rect, WindowId};
#[cfg(windows)]
use windows::Win32::Foundation::{HWND, POINT, RECT};
#[cfg(windows)]
use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONULL};
#[cfg(windows)]
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, GetWindowRect, IsWindow, SetForegroundWindow, SetWindowPos,
    SWP_NOACTIVATE, SWP_NOZORDER,
};

#[cfg(windows)]
#[derive(Debug, thiserror::Error)]
pub enum WindowError {
    #[error("window handle is not a valid window")]
    InvalidWindow,
    #[error("win32 call failed: {0}")]
    Win32(#[from] windows::core::Error),
    #[error("failed to register OS event hooks")]
    EventHookRegistrationFailed,
    #[error("failed to enumerate displays")]
    EnumerateDisplaysFailed,
    #[error("shutdown handler is already registered for this process")]
    ShutdownHandlerAlreadyRegistered,
    #[error("failed to register hotkey: {0}")]
    HotkeyRegistrationFailed(windows::core::Error),
    #[error("failed to start hotkey registration thread")]
    HotkeyThreadStartFailed,
    #[error("the window was moved but a monitor still covers it, so it is not parked")]
    ParkingNotEffective,
}

#[cfg(windows)]
pub type Result<T> = std::result::Result<T, WindowError>;

/// Opts the current process into per-monitor DPI awareness (V2).
///
/// Must be called once, early in process startup, before any window is
/// created or any DPI-dependent call is made -- Win32 does not allow
/// changing DPI awareness after that point. A second call in the same
/// process fails and is surfaced as an error rather than ignored.
#[cfg(windows)]
pub fn enable_per_monitor_dpi_awareness() -> Result<()> {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }
        .map_err(WindowError::from)
}

/// Converts an `events`/`hotkeys`-module [`WindowHandle`] into a domain
/// [`WindowId`]. Lives here rather than in `events.rs`, which is
/// deliberately kept free of a `mosaix-domain` dependency (see that
/// module's doc comment); this is the seam where standalone Win32 spikes
/// meet domain-typed code, same as `enumeration.rs`'s equivalent
/// `WindowId(hwnd.0 as isize)` construction.
#[cfg(windows)]
pub fn window_id_from_handle(handle: WindowHandle) -> WindowId {
    WindowId(handle.0)
}

/// The reverse of [`window_id_from_handle`]: converts a domain [`WindowId`]
/// back to a platform [`WindowHandle`] so callers can pass it to functions
/// like [`observed_window_state`] and [`is_window_elevated`].
#[cfg(windows)]
pub fn window_handle_from_id(id: WindowId) -> WindowHandle {
    WindowHandle(id.0)
}

/// Moves and resizes a top-level window without changing its z-order or
/// activating it. `bounds` is in physical-pixel screen coordinates.
#[cfg(windows)]
pub fn move_resize_window(hwnd: HWND, bounds: Rect) -> Result<()> {
    if !unsafe { IsWindow(hwnd) }.as_bool() {
        return Err(WindowError::InvalidWindow);
    }

    let result = unsafe {
        SetWindowPos(
            hwnd,
            None,
            bounds.x,
            bounds.y,
            bounds.width,
            bounds.height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };

    match result {
        Ok(()) => {
            tracing::debug!(?bounds, ?hwnd, "moved/resized window");
            Ok(())
        }
        Err(err) => {
            tracing::warn!(?bounds, ?hwnd, %err, "SetWindowPos failed");
            Err(WindowError::from(err))
        }
    }
}

/// Like [`move_resize_window`], but takes the domain [`WindowId`] a caller
/// outside this crate actually has (from [`mosaix_engine::EngineState`]'s
/// tracked placements), converting to the raw `HWND` at this boundary so
/// callers like `mosaix-agent`'s executor never need to depend on `windows`
/// crate types themselves.
#[cfg(windows)]
pub fn move_resize_window_by_id(window_id: WindowId, bounds: Rect) -> Result<()> {
    move_resize_window(HWND::from(WindowHandle(window_id.0)), bounds)
}

/// Activates the managed window selected by directional focus.
#[cfg(windows)]
pub fn focus_window_by_id(window_id: WindowId) -> Result<()> {
    let hwnd = HWND::from(WindowHandle(window_id.0));
    if !unsafe { IsWindow(hwnd) }.as_bool() {
        return Err(WindowError::InvalidWindow);
    }
    if unsafe { SetForegroundWindow(hwnd) }.as_bool() {
        Ok(())
    } else {
        Err(WindowError::Win32(windows::core::Error::from_win32()))
    }
}

/// Reads a top-level window's current position and size in physical-pixel
/// screen coordinates.
#[cfg(windows)]
pub fn window_bounds(hwnd: HWND) -> Result<Rect> {
    if !unsafe { IsWindow(hwnd) }.as_bool() {
        return Err(WindowError::InvalidWindow);
    }

    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.map_err(WindowError::from)?;

    Ok(Rect::new(
        rect.left,
        rect.top,
        rect.right - rect.left,
        rect.bottom - rect.top,
    ))
}

/// The display currently containing `hwnd`, identified by
/// `MonitorFromWindow` -- consistent with [`display::enumerate_displays`]'s
/// `DisplayId(hmonitor.0 as isize)`. `None` if the window has no
/// associated monitor (an invalid handle, most commonly).
#[cfg(windows)]
pub fn window_display_id(hwnd: HWND) -> Option<DisplayId> {
    let hmonitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL) };
    if hmonitor.is_invalid() {
        None
    } else {
        Some(DisplayId(hmonitor.0 as isize))
    }
}

/// The display and bounds a `LocationChanged` observation should report
/// for `handle`, in one call -- the bridging seam for callers (like
/// `mosaix-agent`'s bounds-observed forwarder) that only have an
/// `events`-module [`WindowHandle`], not an `HWND`, and so don't want the
/// `windows` crate as a direct dependency, same idea as
/// [`window_id_from_handle`]. `None` if either underlying read fails (the
/// window has since been destroyed, most commonly).
#[cfg(windows)]
pub fn observed_window_state(handle: WindowHandle) -> Option<(DisplayId, Rect)> {
    let hwnd = HWND::from(handle);
    let bounds = window_bounds(hwnd).ok()?;
    let display_id = window_display_id(hwnd)?;
    Some((display_id, bounds))
}

/// Narrows a raw foreground `HWND` to a [`WindowHandle`], or `None` when
/// nothing owns the foreground -- `GetForegroundWindow` legitimately
/// returns a null handle when the foreground belongs to another desktop or
/// is being switched. Split out from [`foreground_window_handle`] so the
/// null guard is exercised without depending on which window happens to be
/// foreground while the tests run.
#[cfg(windows)]
fn foreground_handle_from_hwnd(hwnd: HWND) -> Option<WindowHandle> {
    if hwnd.0.is_null() {
        None
    } else {
        Some(WindowHandle::from(hwnd))
    }
}

/// The window that currently owns the foreground.
///
/// `EngineState::focused_window` is otherwise only ever written from a
/// foreground-*change* notification, so without this the engine has no
/// focus anchor between agent startup and whenever the user next switches
/// windows -- which leaves directional focus/swap as silent no-ops and the
/// Focus border hidden even though automatic tiling is active (spec #11
/// user stories 39 and 43). `mosaix-agent` reads it once during startup
/// reconciliation and feeds it in as an ordinary `Event::WindowFocused`.
#[cfg(windows)]
pub fn foreground_window_handle() -> Option<WindowHandle> {
    foreground_handle_from_hwnd(unsafe { GetForegroundWindow() })
}

/// Returns `true` if the window's owning process is running elevated (as
/// Administrator) or if the token cannot be inspected (which implies an
/// elevated or protected process that Mosaix cannot manage regardless).
///
/// This is a convenience wrapper for [`win32_helpers::is_process_elevated`]
/// used by the placement executor to distinguish "the window is elevated and
/// UIPI blocks us" from other failure modes when `SetWindowPos` returns an
/// error.
pub fn is_window_elevated(handle: WindowHandle) -> bool {
    let hwnd = HWND::from(handle);
    let pid = win32_helpers::get_process_id(hwnd);
    // Treat None (can't open token) the same as Some(true): either way we
    // cannot interact with the window.
    win32_helpers::is_process_elevated(pid).unwrap_or(true)
}

/// The current cursor position in physical-pixel screen coordinates
/// (`GetCursorPos`). Used by the snap-preview drag controller (Feature 34).
#[cfg(windows)]
pub fn cursor_position() -> Result<(i32, i32)> {
    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point) }.map_err(WindowError::from)?;
    Ok((point.x, point.y))
}

#[cfg(all(windows, test))]
mod tests {
    use super::*;
    use crate::test_support::create_test_window;
    use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;

    #[test]
    fn move_resize_and_invalid_handle() {
        let hwnd = create_test_window();

        let target = Rect::new(100, 80, 400, 300);
        move_resize_window(hwnd, target).expect("move/resize should succeed");

        let actual = window_bounds(hwnd).expect("should read back bounds");
        assert_eq!(actual, target);

        let invalid = HWND(std::ptr::null_mut());
        let err = move_resize_window(invalid, Rect::new(0, 0, 100, 100));
        assert!(matches!(err, Err(WindowError::InvalidWindow)));

        unsafe { DestroyWindow(hwnd) }.expect("cleanup should succeed");
    }

    #[test]
    fn window_display_id_resolves_a_real_window_and_rejects_an_invalid_handle() {
        let hwnd = create_test_window();

        assert!(
            window_display_id(hwnd).is_some(),
            "a real, on-screen window should resolve to a display"
        );

        let invalid = HWND(std::ptr::null_mut());
        assert_eq!(window_display_id(invalid), None);

        unsafe { DestroyWindow(hwnd) }.expect("cleanup should succeed");
    }

    #[test]
    fn observed_window_state_reports_bounds_and_display_from_a_handle() {
        let hwnd = create_test_window();
        let target = Rect::new(50, 60, 300, 200);
        move_resize_window(hwnd, target).expect("move/resize should succeed");

        let handle = WindowHandle::from(hwnd);
        let (display_id, bounds) = observed_window_state(handle).expect("should read back state");

        assert_eq!(bounds, target);
        assert_eq!(Some(display_id), window_display_id(hwnd));

        unsafe { DestroyWindow(hwnd) }.expect("cleanup should succeed");
    }

    #[test]
    fn foreground_handle_narrows_a_real_window_and_rejects_a_null_foreground() {
        let hwnd = create_test_window();

        assert_eq!(
            foreground_handle_from_hwnd(hwnd),
            Some(WindowHandle::from(hwnd)),
            "a real foreground window must become the engine's focus anchor"
        );
        assert_eq!(
            foreground_handle_from_hwnd(HWND(std::ptr::null_mut())),
            None,
            "no window owning the foreground must not be reported as focused"
        );

        unsafe { DestroyWindow(hwnd) }.expect("cleanup should succeed");
    }
}
