//! Rule actions.

use serde::{Deserialize, Serialize};

/// How Mosaix should manage a window that matches a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManageAction {
    /// Normal tiling management.
    Tile,
    /// Float the window (exempt from tiling, but still tracked).
    Float,
    /// Completely exclude from management (invisible to the engine).
    Exclude,
}

/// The actions a matched rule applies to a window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleActions {
    pub manage: ManageAction,
    /// The logical workspace a matching managed window belongs to, by
    /// name. `None` leaves the window in the workspace displayed where it
    /// appeared. The engine resolves the name against its pool and
    /// refuses an unknown one rather than creating it (ADR 0028).
    pub workspace: Option<String>,
}

impl Default for RuleActions {
    fn default() -> Self {
        Self {
            manage: ManageAction::Tile,
            workspace: None,
        }
    }
}
