//! Window enumeration and filtering logic.
//!
//! This module implements the core window discovery pipeline:
//! 1. `EnumWindows` collects all top-level window handles.
//! 2. `is_manageable_window` applies a filter chain to reject non-tileable windows.
//! 3. `build_window_info` extracts full metadata for windows that pass filtering.
//!
//! The filter chain is ordered cheapest-first to minimize Win32 calls
//! on windows that will be rejected early.

use mosaix_domain::{
    ApplicationId, Display, DisplayId, Rect, Window, WindowCapabilities, WindowId, WindowLifecycle,
    WindowRole,
};
use tracing::{debug, trace};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, WS_CAPTION, WS_CHILD, WS_DLGFRAME, WS_EX_TOOLWINDOW, WS_MAXIMIZEBOX,
    WS_MINIMIZEBOX, WS_POPUP, WS_THICKFRAME,
};

use crate::win32_helpers;

/// System window class names that should never be managed.
///
/// These are well-known Windows shell and system surfaces.
const BLOCKED_CLASSES: &[&str] = &[
    "Progman",                               // Desktop program manager
    "WorkerW",                               // Desktop icon container
    "Shell_TrayWnd",                         // Primary taskbar
    "Shell_SecondaryTrayWnd",                // Secondary monitor taskbars
    "Windows.UI.Core.CoreWindow",            // UWP core windows (Start, Search, etc.)
    "ForegroundStaging",                     // DWM staging surface
    "MultitaskingViewFrame",                 // Task View
    "Windows.Internal.Shell.TabProxyWindow", // Edge tab proxy
    "Xaml_WindowedPopupClass",               // XAML popup windows
];

/// Enumerate all top-level window handles via `EnumWindows`.
///
/// # Errors
///
/// Returns an error if `EnumWindows` itself fails (extremely rare).
pub fn enumerate_all_hwnds() -> anyhow::Result<Vec<HWND>> {
    let mut handles: Vec<HWND> = Vec::with_capacity(128);
    unsafe {
        EnumWindows(
            Some(enum_windows_callback),
            LPARAM(&mut handles as *mut Vec<HWND> as isize),
        )?;
    }
    debug!(count = handles.len(), "EnumWindows returned handles");
    Ok(handles)
}

/// Callback for `EnumWindows`. Appends each `HWND` to the collector vec.
unsafe extern "system" fn enum_windows_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let handles = &mut *(lparam.0 as *mut Vec<HWND>);
    handles.push(hwnd);
    TRUE // continue enumeration
}

/// Determine whether a top-level window is worth normalizing for the engine.
///
/// Policy belongs to the ordered rule evaluator and the engine eligibility
/// model, not this adapter. Consequently dialogs, tool windows, hidden or
/// cloaked windows, and elevated windows must cross this seam so they can be
/// reported with stable reasons instead of silently disappearing.
pub fn is_observable_window(hwnd: HWND) -> bool {
    let style = win32_helpers::get_window_style(hwnd);
    if style & WS_CHILD.0 != 0 {
        trace!(?hwnd, "rejected: WS_CHILD");
        return false;
    }

    if let Some(rect) = win32_helpers::get_window_rect(hwnd) {
        if !rect.has_positive_area() {
            trace!(?hwnd, ?rect, "rejected: zero/negative dimensions");
            return false;
        }
    } else {
        trace!(?hwnd, "rejected: GetWindowRect failed");
        return false;
    }

    let class_name = win32_helpers::get_class_name(hwnd);
    if is_blocked_class(&class_name) {
        trace!(?hwnd, %class_name, "rejected: blocked class");
        return false;
    }

    // Special case: ApplicationFrameWindow with empty title is an inactive UWP frame
    if class_name == "ApplicationFrameWindow" {
        let title = win32_helpers::get_window_text(hwnd);
        if title.is_empty() {
            trace!(?hwnd, "rejected: empty-title ApplicationFrameWindow");
            return false;
        }
    }

    true
}

/// Backward-compatible name for callers that used the old pre-policy filter.
pub fn is_manageable_window(hwnd: HWND) -> bool {
    is_observable_window(hwnd)
}

/// Check if a class name is in the blocked list.
fn is_blocked_class(class_name: &str) -> bool {
    BLOCKED_CLASSES
        .iter()
        .any(|blocked| class_name.eq_ignore_ascii_case(blocked))
}

/// Extract full window metadata from a handle.
///
/// This should only be called on handles that have passed `is_manageable_window`.
pub fn build_window_info(hwnd: HWND) -> Window {
    let displays = crate::enumerate_displays().unwrap_or_default();
    build_window_info_with_displays(hwnd, &displays)
}

fn build_window_info_with_displays(hwnd: HWND, displays: &[Display]) -> Window {
    let title = win32_helpers::get_window_text(hwnd);
    let class_name = win32_helpers::get_class_name(hwnd);
    let pid = win32_helpers::get_process_id(hwnd);
    let exe_path = win32_helpers::get_executable_path(pid);
    let bounds = win32_helpers::get_window_rect(hwnd).unwrap_or(Rect::new(0, 0, 0, 0));
    let style = win32_helpers::get_window_style(hwnd);
    let ex_style = win32_helpers::get_window_ex_style(hwnd);

    let application_id = exe_path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("pid:{pid}"));

    let role = classify_role(style, ex_style);
    let capabilities = extract_capabilities(style);
    let display_id = crate::window_display_id(hwnd).unwrap_or(DisplayId(0));
    let lifecycle = determine_lifecycle(hwnd, bounds, display_id, displays);
    let elevated = win32_helpers::is_process_elevated(pid).unwrap_or(true);

    if exe_path.is_none() {
        debug!(
            pid,
            "could not retrieve executable path (process may be elevated)"
        );
    }

    Window {
        id: WindowId(hwnd.0 as isize),
        process_id: pid,
        application_id: ApplicationId(application_id),
        executable_path: exe_path,
        title,
        native_class: if class_name.is_empty() {
            None
        } else {
            Some(class_name)
        },
        role,
        bounds,
        display_id,
        capabilities,
        elevated,
        lifecycle,
    }
}

/// Infer the semantic window role from Win32 style flags.
fn classify_role(style: u32, ex_style: u32) -> WindowRole {
    if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
        return WindowRole::ToolWindow;
    }
    if style & WS_POPUP.0 != 0 {
        // Popup with caption is often a dialog
        if style & WS_DLGFRAME.0 != 0 {
            return WindowRole::Dialog;
        }
        return WindowRole::Popup;
    }
    // WS_DLGFRAME without WS_THICKFRAME suggests a dialog
    if style & WS_DLGFRAME.0 != 0 && style & WS_THICKFRAME.0 == 0 {
        return WindowRole::Dialog;
    }
    WindowRole::Normal
}

/// Extract capability flags from Win32 style bits.
fn extract_capabilities(style: u32) -> WindowCapabilities {
    WindowCapabilities {
        // A window with a caption can generally be moved
        can_move: style & WS_CAPTION.0 != 0,
        // WS_THICKFRAME (sizing border) means resizable
        can_resize: style & WS_THICKFRAME.0 != 0,
        can_minimize: style & WS_MINIMIZEBOX.0 != 0,
        can_maximize: style & WS_MAXIMIZEBOX.0 != 0,
    }
}

/// Determine the current lifecycle state of a window.
fn determine_lifecycle(
    hwnd: HWND,
    bounds: Rect,
    display_id: DisplayId,
    displays: &[Display],
) -> WindowLifecycle {
    if !win32_helpers::is_window_visible(hwnd) {
        return WindowLifecycle::Hidden;
    }
    if win32_helpers::is_window_cloaked(hwnd) {
        return WindowLifecycle::Cloaked;
    }
    if win32_helpers::is_iconic(hwnd) {
        return WindowLifecycle::Minimized;
    }
    if win32_helpers::is_zoomed(hwnd) {
        return WindowLifecycle::Maximized;
    }
    if displays
        .iter()
        .find(|display| display.id == display_id)
        .is_some_and(|display| bounds == display.full_bounds)
    {
        return WindowLifecycle::Fullscreen;
    }
    WindowLifecycle::Active
}

/// Perform the full enumeration pipeline: discover → filter → extract.
///
/// Returns a list of `Window` structs for all manageable windows.
pub fn enumerate_windows() -> anyhow::Result<Vec<Window>> {
    let all_hwnds = enumerate_all_hwnds()?;
    let displays = crate::enumerate_displays().unwrap_or_default();
    let manageable_count = all_hwnds
        .iter()
        .filter(|h| is_observable_window(**h))
        .count();
    debug!(
        total = all_hwnds.len(),
        manageable = manageable_count,
        "window enumeration summary"
    );

    let windows: Vec<Window> = all_hwnds
        .into_iter()
        .filter(|hwnd| is_observable_window(*hwnd))
        .map(|hwnd| build_window_info_with_displays(hwnd, &displays))
        .collect();

    Ok(windows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_classes_are_rejected() {
        assert!(is_blocked_class("Progman"));
        assert!(is_blocked_class("WorkerW"));
        assert!(is_blocked_class("Shell_TrayWnd"));
        assert!(is_blocked_class("shell_traywnd")); // case-insensitive
        assert!(is_blocked_class("Windows.UI.Core.CoreWindow"));
    }

    #[test]
    fn normal_classes_are_allowed() {
        assert!(!is_blocked_class("Notepad"));
        assert!(!is_blocked_class("Chrome_WidgetWin_1"));
        assert!(!is_blocked_class("ApplicationFrameWindow"));
        assert!(!is_blocked_class(""));
    }

    #[test]
    fn role_classification() {
        // Normal window: WS_OVERLAPPEDWINDOW style
        let normal_style = WS_CAPTION.0 | WS_THICKFRAME.0;
        assert_eq!(classify_role(normal_style, 0), WindowRole::Normal);

        // Tool window
        let tool_ex = WS_EX_TOOLWINDOW.0;
        assert_eq!(classify_role(normal_style, tool_ex), WindowRole::ToolWindow);

        // Dialog (popup + dlgframe)
        let dialog_style = WS_POPUP.0 | WS_DLGFRAME.0;
        assert_eq!(classify_role(dialog_style, 0), WindowRole::Dialog);

        // Pure popup
        let popup_style = WS_POPUP.0;
        assert_eq!(classify_role(popup_style, 0), WindowRole::Popup);
    }

    #[test]
    fn capability_extraction() {
        let full_style = WS_CAPTION.0 | WS_THICKFRAME.0 | WS_MINIMIZEBOX.0 | WS_MAXIMIZEBOX.0;
        let caps = extract_capabilities(full_style);
        assert!(caps.can_move);
        assert!(caps.can_resize);
        assert!(caps.can_minimize);
        assert!(caps.can_maximize);
        assert!(caps.is_tileable());

        // No thick frame → not resizable
        let no_resize = WS_CAPTION.0 | WS_MINIMIZEBOX.0;
        let caps2 = extract_capabilities(no_resize);
        assert!(caps2.can_move);
        assert!(!caps2.can_resize);
        assert!(!caps2.is_tileable());
    }
}
