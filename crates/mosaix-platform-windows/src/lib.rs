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
pub mod hotkeys;
#[cfg(windows)]
pub mod shutdown;
#[cfg(windows)]
pub mod win32_helpers;
#[cfg(all(windows, test))]
mod test_support;

#[cfg(windows)]
pub use adapter::WindowsPlatformAdapter;
#[cfg(windows)]
pub use display::{enumerate_displays, watch_display_topology, DisplayWatcher, TopologyEvent};
#[cfg(windows)]
pub use events::{start_event_hooks, EventHooks, RawEvent, WindowHandle};
#[cfg(windows)]
pub use hotkeys::{
    start_hotkeys, HotkeyBinding, HotkeyFired, HotkeyRegistrationResult, HotkeyRegistrations,
};
#[cfg(windows)]
pub use shutdown::register_shutdown_signal;

#[cfg(windows)]
use mosaix_domain::Rect;
#[cfg(windows)]
use windows::Win32::Foundation::{HWND, RECT};
#[cfg(windows)]
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, IsWindow, SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER,
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
}
