//! Window domain types: role, capabilities, lifecycle, and the aggregate.
//!
//! These types correspond to ARCHITECTURE §7.2. Platform adapters populate
//! them from native APIs; the rest of the domain operates on these types
//! without knowing which platform produced them.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::geometry::Rect;
use crate::id::{ApplicationId, DisplayId, WindowId};

/// The semantic role of a window, inferred from native style flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WindowRole {
    /// A standard top-level application window.
    Normal,
    /// A modal or modeless dialog.
    Dialog,
    /// A floating tool palette or properties panel.
    ToolWindow,
    /// A popup menu, tooltip, or transient surface.
    Popup,
    /// A splash screen shown during application startup.
    Splash,
    /// Role could not be determined.
    Unknown,
}

/// Capability flags describing what operations the platform allows on a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowCapabilities {
    /// The window can be repositioned.
    pub can_move: bool,
    /// The window can be resized by the user (has a sizing border).
    pub can_resize: bool,
    /// The window has a minimize button / can be minimized.
    pub can_minimize: bool,
    /// The window has a maximize button / can be maximized.
    pub can_maximize: bool,
}

impl WindowCapabilities {
    /// Returns `true` if the window supports the minimum operations needed
    /// for tiling (move + resize).
    pub const fn is_tileable(&self) -> bool {
        self.can_move && self.can_resize
    }
}

/// Current lifecycle / visibility state of a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WindowLifecycle {
    /// Visible and in normal restored state.
    Active,
    /// Minimized to taskbar / Dock.
    Minimized,
    /// Maximized to fill the work area.
    Maximized,
    /// Application-controlled full-screen presentation. Unlike maximize,
    /// this is temporarily ineligible and restores its visual-order slot on
    /// exit (ADR 0014).
    Fullscreen,
    /// Not visible (hidden by the application or system).
    Hidden,
    /// Cloaked by DWM (e.g. on another virtual desktop).
    Cloaked,
}

/// A discovered top-level window with all metadata the domain needs.
///
/// Platform adapters populate this from native APIs. Native handles are
/// stored only in `WindowId` and must never be persisted across sessions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    /// Ephemeral native handle.
    pub id: WindowId,
    /// OS process identifier.
    pub process_id: u32,
    /// Application identity (exe name or bundle ID).
    pub application_id: ApplicationId,
    /// Full path to the executable, if retrievable.
    pub executable_path: Option<PathBuf>,
    /// Current window title.
    pub title: String,
    /// Native window class name (e.g. Win32 class).
    pub native_class: Option<String>,
    /// Semantic role inferred from style flags.
    pub role: WindowRole,
    /// Current bounds in logical coordinates.
    pub bounds: Rect,
    /// The display this window is primarily on.
    pub display_id: DisplayId,
    /// What the platform allows us to do with this window.
    pub capabilities: WindowCapabilities,
    /// Whether the owning process has a higher integrity level than Mosaix.
    /// Elevated windows remain observable for diagnostics and rules, but are
    /// ineligible for placement because UIPI can reject the native call.
    pub elevated: bool,
    /// Current lifecycle state.
    pub lifecycle: WindowLifecycle,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tileable_requires_move_and_resize() {
        let caps = WindowCapabilities {
            can_move: true,
            can_resize: true,
            can_minimize: false,
            can_maximize: false,
        };
        assert!(caps.is_tileable());

        let no_resize = WindowCapabilities {
            can_resize: false,
            ..caps
        };
        assert!(!no_resize.is_tileable());

        let no_move = WindowCapabilities {
            can_move: false,
            ..caps
        };
        assert!(!no_move.is_tileable());
    }
}
