//! Core domain types: IDs, geometry primitives, state models, commands, and events.

pub mod commands;
mod coordinates;
pub mod display;
pub mod geometry;
pub mod id;
pub mod identity;
pub mod tree;
pub mod undo;
pub mod window;

// Re-export commonly used types at the crate root for convenience.
pub use commands::{
    TreeResizeApplied, TreeResizeRefusal, TreeResizeResult, TREE_RESIZE_STEP_PERCENT,
};
pub use coordinates::{allocate_edges, LogicalRect, NormalizedRect};
pub use display::{topology_fingerprint, Display, Rotation};
pub use geometry::{Gaps, Rect, Size};
pub use id::{ApplicationId, DisplayId, WindowId};
pub use identity::{
    match_window, match_window_with_order, EvidenceContribution, EvidenceSignal, MatchOutcome,
    ScoredCandidate, WindowEvidence,
};
pub use tree::{
    Child, ContainerTree, DividerChange, Leaf, Node, PersistedTree, SplitAxis, Toward, Tree,
};
pub use undo::{
    UndoApplied, UndoMember, UndoRefusal, UndoRestoredWindow, UndoResult, UndoTargetOutcome,
    UndoTransaction, UndoTransactionDraft, UndoTransactionId, UndoTreeSnapshot,
};
pub use window::{Window, WindowCapabilities, WindowLifecycle, WindowRole};
