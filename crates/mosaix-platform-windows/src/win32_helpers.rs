//! Safe Rust wrappers around raw Win32 API calls.
//!
//! Each function encapsulates `unsafe` blocks and returns safe Rust types.
//! These are low-level building blocks used by the enumeration module.

use std::path::PathBuf;

use mosaix_domain::Rect;
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{
    OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetWindowLongW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, IsZoomed, GWL_EXSTYLE, GWL_STYLE,
};

/// Retrieve the window title text.
///
/// Returns an empty string if the window has no title or the call fails.
pub fn get_window_text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len == 0 {
            return String::new();
        }
        // +1 for the null terminator
        let mut buf = vec![0u16; (len + 1) as usize];
        let copied = GetWindowTextW(hwnd, &mut buf);
        if copied == 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..copied as usize])
    }
}

/// Retrieve the Win32 window class name.
///
/// Returns an empty string if the call fails.
pub fn get_class_name(hwnd: HWND) -> String {
    unsafe {
        let mut buf = [0u16; 256];
        let len = GetClassNameW(hwnd, &mut buf);
        if len == 0 {
            return String::new();
        }
        String::from_utf16_lossy(&buf[..len as usize])
    }
}

/// Get the window bounding rectangle in screen coordinates.
///
/// Returns `None` if the call fails.
pub fn get_window_rect(hwnd: HWND) -> Option<Rect> {
    unsafe {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_ok() {
            Some(Rect::new(
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
            ))
        } else {
            None
        }
    }
}

/// Get the `GWL_STYLE` flags for a window.
pub fn get_window_style(hwnd: HWND) -> u32 {
    unsafe { GetWindowLongW(hwnd, GWL_STYLE) as u32 }
}

/// Get the `GWL_EXSTYLE` (extended style) flags for a window.
pub fn get_window_ex_style(hwnd: HWND) -> u32 {
    unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 }
}

/// Get the process ID that owns a window.
pub fn get_process_id(hwnd: HWND) -> u32 {
    unsafe {
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid
    }
}

/// Attempt to retrieve the full executable path for a given process ID.
///
/// Returns `None` if the process cannot be opened (e.g. elevated process
/// when we are running unelevated) or the query fails.
pub fn get_executable_path(pid: u32) -> Option<PathBuf> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(process);
        if result.is_ok() && size > 0 {
            let path_str = String::from_utf16_lossy(&buf[..size as usize]);
            Some(PathBuf::from(path_str))
        } else {
            None
        }
    }
}

/// Check if a window is visible according to `IsWindowVisible`.
pub fn is_window_visible(hwnd: HWND) -> bool {
    unsafe { IsWindowVisible(hwnd).as_bool() }
}

/// Check if a window is cloaked by DWM.
///
/// Cloaked windows include those on other virtual desktops and
/// UWP app frames that are not currently active.
pub fn is_window_cloaked(hwnd: HWND) -> bool {
    unsafe {
        let mut cloaked: u32 = 0;
        let result = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        );
        result.is_ok() && cloaked != 0
    }
}

/// Check if a window is minimized (iconic).
pub fn is_iconic(hwnd: HWND) -> bool {
    unsafe { IsIconic(hwnd).as_bool() }
}

/// Check if a window is maximized (zoomed).
pub fn is_zoomed(hwnd: HWND) -> bool {
    unsafe { IsZoomed(hwnd).as_bool() }
}

/// Check whether the process owning `pid` is running elevated (as Administrator).
///
/// Returns:
/// - `Some(true)` — the process is elevated (high mandatory integrity level).
/// - `Some(false)` — the process is not elevated.
/// - `None` — the process token could not be opened.  This typically means
///   Mosaix itself is running unelevated and the target process is elevated
///   (or is a protected system process), which is functionally equivalent to
///   `Some(true)` for the purposes of window management: we cannot send it
///   `SetWindowPos` calls either way.
///
/// Callers that cannot distinguish `None` from `Some(true)` should treat them
/// identically — if we cannot even inspect the token, we certainly cannot
/// manage the window.
pub fn is_process_elevated(pid: u32) -> Option<bool> {
    unsafe {
        // We only need PROCESS_QUERY_LIMITED_INFORMATION to open the token.
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut token = windows::Win32::Foundation::HANDLE::default();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
        let _ = CloseHandle(process);
        if !opened.is_ok() {
            return None;
        }

        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut return_length: u32 = 0;
        let queried = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut TOKEN_ELEVATION as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut return_length,
        );
        let _ = CloseHandle(token);

        if queried.is_ok() {
            Some(elevation.TokenIsElevated != 0)
        } else {
            None
        }
    }
}
