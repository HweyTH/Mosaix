//! Typed outcomes of the container-tree commands.
//!
//! A tree command either applies or refuses, and both are answers rather
//! than errors: IPC returns them as data and the CLI renders them, the way
//! persistent undo already reports. Every refusal names what the user can
//! act on, and a refused command mutates nothing.

use serde::{Deserialize, Serialize};

use crate::id::{DisplayId, WindowId};

/// How far one tree-resize command moves its divider, in percentage points
/// of the container's extent. Fixed in the first release; a smaller step
/// is taken only when the full one would push a window below its minimum
/// size.
pub const TREE_RESIZE_STEP_PERCENT: u32 = 5;

/// Why a tree resize did nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TreeResizeRefusal {
    /// Window management is paused, as it is for every other placement.
    Paused,
    /// Automatic tiling is not currently producing a container tree.
    NotTreeMode,
    /// Nothing is focused, or focus is on a window Mosaix does not manage.
    NoFocusedWindow,
    /// The focused window is not an arranged leaf: it floats, or the
    /// display cannot fit it and it is in constraint overflow.
    NotArranged { window_id: WindowId },
    /// The focused window's display is no longer connected.
    DisplayUnavailable { display_id: DisplayId },
    /// No divider faces that way: the window is at the arrangement's edge
    /// on that side.
    NoDivider { command: String },
    /// A divider exists, but not even a one-point move keeps every
    /// affected window at or above its minimum size.
    MinimumSizeReached { command: String },
}

impl TreeResizeRefusal {
    /// A stable machine-readable reason code.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Paused => "paused",
            Self::NotTreeMode => "not_tree_mode",
            Self::NoFocusedWindow => "no_focused_window",
            Self::NotArranged { .. } => "not_arranged",
            Self::DisplayUnavailable { .. } => "display_unavailable",
            Self::NoDivider { .. } => "no_divider",
            Self::MinimumSizeReached { .. } => "minimum_size_reached",
        }
    }
}

impl std::fmt::Display for TreeResizeRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Paused => formatter.write_str("window management is paused"),
            Self::NotTreeMode => {
                formatter.write_str("automatic tiling is not arranging a container tree")
            }
            Self::NoFocusedWindow => formatter.write_str("no managed window is focused"),
            Self::NotArranged { window_id } => write!(
                formatter,
                "window {} is not arranged by the tree, so it has no divider to move",
                window_id.0
            ),
            Self::DisplayUnavailable { display_id } => write!(
                formatter,
                "the focused window's display {} is no longer connected",
                display_id.0
            ),
            Self::NoDivider { command } => write!(
                formatter,
                "{command}: the window is at the edge of the arrangement on that side"
            ),
            Self::MinimumSizeReached { command } => write!(
                formatter,
                "{command}: a window beside that divider is already at its minimum size"
            ),
        }
    }
}

/// What a tree resize changed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TreeResizeApplied {
    pub command: String,
    pub display_id: DisplayId,
    /// The focused window, whose side of the divider grew.
    pub window_id: WindowId,
    /// The step actually taken. [`TREE_RESIZE_STEP_PERCENT`] unless a
    /// minimum size clamped it to a smaller whole number of points.
    pub percentage_points: u32,
    /// Every window that gained space, in visual order.
    pub grew: Vec<WindowId>,
    /// Every window that gave space up, in visual order.
    pub shrank: Vec<WindowId>,
    /// The moved container's child weights before and after, normalized
    /// to sum to one.
    pub weights_before: Vec<f64>,
    pub weights_after: Vec<f64>,
}

/// The result of asking for a tree resize.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TreeResizeResult {
    Applied(TreeResizeApplied),
    Refused(TreeResizeRefusal),
}

impl TreeResizeResult {
    pub const fn is_applied(&self) -> bool {
        matches!(self, Self::Applied(_))
    }
}

/// Why a directional swap did nothing. Every variant leaves the
/// arrangement untouched.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectionalSwapRefusal {
    /// Window management is paused, as it is for every other placement.
    Paused,
    /// Nothing is focused, or focus is on a window Mosaix does not manage.
    NoFocusedWindow,
    /// The focused window is not an arranged endpoint: it floats, or the
    /// display cannot fit it and it is in constraint overflow.
    NotArranged { window_id: WindowId },
    /// The focused window's display is no longer connected.
    DisplayUnavailable { display_id: DisplayId },
    /// No arranged window lies that way on the same display. Swap does
    /// not wrap and does not cross displays; display transfer is the
    /// explicit command for that.
    NoNeighbor { command: String },
}

impl DirectionalSwapRefusal {
    /// A stable machine-readable reason code.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Paused => "paused",
            Self::NoFocusedWindow => "no_focused_window",
            Self::NotArranged { .. } => "not_arranged",
            Self::DisplayUnavailable { .. } => "display_unavailable",
            Self::NoNeighbor { .. } => "no_neighbor",
        }
    }
}

impl std::fmt::Display for DirectionalSwapRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Paused => formatter.write_str("window management is paused"),
            Self::NoFocusedWindow => formatter.write_str("no managed window is focused"),
            Self::NotArranged { window_id } => write!(
                formatter,
                "window {} is not arranged by the tree (floating, or in constraint overflow), \
                 so it has no place to swap from",
                window_id.0
            ),
            Self::DisplayUnavailable { display_id } => write!(
                formatter,
                "the focused window's display {} is no longer connected",
                display_id.0
            ),
            Self::NoNeighbor { command } => write!(
                formatter,
                "{command}: no arranged window lies that way on this display"
            ),
        }
    }
}

/// What a directional swap exchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectionalSwapApplied {
    pub command: String,
    pub display_id: DisplayId,
    /// The focused window, which keeps focus and moves to the neighbor's
    /// former place.
    pub window_id: WindowId,
    /// The neighbor directional focus would have selected, which moves
    /// to the focused window's former place.
    pub neighbor_id: WindowId,
}

/// The result of asking for a directional swap.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectionalSwapResult {
    Applied(DirectionalSwapApplied),
    Refused(DirectionalSwapRefusal),
}

impl DirectionalSwapResult {
    pub const fn is_applied(&self) -> bool {
        matches!(self, Self::Applied(_))
    }
}

/// Why removing a dormant position did nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovePositionRefusal {
    /// Window management is paused, as it is for every other placement.
    Paused,
    /// Automatic tiling is not currently producing a container tree.
    NotTreeMode,
    /// No connected display has that id.
    DisplayUnavailable { display_id: DisplayId },
    /// That display's tree holds no dormant slot with that number. A live
    /// slot is never removed this way; close its window instead.
    UnknownPosition {
        display_id: DisplayId,
        position: u64,
    },
}

impl RemovePositionRefusal {
    /// A stable machine-readable reason code.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Paused => "paused",
            Self::NotTreeMode => "not_tree_mode",
            Self::DisplayUnavailable { .. } => "display_unavailable",
            Self::UnknownPosition { .. } => "unknown_position",
        }
    }
}

impl std::fmt::Display for RemovePositionRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Paused => formatter.write_str("window management is paused"),
            Self::NotTreeMode => {
                formatter.write_str("automatic tiling is not arranging a container tree")
            }
            Self::DisplayUnavailable { display_id } => {
                write!(formatter, "display {} is not connected", display_id.0)
            }
            Self::UnknownPosition {
                display_id,
                position,
            } => write!(
                formatter,
                "display {} has no dormant position {position}",
                display_id.0
            ),
        }
    }
}

/// What removing a dormant position changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovePositionApplied {
    pub display_id: DisplayId,
    pub position: u64,
    /// The application the removed slot was waiting for. Never a title.
    pub application: String,
}

/// The result of asking to remove a dormant position.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovePositionResult {
    Applied(RemovePositionApplied),
    Refused(RemovePositionRefusal),
}

impl RemovePositionResult {
    pub const fn is_applied(&self) -> bool {
        matches!(self, Self::Applied(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_refusal_codes_are_stable_and_distinct() {
        let refusals = [
            TreeResizeRefusal::Paused,
            TreeResizeRefusal::NotTreeMode,
            TreeResizeRefusal::NoFocusedWindow,
            TreeResizeRefusal::NotArranged {
                window_id: WindowId(1),
            },
            TreeResizeRefusal::DisplayUnavailable {
                display_id: DisplayId(1),
            },
            TreeResizeRefusal::NoDivider {
                command: "resize-left".to_owned(),
            },
            TreeResizeRefusal::MinimumSizeReached {
                command: "resize-left".to_owned(),
            },
        ];
        let mut codes: Vec<&str> = refusals.iter().map(TreeResizeRefusal::code).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), refusals.len());
    }

    #[test]
    fn a_result_round_trips_as_tagged_json() {
        let result = TreeResizeResult::Refused(TreeResizeRefusal::NoDivider {
            command: "resize-up".to_owned(),
        });
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["refused"]["no_divider"]["command"], "resize-up");
        let back: TreeResizeResult = serde_json::from_value(json).unwrap();
        assert_eq!(back, result);
    }
}
