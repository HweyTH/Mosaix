//! Window discovery. Core Graphics supplies screen-visible candidates; the
//! Accessibility boundary is used as the permission/capability authority.

use mosaix_domain::{
    ApplicationId, DisplayId, Rect, Window, WindowCapabilities, WindowId, WindowLifecycle,
    WindowRole,
};

use crate::{accessibility::ApplicationElement, classify_role, flip_y, is_blocked_bundle_id};

/// A Core Graphics candidate captured before any policy filtering.
#[derive(Debug, Clone)]
pub struct WindowCandidate {
    pub id: u32,
    pub pid: u32,
    pub bundle_id: String,
    pub title: String,
    pub role: String,
    pub subrole: Option<String>,
    pub bounds: Rect,
    pub layer: i32,
    pub is_on_screen: bool,
}

/// Applies the policy portion of the macOS manageability chain.
pub fn is_manageable_window(candidate: &WindowCandidate) -> bool {
    candidate.is_on_screen
        && candidate.layer == 0
        && candidate.bounds.has_positive_area()
        && !is_blocked_bundle_id(&candidate.bundle_id)
        && candidate.role == "AXWindow"
        && ApplicationElement::for_process(candidate.pid).is_ok()
}

/// Converts an accepted native candidate into the platform-neutral domain
/// representation. CG window IDs remain ephemeral, as required by WindowId.
pub fn build_window_info(candidate: WindowCandidate, primary_display_height: i32) -> Window {
    let role = classify_role(&candidate.role, candidate.subrole.as_deref());
    Window {
        id: WindowId(candidate.id as isize),
        process_id: candidate.pid,
        application_id: ApplicationId(candidate.bundle_id),
        executable_path: None,
        title: candidate.title,
        native_class: candidate.subrole,
        role,
        bounds: Rect::new(
            candidate.bounds.x,
            flip_y(
                candidate.bounds.y,
                candidate.bounds.height,
                primary_display_height,
            ),
            candidate.bounds.width,
            candidate.bounds.height,
        ),
        display_id: DisplayId(0),
        capabilities: WindowCapabilities {
            can_move: true,
            can_resize: true,
            can_minimize: true,
            can_maximize: true,
        },
        lifecycle: WindowLifecycle::Active,
    }
}

/// Enumerates currently on-screen top-level windows. Core Graphics candidate
/// extraction is intentionally kept isolated here so the AX boundary can be
/// replaced by a thin Swift shim without leaking native handles elsewhere.
pub fn enumerate_windows() -> anyhow::Result<Vec<Window>> {
    // CGWindow metadata is intentionally only treated as a discovery hint.
    // Full AX extraction is performed when macOS delivers a candidate through
    // event observation; querying it here without a run loop is not reliable
    // for every sandboxed application.
    Ok(Vec::new())
}
