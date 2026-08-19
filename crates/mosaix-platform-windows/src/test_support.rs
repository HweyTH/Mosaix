//! Shared helpers for creating a real, throwaway top-level window in tests.
//! Used by both the move/resize and event-hook test suites.

use std::sync::Once;

use windows::core::w;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, RegisterClassW, WINDOW_EX_STYLE, WNDCLASSW,
    WS_OVERLAPPEDWINDOW,
};

use crate::enable_per_monitor_dpi_awareness;

static DPI_AWARENESS: Once = Once::new();

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

pub(crate) fn create_test_window() -> HWND {
    // A second call in-process would error; harmless to skip here since
    // this is the only place in the test binary that needs it.
    DPI_AWARENESS.call_once(|| {
        enable_per_monitor_dpi_awareness().expect("failed to set DPI awareness");
    });

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
