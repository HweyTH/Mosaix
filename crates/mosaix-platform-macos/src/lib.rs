//! macOS platform adapter: Accessibility API, AppKit integration, and coordinate conversion.

mod model;

pub use model::{
    classify_role, display_fingerprint, flip_y, is_blocked_bundle_id, MacosWindowRole,
};

#[cfg(target_os = "macos")]
pub mod accessibility;
#[cfg(target_os = "macos")]
pub mod adapter;
#[cfg(target_os = "macos")]
pub mod display;
#[cfg(target_os = "macos")]
pub mod enumeration;
#[cfg(target_os = "macos")]
pub mod events;
#[cfg(target_os = "macos")]
pub mod hotkeys;
#[cfg(target_os = "macos")]
pub mod menubar;
#[cfg(target_os = "macos")]
pub mod overlay;
#[cfg(target_os = "macos")]
pub mod shutdown;

#[cfg(target_os = "macos")]
pub use adapter::MacosPlatformAdapter;
#[cfg(target_os = "macos")]
pub use display::{enumerate_displays, watch_display_topology, DisplayWatcher, TopologyEvent};
#[cfg(target_os = "macos")]
pub use enumeration::enumerate_windows;
#[cfg(target_os = "macos")]
pub use events::{start_event_hooks, EventHooks, RawEvent, WindowHandle};
#[cfg(target_os = "macos")]
pub use hotkeys::{
    start_hotkeys, HotkeyBinding, HotkeyFired, HotkeyRegistrationResult, HotkeyRegistrations,
};
#[cfg(target_os = "macos")]
pub use menubar::{start_menu_bar, MenuBarEvent, MenuBarHandle};
#[cfg(target_os = "macos")]
pub use overlay::{start_preview_overlay, PreviewOverlay};
#[cfg(target_os = "macos")]
pub use shutdown::register_shutdown_signal;

#[cfg(target_os = "macos")]
#[derive(Debug, thiserror::Error)]
pub enum MacosError {
    #[error("Mosaix requires Accessibility permission. Enable it in System Settings > Privacy & Security > Accessibility")]
    AccessibilityPermissionDenied,
    #[error("Accessibility API error: {0}")]
    Accessibility(i32),
    #[error("Core Graphics error: {0}")]
    CoreGraphics(String),
    #[error("failed to start platform event loop")]
    EventLoopStartFailed,
    #[error("shutdown handler is already registered for this process")]
    ShutdownHandlerAlreadyRegistered,
}

#[cfg(target_os = "macos")]
pub type Result<T> = std::result::Result<T, MacosError>;

#[cfg(test)]
mod tests {
    use super::*;
    use mosaix_domain::WindowRole;

    #[test]
    fn flips_core_graphics_y_to_top_left_coordinates() {
        assert_eq!(flip_y(100, 200, 1_080), 780);
        assert_eq!(flip_y(-100, 400, 1_080), 780);
    }

    #[test]
    fn excludes_known_system_bundle_ids() {
        assert!(is_blocked_bundle_id("com.apple.dock"));
        assert!(is_blocked_bundle_id("com.apple.controlcenter"));
        assert!(!is_blocked_bundle_id("com.apple.Safari"));
    }

    #[test]
    fn maps_accessibility_subroles_to_domain_roles() {
        assert_eq!(
            classify_role("AXWindow", Some("AXStandardWindow")),
            WindowRole::Normal
        );
        assert_eq!(
            classify_role("AXWindow", Some("AXDialog")),
            WindowRole::Dialog
        );
        assert_eq!(
            classify_role("AXWindow", Some("AXFloatingWindow")),
            WindowRole::ToolWindow
        );
    }

    #[test]
    fn display_fingerprint_includes_hardware_identity_and_resolution() {
        assert_eq!(
            display_fingerprint(1552, 628, 42, 3_456, 2_234),
            "vendor=1552|model=628|serial=42|3456x2234"
        );
    }
}
