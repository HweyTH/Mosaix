//! Pure macOS-to-domain mappings.
//!
//! Keeping these conversions independent of Apple frameworks makes the
//! coordinate and filtering policy testable on every supported host.

use mosaix_domain::WindowRole;

/// AX roles used by the accessibility bridge without exposing CF strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacosWindowRole {
    Window,
    MenuBar,
    Sheet,
    Other,
}

/// Converts a Core Graphics bottom-left coordinate to Mosaix's top-left
/// coordinate system. `height` is required because Core Graphics positions a
/// rectangle by its bottom edge while Mosaix positions it by its top edge.
pub const fn flip_y(y: i32, height: i32, primary_display_height: i32) -> i32 {
    primary_display_height - y - height
}

/// Returns whether the bundle belongs to a system surface Mosaix must never
/// try to tile.
pub fn is_blocked_bundle_id(bundle_id: &str) -> bool {
    matches!(
        bundle_id,
        "com.apple.dock"
            | "com.apple.WindowManager"
            | "com.apple.notificationcenterui"
            | "com.apple.controlcenter"
    )
}

/// Maps an Accessibility role/subrole pair to the shared semantic role.
pub fn classify_role(role: &str, subrole: Option<&str>) -> WindowRole {
    match (role, subrole) {
        ("AXWindow", Some("AXStandardWindow")) => WindowRole::Normal,
        ("AXWindow", Some("AXDialog")) => WindowRole::Dialog,
        ("AXWindow", Some("AXFloatingWindow")) => WindowRole::ToolWindow,
        ("AXWindow", _) => WindowRole::Unknown,
        (_, _) => WindowRole::Popup,
    }
}

/// Produces the stable per-display identity used by topology fingerprints.
pub fn display_fingerprint(
    vendor: u32,
    model: u32,
    serial: u32,
    width: i32,
    height: i32,
) -> String {
    format!("vendor={vendor}|model={model}|serial={serial}|{width}x{height}")
}
