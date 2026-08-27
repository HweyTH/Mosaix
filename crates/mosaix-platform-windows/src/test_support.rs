//! Shared helpers for creating a real, throwaway top-level window and for
//! polling a channel with a timeout in tests. Used by the move/resize,
//! event-hook, and hotkey test suites.

use std::sync::mpsc::Receiver;
use std::sync::Once;
use std::time::{Duration, Instant};

use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, RegisterClassW, WINDOW_EX_STYLE, WNDCLASSW,
    WS_OVERLAPPEDWINDOW,
};

use crate::enable_per_monitor_dpi_awareness;

static DPI_AWARENESS: Once = Once::new();

/// Ensures per-monitor DPI awareness for tests. Safe to call from any test
/// module in this crate: the first caller wins, later callers are no-ops
/// (a second OS call would return access-denied).
pub(crate) fn ensure_dpi_awareness() {
    DPI_AWARENESS.call_once(|| {
        // Ignore failure: another test may already have set it, or the OS
        // may refuse a second call in-process.
        let _ = enable_per_monitor_dpi_awareness();
    });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

pub(crate) fn create_test_window() -> HWND {
    ensure_dpi_awareness();

    let class_name = w!("MosaixPlatformWindowsSpikeTestWindow");
    let hinstance: HINSTANCE = unsafe { GetModuleHandleW(None) }
        .expect("failed to get module handle")
        .into();

    let class = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: class_name,
        ..Default::default()
    };
    // Ignore failure: a prior test in the same process may have already
    // registered this class.
    unsafe { RegisterClassW(&class) };

    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("mosaix-platform-windows spike test window"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            200,
            150,
            None,
            None,
            hinstance,
            None,
        )
    }
    .expect("failed to create test window")
}

/// Polls `rx` until an event matching `matches` arrives or `timeout` elapses.
pub(crate) fn wait_for<T>(
    rx: &Receiver<T>,
    matches: impl Fn(&T) -> bool,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match rx.recv_timeout(remaining) {
            Ok(event) if matches(&event) => return true,
            Ok(_) => continue,
            Err(_) => return false,
        }
    }
}
