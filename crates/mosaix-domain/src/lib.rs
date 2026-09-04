//! Core domain types: IDs, geometry primitives, state models, commands, and events.

pub mod commands;
mod coordinates;
pub mod display;
pub mod geometry;
pub mod id;
pub mod identity;
pub mod recovery;
pub mod tree;
pub mod undo;
pub mod window;
pub mod workspace;

// Re-export commonly used types at the crate root for convenience.
pub use commands::{
    DirectionalSwapApplied, DirectionalSwapRefusal, DirectionalSwapResult, RemovePositionApplied,
    RemovePositionRefusal, RemovePositionResult, TreeResizeApplied, TreeResizeRefusal,
    TreeResizeResult, TREE_RESIZE_STEP_PERCENT,
};
pub use coordinates::{allocate_edges, LogicalRect, NormalizedRect};
pub use display::{display_fingerprints, topology_fingerprint, Display, Rotation};
pub use geometry::{Gaps, Rect, Size};
pub use id::{ApplicationId, DisplayId, WindowId};
pub use identity::{
    match_window, match_window_with_order, EvidenceContribution, EvidenceSignal, MatchOutcome,
    ScoredCandidate, WindowEvidence,
};
pub use recovery::{
    plan_recovery, HandleVerdict, LiveHandleEvidence, ParkingRefusal, ProcessInstance,
    RecoveryDraft, RecoveryEntry, RecoveryEntryId, RecoveryOutcome, ShowState,
};
pub use tree::{
    Child, ContainerTree, DividerChange, DormantPosition, Leaf, LeafFate, Node, Occupant,
    PersistedTree, SplitAxis, Toward, Tree, DORMANT_RETENTION_SECONDS,
};
pub use undo::{
    UndoApplied, UndoMember, UndoRefusal, UndoRestoredWindow, UndoResult, UndoTargetOutcome,
    UndoTransaction, UndoTransactionDraft, UndoTransactionId, UndoTreeSnapshot,
};
pub use window::{Window, WindowCapabilities, WindowLifecycle, WindowRole};
pub use workspace::{
    ParkingCapability, PersistedWorkspace, SwitchingPending, Workspace, WorkspaceCommandResult,
    WorkspaceCreateApplied, WorkspaceDeleteApplied, WorkspaceFocusApplied, WorkspaceMoveApplied,
    WorkspaceName, WorkspaceNameError, WorkspaceOrigin, WorkspacePool, WorkspaceRefusal,
    WorkspaceSwitchingStatus, WorkspaceSwitchingUnavailable, MAX_WORKSPACE_NAME_CHARS,
};
