//! Authoritative state reducer, reconciliation, placement diff, and transaction planning.
//!
//! Holds the event queue and reducer: native adapter threads translate
//! callbacks into normalized events, a bounded multi-producer queue feeds
//! one reducer task, and the reducer is the only writer of domain state.
//!
//! [`spawn_engine`] starts one dedicated thread that owns [`EngineState`]
//! outright -- no lock guards the mutation itself, because nothing else
//! ever touches it. After each event, the thread publishes a clone of the
//! new state into an [`EngineHandle`]-shared `Mutex` purely so other
//! threads can read a consistent snapshot; that `Mutex` is not a second
//! mutation path.
//!
//! ## Resilience features
//!
//! - **Startup reconciliation** ([`Event::StartupReconciliation`]): The
//!   agent enumerates all open windows at launch and bulk-registers them
//!   so the engine starts with an accurate picture of what's already on
//!   screen.
//!
//! - **Sleep/wake recovery** ([`Event::WakeReconciliation`]): After the
//!   system resumes from sleep the display topology may have changed. The
//!   agent re-enumerates displays and windows and sends this event, which
//!   migrates any window whose previous display is gone to the nearest
//!   surviving one.
//!
//! - **Display hotplug** ([`Event::DisplayTopologyChanged`]): When a
//!   monitor is unplugged, windows tracked on the vanished display are
//!   migrated to the nearest surviving display rather than being left
//!   off-screen.
//!
//! - **Per-window circuit breaker** ([`Event::PlacementRejected`],
//!   [`CIRCUIT_BREAKER_THRESHOLD`]): If a window repeatedly rejects
//!   `SetWindowPos` (e.g. because it enforces a minimum size), the engine
//!   marks it temporarily unmanaged after [`CIRCUIT_BREAKER_THRESHOLD`]
//!   consecutive rejections. A deliberate zone-snap command from the user
//!   resets the breaker.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use mosaix_config::{Command, ResolvedConfig, ResolvedConfigSet, TilingMode};
use mosaix_domain::commands::{
    DirectionalSwapApplied, DirectionalSwapRefusal, RemovePositionApplied, RemovePositionRefusal,
    TreeResizeApplied, TreeResizeRefusal, TREE_RESIZE_STEP_PERCENT,
};
use mosaix_domain::identity::{match_window_with_order, MatchOutcome, WindowEvidence};
use mosaix_domain::recovery::{
    ParkingFailure, ParkingRefusal, ParkingStage, RecoveryDraft, RecoveryEntryId, RecoveryOutcome,
};
use mosaix_domain::tree::{
    ContainerTree, DormantPosition, LeafFate, Occupant, PersistedTree, SplitAxis, Toward,
};
use mosaix_domain::undo::{
    now_unix, UndoApplied, UndoAssignment, UndoMember, UndoRefusal, UndoRestoredWindow, UndoResult,
    UndoTargetOutcome, UndoTransaction, UndoTransactionDraft, UndoTransactionId, UndoTreeSnapshot,
};
use mosaix_domain::workspace::{
    ParkingCapability, PersistedWorkspace, SwitchingPending, WorkspaceCommandResult,
    WorkspaceCreateApplied, WorkspaceDeleteApplied, WorkspaceFocusApplied, WorkspaceMoveApplied,
    WorkspaceName, WorkspaceOrigin, WorkspacePool, WorkspaceRefusal, WorkspaceSwitchDegraded,
    WorkspaceSwitchFailed, WorkspaceSwitchPhase, WorkspaceSwitchRestoreResult,
    WorkspaceSwitchingStatus, WorkspaceSwitchingUnavailable,
};
use mosaix_domain::{
    topology_fingerprint, Display, DisplayId, Rect, Window, WindowId, WindowLifecycle,
};
use mosaix_layout::{
    apply_gaps, choose_insertion, cycle_display, plan_balanced_grid, plan_tree_constrained,
    plan_tree_raw, resolve_saved_layout, resolve_zone_cycle, snap_to_half, throw_preserving_ratio,
    CycleStep, DisplayDirection, HalfZone, HorizontalDirection,
};
use mosaix_persistence::PersistenceHealth;
use mosaix_rules::{builtin_rules, ManageAction, Rule, RuleEvaluator};

/// Default bound on the event queue before a sender blocks. Chosen
/// generously relative to expected event rates: the budget from an
/// ordinary OS event to a stable layout plan is under 100ms at p95, so a
/// deep queue is not needed to absorb bursts.
pub const DEFAULT_QUEUE_CAPACITY: usize = 256;

/// Number of consecutive placement rejections before a window's circuit
/// breaker opens and the engine stops trying to manage it. Chosen to
/// tolerate a transient mis-report while reacting quickly enough that the
/// user never sees a sustained battle between Mosaix and a stubborn app.
/// The breaker resets automatically on any explicit zone-snap command.
pub const CIRCUIT_BREAKER_THRESHOLD: u8 = 3;

/// A platform-neutral operation the engine has committed and an adapter
/// must perform, in reducer order. Effects contain no native handles or
/// Win32 structures so deterministic engine tests can observe intent
/// directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineEffect {
    PlaceWindow {
        window_id: WindowId,
        display_id: DisplayId,
        bounds: Rect,
    },
    FocusWindow {
        window_id: WindowId,
    },
    /// Requests a fresh native display/window observation before recovery.
    ReconcileWindows,
    /// Move `window_id` out of visible geometry. Emitted only once the
    /// recovery ledger has acknowledged entry `entry_id` as durable, so a
    /// crash between this effect and its completion still leaves enough on
    /// disk to put the window back. The adapter answers with
    /// [`Event::WindowParked`] when the move landed.
    ParkWindow {
        window_id: WindowId,
        entry_id: RecoveryEntryId,
    },
    /// Put the parked `window_id` back where ledger entry `entry_id`
    /// recorded it, without activating it. The adapter reads the entry,
    /// verifies the handle still names that window, and answers with
    /// [`Event::WindowRestored`] or [`Event::WindowRestoreFailed`].
    RestoreWindow {
        window_id: WindowId,
        entry_id: RecoveryEntryId,
    },
}

/// A durable write the reducer has committed to but does not perform.
///
/// The reducer owns no SQL connection, so it records what must become
/// durable and the persistence worker drains these in order, the same way
/// the placement executor drains [`EngineEffect`]. Kept separate from
/// effects because an effect touches the desktop and an intent touches the
/// disk -- and because a storage failure must never look like a placement
/// failure.
///
/// Not `Eq`: a container tree carries float weights.
#[derive(Debug, Clone, PartialEq)]
pub enum PersistenceIntent {
    /// Store one explicit command's reversible placements.
    RecordUndoTransaction(UndoTransactionDraft),
    /// Remove a transaction that has just been undone successfully.
    ConsumeUndoTransaction(UndoTransactionId),
    /// Store one display's container tree, so the arrangement survives a
    /// restart. Keyed by the display's stable fingerprint rather than its
    /// id, which is a native handle and means nothing next session.
    SaveContainerTree {
        display_fingerprint: String,
        tree: Box<PersistedTree>,
    },
    /// Store one logical workspace: its origin, the display it is shown
    /// on, and the tree it owns. Written whenever any of those change, so
    /// a restart finds the pool as it was.
    SaveWorkspace(Box<PersistedWorkspace>),
    /// Forget a workspace the user deleted.
    DeleteWorkspace(WorkspaceName),
    /// Record recovery data for a window about to be parked. Acknowledged
    /// back through [`Event::RecoveryEntryDurable`] with the same token;
    /// until then the window is pending and no parking effect exists.
    RecordRecovery {
        token: u64,
        draft: Box<RecoveryDraft>,
    },
    /// The parking effect for this entry was carried out.
    MarkParked(RecoveryEntryId),
    /// The window this entry describes is back in visible geometry.
    MarkRestored(RecoveryEntryId),
}

/// A window waiting for its recovery entry to become durable before it
/// may be parked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingParking {
    pub token: u64,
    pub window_id: WindowId,
    /// The workspace switch transaction this parking belongs to, when it
    /// is part of one rather than an explicit `workspace park`. A refused
    /// entry cancels the transaction; a standalone one only refuses.
    pub transaction: Option<u64>,
}

/// One all-or-nothing change of the workspace displayed on one monitor.
///
/// The displayed assignment is not touched until every native move has
/// landed. Until then this record is the whole memory of what the switch
/// has already done, so a failure at any point can put it back: the
/// windows it parked have to come back on screen, and the windows it
/// restored have to leave again.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceSwitchTransaction {
    pub id: u64,
    /// The monitor whose displayed workspace is changing.
    pub display_id: DisplayId,
    /// The hidden workspace being displayed.
    pub target: WorkspaceName,
    /// The workspace displayed there now, which becomes hidden on
    /// success and stays displayed on failure.
    pub outgoing: Option<WorkspaceName>,
    pub phase: WorkspaceSwitchPhase,
    /// Recovery tokens whose entries are not yet acknowledged durable.
    /// No window behind one of these has moved.
    pub recording: Vec<u64>,
    /// Windows the current phase has asked the adapter to move and has
    /// not heard back about.
    pub in_flight: Vec<WindowId>,
    /// Windows still to be moved once the phase's queue is drained.
    pub queued: Vec<WindowId>,
    /// Windows this transaction has parked, and so must restore to
    /// compensate.
    pub parked: Vec<WindowId>,
    /// Windows this transaction has restored, and so must park again to
    /// compensate.
    pub restored: Vec<WindowId>,
    /// Windows compensation could not put back.
    pub stranded: Vec<WindowId>,
    /// The reason for the failure that started compensation.
    pub failure: Option<String>,
    /// Where each window sat before this transaction moved it, so the
    /// committed switch can be recorded as a reversible transaction.
    pub prior_placements: Vec<(WindowId, DisplayId, Rect)>,
    /// Whether this switch is undoing a recorded one, in which case
    /// committing it must not record a new transaction: undo is not itself
    /// undoable.
    pub reverses_undo: Option<UndoTransactionId>,
}

impl WorkspaceSwitchTransaction {
    /// Whether the current phase has heard back about everything it
    /// started, and so may advance.
    ///
    /// `queued` is deliberately not part of this: it holds what the *next*
    /// phase will move, not what this one is waiting on.
    fn phase_is_settled(&self) -> bool {
        self.recording.is_empty() && self.in_flight.is_empty()
    }
}

/// What a workspace switch would move, decided before anything does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSwitchPlan {
    pub display_id: DisplayId,
    pub target: WorkspaceName,
    pub outgoing: Option<WorkspaceName>,
    /// The outgoing workspace's windows that must leave the screen, in
    /// window-id order. A minimized member occupies no screen and is not
    /// here.
    pub park: Vec<WindowId>,
    /// The target workspace's parked windows, in window-id order.
    pub restore: Vec<WindowId>,
}

impl WorkspaceSwitchPlan {
    /// Whether carrying this out touches the desktop at all. A switch
    /// that moves nothing is pure bookkeeping and needs no parking site,
    /// no durable recovery data, and no authorised switching.
    pub fn moves_windows(&self) -> bool {
        !self.park.is_empty() || !self.restore.is_empty()
    }
}

/// A rule named a workspace the pool does not hold. The window stays in
/// the workspace displayed where it appeared, and this records why, so a
/// typo in a rule is visible rather than silently creating a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleWorkspaceRefusal {
    pub window_id: WindowId,
    pub rule_id: Option<String>,
    pub workspace: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardinalDirection {
    Left,
    Right,
    Up,
    Down,
}

/// Why an observed managed window currently can or cannot enter the active
/// tiling set. These stable enums are safe to publish in machine-readable
/// state and deliberately contain neither titles nor executable paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EligibilityReason {
    Eligible,
    FloatingRule,
    NotTileable,
    Elevated,
    Minimized,
    Maximized,
    Fullscreen,
    Hidden,
    Cloaked,
    CircuitOpen,
    SessionFloating,
}

/// The engine-owned record for one observed window. `Exclude` windows are
/// deliberately absent from this inventory; `Tile` and `Float` stay
/// inspectable even when not currently eligible for a grid cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedWindow {
    pub window: Window,
    pub action: ManageAction,
    pub eligibility: EligibilityReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractivePlacementSession {
    pub window_id: WindowId,
    pub display_id: DisplayId,
}

/// Everything one explicit command has moved so far, gathered so it can be
/// reversed as a unit.
///
/// A window is captured the first time the command moves it, at the
/// position it held beforehand. Moving it again within the same command --
/// as an automatic reflow does -- does not overwrite that, because undo
/// restores where the window was before the *command*, not before its last
/// internal step.
#[derive(Debug, Clone, PartialEq)]
struct UndoScope {
    command: String,
    members: Vec<UndoMember>,
    claimed: HashSet<WindowId>,
    /// Each container tree the command is about to reshape, as it stood
    /// when the command started. Captured once per display; the first
    /// capture wins for the same reason the first placement does.
    prior_trees: Vec<(DisplayId, ContainerTree)>,
    /// Each display whose displayed workspace the command is about to
    /// change, as it stood when the command started. Only a workspace
    /// switch captures any.
    prior_assignments: Vec<(DisplayId, Option<WorkspaceName>)>,
}

/// State the reducer owns and is the only writer of.
///
/// Display topology and per-window placement exist as real domain state
/// today; a full window registry, workspaces, and rules will extend this
/// as those domain types land.
#[derive(Debug, Clone, Default)]
pub struct EngineState {
    /// Bumped on every committed mutation.
    pub revision: u64,
    /// Whether committed state is currently durable independently of the
    /// reducer's in-memory authority.
    pub persistence_health: PersistenceHealth,
    /// Durable writes the reducer has committed to, drained in order by
    /// the persistence worker. Appended to, never rewritten.
    pub persistence_intents: Vec<PersistenceIntent>,
    /// The only transaction undo will consider. Published by the agent
    /// from the state database, so the reducer never reads SQL to decide
    /// what undo would do.
    pub newest_undo: Option<UndoTransaction>,
    /// What the last undo request concluded, kept so IPC and the CLI can
    /// report a refusal that happened between requests.
    pub last_undo_result: Option<UndoResult>,
    /// Transient: the explicit command currently being applied, collecting
    /// everything it moves. Lives only for the duration of one [`apply`]
    /// call, and is `None` between events.
    undo_scope: Option<UndoScope>,
    /// Each display's container tree, when tree mode is the resolved
    /// arrangement. Empty under the balanced grid, which keeps no
    /// structure between reflows. A display that leaves the topology takes
    /// its tree with it, the way its arrangement name does.
    pub trees: HashMap<DisplayId, ContainerTree>,
    /// Stored arrangements the agent read at startup, waiting for their
    /// display to have windows to match them against.
    ///
    /// Consumed the first time that display reflows, after which the
    /// reducer is the authority on its tree and the database follows.
    /// Holding them here rather than adopting them on arrival is what makes
    /// the order of "the database answered" and "the windows were observed"
    /// not matter.
    pub pending_trees: HashMap<String, PersistedTree>,
    /// Each display's tree as it was last written out.
    ///
    /// Compared against rather than a snapshot taken at the start of the
    /// reflow, because commands reshape the tree before asking for a
    /// reflow -- a directional swap does exactly that -- and such a change
    /// would otherwise look like no change at all.
    saved_trees: HashMap<DisplayId, ContainerTree>,
    /// What would recognise each tiled window if it closed: evidence
    /// captured at the last reflow, so a leaf can go dormant with it after
    /// the window -- and its inventory entry -- are gone.
    leaf_evidence: HashMap<WindowId, WindowEvidence>,
    /// The windows each display's tree could not fit at their minimum
    /// size, newest insertion first. They keep their leaves and stay
    /// managed; they are simply not placed until the tree can satisfy them
    /// again. Distinct from session-floating, which is the user's choice
    /// and outlives a reflow.
    pub constraint_overflow: HashMap<DisplayId, Vec<WindowId>>,
    /// The global pool of logical workspaces: which is displayed where,
    /// which window belongs to which, and each one's stashed tree while
    /// hidden. A displayed workspace's tree is the entry in `trees` for
    /// its display; the two are exchanged whenever a display changes
    /// workspace, so the tree follows the workspace.
    pub workspaces: WorkspacePool,
    /// Stored workspace trees the agent read at startup, waiting for
    /// their workspace to be displayed and reflowed, exactly as
    /// `pending_trees` waits for a display.
    pub pending_workspace_trees: HashMap<WorkspaceName, PersistedTree>,
    /// Where each stored workspace was displayed when last written, by
    /// display fingerprint. Consumed when a display with no workspace is
    /// filled: a workspace goes back where it was if that display is here,
    /// and stays hidden otherwise.
    pending_displayed: HashMap<WorkspaceName, String>,
    /// Each workspace as it was last written out, so a reflow that
    /// changed nothing about it writes nothing.
    saved_workspaces: HashMap<WorkspaceName, PersistedWorkspace>,
    /// What the last workspace lifecycle command concluded, kept so IPC
    /// and the CLI can report a refusal that happened between requests.
    pub last_workspace_result: Option<WorkspaceCommandResult>,
    /// Rules that named a workspace the pool does not hold, one entry per
    /// affected window. Cleared for a window when it leaves management.
    pub rule_workspace_refusals: Vec<RuleWorkspaceRefusal>,
    /// Why the matched profile's switching mapping is not in effect, when
    /// it asked for one and it could not be applied. `None` when it is in
    /// effect, or when nothing asked. Read through
    /// [`EngineState::workspace_switching_status`].
    switching_unavailable: Option<WorkspaceSwitchingUnavailable>,
    /// What the platform adapter last said about a recoverable parking
    /// site for this topology. Parking is authorised only on `Verified`;
    /// nothing in the reducer verifies it.
    pub parking_capability: ParkingCapability,
    /// This agent session's identity in the recovery ledger, so a later
    /// session can tell its own entries from a previous session's.
    pub session_id: String,
    /// Windows whose recovery entry has been requested but not yet
    /// acknowledged durable. No parking effect exists for any of them.
    pub pending_parking: Vec<PendingParking>,
    next_parking_token: u64,
    /// Every window this session has parked, with the ledger entry that
    /// authorised it. Restoration consumes the entry.
    pub parked_windows: HashMap<WindowId, RecoveryEntryId>,
    /// The workspace switch currently in flight, if any. While this is set
    /// the displayed assignment is still the one the switch started from.
    pub switch: Option<WorkspaceSwitchTransaction>,
    next_switch_id: u64,
    /// Set when compensation for a failed switch could not put every
    /// window back. Switching stays blocked until the explicit restore
    /// path reconciles it.
    pub switch_degraded: Option<WorkspaceSwitchDegraded>,
    /// What the last switch transaction concluded, kept so IPC and the
    /// CLI can report a failure that happened between requests.
    pub last_switch_result: Option<WorkspaceCommandResult>,
    /// What the last parking authorisation request concluded.
    pub last_parking_refusal: Option<ParkingRefusal>,
    /// The last native park or restore the adapter could not carry out.
    pub last_parking_failure: Option<ParkingFailure>,
    /// What startup recovery did with the previous session's ledger:
    /// each open entry's verdict and whether its window was put back.
    pub recovery_outcomes: Vec<RecoveryOutcome>,
    pub displays: Vec<Display>,
    /// Where each tracked window currently sits, keyed by window. Entries
    /// are created (and their `previous_placement` remembered) by
    /// [`Event::WindowPlaced`] and [`Event::WindowThrowToDisplayRequested`],
    /// or, for a window seen focused before either of those ever fires,
    /// by [`Event::WindowFocused`] itself (with no `previous_placement`).
    pub windows: HashMap<WindowId, WindowPlacement>,
    /// Authoritative managed-window inventory. The engine is its sole writer.
    pub inventory: HashMap<WindowId, ManagedWindow>,
    /// Latest normalized observations, including rule-excluded windows so a
    /// rule reload can reconsider them without an adapter round-trip.
    observed_windows: HashMap<WindowId, Window>,
    /// Ordered rules currently used to resolve the inventory.
    pub rules: Vec<Rule>,
    /// Stable member order for each display's Balanced grid.
    pub visual_window_order: HashMap<DisplayId, Vec<WindowId>>,
    /// Session-only manual-placement overrides. They deliberately do not
    /// mutate persistent rules and clear on engine restart.
    pub session_floating: HashSet<WindowId>,
    /// Session-only overrides that allow a rule-Float window to join the grid.
    pub session_tiled: HashSet<WindowId>,
    /// The window that currently has OS foreground focus, `None` until the
    /// first [`Event::WindowFocused`] is observed. Sourced from the OS's
    /// foreground-change notification.
    pub focused_window: Option<WindowId>,
    /// Last display targeted by focus or an explicit display command.
    pub focused_display: Option<DisplayId>,
    /// The currently active hotkeys/gaps/behavior settings -- whichever of
    /// `config_set`'s `base` or one of its `profiles` currently matches
    /// `displays`' topology. Updated by [`Event::ConfigChanged`] and by
    /// [`Event::DisplayTopologyChanged`] re-selecting against the same
    /// `config_set` whenever the topology itself changes. Defaults to
    /// `ResolvedConfig::default()` (no hotkeys bound) before the first
    /// config load completes -- the same "nothing observed yet" role
    /// `displays: Vec::new()` plays for topology.
    pub resolved_config: ResolvedConfig,
    /// The full base-config-plus-profiles set most recently delivered by
    /// [`Event::ConfigChanged`] -- kept around so
    /// [`Event::DisplayTopologyChanged`] has something to re-select
    /// `resolved_config` from without needing its own copy of every
    /// profile. Never read directly by anything outside the reducer;
    /// `resolved_config` is what the rest of the system consults.
    pub config_set: ResolvedConfigSet,
    /// Whether window management is paused. When `true`, placement-related
    /// events (`ZoneSnapRequested`, `WindowPlaced`, `WindowThrowToDisplayRequested`)
    /// are suppressed — logged and discarded. Observation events
    /// (`DisplayTopologyChanged`, `WindowFocused`, `WindowBoundsObserved`,
    /// `ConfigChanged`) still process normally so state stays accurate for
    /// when the user resumes.
    pub paused: bool,
    /// Whether the matched topology profile currently owns automatic tiling.
    pub automatic_tiling_active: bool,
    /// Session-only override over a tiling-enabled profile. It clears when
    /// topology changes or the agent restarts.
    pub automatic_tiling_suspended: bool,
    /// The one native move/resize session currently owned by the pointer.
    pub interactive_placement: Option<InteractivePlacementSession>,
    /// Displays whose final grid plan is waiting for interactive placement
    /// to end. Other displays remain independently reflowable.
    deferred_reflow_displays: HashSet<DisplayId>,
    /// Ordered effects emitted by committed placement transitions. Consumers
    /// retain a cursor; the log is part of the published deterministic state.
    pub effects: Vec<EngineEffect>,
    /// The saved layout most recently applied to each display, by name.
    ///
    /// Published state rather than an internal note: machine-readable
    /// state carries the saved layouts and which one was last applied per
    /// display, so tooling built on top can tell what a display is
    /// currently arranged as.
    ///
    /// Recorded only for an application that actually placed something --
    /// a rejected apply changes nothing, and claiming otherwise would
    /// make this the one part of published state that lies. Entries for a
    /// display that leaves the topology are dropped with it.
    pub last_applied_layouts: HashMap<DisplayId, String>,
    /// Whether a hotkey editor is open and every binding must therefore
    /// stay unregistered.
    ///
    /// A flag rather than an effect. The agent's hotkey-rebind poller --
    /// the same path a profile switch already re-registers through --
    /// reads it and registers nothing while it is set, which keeps hotkey
    /// ownership in one place and adds no new effect kind.
    pub hotkey_capture_suspended: bool,
    /// How many editors currently hold suspension.
    ///
    /// A count, not a bool, so two settings windows behave: the second to
    /// open does not re-suspend something already suspended, and the
    /// first to close does not lift a suspension the other still needs
    /// while its capture dialog is armed. `hotkey_capture_suspended` is
    /// this reaching zero or not, and stays the published fact.
    capture_holds: usize,
    /// The bindings the most recent registration pass could not register,
    /// in the spelling configuration files use.
    ///
    /// Re-registration after capture is partial-success, as registration
    /// already is: a combination another application took while Mosaix was
    /// suspended comes back failed. Recording which ones is what lets the
    /// editor name them rather than leaving the user to discover a dead
    /// shortcut.
    pub unregistered_bindings: Vec<Command>,
}

impl EngineState {
    /// The state of experimental workspace switching for the current
    /// topology, derived rather than stored so it can never disagree
    /// with the facts it summarises: whether the matched profile asks,
    /// whether its mapping applied, whether recovery data would be
    /// durable, and whether a parking site is verified.
    pub fn workspace_switching_status(&self) -> WorkspaceSwitchingStatus {
        let requested = self
            .resolved_config
            .workspace_switching
            .as_ref()
            .is_some_and(|switching| switching.experimental);
        if !requested {
            return WorkspaceSwitchingStatus::Disabled;
        }
        if let Some(reason) = &self.switching_unavailable {
            return WorkspaceSwitchingStatus::Unavailable {
                reason: reason.clone(),
            };
        }
        if matches!(self.persistence_health, PersistenceHealth::Degraded { .. }) {
            return WorkspaceSwitchingStatus::Requested {
                pending: SwitchingPending::PersistenceDegraded,
            };
        }
        match &self.parking_capability {
            ParkingCapability::Verified => WorkspaceSwitchingStatus::Experimental,
            ParkingCapability::Unverified => WorkspaceSwitchingStatus::Requested {
                pending: SwitchingPending::ParkingCapabilityUnverified,
            },
            ParkingCapability::Refused { reason } => WorkspaceSwitchingStatus::Unavailable {
                reason: WorkspaceSwitchingUnavailable::ParkingRefused {
                    reason: reason.clone(),
                },
            },
        }
    }

    /// The number of windows whose circuit breaker is currently open.
    /// Useful for diagnostics via `mosaix state --json`.
    pub fn circuit_breaker_count(&self) -> usize {
        self.windows.values().filter(|p| p.circuit_open()).count()
    }
}

/// A tracked window's current bounds and display, plus the display and
/// bounds it had immediately before its most recent placement, if any --
/// the remembered pre-snap size that belongs with whatever tracks window
/// state, not with [`mosaix_layout`]'s stateless zone planner.
///
/// Both the display and the bounds are remembered together, not bounds
/// alone: a placement can move a window to a different display (a throw),
/// so bounds computed relative to the source display would be wrong if
/// reapplied under the target display's `display_id`.
///
/// Restore is a single remembered step, not a full undo stack, so
/// `previous_placement` holds at most one prior placement, and restoring
/// clears it rather than pushing the restored state back onto a stack.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowPlacement {
    pub display_id: DisplayId,
    pub bounds: Rect,
    /// Where the OS last reported this window, as opposed to [`bounds`],
    /// the placement Mosaix last *intended*.
    /// The two agree whenever Mosaix owns the window's position, and
    /// diverge the moment anything else moves it -- an app repositioning
    /// its own window, a native OS snap, a session-floating window dragged
    /// by the user. `bounds` deliberately keeps holding the intent,
    /// because comparing the two is exactly how
    /// [`Event::WindowBoundsObserved`] detects an external move and resets
    /// cycle state; readers that want to know where the window actually
    /// *is* -- drawing on or around it, say -- want this field instead.
    ///
    /// [`bounds`]: Self::bounds
    pub observed_bounds: Rect,
    pub previous_placement: Option<(DisplayId, Rect)>,
    /// The horizontal zone command and step that produced this placement,
    /// if it came from [`Event::ZoneSnapRequested`] with a left/right
    /// direction. `None` for windows never horizontally zone-snapped, and
    /// left untouched by placements that don't participate in cycling
    /// (top/bottom zone-snaps, restores, throws, plain
    /// [`Event::WindowPlaced`]) -- only a repeated same-direction
    /// [`Event::ZoneSnapRequested`] advances it, and only a future
    /// bounds-observed event mismatching the expected placement
    /// transaction resets it.
    pub cycle_step: Option<(HorizontalDirection, CycleStep)>,
    /// Consecutive `SetWindowPos` rejection count for the circuit breaker
    /// (see [`CIRCUIT_BREAKER_THRESHOLD`]). Each
    /// [`Event::PlacementRejected`] increments this; reaching the
    /// threshold causes the engine to stop issuing placements for this
    /// window. Any explicit user zone-snap command resets it to zero,
    /// giving the user a deliberate escape hatch.
    pub rejection_count: u8,
}

impl WindowPlacement {
    /// Returns `true` if the circuit breaker has opened for this window --
    /// i.e. the engine should not issue any further automatic placements
    /// until the user explicitly resets it with a zone-snap command.
    pub fn circuit_open(&self) -> bool {
        self.rejection_count >= CIRCUIT_BREAKER_THRESHOLD
    }
}

/// The direction a zone-snap hotkey requests. Only
/// [`ZoneSnapDirection::Left`]/[`ZoneSnapDirection::Right`] participate in
/// zone cycling -- `ZoneSnapDirection::horizontal` is how
/// [`Event::ZoneSnapRequested`]'s handler tells them apart from
/// top/bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneSnapDirection {
    Left,
    Right,
    Top,
    Bottom,
}

/// The command name an undo transaction records for a zone snap. Stable
/// text, because it is stored and shown back to the user later.
const fn zone_snap_command(direction: ZoneSnapDirection) -> &'static str {
    match direction {
        ZoneSnapDirection::Left => "snap-left",
        ZoneSnapDirection::Right => "snap-right",
        ZoneSnapDirection::Top => "snap-top",
        ZoneSnapDirection::Bottom => "snap-bottom",
    }
}

/// The stored command name for a directional swap.
const fn swap_command(direction: CardinalDirection) -> &'static str {
    match direction {
        CardinalDirection::Left => "swap-left",
        CardinalDirection::Right => "swap-right",
        CardinalDirection::Up => "swap-up",
        CardinalDirection::Down => "swap-down",
    }
}

/// The stored command name for a tree resize.
const fn resize_command(direction: CardinalDirection) -> &'static str {
    match direction {
        CardinalDirection::Left => "resize-left",
        CardinalDirection::Right => "resize-right",
        CardinalDirection::Up => "resize-up",
        CardinalDirection::Down => "resize-down",
    }
}

/// The container axis a cardinal direction moves along, and which way.
const fn divider_for(direction: CardinalDirection) -> (SplitAxis, Toward) {
    match direction {
        CardinalDirection::Left => (SplitAxis::Horizontal, Toward::Start),
        CardinalDirection::Right => (SplitAxis::Horizontal, Toward::End),
        CardinalDirection::Up => (SplitAxis::Vertical, Toward::Start),
        CardinalDirection::Down => (SplitAxis::Vertical, Toward::End),
    }
}

/// The stored command name for a throw to an adjacent display.
const fn throw_command(direction: DisplayDirection) -> &'static str {
    match direction {
        DisplayDirection::Next => "throw-next-display",
        DisplayDirection::Prev => "throw-previous-display",
    }
}

impl ZoneSnapDirection {
    /// The matching [`HorizontalDirection`] for `Left`/`Right`, `None` for
    /// `Top`/`Bottom` (which stay single-shot and never cycle).
    fn horizontal(self) -> Option<HorizontalDirection> {
        match self {
            ZoneSnapDirection::Left => Some(HorizontalDirection::Left),
            ZoneSnapDirection::Right => Some(HorizontalDirection::Right),
            ZoneSnapDirection::Top | ZoneSnapDirection::Bottom => None,
        }
    }
}

/// An event the reducer applies to [`EngineState`]. Producers (platform
/// adapters, commands, timers) send these into the bounded queue.
#[derive(Debug, Clone)]
pub enum Event {
    /// A platform adapter observed (or suspects) a display topology
    /// change; carries a fresh enumeration, not a diff. Adapters may emit
    /// incomplete events, so the reducer treats them as hints and decides,
    /// via [`topology_fingerprint`], whether anything actually changed.
    DisplayTopologyChanged(Vec<Display>),

    /// Ordered persistence acknowledgement or failure from the worker.
    PersistenceHealthChanged(PersistenceHealth),

    /// The newest stored undo transaction, or `None` for empty history.
    /// Published by the agent at startup and after every durable write, so
    /// the reducer's view of history matches the database's.
    UndoHistoryLoaded(Option<Box<UndoTransaction>>),

    /// Reverse the newest transaction, or refuse and keep it.
    UndoRequested,

    /// The container trees the state database holds, keyed by display
    /// fingerprint. Published once at startup.
    ContainerTreesLoaded(HashMap<String, PersistedTree>),

    /// The logical workspaces the state database holds. Published once at
    /// startup. Command-created workspaces rejoin the pool; every
    /// workspace's tree waits for it to be displayed; and one that was
    /// displayed on a display that is here again goes back to it.
    WorkspacesLoaded(Vec<PersistedWorkspace>),

    /// Create a hidden, empty workspace called `name`. Every way this
    /// changes nothing is a typed [`WorkspaceRefusal`], reached by
    /// [`plan_workspace_create`].
    WorkspaceCreateRequested {
        name: String,
    },

    /// Delete the workspace called `name`, which must be hidden, empty of
    /// live and dormant members, and not declared by configuration.
    WorkspaceDeleteRequested {
        name: String,
    },

    /// Display the hidden workspace `name` on the focused display, or, if
    /// it is already displayed somewhere, focus its last-focused live
    /// window there. A focus never moves a displayed workspace between
    /// monitors.
    WorkspaceFocusRequested {
        name: String,
    },

    /// Move the displayed workspace `name` to `display_id`, exchanging it
    /// with whatever that display showed. Identity and tree travel with
    /// it.
    WorkspaceMoveRequested {
        name: String,
        display_id: DisplayId,
    },

    /// What the platform adapter concluded about a recoverable parking
    /// site for the current topology.
    ParkingCapabilityReported(ParkingCapability),

    /// Ask to park `window_id`. Every refusal is typed and reached by
    /// [`plan_parking_authorization`] before any write; on success the
    /// reducer records a recovery intent and waits, emitting no parking
    /// effect until [`Event::RecoveryEntryDurable`] answers.
    ParkingAuthorizationRequested {
        window_id: WindowId,
    },

    /// The ledger made the entry for `token` durable. This, and only
    /// this, authorises the parking effect.
    RecoveryEntryDurable {
        token: u64,
        entry_id: RecoveryEntryId,
    },

    /// The ledger could not write the entry for `token`. The window
    /// stays visible.
    RecoveryEntryRefused {
        token: u64,
    },

    /// The adapter carried out the parking effect for `entry_id`.
    WindowParked {
        window_id: WindowId,
        entry_id: RecoveryEntryId,
    },

    /// The adapter could not carry out the parking effect for `entry_id`.
    /// The window stays where it was; the entry stays recorded but never
    /// parked, which restoration ignores.
    WindowParkFailed {
        window_id: WindowId,
        entry_id: RecoveryEntryId,
        reason: String,
    },

    /// The window `entry_id` described is back in visible geometry.
    WindowRestored {
        window_id: WindowId,
    },

    /// The adapter could not put the parked window back. It stays
    /// parked with its entry open, so recovery can still find it.
    WindowRestoreFailed {
        window_id: WindowId,
        entry_id: RecoveryEntryId,
        reason: String,
    },

    /// Put back every window this session parked, through the verified
    /// restore path. The explicit restore action published state points
    /// at, usable whether or not anything is degraded.
    RestoreParkedWindowsRequested,
    /// Reconcile the windows a failed compensation left unaccounted for,
    /// which is the only way out of the workspace-switch-degraded
    /// condition.
    WorkspaceSwitchRestoreRequested,

    /// What startup recovery did with the previous session's ledger,
    /// published so state and clients can report it.
    RecoveryReported(Vec<RecoveryOutcome>),

    /// A window was snapped or otherwise placed at `bounds` on
    /// `display_id`. Producers (a zone-snap command that resolved bounds
    /// via [`mosaix_layout`], drag-to-snap, etc.) send this after
    /// computing the new bounds; the reducer records it as the window's
    /// current placement and stashes wherever it was before as the one
    /// step [`Event::WindowRestoreRequested`] can undo.
    WindowPlaced {
        window_id: WindowId,
        display_id: DisplayId,
        bounds: Rect,
    },

    /// Undo the window's most recent placement, returning it to the
    /// display and bounds it had immediately before. A no-op if the window
    /// isn't tracked, or has no remembered prior placement (e.g. it was
    /// only ever placed once, or was already restored).
    WindowRestoreRequested {
        window_id: WindowId,
    },

    /// Move a window to the adjacent display in `direction`, preserving
    /// its position/size as a fraction of the display's work area. A no-op
    /// if the window isn't tracked, its current display is no longer in
    /// the topology, or there's no adjacent display to move to (e.g. only
    /// one display is connected).
    WindowThrowToDisplayRequested {
        window_id: WindowId,
        direction: DisplayDirection,
    },

    /// The OS reported `window_id` as having gained foreground focus, along
    /// with its current display and bounds as observed at that moment.
    /// Sourced from the platform adapter's foreground-change notification;
    /// updates [`EngineState::focused_window`]. If `window_id` isn't
    /// already tracked, it's registered using the observed placement (with
    /// no `previous_placement`) -- otherwise its existing tracked placement
    /// is left untouched, since Mosaix's own last placement is more
    /// trustworthy than a point-in-time OS observation. This is how a
    /// window becomes tracked in the first place: nothing else in the
    /// system ever sends [`Event::WindowPlaced`] for a window Mosaix didn't
    /// itself just place.
    WindowFocused {
        window_id: WindowId,
        display_id: DisplayId,
        bounds: Rect,
    },

    /// Focus is on the desktop or another unmanaged surface.
    DesktopFocused,

    /// Selects a display even when it has no managed focused window.
    FocusDisplayRequested {
        display_id: DisplayId,
    },

    /// A zone-snap hotkey fired for `direction`. Carries no
    /// window id -- it resolves against [`EngineState::focused_window`] at
    /// apply time and is a no-op if nothing is focused or the focused
    /// window isn't tracked. For [`ZoneSnapDirection::Left`]/`Right`, a
    /// repeated same-direction request advances the focused window's zone
    /// cycle (half -> third -> two-thirds -> half); any other case (first
    /// press, or a different direction than last time) starts that
    /// direction's cycle fresh at half. For `Top`/`Bottom` this always
    /// resolves to half with no cycle-step bookkeeping. Either way, the
    /// resolved bounds are placed through the same `place_window` path as
    /// any other placement, so the result remains restorable.
    ZoneSnapRequested {
        direction: ZoneSnapDirection,
    },

    /// The OS reported `window_id`'s current display and bounds, following
    /// its own location-changed notification -- which fires for both
    /// programmatic and interactive moves alike, so this does not by
    /// itself mean something *other* than Mosaix moved the window.
    /// Compared against the window's own last placement transaction: a
    /// match confirms the observation is just an echo of Mosaix's own last
    /// placement and leaves cycle-step state untouched; a mismatch means
    /// something else moved or resized the window (a manual drag, another
    /// app, a native OS snap), which invalidates (resets to step 1) the
    /// window's cycle-step state. Either way this never alters the
    /// window's tracked placement bounds -- reconciling tracked state from
    /// raw observation is a separate concern. A no-op for an untracked
    /// window.
    WindowBoundsObserved {
        window_id: WindowId,
        display_id: DisplayId,
        bounds: Rect,
    },

    /// `mosaix-config`'s directory watcher validated a new candidate
    /// config directory successfully; carries the full
    /// base-config-plus-profiles set, not a diff. Sent for the initial
    /// startup load as well as every subsequent hot-edit -- an edit that
    /// fails validation never produces this event at all, so
    /// [`EngineState::config_set`] (and the
    /// [`EngineState::resolved_config`] re-selected from it) simply keeps
    /// its last-known-good value. The active profile is re-selected
    /// against [`EngineState`]'s current topology exactly the way
    /// [`Event::DisplayTopologyChanged`] re-selects it against the current
    /// config set.
    ///
    /// Boxed because it dwarfs every other variant: a resolved config
    /// carries three maps plus its provenance, and `Event` is moved
    /// through the queue on every window message. One heap allocation per
    /// config reload is cheaper than paying that width on every event.
    ConfigChanged(Box<ResolvedConfigSet>),

    /// Pause all window management. Placement-related events become no-ops
    /// until [`Event::ResumeRequested`] fires. Idempotent — pausing when
    /// already paused is a no-op.
    PauseRequested,

    /// Resume window management after a pause. Idempotent — resuming when
    /// not paused is a no-op.
    ResumeRequested,

    /// Toggles the session-only automatic-tiling suspension for the matched
    /// tiling-enabled profile. Manual zone placement remains available.
    ToggleAutomaticTilingRequested,

    /// Toggles the focused managed window between session-floating and the
    /// active tiling set.
    ToggleFloatingRequested,

    /// Focuses the nearest eligible managed window in the requested
    /// display-local cardinal direction without changing visual order.
    DirectionalFocusRequested {
        direction: CardinalDirection,
    },

    /// Swaps the focused window with its nearest display-local cardinal
    /// neighbor, then recomputes the affected Balanced grid.
    DirectionalSwapRequested {
        direction: CardinalDirection,
    },

    /// Move the nearest container-tree divider facing `direction` by
    /// [`TREE_RESIZE_STEP_PERCENT`] points, growing the focused window's
    /// side. Every way it can change nothing is a typed
    /// [`TreeResizeRefusal`], reached by [`plan_tree_resize`] so a
    /// synchronous caller can report it; the reducer reaches the same
    /// verdict and, on a refusal, mutates nothing.
    TreeResizeRequested {
        direction: CardinalDirection,
    },

    /// Delete the dormant slot numbered `position` from `display_id`'s
    /// tree, closing the space its ancestors held for it. Refusals are
    /// typed, reached by [`plan_remove_position`], and mutate nothing.
    TreePositionRemoveRequested {
        display_id: DisplayId,
        position: u64,
    },

    InteractivePlacementStarted {
        window_id: WindowId,
    },

    InteractivePlacementEnded {
        window_id: WindowId,
        committed_manual_placement: bool,
    },

    /// Bulk-register all windows that were already open when the agent
    /// started (startup reconciliation). Each entry is `(window_id,
    /// display_id, bounds)` as observed by the platform adapter's initial
    /// enumeration. Windows already tracked (e.g. by an earlier
    /// [`Event::WindowFocused`]) are silently skipped; windows not yet
    /// tracked are registered with no `previous_placement`. The event is
    /// sent once, right after the engine is spawned, before any OS event
    /// hooks are active.
    StartupReconciliation {
        windows: Vec<(WindowId, DisplayId, Rect)>,
    },

    /// Re-synchronise display topology and tracked windows after the
    /// system wakes from sleep (sleep/wake recovery).
    ///
    /// The agent re-enumerates both displays and windows after a
    /// configurable settling delay and sends this event. The handler:
    /// 1. Applies the new display topology (same fingerprint-based guard
    ///    as [`Event::DisplayTopologyChanged`]).
    /// 2. Migrates orphaned windows (whose previous `display_id` no longer
    ///    exists) to the nearest surviving display, preserving their
    ///    normalized position via [`throw_preserving_ratio`].
    /// 3. Bulk-registers any newly observed windows that the engine
    ///    doesn't know about yet.
    WakeReconciliation {
        displays: Vec<Display>,
        windows: Option<Vec<Window>>,
    },

    /// The platform reported that the `SetWindowPos` call for `window_id`
    /// was rejected — the window's actual bounds after a settling period
    /// differ too much from the target (circuit breaker).
    ///
    /// Increments the window's `rejection_count`. When the count reaches
    /// [`CIRCUIT_BREAKER_THRESHOLD`], subsequent automatic placements for
    /// that window are suppressed and a warning is logged. The count is
    /// reset to zero by any explicit [`Event::ZoneSnapRequested`] so the
    /// user always has an escape hatch.
    PlacementRejected {
        window_id: WindowId,
    },

    /// Confirms a placement settled within tolerance and therefore breaks a
    /// rejection streak without changing the committed geometry.
    PlacementAccepted {
        window_id: WindowId,
    },

    /// Clears every open placement circuit once and performs one fresh Grid
    /// reflow. The agent performs its native display/window enumeration before
    /// sending this recovery request.
    RearrangeRequested,

    /// Native observations collected in response to [`EngineEffect::ReconcileWindows`].
    RearrangeReconciliationComplete {
        windows: Vec<Window>,
    },

    /// A complete normalized observation batch. It is authoritative for the
    /// observed windows: missing entries are removed, and `Exclude` results
    /// do not enter the managed inventory.
    WindowsObserved {
        windows: Vec<Window>,
    },

    /// Replaces the ordered user rules; built-ins remain the low-priority
    /// fallback rules. Every currently observed window is re-evaluated.
    RulesChanged {
        rules: Vec<Rule>,
    },

    /// Apply the saved layout called `name` to the display of the focused
    /// managed window, filling its cells from that display's managed
    /// windows in visual window order.
    ///
    /// Carries no display id: like the directional commands, it resolves
    /// its target at apply time. Every way it can change nothing is a
    /// [`SavedLayoutRejection`] rather than a silent no-op, and
    /// [`plan_saved_layout`] is where that verdict is reached -- a
    /// synchronous caller asks it first so it can report the reason
    /// instead of firing this event into the queue and hearing nothing
    /// back.
    SavedLayoutApplyRequested {
        name: String,
    },

    /// The settings application opened its hotkey editor, so every binding
    /// must stay unregistered until it closes.
    ///
    /// Sets [`EngineState::hotkey_capture_suspended`]. Idempotent: a
    /// second editor window sets a flag that is already set, and the
    /// connection that closes last clears it.
    HotkeyCaptureStarted,

    /// The hotkey editor is gone -- closed, crashed, or killed -- so
    /// bindings can be registered again.
    ///
    /// Emitted by the agent when the settings application's connection
    /// ends for any reason, because Windows closes the pipe handle even on
    /// a hard kill. That is what bounds suspension by the connection
    /// rather than by a message that can be lost.
    HotkeyCaptureEnded,

    /// The outcome of a registration pass: the bindings the platform
    /// refused, in the spelling configuration files use.
    ///
    /// Sent by the agent after each pass, so a binding that did not come
    /// back from capture can be named to the user rather than discovered
    /// as a dead shortcut.
    HotkeyRegistrationReported {
        unregistered: Vec<Command>,
    },
}

/// Why applying a saved layout changes nothing. Every variant names
/// something the user can act on: a layout command never silently does
/// nothing, and never falls back to another display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SavedLayoutRejection {
    /// Window management is paused, as it is for every other placement.
    Paused,
    /// No saved layout carries this name in the resolved config.
    UnknownLayout { name: String },
    /// Focus rests on the desktop, on an excluded window, or nowhere, so
    /// there is no display to target.
    NoFocusedManagedWindow,
    /// There is no display target in the current usable topology.
    NoFocusedDisplay,
    /// The focused window's display left the topology between the command
    /// being issued and being applied.
    DisplayUnavailable { display_id: DisplayId },
}

impl std::fmt::Display for SavedLayoutRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Paused => formatter.write_str("window management is paused"),
            Self::UnknownLayout { name } => {
                write!(formatter, "no saved layout named {name:?}")
            }
            Self::NoFocusedManagedWindow => formatter.write_str(
                "no managed window is focused, so there is no display to apply a layout to",
            ),
            Self::NoFocusedDisplay => formatter
                .write_str("no display is focused, so there is no display to apply a layout to"),
            Self::DisplayUnavailable { display_id } => write!(
                formatter,
                "the focused window's display {} is no longer connected",
                display_id.0
            ),
        }
    }
}

/// What applying a saved layout would do to the target display right now.
///
/// Counts rarely match, and neither mismatch is a failure. Surplus cells
/// simply go unfilled, which needs no reporting -- an empty cell is
/// visible. Surplus *windows* do need reporting: a window the layout had
/// no cell for stays exactly where it is, and the user is owed the count
/// rather than left to notice the omission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedLayoutPlan {
    /// The one display every placement lands on: the focused managed
    /// window's. A field rather than a column repeated down `placements`,
    /// because a plan touching two displays is not a thing this type can
    /// represent.
    pub display_id: DisplayId,
    /// Cell *i* of the layout paired with window *i* of the target
    /// display's visual window order, gaps already applied. Stops at
    /// whichever of the two lists runs out.
    pub placements: Vec<(WindowId, Rect)>,
    /// Managed windows on the target display the layout had no cell for.
    /// Zero whenever the layout has at least as many cells as the display
    /// has windows.
    pub unplaced: usize,
}

/// The placements applying saved layout `name` would commit against
/// `state` right now, or the reason it would change nothing.
///
/// Pure over `state` so the reducer and a synchronous caller reach the
/// same verdict from the same state: the IPC handler has to answer the CLI
/// with a reason, and an event sent into the queue can only answer with
/// silence.
///
/// Gaps are applied here, by the same [`apply_gaps`] post-processing step
/// the balanced grid and every zone snap already run through, so
/// [`resolve_saved_layout`] stays gap-unaware.
pub fn plan_saved_layout(
    state: &EngineState,
    name: &str,
) -> Result<SavedLayoutPlan, SavedLayoutRejection> {
    if state.paused {
        return Err(SavedLayoutRejection::Paused);
    }
    let Some(layout) = state.resolved_config.layouts.get(name) else {
        return Err(SavedLayoutRejection::UnknownLayout {
            name: name.to_owned(),
        });
    };
    // Existing callers that construct a state synchronously may not yet
    // have observed a focus event. Derive the same target once for that
    // compatibility path; live reducer state always carries it explicitly.
    let display_id = state
        .focused_display
        .or_else(|| {
            state
                .focused_window
                .and_then(|window_id| state.inventory.get(&window_id))
                .map(|managed| managed.window.display_id)
        })
        .ok_or(SavedLayoutRejection::NoFocusedDisplay)?;
    let Some(work_area) = work_area_of(&state.displays, display_id) else {
        return Err(SavedLayoutRejection::DisplayUnavailable { display_id });
    };

    let windows = display_window_order(state, display_id);
    let cells = resolve_saved_layout(work_area, &layout.cells);
    let unplaced = windows.len().saturating_sub(cells.len());

    Ok(SavedLayoutPlan {
        display_id,
        placements: windows
            .into_iter()
            .zip(cells)
            .map(|(window_id, raw_bounds)| {
                (
                    window_id,
                    apply_gaps(raw_bounds, work_area, state.resolved_config.gaps),
                )
            })
            .collect(),
        unplaced,
    })
}

/// `display_id`'s managed windows in visual window order: whatever order
/// the engine already recorded for that display, then any managed window
/// that order doesn't mention, seeded top-to-bottom then left-to-right
/// with the native window id breaking final ties -- the same seeding
/// [`reconcile_balanced_grids`] performs.
///
/// A recorded order wins over the geometric seeding even when it disagrees
/// with where the windows currently sit, and even after automatic tiling
/// has been switched off. That is the point of the concept: the order is
/// *stable*, changed deliberately by directional swap and by nothing else,
/// so a layout applied twice puts the same window in the same cell. The
/// recorded order only ever exists once tiling has run, so in a
/// manual-only session the seeding half is the whole answer.
///
/// Two things differ from [`reconcile_balanced_grids`]' membership, both
/// deliberately. A window with nothing on screen to place -- minimized,
/// hidden, cloaked, full-screen -- is skipped rather than handed a cell.
/// And a `Float` window *is* handed one: applying a saved layout is an
/// explicit user command, and an explicit command places a managed window
/// whatever its automatic-tiling eligibility, exactly as a zone snap
/// places whichever window is focused.
fn display_window_order(state: &EngineState, display_id: DisplayId) -> Vec<WindowId> {
    let placeable = |window_id: &WindowId| {
        state.inventory.get(window_id).is_some_and(|managed| {
            managed.window.display_id == display_id
                && matches!(
                    managed.window.lifecycle,
                    WindowLifecycle::Active | WindowLifecycle::Maximized
                )
        })
    };

    let mut ordered: Vec<WindowId> = state
        .visual_window_order
        .get(&display_id)
        .map(|order| order.iter().copied().filter(placeable).collect())
        .unwrap_or_default();
    let mut unordered: Vec<_> = state
        .inventory
        .values()
        .filter(|managed| placeable(&managed.window.id) && !ordered.contains(&managed.window.id))
        .map(|managed| {
            (
                managed.window.bounds.y,
                managed.window.bounds.x,
                managed.window.id,
            )
        })
        .collect();
    unordered.sort_by_key(|(y, x, window_id)| (*y, *x, window_id.0));
    ordered.extend(unordered.into_iter().map(|(_, _, window_id)| window_id));
    ordered
}

/// Applies one event to `state`. Must never panic -- a single bad event
/// must not take down a reducer thread meant to run all day. Every `Event`
/// variant is handled explicitly so this stays true as the enum grows.
fn apply(state: &mut EngineState, event: Event) {
    // An undo scope never outlives the event that opened it. Clearing it
    // here means a future early return inside a command arm cannot leak
    // that command's captures into the next command's transaction.
    debug_assert!(
        state.undo_scope.is_none(),
        "an undo scope outlived the event that opened it"
    );
    state.undo_scope = None;

    match event {
        Event::PersistenceHealthChanged(health) => {
            if state.persistence_health != health {
                state.persistence_health = health;
            }
        }

        Event::UndoHistoryLoaded(transaction) => {
            let transaction = transaction.map(|boxed| *boxed);
            if state.newest_undo != transaction {
                state.newest_undo = transaction;
                state.revision += 1;
            }
        }

        Event::ContainerTreesLoaded(trees) => {
            if trees.is_empty() {
                return;
            }
            state.pending_trees = trees;
            // A display that already has windows adopts its arrangement
            // now; one that does not will adopt it when it first reflows.
            reconcile_arrangements(state);
            state.revision += 1;
        }

        Event::WorkspacesLoaded(stored) => {
            if stored.is_empty() {
                return;
            }
            for persisted in stored {
                let declared = state
                    .resolved_config
                    .workspaces
                    .iter()
                    .any(|name| name.collides_with(&persisted.name));
                match state.workspaces.resolve(persisted.name.as_str()) {
                    // Configuration already declared it this session; the
                    // database only adds what the file cannot know.
                    Some(_) => {}
                    None => {
                        // A workspace that configuration declared last
                        // session and no longer does is not deleted --
                        // deletion is never implicit -- but it is no
                        // longer configuration's, so a command may delete
                        // it.
                        let origin = if declared {
                            WorkspaceOrigin::Configuration
                        } else {
                            WorkspaceOrigin::Command
                        };
                        if state
                            .workspaces
                            .create(persisted.name.clone(), origin)
                            .is_err()
                        {
                            continue;
                        }
                    }
                }
                let name = state
                    .workspaces
                    .resolve(persisted.name.as_str())
                    .expect("just created or found");
                if let Some(tree) = persisted.tree {
                    state.pending_workspace_trees.insert(name.clone(), tree);
                }
                if let Some(fingerprint) = persisted.displayed_fingerprint {
                    state.pending_displayed.insert(name.clone(), fingerprint);
                }
                // What the database holds is what was last written, so
                // nothing needs rewriting until something changes.
                state.saved_workspaces.insert(
                    name.clone(),
                    PersistedWorkspace {
                        name,
                        origin: persisted.origin,
                        displayed_fingerprint: None,
                        tree: None,
                    },
                );
            }
            // A workspace goes back to the display it was on. The startup
            // fill may already have put a configuration workspace there;
            // one that has no window yet gives way, because nothing is
            // revealed or lost by exchanging two empty workspaces, and
            // the remembered assignment is the one the user last had. A
            // workspace that already has windows is kept: the database
            // answered after the user started working, and what they
            // can see wins over what was stored.
            let remembered: Vec<(WorkspaceName, String)> = state
                .pending_displayed
                .iter()
                .map(|(name, fingerprint)| (name.clone(), fingerprint.clone()))
                .collect();
            for (name, fingerprint) in remembered {
                let Some(display_id) = display_id_of(state, &fingerprint) else {
                    continue;
                };
                if state.workspaces.is_displayed(&name) {
                    state.pending_displayed.remove(&name);
                    continue;
                }
                let current_is_empty = state
                    .workspaces
                    .displayed_on(display_id)
                    .is_none_or(|current| state.workspaces.members_of(current).is_empty());
                if !current_is_empty {
                    continue;
                }
                stash_display_tree(state, display_id);
                state.workspaces.display(&name, display_id);
                adopt_workspace_tree(state, &name, display_id);
                state.pending_displayed.remove(&name);
            }
            fill_empty_displays(state);
            // A resolved topology-profile preference outranks what was
            // remembered.
            apply_switching_mapping(state);
            assign_unassigned_windows(state);
            reconcile_hidden_workspace_windows(state);
            reconcile_arrangements(state);
            persist_workspaces(state);
            state.revision += 1;
        }

        Event::WorkspaceCreateRequested { name } => {
            let result = match plan_workspace_create(state, &name) {
                Ok(_) => {
                    let name = WorkspaceName::new(&name).expect("planned");
                    let applied = state
                        .workspaces
                        .create(name, WorkspaceOrigin::Command)
                        .expect("planned");
                    persist_workspaces(state);
                    WorkspaceCommandResult::Created(applied)
                }
                Err(refusal) => {
                    tracing::info!(%refusal, "workspace create refused; nothing changed");
                    WorkspaceCommandResult::Refused(refusal)
                }
            };
            state.last_workspace_result = Some(result);
            state.revision += 1;
        }

        Event::WorkspaceDeleteRequested { name } => {
            let result = match plan_workspace_delete(state, &name) {
                Ok(_) => {
                    let applied = state.workspaces.delete(&name).expect("planned");
                    state.saved_workspaces.remove(&applied.name);
                    state.pending_workspace_trees.remove(&applied.name);
                    state.pending_displayed.remove(&applied.name);
                    state
                        .persistence_intents
                        .push(PersistenceIntent::DeleteWorkspace(applied.name.clone()));
                    WorkspaceCommandResult::Deleted(applied)
                }
                Err(refusal) => {
                    tracing::info!(%refusal, "workspace delete refused; nothing changed");
                    WorkspaceCommandResult::Refused(refusal)
                }
            };
            state.last_workspace_result = Some(result);
            state.revision += 1;
        }

        Event::WorkspaceFocusRequested { name } => {
            let result = match plan_workspace_focus(state, &name) {
                Ok(WorkspaceFocusApplied::Displayed {
                    name,
                    display_id,
                    replaced,
                }) => {
                    // A switch that moves no window is pure bookkeeping,
                    // so it commits here rather than through the
                    // transaction: there is no native move that could
                    // fail, and so nothing to compensate. It is still one
                    // reversible change of the displayed assignment.
                    open_undo_scope(state, &format!("workspace-focus {name}"));
                    capture_assignment_for_undo(state, display_id);
                    if replaced.is_some() {
                        stash_display_tree(state, display_id);
                    }
                    state.workspaces.display(&name, display_id);
                    adopt_workspace_tree(state, &name, display_id);
                    state.focused_display = Some(display_id);
                    reconcile_arrangements(state);
                    persist_workspaces(state);
                    close_undo_scope(state);
                    WorkspaceCommandResult::Focused(WorkspaceFocusApplied::Displayed {
                        name,
                        display_id,
                        replaced,
                    })
                }
                Ok(applied @ WorkspaceFocusApplied::SwitchStarted { .. }) => {
                    // The preflight above already decided this; planning
                    // again is what hands the transaction the window
                    // lists behind that answer.
                    match plan_workspace_switch(state, &name) {
                        Ok(plan) => {
                            begin_workspace_switch(state, plan, None);
                            // A switch that settled inside `begin` has
                            // published its own result already; one still
                            // in flight reports that it started.
                            match state.switch {
                                Some(_) => WorkspaceCommandResult::Focused(applied),
                                None => state
                                    .last_switch_result
                                    .clone()
                                    .unwrap_or(WorkspaceCommandResult::Focused(applied)),
                            }
                        }
                        Err(refusal) => {
                            tracing::info!(%refusal, "workspace switch refused; nothing changed");
                            WorkspaceCommandResult::Refused(refusal)
                        }
                    }
                }
                Ok(WorkspaceFocusApplied::FocusedExisting {
                    name,
                    display_id,
                    focused_window,
                }) => {
                    state.focused_display = Some(display_id);
                    if let Some(window_id) = focused_window {
                        state.effects.push(EngineEffect::FocusWindow { window_id });
                    }
                    WorkspaceCommandResult::Focused(WorkspaceFocusApplied::FocusedExisting {
                        name,
                        display_id,
                        focused_window,
                    })
                }
                Err(refusal) => {
                    tracing::info!(%refusal, "workspace focus refused; nothing changed");
                    WorkspaceCommandResult::Refused(refusal)
                }
            };
            state.last_workspace_result = Some(result);
            state.revision += 1;
        }

        Event::WorkspaceMoveRequested { name, display_id } => {
            let result = match plan_workspace_move(state, &name, display_id) {
                Ok(applied) => {
                    // Both displays give up their trees and take the
                    // other's, so neither workspace loses structure and
                    // nothing becomes hidden.
                    let moving = state.trees.remove(&applied.from_display_id);
                    let displaced = state.trees.remove(&applied.to_display_id);
                    for display in [applied.from_display_id, applied.to_display_id] {
                        state.saved_trees.remove(&display);
                        state.constraint_overflow.remove(&display);
                        state.visual_window_order.remove(&display);
                    }
                    if let Some(tree) = moving {
                        state.trees.insert(applied.to_display_id, tree);
                    }
                    if let Some(tree) = displaced {
                        state.trees.insert(applied.from_display_id, tree);
                    }
                    state
                        .workspaces
                        .swap_displays(applied.from_display_id, applied.to_display_id);
                    state.focused_display = Some(applied.to_display_id);
                    reconcile_arrangements(state);
                    persist_workspaces(state);
                    WorkspaceCommandResult::Moved(applied)
                }
                Err(refusal) => {
                    tracing::info!(%refusal, "workspace move refused; nothing changed");
                    WorkspaceCommandResult::Refused(refusal)
                }
            };
            state.last_workspace_result = Some(result);
            state.revision += 1;
        }

        Event::ParkingCapabilityReported(capability) => {
            if state.parking_capability != capability {
                state.parking_capability = capability;
                state.revision += 1;
            }
        }

        Event::ParkingAuthorizationRequested { window_id } => {
            match plan_parking_authorization(state, window_id) {
                Ok(draft) => {
                    request_recovery_entry(state, window_id, draft, None);
                    state.last_parking_refusal = None;
                }
                Err(refusal) => {
                    tracing::info!(%refusal, "parking refused; the window stays visible");
                    state.last_parking_refusal = Some(refusal);
                }
            }
            state.revision += 1;
        }

        Event::RecoveryEntryDurable { token, entry_id } => {
            let Some(index) = state
                .pending_parking
                .iter()
                .position(|pending| pending.token == token)
            else {
                return;
            };
            let pending = state.pending_parking.remove(index);
            // Recovery data is durable; the window may leave visible
            // geometry now, and not before. A window that has since left
            // management is not parked at all.
            let parks = state.inventory.contains_key(&pending.window_id);
            if let Some(switch) = state
                .switch
                .as_mut()
                .filter(|switch| pending.transaction == Some(switch.id))
            {
                switch.recording.retain(|other| *other != token);
                if switch.phase == WorkspaceSwitchPhase::Recording && switch.recording.is_empty() {
                    switch.phase = WorkspaceSwitchPhase::Parking;
                }
                // Compensation puts the whole re-park set in flight up
                // front, so that it cannot look settled before its drafts
                // exist. Only the forward path adds a window here.
                if parks && !switch.in_flight.contains(&pending.window_id) {
                    switch.in_flight.push(pending.window_id);
                }
            }
            if parks {
                state.effects.push(EngineEffect::ParkWindow {
                    window_id: pending.window_id,
                    entry_id,
                });
            }
            advance_switch(state);
            state.revision += 1;
        }

        Event::RecoveryEntryRefused { token } => {
            let Some(refused) = state
                .pending_parking
                .iter()
                .find(|pending| pending.token == token)
                .cloned()
            else {
                return;
            };
            state
                .pending_parking
                .retain(|pending| pending.token != token);
            tracing::warn!(
                token,
                "recovery data was not durable; the window stays visible"
            );
            // A switch may never park a window it could not promise to
            // put back, so one refused entry cancels the whole switch.
            if state
                .switch
                .as_ref()
                .is_some_and(|switch| refused.transaction == Some(switch.id))
            {
                if let Some(switch) = state.switch.as_mut() {
                    switch.recording.retain(|other| *other != token);
                }
                begin_switch_compensation(
                    state,
                    "recovery data for a window could not be made durable".to_owned(),
                );
            }
            state.revision += 1;
        }

        Event::WindowParked {
            window_id,
            entry_id,
        } => {
            state.parked_windows.insert(window_id, entry_id);
            state.last_parking_failure = None;
            state
                .persistence_intents
                .push(PersistenceIntent::MarkParked(entry_id));
            settle_stranded_window(state, window_id);
            switch_window_settled(state, window_id, None);
            state.revision += 1;
        }

        Event::WindowParkFailed {
            window_id,
            entry_id,
            reason,
        } => {
            tracing::warn!(?window_id, entry = entry_id.0, %reason, "parking failed; the window stays visible");
            state.last_parking_failure = Some(ParkingFailure {
                window_id,
                entry_id,
                stage: ParkingStage::Park,
                reason: reason.clone(),
            });
            let in_switch = state
                .switch
                .as_ref()
                .is_some_and(|switch| switch.in_flight.contains(&window_id));
            switch_window_settled(state, window_id, Some(&reason));
            if !in_switch {
                // Nothing was compensating this one: it was parked to
                // keep a hidden workspace off the screen, and it is still
                // on it.
                strand_window(state, window_id, reason);
            }
            state.revision += 1;
        }

        Event::WindowRestored { window_id } => {
            if let Some(entry_id) = state.parked_windows.remove(&window_id) {
                state.last_parking_failure = None;
                state
                    .persistence_intents
                    .push(PersistenceIntent::MarkRestored(entry_id));
                state.revision += 1;
            }
            settle_stranded_window(state, window_id);
            switch_window_settled(state, window_id, None);
        }

        Event::WindowRestoreFailed {
            window_id,
            entry_id,
            reason,
        } => {
            tracing::warn!(?window_id, entry = entry_id.0, %reason, "restoring a parked window failed; it stays parked with its entry open");
            state.last_parking_failure = Some(ParkingFailure {
                window_id,
                entry_id,
                stage: ParkingStage::Restore,
                reason: reason.clone(),
            });
            switch_window_settled(state, window_id, Some(&reason));
            state.revision += 1;
        }

        Event::WorkspaceSwitchRestoreRequested => {
            match plan_workspace_switch_restore(state) {
                WorkspaceSwitchRestoreResult::NotDegraded => {
                    tracing::info!("no degraded workspace switch to reconcile");
                }
                WorkspaceSwitchRestoreResult::Reconciled { .. } => {
                    // Every stranded window is already where its
                    // workspace says it belongs, so there is nothing left
                    // but the accounting.
                    tracing::info!(
                        "every stranded window is already in place; workspace switching is unblocked"
                    );
                    state.switch_degraded = None;
                }
                WorkspaceSwitchRestoreResult::Requested { windows } => {
                    for window_id in windows {
                        match stranded_move_for(state, window_id) {
                            Some(StrandedMove::Restore) => {
                                let Some(entry_id) = state.parked_windows.get(&window_id).copied()
                                else {
                                    continue;
                                };
                                state.effects.push(EngineEffect::RestoreWindow {
                                    window_id,
                                    entry_id,
                                });
                            }
                            Some(StrandedMove::Park) => {
                                // Visible, but its workspace is hidden.
                                // It leaves the screen the only way any
                                // window does: recovery data first.
                                match plan_parking_authorization(state, window_id) {
                                    Ok(draft) => {
                                        request_recovery_entry(state, window_id, draft, None);
                                    }
                                    Err(refusal) => {
                                        tracing::error!(
                                            ?window_id,
                                            %refusal,
                                            "a stranded window cannot be parked; it stays stranded"
                                        );
                                        state.last_parking_refusal = Some(refusal);
                                    }
                                }
                            }
                            None => {}
                        }
                    }
                }
            }
            state.revision += 1;
        }

        Event::RestoreParkedWindowsRequested => {
            let mut parked: Vec<(WindowId, RecoveryEntryId)> = state
                .parked_windows
                .iter()
                .map(|(window_id, entry_id)| (*window_id, *entry_id))
                .collect();
            parked.sort_by_key(|(window_id, _)| window_id.0);
            for (window_id, entry_id) in parked {
                state.effects.push(EngineEffect::RestoreWindow {
                    window_id,
                    entry_id,
                });
            }
            state.revision += 1;
        }

        Event::RecoveryReported(outcomes) => {
            state.recovery_outcomes = outcomes;
            state.revision += 1;
        }

        Event::UndoRequested => {
            if state.paused {
                tracing::debug!("undo requested while paused; ignoring");
                return;
            }
            let result = plan_undo(state);
            if let UndoResult::Applied(applied) = &result {
                // No scope is opened here: undo is not itself undoable.
                // Recording one would make a second undo reverse the
                // first.
                //
                // Structure first, then windows. The trees the command
                // reshaped go back to what they were, through the same
                // confident matching a stored arrangement uses, so the
                // next reflow agrees with the restored placements rather
                // than quietly redoing the command.
                let prior_trees = state
                    .newest_undo
                    .as_ref()
                    .map(|transaction| transaction.prior_trees.clone())
                    .unwrap_or_default();
                let mut restored_structure = false;
                for snapshot in &prior_trees {
                    let Some(display_id) = display_id_of(state, &snapshot.display_fingerprint)
                    else {
                        continue;
                    };
                    let active = active_tiled_windows_on(state, display_id);
                    let restored = restore_tree(state, &snapshot.tree, &active, now_unix());
                    state.trees.insert(display_id, restored);
                    restored_structure = true;
                }
                // A switch is reversed by switching back, not by placing
                // its windows: the parked ones come off the parking site
                // through the ledger, at exactly the bounds the members
                // record. Placing them here as well would move a window
                // the transaction is already moving.
                let switch_back = state.newest_undo.as_ref().and_then(|transaction| {
                    plan_undo_switch_back(state, transaction)
                        .ok()
                        .flatten()
                        .map(|plan| (plan, transaction.id))
                });
                match switch_back {
                    Some((plan, transaction_id)) => {
                        begin_workspace_switch(state, plan, Some(transaction_id));
                    }
                    None => {
                        for restored in &applied.restored {
                            place_window(
                                state,
                                restored.window_id,
                                restored.display_id,
                                restored.placement,
                                None,
                            );
                        }
                    }
                }
                if restored_structure {
                    // Passive: no scope is open, so whatever this moves is
                    // part of the undo rather than a new transaction. With
                    // consistent snapshots it moves nothing, and stores
                    // the restored tree.
                    reconcile_arrangements(state);
                }
                state
                    .persistence_intents
                    .push(PersistenceIntent::ConsumeUndoTransaction(
                        applied.transaction_id,
                    ));
                // The agent republishes whatever the database holds next.
                // Clearing it here stops a second undo re-applying a
                // transaction whose deletion has not landed yet.
                state.newest_undo = None;
            } else {
                tracing::info!(result = ?result, "undo refused; the transaction is kept");
            }
            state.last_undo_result = Some(result);
            state.revision += 1;
        }
        Event::DisplayTopologyChanged(displays) => {
            if displays.is_empty() {
                tracing::warn!("empty display observation; retaining last usable topology");
                return;
            }
            if displays == state.displays {
                tracing::debug!("display topology snapshot was unchanged; ignoring");
                return;
            }
            let topology_identity_changed =
                topology_fingerprint(&displays) != topology_fingerprint(&state.displays);
            tracing::info!(display_count = displays.len(), "display topology changed");
            // Display hotplug: migrate windows whose previous display is
            // no longer present in the new topology to the nearest
            // surviving display. We do this *before* committing `displays`
            // so we can still read the old topology to compute the
            // migration.
            state.interactive_placement = None;
            state.deferred_reflow_displays.clear();
            migrate_orphaned_windows(state, &displays);
            migrate_focused_display(state, &displays);
            // A vanished display's workspace becomes hidden with its tree
            // intact, and no surviving display's workspace is displaced to
            // make room for it. Its windows have already migrated
            // physically, above; they simply are not arranged until the
            // workspace is displayed again.
            hide_workspaces_on_vanished_displays(state, &displays);
            state.displays = displays;
            // A display that is gone has no arrangement to report. Keeping
            // its entry would leave published state naming a layout as
            // current for a screen nobody can see.
            state
                .last_applied_layouts
                .retain(|display_id, _| state.displays.iter().any(|d| d.id == *display_id));
            // A container tree for a display that is gone is structure with
            // nowhere to go. Dropped here rather than during the reflow
            // below, because the reflow does not run when the new topology
            // resolves to a configuration without automatic tiling.
            state
                .trees
                .retain(|display_id, _| state.displays.iter().any(|d| d.id == *display_id));
            state
                .saved_trees
                .retain(|display_id, _| state.displays.iter().any(|d| d.id == *display_id));
            if topology_identity_changed {
                state.resolved_config = select_resolved_config(&state.config_set, &state.displays);
                state.automatic_tiling_suspended = false;
                state.automatic_tiling_active = state.resolved_config.automatic_tiling_enabled;
                sync_workspaces_from_config(state);
            }
            fill_empty_displays(state);
            apply_switching_mapping(state);
            assign_unassigned_windows(state);
            reconcile_hidden_workspace_windows(state);
            reconcile_arrangements(state);
            persist_workspaces(state);
            state.revision += 1;
        }

        Event::WindowPlaced {
            window_id,
            display_id,
            bounds,
        } => {
            if state.paused {
                tracing::debug!("window placed while paused; ignoring");
                return;
            }
            if state.automatic_tiling_active {
                set_session_floating(state, window_id, true);
            }
            place_window(state, window_id, display_id, bounds, None);
            reconcile_arrangements(state);
        }

        Event::WindowRestoreRequested { window_id } => {
            let Some(placement) = state.windows.get_mut(&window_id) else {
                tracing::debug!(
                    ?window_id,
                    "restore requested for an untracked window; ignoring"
                );
                return;
            };
            let Some((previous_display_id, previous_bounds)) = placement.previous_placement.take()
            else {
                tracing::debug!(
                    ?window_id,
                    "restore requested but no prior placement is remembered; ignoring"
                );
                return;
            };
            let restored_from = (placement.display_id, placement.bounds);
            placement.display_id = previous_display_id;
            placement.bounds = previous_bounds;
            // The inventory is the source the identity matcher reads, so it
            // has to follow the window here as it does in `place_window`.
            if let Some(managed) = state.inventory.get_mut(&window_id) {
                managed.window.display_id = previous_display_id;
                managed.window.bounds = previous_bounds;
            }
            // Restore keeps its own meaning -- one remembered in-session
            // placement, consumed when used -- and is also an explicit
            // placement command, so it is durably reversible like any other.
            open_undo_scope(state, "restore");
            record_undo_member(state, window_id, Some(restored_from));
            state.effects.push(EngineEffect::PlaceWindow {
                window_id,
                display_id: previous_display_id,
                bounds: previous_bounds,
            });
            state.revision += 1;
            close_undo_scope(state);
        }

        Event::WindowThrowToDisplayRequested {
            window_id,
            direction,
        } => {
            if state.paused {
                tracing::debug!("throw-to-display requested while paused; ignoring");
                return;
            }
            let Some(placement) = state.windows.get(&window_id) else {
                tracing::debug!(
                    ?window_id,
                    "throw-to-display requested for an untracked window; ignoring"
                );
                return;
            };
            let (from_display_id, bounds) = (placement.display_id, placement.bounds);

            let Some(to_display_id) = cycle_display(&state.displays, from_display_id, direction)
            else {
                tracing::debug!(
                    ?window_id,
                    "no adjacent display to throw the window to; ignoring"
                );
                return;
            };
            let Some(from_work_area) = work_area_of(&state.displays, from_display_id) else {
                tracing::debug!(
                    ?window_id,
                    ?from_display_id,
                    "window's display is no longer in the topology; ignoring throw"
                );
                return;
            };
            let Some(to_work_area) = work_area_of(&state.displays, to_display_id) else {
                tracing::debug!(
                    ?window_id,
                    ?to_display_id,
                    "target display vanished mid-throw; ignoring"
                );
                return;
            };

            let new_bounds = throw_preserving_ratio(bounds, from_work_area, to_work_area);
            // Opened only now that every guard has passed, so a throw that
            // does nothing records nothing.
            open_undo_scope(state, throw_command(direction));
            if state.automatic_tiling_active && state.inventory.contains_key(&window_id) {
                if let Some(order) = state.visual_window_order.get_mut(&from_display_id) {
                    order.retain(|id| *id != window_id);
                }
                let target_order = state.visual_window_order.entry(to_display_id).or_default();
                if !target_order.contains(&window_id) {
                    target_order.push(window_id);
                }
                if let Some(placement) = state.windows.get_mut(&window_id) {
                    placement.display_id = to_display_id;
                    placement.bounds = new_bounds;
                }
                if let Some(managed) = state.inventory.get_mut(&window_id) {
                    managed.window.display_id = to_display_id;
                    managed.window.bounds = new_bounds;
                }
                // This branch moves the thrown window itself without going
                // through `place_window`, so it records its own member.
                record_undo_member(state, window_id, Some((from_display_id, bounds)));
                reconcile_arrangements(state);
                state.revision += 1;
            } else {
                place_window(state, window_id, to_display_id, new_bounds, None);
            }
            close_undo_scope(state);
        }

        Event::WindowFocused {
            window_id,
            display_id,
            bounds,
        } => {
            state.windows.entry(window_id).or_insert(WindowPlacement {
                display_id,
                bounds,
                observed_bounds: bounds,
                previous_placement: None,
                cycle_step: None,
                rejection_count: 0,
            });
            state.focused_window = Some(window_id);
            state.workspaces.note_focus(window_id);
            if state
                .displays
                .iter()
                .any(|display| display.id == display_id)
            {
                state.focused_display = Some(display_id);
            }
            state.revision += 1;
        }

        Event::DesktopFocused => {
            if state.focused_window.take().is_some() {
                state.revision += 1;
            }
        }

        Event::FocusDisplayRequested { display_id } => {
            if state
                .displays
                .iter()
                .any(|display| display.id == display_id)
                && state.focused_display != Some(display_id)
            {
                state.focused_display = Some(display_id);
                state.revision += 1;
            }
        }

        Event::ZoneSnapRequested { direction } => {
            if state.paused {
                tracing::debug!("zone-snap requested while paused; ignoring");
                return;
            }
            let Some(window_id) = state.focused_window else {
                tracing::debug!("zone-snap requested with no focused window; ignoring");
                return;
            };
            let Some(placement) = state.windows.get(&window_id) else {
                tracing::debug!(
                    ?window_id,
                    "zone-snap requested for an untracked focused window; ignoring"
                );
                return;
            };
            let display_id = placement.display_id;
            let Some(work_area) = work_area_of(&state.displays, display_id) else {
                tracing::debug!(
                    ?window_id,
                    ?display_id,
                    "focused window's display is no longer in the topology; ignoring zone-snap"
                );
                return;
            };

            // Circuit breaker reset: an explicit zone-snap command from
            // the user always resets the rejection counter, giving a
            // deliberate escape hatch even for windows that previously
            // rejected automatic placements.
            if let Some(placement) = state.windows.get_mut(&window_id) {
                if placement.rejection_count > 0 {
                    tracing::info!(
                        ?window_id,
                        "zone-snap command resetting circuit breaker for window"
                    );
                    placement.rejection_count = 0;
                }
            }

            // Everything this command moves -- the snapped window and the
            // reflow it provokes below -- belongs to one undo transaction.
            open_undo_scope(state, zone_snap_command(direction));

            // Re-borrow immutably after the mutable get above.
            let placement = state.windows.get(&window_id).expect("checked above");
            if let Some(horizontal_direction) = direction.horizontal() {
                let next_step = match placement.cycle_step {
                    Some((previous_direction, previous_step))
                        if previous_direction == horizontal_direction =>
                    {
                        previous_step.next()
                    }
                    _ => CycleStep::Half,
                };
                let raw_bounds = resolve_zone_cycle(work_area, horizontal_direction, next_step);
                let bounds = apply_gaps(raw_bounds, work_area, state.resolved_config.gaps);
                place_window(
                    state,
                    window_id,
                    display_id,
                    bounds,
                    Some((horizontal_direction, next_step)),
                );
            } else {
                let zone = if direction == ZoneSnapDirection::Top {
                    HalfZone::TopHalf
                } else {
                    HalfZone::BottomHalf
                };
                let raw_bounds = snap_to_half(work_area, zone);
                let bounds = apply_gaps(raw_bounds, work_area, state.resolved_config.gaps);
                place_window(state, window_id, display_id, bounds, None);
            }
            if state.automatic_tiling_active {
                set_session_floating(state, window_id, true);
                reconcile_arrangements(state);
            }
            close_undo_scope(state);
        }

        Event::WindowBoundsObserved {
            window_id,
            display_id,
            bounds,
        } => {
            let Some(placement) = state.windows.get_mut(&window_id) else {
                tracing::debug!(
                    ?window_id,
                    "bounds observed for an untracked window; ignoring"
                );
                return;
            };
            // `observed_bounds` tracks reality even when it agrees with the
            // intended placement, so it is written before the correlation
            // check below returns.
            let moved = placement.observed_bounds != bounds;
            placement.observed_bounds = bounds;
            if placement.display_id == display_id && placement.bounds == bounds {
                tracing::debug!(
                    ?window_id,
                    "observed bounds match the last placement transaction; cycle state unaffected"
                );
                if moved {
                    state.revision += 1;
                }
                return;
            }
            if placement.cycle_step.take().is_some() {
                tracing::debug!(
                    ?window_id,
                    "observed bounds don't match the last placement transaction; cycle step reset"
                );
                state.revision += 1;
            } else if moved {
                state.revision += 1;
            }
        }

        Event::ConfigChanged(config_set) => {
            if *config_set == state.config_set {
                tracing::debug!("config event was not a real change; ignoring");
                return;
            }
            tracing::info!("resolved config changed");
            state.resolved_config = select_resolved_config(&config_set, &state.displays);
            state.rules = compile_rules(&state.resolved_config);
            if !state.resolved_config.automatic_tiling_enabled {
                state.automatic_tiling_suspended = false;
            }
            state.automatic_tiling_active =
                state.resolved_config.automatic_tiling_enabled && !state.automatic_tiling_suspended;
            state.config_set = *config_set;
            sync_workspaces_from_config(state);
            fill_empty_displays(state);
            apply_switching_mapping(state);
            assign_unassigned_windows(state);
            reconcile_hidden_workspace_windows(state);
            reconcile_arrangements(state);
            persist_workspaces(state);
            state.revision += 1;
        }

        Event::PauseRequested => {
            if state.paused {
                tracing::debug!("pause requested but already paused; ignoring");
                return;
            }
            tracing::info!("window management paused");
            state.paused = true;
            state.revision += 1;
        }

        Event::ResumeRequested => {
            if !state.paused {
                tracing::debug!("resume requested but not paused; ignoring");
                return;
            }
            tracing::info!("window management resumed");
            state.paused = false;
            reconcile_arrangements(state);
            state.revision += 1;
        }

        Event::ToggleAutomaticTilingRequested => {
            if !state.resolved_config.automatic_tiling_enabled {
                tracing::debug!(
                    "automatic-tiling toggle requested for a manual topology; ignoring"
                );
                return;
            }
            state.automatic_tiling_suspended = !state.automatic_tiling_suspended;
            state.automatic_tiling_active = !state.automatic_tiling_suspended;
            open_undo_scope(state, "toggle-automatic-tiling");
            if state.automatic_tiling_active {
                reconcile_arrangements(state);
            }
            state.revision += 1;
            close_undo_scope(state);
        }

        Event::ToggleFloatingRequested => {
            let Some(window_id) = state.focused_window else {
                return;
            };
            if !state.inventory.contains_key(&window_id) {
                return;
            }
            let action = state.inventory[&window_id].action;
            open_undo_scope(state, "toggle-floating");
            if action == ManageAction::Float {
                let force_tiled = !state.session_tiled.contains(&window_id);
                if force_tiled {
                    state.session_tiled.insert(window_id);
                    state.session_floating.remove(&window_id);
                } else {
                    state.session_tiled.remove(&window_id);
                }
                if let Some(managed) = state.inventory.get_mut(&window_id) {
                    managed.eligibility = if force_tiled {
                        EligibilityReason::Eligible
                    } else {
                        EligibilityReason::FloatingRule
                    };
                }
            } else {
                let floating = !state.session_floating.contains(&window_id);
                set_session_floating(state, window_id, floating);
            }
            reconcile_arrangements(state);
            state.revision += 1;
            close_undo_scope(state);
        }

        Event::DirectionalFocusRequested { direction } => {
            let Some(window_id) = directional_neighbor(state, direction) else {
                return;
            };
            state.effects.push(EngineEffect::FocusWindow { window_id });
            state.revision += 1;
        }

        Event::DirectionalSwapRequested { direction } => {
            let plan = match plan_directional_swap(state, direction) {
                Ok(plan) => plan,
                Err(refusal) => {
                    tracing::info!(%refusal, "directional swap refused; nothing changed");
                    return;
                }
            };
            let (focused, neighbor, display_id) =
                (plan.window_id, plan.neighbor_id, plan.display_id);
            let Some(order) = state.visual_window_order.get_mut(&display_id) else {
                return;
            };
            let Some(focused_index) = order.iter().position(|id| *id == focused) else {
                return;
            };
            let Some(neighbor_index) = order.iter().position(|id| *id == neighbor) else {
                return;
            };
            order.swap(focused_index, neighbor_index);
            // The scope opens before the tree changes, so the transaction
            // can carry the structure as it was and not only the windows.
            open_undo_scope(state, swap_command(direction));
            // In tree mode the arrangement comes from the tree, not from
            // visual order, so the exchange has to happen there too. It is
            // an exchange of the two leaves' occupants and nothing else:
            // containers, axes, weights, and parentage all stay as they
            // were. Focus is deliberately not moved, so the user keeps
            // controlling the window they just moved.
            if state.resolved_config.tiling_mode == TilingMode::Tree {
                capture_tree_for_undo(state, display_id);
                if let Some(tree) = state.trees.get_mut(&display_id) {
                    tree.swap_leaves(&focused, &neighbor);
                }
            }
            reconcile_arrangements(state);
            state.revision += 1;
            close_undo_scope(state);
        }

        Event::TreePositionRemoveRequested {
            display_id,
            position,
        } => {
            if let Err(refusal) = plan_remove_position(state, display_id, position) {
                tracing::info!(%refusal, "remove-position refused; nothing changed");
                return;
            }
            open_undo_scope(state, "remove-position");
            capture_tree_for_undo(state, display_id);
            if let Some(tree) = state.trees.get_mut(&display_id) {
                tree.remove_position(position);
            }
            reconcile_arrangements(state);
            state.revision += 1;
            close_undo_scope(state);
        }

        Event::TreeResizeRequested { direction } => {
            let plan = match plan_tree_resize(state, direction) {
                Ok(plan) => plan,
                Err(refusal) => {
                    tracing::info!(%refusal, "tree resize refused; nothing changed");
                    return;
                }
            };
            let display_id = plan.applied.display_id;
            open_undo_scope(state, resize_command(direction));
            capture_tree_for_undo(state, display_id);
            state.trees.insert(display_id, plan.tree);
            reconcile_arrangements(state);
            state.revision += 1;
            close_undo_scope(state);
        }

        Event::InteractivePlacementStarted { window_id } => {
            let display_id = state
                .inventory
                .get(&window_id)
                .map(|managed| managed.window.display_id)
                .or_else(|| {
                    state
                        .windows
                        .get(&window_id)
                        .map(|placement| placement.display_id)
                });
            let Some(display_id) = display_id else {
                return;
            };
            state.interactive_placement = Some(InteractivePlacementSession {
                window_id,
                display_id,
            });
            state.revision += 1;
        }

        Event::InteractivePlacementEnded {
            window_id,
            committed_manual_placement,
        } => {
            if !state
                .interactive_placement
                .is_some_and(|session| session.window_id == window_id)
            {
                return;
            }
            state.interactive_placement = None;
            state.deferred_reflow_displays.clear();
            let effect_start = state.effects.len();
            reconcile_arrangements(state);
            if !committed_manual_placement
                && !state.effects[effect_start..].iter().any(|effect| {
                    matches!(effect, EngineEffect::PlaceWindow { window_id: id, .. } if *id == window_id)
                })
            {
                if let Some(placement) = state.windows.get(&window_id) {
                    state.effects.push(EngineEffect::PlaceWindow {
                        window_id,
                        display_id: placement.display_id,
                        bounds: placement.bounds,
                    });
                }
            }
            state.revision += 1;
        }

        // Startup reconciliation.
        Event::StartupReconciliation { windows } => {
            let mut registered = 0usize;
            for (window_id, display_id, bounds) in windows {
                state.windows.entry(window_id).or_insert_with(|| {
                    registered += 1;
                    WindowPlacement {
                        display_id,
                        bounds,
                        observed_bounds: bounds,
                        previous_placement: None,
                        cycle_step: None,
                        rejection_count: 0,
                    }
                });
            }
            if registered > 0 {
                tracing::info!(
                    registered,
                    "startup reconciliation registered existing windows"
                );
                state.revision += 1;
            } else {
                tracing::debug!("startup reconciliation: no new windows to register");
            }
        }

        // Sleep/wake recovery.
        Event::WakeReconciliation { displays, windows } => {
            tracing::info!(
                display_count = displays.len(),
                window_count = windows.as_ref().map_or(0, Vec::len),
                "wake reconciliation starting"
            );
            // Step 1: migrate orphaned windows using the old topology before
            // committing the new one (same as the hotplug path in
            // DisplayTopologyChanged).
            if displays.is_empty() {
                tracing::warn!("empty wake display observation; retaining last usable topology");
            } else if topology_fingerprint(&displays) != topology_fingerprint(&state.displays) {
                migrate_orphaned_windows(state, &displays);
                hide_workspaces_on_vanished_displays(state, &displays);
                state.displays = displays;
                state.resolved_config = select_resolved_config(&state.config_set, &state.displays);
                state.automatic_tiling_suspended = false;
                state.automatic_tiling_active = state.resolved_config.automatic_tiling_enabled;
                sync_workspaces_from_config(state);
                fill_empty_displays(state);
                apply_switching_mapping(state);
            }
            if let Some(windows) = windows {
                state.observed_windows = windows
                    .iter()
                    .cloned()
                    .map(|window| (window.id, window))
                    .collect();
                replace_inventory_from_observations(state, windows);
            }
            tracing::info!("wake reconciliation complete");
            reconcile_arrangements(state);
            state.revision += 1;
        }

        // Per-window circuit breaker.
        Event::PlacementRejected { window_id } => {
            let Some(rejection_count) = state.windows.get_mut(&window_id).map(|placement| {
                placement.rejection_count = placement.rejection_count.saturating_add(1);
                placement.rejection_count
            }) else {
                tracing::debug!(
                    ?window_id,
                    "placement rejection for an untracked window; ignoring"
                );
                return;
            };
            if rejection_count == CIRCUIT_BREAKER_THRESHOLD {
                tracing::warn!(
                    ?window_id,
                    threshold = CIRCUIT_BREAKER_THRESHOLD,
                    "circuit breaker opened: window repeatedly rejected placement; \
                     stopping automatic management until user resets with a zone-snap command"
                );
                if let Some(managed) = state.inventory.get_mut(&window_id) {
                    managed.eligibility = EligibilityReason::CircuitOpen;
                }
                reconcile_arrangements(state);
                state.revision += 1;
            } else {
                tracing::debug!(
                    ?window_id,
                    rejection_count,
                    threshold = CIRCUIT_BREAKER_THRESHOLD,
                    "placement rejection recorded"
                );
            }
        }

        Event::PlacementAccepted { window_id } => {
            let Some(placement) = state.windows.get_mut(&window_id) else {
                return;
            };
            if placement.rejection_count == 0 {
                return;
            }
            placement.rejection_count = 0;
            if let Some(managed) = state.inventory.get_mut(&window_id) {
                managed.eligibility = eligibility_for(&managed.window, managed.action, false);
            }
            state.revision += 1;
        }

        Event::RearrangeRequested => {
            state.effects.push(EngineEffect::ReconcileWindows);
            state.revision += 1;
        }

        Event::RearrangeReconciliationComplete { windows } => {
            state.observed_windows = windows
                .iter()
                .cloned()
                .map(|window| (window.id, window))
                .collect();
            replace_inventory_from_observations(state, windows);
            let mut reset = 0usize;
            for placement in state.windows.values_mut() {
                if placement.circuit_open() {
                    placement.rejection_count = 0;
                    reset += 1;
                }
            }
            for managed in state.inventory.values_mut() {
                if managed.eligibility == EligibilityReason::CircuitOpen {
                    managed.eligibility = eligibility_for(&managed.window, managed.action, false);
                }
            }
            tracing::info!(reset, "rearrange reset open placement circuits");
            // This event only ever follows an explicit rearrange, so its
            // placements belong to that command's transaction. The passive
            // startup and wake reconciliations deliberately have no scope.
            open_undo_scope(state, "rearrange");
            reconcile_arrangements(state);
            state.revision += 1;
            close_undo_scope(state);
        }

        Event::WindowsObserved { windows } => {
            if state.automatic_tiling_active {
                for window in &windows {
                    let was_tiled_and_normal =
                        state.inventory.get(&window.id).is_some_and(|managed| {
                            managed.action == ManageAction::Tile
                                && managed.window.lifecycle != WindowLifecycle::Maximized
                        });
                    if was_tiled_and_normal && window.lifecycle == WindowLifecycle::Maximized {
                        state.session_floating.insert(window.id);
                    }
                }
            }
            state.observed_windows = windows
                .iter()
                .cloned()
                .map(|window| (window.id, window))
                .collect();
            if replace_inventory_from_observations(state, windows) {
                reconcile_arrangements(state);
                state.revision += 1;
            }
        }

        Event::RulesChanged { rules } => {
            state.rules = rules;
            let observed: Vec<Window> = state.observed_windows.values().cloned().collect();
            replace_inventory_from_observations(state, observed);
            reconcile_arrangements(state);
            state.revision += 1;
        }

        Event::SavedLayoutApplyRequested { name } => {
            // The verdict is re-reached here rather than trusted from
            // whoever sent the event: state can have moved on between the
            // two, and a hotkey has no synchronous caller to ask at all.
            match plan_saved_layout(state, &name) {
                Ok(plan) => {
                    // One command, one transaction, however many windows
                    // it moves -- including the reflow below.
                    open_undo_scope(state, &format!("apply-layout {name}"));
                    if plan.unplaced > 0 {
                        tracing::info!(
                            layout = %name,
                            unplaced = plan.unplaced,
                            "saved layout has fewer cells than the display has windows; \
                             the surplus windows were left where they are"
                        );
                    }
                    // Only windows the placement actually reached are
                    // taken out of the tiling set: a window whose circuit
                    // breaker suppressed its placement was not moved, so
                    // floating it would drop it from the grid for nothing.
                    let mut placed = Vec::new();
                    for (window_id, bounds) in plan.placements {
                        if place_window(state, window_id, plan.display_id, bounds, None) {
                            placed.push(window_id);
                        }
                    }
                    if !placed.is_empty() {
                        state
                            .last_applied_layouts
                            .insert(plan.display_id, name.clone());
                    }
                    // Applying a layout is an explicit placement, so it
                    // session-floats what it placed exactly as a zone snap
                    // does, and the rest of the tiling set reflows around
                    // the result in one pass.
                    if state.automatic_tiling_active && !placed.is_empty() {
                        for window_id in placed {
                            set_session_floating(state, window_id, true);
                        }
                        reconcile_arrangements(state);
                    }
                    close_undo_scope(state);
                }
                Err(rejection) => {
                    tracing::warn!(
                        layout = %name,
                        reason = %rejection,
                        "saved layout not applied"
                    );
                }
            }
        }

        Event::HotkeyCaptureStarted => {
            state.capture_holds += 1;
            if state.hotkey_capture_suspended {
                tracing::debug!(
                    holds = state.capture_holds,
                    "another editor already holds hotkey capture"
                );
                return;
            }
            tracing::info!("hotkey editor opened; hotkey registration suspended");
            state.hotkey_capture_suspended = true;
            // What did not come back from the *last* capture is
            // deliberately left standing. It is only known once
            // registration resumes, by which time the editor that caused
            // it has closed -- so the next editor to open is the only one
            // that can tell the user.
            state.revision += 1;
        }

        Event::HotkeyCaptureEnded => {
            if state.capture_holds == 0 {
                tracing::debug!("hotkey capture ended but no editor held it");
                return;
            }
            state.capture_holds -= 1;
            if state.capture_holds > 0 {
                tracing::debug!(
                    holds = state.capture_holds,
                    "an editor closed but another still holds hotkey capture"
                );
                return;
            }
            tracing::info!("hotkey editor closed; hotkey registration resumed");
            state.hotkey_capture_suspended = false;
            state.revision += 1;
        }

        Event::HotkeyRegistrationReported { unregistered } => {
            if state.unregistered_bindings == unregistered {
                return;
            }
            if !unregistered.is_empty() {
                tracing::warn!(
                    count = unregistered.len(),
                    "some hotkey bindings did not register; another application may own them"
                );
            }
            state.unregistered_bindings = unregistered;
            state.revision += 1;
        }
    }
}

fn eligibility_for(window: &Window, action: ManageAction, circuit_open: bool) -> EligibilityReason {
    if action == ManageAction::Float {
        return EligibilityReason::FloatingRule;
    }
    if !window.capabilities.is_tileable() {
        return EligibilityReason::NotTileable;
    }
    if window.elevated {
        return EligibilityReason::Elevated;
    }
    if circuit_open {
        return EligibilityReason::CircuitOpen;
    }
    match window.lifecycle {
        WindowLifecycle::Active => EligibilityReason::Eligible,
        WindowLifecycle::Minimized => EligibilityReason::Minimized,
        WindowLifecycle::Maximized => EligibilityReason::Maximized,
        WindowLifecycle::Fullscreen => EligibilityReason::Fullscreen,
        WindowLifecycle::Hidden => EligibilityReason::Hidden,
        WindowLifecycle::Cloaked => EligibilityReason::Cloaked,
    }
}

/// Whether `window_id` currently occupies a live, actively arranged leaf
/// of `display_id`'s container tree: in it, and not in constraint
/// overflow. Under the balanced grid every eligible window is arranged,
/// so this is only asked in tree mode.
fn arranged_in_tree(state: &EngineState, display_id: DisplayId, window_id: WindowId) -> bool {
    state
        .trees
        .get(&display_id)
        .is_some_and(|tree| tree.contains(&window_id))
        && !state
            .constraint_overflow
            .get(&display_id)
            .is_some_and(|overflow| overflow.contains(&window_id))
}

/// Whether `window_id` can be an endpoint of a directional command on
/// `display_id`: eligible for tiling and, in tree mode, actually arranged.
fn directional_endpoint(state: &EngineState, display_id: DisplayId, window_id: WindowId) -> bool {
    let eligible = state.inventory.get(&window_id).is_some_and(|managed| {
        managed.eligibility == EligibilityReason::Eligible
            && managed.window.display_id == display_id
    });
    eligible
        && (state.resolved_config.tiling_mode != TilingMode::Tree
            || !state.automatic_tiling_active
            || arranged_in_tree(state, display_id, window_id))
}

fn directional_neighbor(state: &EngineState, direction: CardinalDirection) -> Option<WindowId> {
    let focused = state.focused_window?;
    let source = state.inventory.get(&focused)?;
    if !directional_endpoint(state, source.window.display_id, focused) {
        return None;
    }
    let source_bounds = state.windows.get(&focused)?.bounds;
    let source_center = (
        i64::from(source_bounds.x) * 2 + i64::from(source_bounds.width),
        i64::from(source_bounds.y) * 2 + i64::from(source_bounds.height),
    );

    state
        .visual_window_order
        .get(&source.window.display_id)?
        .iter()
        .enumerate()
        .filter_map(|(order_index, candidate_id)| {
            if *candidate_id == focused {
                return None;
            }
            if !directional_endpoint(state, source.window.display_id, *candidate_id) {
                return None;
            }
            let bounds = state.windows.get(candidate_id)?.bounds;
            let center = (
                i64::from(bounds.x) * 2 + i64::from(bounds.width),
                i64::from(bounds.y) * 2 + i64::from(bounds.height),
            );
            let (primary, perpendicular) = match direction {
                CardinalDirection::Left if center.0 < source_center.0 => (
                    source_center.0 - center.0,
                    (source_center.1 - center.1).abs(),
                ),
                CardinalDirection::Right if center.0 > source_center.0 => (
                    center.0 - source_center.0,
                    (source_center.1 - center.1).abs(),
                ),
                CardinalDirection::Up if center.1 < source_center.1 => (
                    source_center.1 - center.1,
                    (source_center.0 - center.0).abs(),
                ),
                CardinalDirection::Down if center.1 > source_center.1 => (
                    center.1 - source_center.1,
                    (source_center.0 - center.0).abs(),
                ),
                _ => return None,
            };
            Some(((primary, perpendicular, order_index), *candidate_id))
        })
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, id)| id)
}

fn set_session_floating(state: &mut EngineState, window_id: WindowId, floating: bool) {
    if floating {
        state.session_floating.insert(window_id);
    } else {
        state.session_floating.remove(&window_id);
    }
    if let Some(managed) = state.inventory.get_mut(&window_id) {
        managed.eligibility = if floating {
            EligibilityReason::SessionFloating
        } else {
            let circuit_open = state
                .windows
                .get(&window_id)
                .is_some_and(WindowPlacement::circuit_open);
            eligibility_for(&managed.window, managed.action, circuit_open)
        };
    }
}

/// Rebuilds the inspectable inventory from one normalized observation batch.
/// The evaluator never logs the `Window`, protecting title/path metadata from
/// diagnostics while still retaining it in engine-owned state for reloads.
fn replace_inventory_from_observations(state: &mut EngineState, observed: Vec<Window>) -> bool {
    let evaluator =
        RuleEvaluator::new(state.rules.iter().cloned().chain(builtin_rules()).collect());
    let mut next = HashMap::new();
    let mut rule_targets: Vec<(WindowId, Option<String>, String)> = Vec::new();
    for window in observed {
        let evaluation = evaluator.evaluate(&window);
        let action = evaluation.actions.manage;
        if action == ManageAction::Exclude {
            continue;
        }
        // A rule's workspace target is applied once, when the window is
        // first managed; membership is a fact about the window from then
        // on, and re-evaluating it on every observation would fight a
        // workspace move the user makes later.
        if let Some(target) = evaluation.actions.workspace {
            if !state.inventory.contains_key(&window.id) {
                rule_targets.push((window.id, evaluation.matching_rule_id, target));
            }
        }
        let circuit_open = state
            .windows
            .get(&window.id)
            .is_some_and(WindowPlacement::circuit_open);
        let eligibility = if state.session_tiled.contains(&window.id) {
            eligibility_for(&window, ManageAction::Tile, circuit_open)
        } else if state.session_floating.contains(&window.id) {
            EligibilityReason::SessionFloating
        } else {
            eligibility_for(&window, action, circuit_open)
        };
        state.windows.entry(window.id).or_insert(WindowPlacement {
            display_id: window.display_id,
            bounds: window.bounds,
            observed_bounds: window.bounds,
            previous_placement: None,
            cycle_step: None,
            rejection_count: 0,
        });
        next.insert(
            window.id,
            ManagedWindow {
                window,
                action,
                eligibility,
            },
        );
    }
    let inventory_changed = next != state.inventory;
    let managed_ids: HashSet<_> = next.keys().copied().collect();
    let placements_changed = state.windows.keys().any(|id| !managed_ids.contains(id));
    state.windows.retain(|id, _| managed_ids.contains(id));
    state.session_floating.retain(|id| managed_ids.contains(id));
    state.session_tiled.retain(|id| managed_ids.contains(id));
    if state
        .focused_window
        .is_some_and(|id| !managed_ids.contains(&id))
    {
        state.focused_window = None;
    }
    // A member of a hidden workspace that closed while hidden goes
    // dormant in that workspace's stashed tree now, while the evidence
    // captured at its last reflow is still here. The displayed trees do
    // this for themselves on their next reflow.
    let departed: Vec<WindowId> = state
        .inventory
        .keys()
        .filter(|id| !managed_ids.contains(id))
        .copied()
        .collect();
    let now = now_unix();
    for window_id in departed {
        let Some(name) = state.workspaces.workspace_of(window_id).cloned() else {
            continue;
        };
        if state.workspaces.is_displayed(&name) {
            continue;
        }
        let evidence = state.leaf_evidence.remove(&window_id);
        if let Some(workspace) = state.workspaces.get_mut(&name) {
            if let Some(tree) = workspace.stashed_tree.as_mut() {
                match evidence {
                    Some(evidence) => {
                        tree.make_dormant(
                            &window_id,
                            DormantPosition {
                                evidence,
                                since_unix: now,
                            },
                        );
                    }
                    None => {
                        tree.remove(&window_id);
                    }
                }
            }
        }
    }
    state
        .workspaces
        .retain_members(|id| managed_ids.contains(&id));
    state
        .rule_workspace_refusals
        .retain(|refusal| managed_ids.contains(&refusal.window_id));
    // A parked window that closed has nothing left to restore. Its entry
    // becomes history now, so the ledger does not carry a dead handle
    // forward for every later session to probe and report as stale.
    let closed_parked: Vec<(WindowId, RecoveryEntryId)> = state
        .parked_windows
        .iter()
        .filter(|(id, _)| !managed_ids.contains(id))
        .map(|(id, entry_id)| (*id, *entry_id))
        .collect();
    for (window_id, entry_id) in closed_parked {
        state.parked_windows.remove(&window_id);
        state
            .persistence_intents
            .push(PersistenceIntent::MarkRestored(entry_id));
    }
    state.inventory = next;
    // Rule targets first, so a window a rule sends elsewhere is never
    // first placed in the workspace of the display it appeared on.
    for (window_id, rule_id, target) in rule_targets {
        match state.workspaces.resolve(&target) {
            Some(name) => {
                state.workspaces.assign(window_id, &name);
            }
            None => {
                tracing::warn!(
                    ?window_id,
                    workspace = %target,
                    "a rule names a workspace that does not exist; the window keeps its display's workspace"
                );
                state.rule_workspace_refusals.push(RuleWorkspaceRefusal {
                    window_id,
                    rule_id,
                    workspace: target,
                });
            }
        }
    }
    assign_unassigned_windows(state);
    reconcile_hidden_workspace_windows(state);
    inventory_changed || placements_changed
}

/// Brings the parking site into agreement with the displayed assignment: a
/// managed window whose workspace is hidden belongs there, and one whose
/// workspace is displayed does not.
///
/// The switch transaction moves windows when the *assignment* changes.
/// This covers everything else that can put the two out of step without a
/// switch: a monitor disappearing and taking its displayed workspace with
/// it, a rule sending a brand-new window to a hidden workspace, an
/// application restoring a window that was minimized while hidden, and a
/// restart that reapplies stored assignments. Every one of those goes
/// through the same durable-first path a switch uses, so a window never
/// leaves the screen without its way back already on disk.
///
/// It stands aside entirely while a switch is in flight or a failed one
/// is unreconciled: both mean the truth about where windows are is the
/// transaction's to settle, and a second mover would fight it.
fn reconcile_hidden_workspace_windows(state: &mut EngineState) {
    if state.switch.is_some() || state.switch_degraded.is_some() {
        return;
    }
    let mut to_park: Vec<WindowId> = Vec::new();
    let mut to_restore: Vec<WindowId> = Vec::new();
    for (window_id, managed) in &state.inventory {
        let hidden = match state.workspaces.workspace_of(*window_id) {
            Some(name) => !state.workspaces.is_displayed(name),
            // A window that belongs to no workspace is on an unfilled
            // display, where it is arranged exactly as it was before
            // workspaces existed.
            None => false,
        };
        let parked = state.parked_windows.contains_key(window_id);
        let pending = state
            .pending_parking
            .iter()
            .any(|waiting| waiting.window_id == *window_id);
        if hidden && !parked && !pending {
            // A minimized window occupies no screen, and restoring it in
            // order to park it would change a state the user chose. A
            // full-screen window is never forced out of full-screen
            // either; it simply stays where it is.
            if matches!(
                managed.window.lifecycle,
                WindowLifecycle::Minimized | WindowLifecycle::Fullscreen
            ) {
                continue;
            }
            to_park.push(*window_id);
        } else if !hidden && parked {
            to_restore.push(*window_id);
        }
    }
    to_park.sort_by_key(|window_id| window_id.0);
    to_restore.sort_by_key(|window_id| window_id.0);

    for window_id in to_park {
        match plan_parking_authorization(state, window_id) {
            Ok(draft) => {
                request_recovery_entry(state, window_id, draft, None);
            }
            Err(refusal) => {
                // Nothing is forced. The window stays visible where it
                // is, which is honest about what the engine could do,
                // and the reason is published rather than swallowed.
                tracing::info!(
                    ?window_id,
                    %refusal,
                    "a window of a hidden workspace cannot be parked; it stays visible"
                );
                state.last_parking_refusal = Some(refusal);
            }
        }
    }
    for window_id in to_restore {
        let Some(entry_id) = state.parked_windows.get(&window_id).copied() else {
            continue;
        };
        state.effects.push(EngineEffect::RestoreWindow {
            window_id,
            entry_id,
        });
    }
}

/// The display `window_id` should be arranged on: where its workspace is
/// displayed, or nowhere while that workspace is hidden. A window that
/// belongs to no workspace -- one on a display the pool has no workspace
/// left to fill -- is arranged where it physically is, exactly as before
/// workspaces existed.
fn arrangement_display_of(state: &EngineState, window_id: WindowId) -> Option<DisplayId> {
    match state.workspaces.workspace_of(window_id) {
        Some(name) => state.workspaces.display_of(name),
        None => state
            .inventory
            .get(&window_id)
            .map(|managed| managed.window.display_id),
    }
}

/// Gives every managed window that belongs to no workspace the workspace
/// displayed where it sits, if that display has one. Every tiled and
/// floating managed window belongs to exactly one workspace whenever the
/// pool can provide one; the engine never creates one to make that true.
fn assign_unassigned_windows(state: &mut EngineState) {
    let unassigned: Vec<(WindowId, DisplayId)> = state
        .inventory
        .values()
        .filter(|managed| state.workspaces.workspace_of(managed.window.id).is_none())
        .map(|managed| (managed.window.id, managed.window.display_id))
        .collect();
    for (window_id, display_id) in unassigned {
        if let Some(name) = state.workspaces.displayed_on(display_id).cloned() {
            state.workspaces.assign(window_id, &name);
        }
    }
}

/// Brings the pool up to date with what configuration declares: every
/// declared name exists, owned by configuration; a name configuration
/// stopped declaring stays -- deletion is never implicit -- but becomes
/// deletable by command.
fn sync_workspaces_from_config(state: &mut EngineState) {
    let declared: Vec<WorkspaceName> = state.resolved_config.workspaces.clone();
    for name in &declared {
        match state.workspaces.resolve(name.as_str()) {
            Some(existing) => {
                if let Some(workspace) = state.workspaces.get_mut(&existing) {
                    workspace.origin = WorkspaceOrigin::Configuration;
                }
            }
            None => {
                let _ = state
                    .workspaces
                    .create(name.clone(), WorkspaceOrigin::Configuration);
            }
        }
    }
    for name in state.workspaces.names() {
        if !declared
            .iter()
            .any(|declared| declared.collides_with(&name))
        {
            if let Some(workspace) = state.workspaces.get_mut(&name) {
                workspace.origin = WorkspaceOrigin::Command;
            }
        }
    }
}

/// Gives every display with no displayed workspace one, if the pool has
/// one to give: first the workspace that was displayed on that display
/// when last written, then any hidden workspace that owns no live window,
/// in name order. A hidden workspace with windows is never revealed by a
/// display appearing. A display the pool cannot fill stays without a
/// workspace, and arranges its windows as it did before workspaces
/// existed.
fn fill_empty_displays(state: &mut EngineState) {
    let mut displays: Vec<&Display> = state.displays.iter().collect();
    displays.sort_by_key(|display| (!display.is_primary, display.id.0));
    let displays: Vec<(DisplayId, String)> = displays
        .into_iter()
        .map(|display| (display.id, display.stable_fingerprint.clone()))
        .collect();
    for (display_id, fingerprint) in displays {
        if state.workspaces.displayed_on(display_id).is_some() {
            continue;
        }
        let remembered = state
            .pending_displayed
            .iter()
            .filter(|(name, stored)| {
                **stored == fingerprint && !state.workspaces.is_displayed(name)
            })
            .map(|(name, _)| name.clone())
            .min();
        // Configuration's own order first, so `workspaces = ["dev",
        // "chat"]` puts dev on the primary display; then anything else
        // the pool holds, by name.
        let chosen = remembered.or_else(|| {
            let hidden_and_empty = state.workspaces.hidden_and_empty();
            state
                .resolved_config
                .workspaces
                .iter()
                .filter_map(|declared| {
                    hidden_and_empty
                        .iter()
                        .find(|name| name.collides_with(declared))
                        .cloned()
                })
                .chain(hidden_and_empty.iter().cloned())
                .next()
        });
        let Some(name) = chosen else {
            continue;
        };
        state.pending_displayed.remove(&name);
        state.workspaces.display(&name, display_id);
        adopt_workspace_tree(state, &name, display_id);
    }
}

/// Applies the matched profile's switching mapping as one transition, or
/// leaves the displayed assignment exactly as it was.
///
/// Every display and every name is resolved before anything changes, so
/// a mapping that cannot be completed changes nothing and is reported as
/// unavailable with the first reason found; the engine never invents a
/// workspace to finish it. When the mapping already holds, nothing is
/// stashed or adopted, so an unrelated config reload does not disturb
/// the trees.
fn apply_switching_mapping(state: &mut EngineState) {
    state.switching_unavailable = None;
    let Some(switching) = state.resolved_config.workspace_switching.clone() else {
        return;
    };
    if !switching.experimental {
        return;
    }
    let mut target: Vec<(DisplayId, WorkspaceName)> = Vec::new();
    for (fingerprint, name) in &switching.displayed {
        let Some(display_id) = display_id_of(state, fingerprint) else {
            state.switching_unavailable =
                Some(WorkspaceSwitchingUnavailable::DisplayNotConnected {
                    display_fingerprint: fingerprint.clone(),
                });
            return;
        };
        let Some(name) = state.workspaces.resolve(name.as_str()) else {
            state.switching_unavailable = Some(WorkspaceSwitchingUnavailable::UnknownWorkspace {
                name: name.as_str().to_owned(),
            });
            return;
        };
        target.push((display_id, name));
    }
    for display in &state.displays {
        if !target
            .iter()
            .any(|(display_id, _)| *display_id == display.id)
        {
            state.switching_unavailable = Some(WorkspaceSwitchingUnavailable::MappingIncomplete {
                display_fingerprint: display.stable_fingerprint.clone(),
            });
            return;
        }
    }
    target.sort_by_key(|(display_id, _)| display_id.0);
    if state.workspaces.displayed() == target {
        return;
    }
    tracing::info!("applying the profile's workspace mapping to every display");
    // Every display gives up what it shows before any takes what it is
    // mapped to, so two workspaces exchanging displays each carry their
    // own tree rather than inheriting the other's.
    for (display_id, _) in &target {
        if state.workspaces.displayed_on(*display_id).is_some() {
            stash_display_tree(state, *display_id);
            state.workspaces.hide_display(*display_id);
        }
    }
    for (display_id, name) in &target {
        state.workspaces.display(name, *display_id);
        adopt_workspace_tree(state, name, *display_id);
    }
}

/// Moves `display_id`'s live tree into the workspace displayed there, so
/// the workspace keeps its structure while hidden. The display's
/// per-display bookkeeping goes with it.
fn stash_display_tree(state: &mut EngineState, display_id: DisplayId) {
    let tree = state.trees.remove(&display_id);
    state.saved_trees.remove(&display_id);
    state.constraint_overflow.remove(&display_id);
    state.visual_window_order.remove(&display_id);
    let Some(name) = state.workspaces.displayed_on(display_id).cloned() else {
        return;
    };
    if let Some(workspace) = state.workspaces.get_mut(&name) {
        workspace.stashed_tree = tree;
    }
}

/// Takes `name`'s stashed tree out and makes it `display_id`'s live tree.
/// A workspace with nothing stashed leaves the display to build one, or
/// to adopt the stored tree waiting in `pending_workspace_trees`.
fn adopt_workspace_tree(state: &mut EngineState, name: &WorkspaceName, display_id: DisplayId) {
    let stashed = state
        .workspaces
        .get_mut(name)
        .and_then(|workspace| workspace.stashed_tree.take());
    state.saved_trees.remove(&display_id);
    state.constraint_overflow.remove(&display_id);
    match stashed {
        Some(tree) => {
            state.trees.insert(display_id, tree);
        }
        None => {
            state.trees.remove(&display_id);
        }
    }
}

/// Hides the workspace of every display absent from `new_displays`,
/// stashing its tree first so the retain below does not drop it.
fn hide_workspaces_on_vanished_displays(state: &mut EngineState, new_displays: &[Display]) {
    let vanished: Vec<DisplayId> = state
        .displays
        .iter()
        .filter(|display| {
            !new_displays
                .iter()
                .any(|survivor| survivor.id == display.id)
        })
        .map(|display| display.id)
        .collect();
    for display_id in vanished {
        stash_display_tree(state, display_id);
    }
    let hidden = state
        .workspaces
        .retain_displays(|display_id| new_displays.iter().any(|display| display.id == display_id));
    if !hidden.is_empty() {
        tracing::info!(
            count = hidden.len(),
            "workspaces on vanished displays are now hidden"
        );
    }
}

/// The durable form of one workspace as it stands now.
fn persisted_workspace(state: &EngineState, name: &WorkspaceName) -> Option<PersistedWorkspace> {
    let workspace = state.workspaces.get(name)?;
    let display_id = state.workspaces.display_of(name);
    let tree = match display_id {
        Some(display_id) => state
            .trees
            .get(&display_id)
            .map(|tree| durable_tree(state, tree)),
        None => workspace
            .stashed_tree
            .as_ref()
            .map(|tree| durable_tree(state, tree)),
    };
    Some(PersistedWorkspace {
        name: name.clone(),
        origin: workspace.origin,
        displayed_fingerprint: display_id
            .and_then(|display_id| display_fingerprint_of(state, display_id)),
        tree: tree.filter(|tree| !tree.is_empty()),
    })
}

/// Writes every workspace whose durable form changed since it was last
/// written. A workspace whose tree is still waiting in
/// `pending_workspace_trees` is left alone: writing its empty live tree
/// now would overwrite the stored one before it was ever adopted.
fn persist_workspaces(state: &mut EngineState) {
    for name in state.workspaces.names() {
        if state.pending_workspace_trees.contains_key(&name) {
            continue;
        }
        let Some(persisted) = persisted_workspace(state, &name) else {
            continue;
        };
        if state.saved_workspaces.get(&name) == Some(&persisted) {
            continue;
        }
        state
            .persistence_intents
            .push(PersistenceIntent::SaveWorkspace(Box::new(
                persisted.clone(),
            )));
        state.saved_workspaces.insert(name, persisted);
    }
}

// ---- Workspace switch transaction -------------------------------------
//
// A switch is all-or-nothing across native moves that each answer
// separately, so it cannot be a single reducer arm. It is a small state
// machine instead: `begin_workspace_switch` opens it, every park and
// restore outcome feeds `advance_switch`, and exactly one of
// `commit_switch` or `finish_compensation` closes it. The displayed
// assignment changes only in `commit_switch`, which is what makes a
// failure at any earlier point invisible in published state beyond the
// windows compensation could not reach.

/// Opens a switch transaction for `plan` and starts recording recovery
/// data for everything it will park.
///
/// Nothing moves here. The windows leave the screen only as their ledger
/// entries are acknowledged durable: a crash between this and the parking
/// effect still leaves enough on disk to put the window back.
fn begin_workspace_switch(
    state: &mut EngineState,
    plan: WorkspaceSwitchPlan,
    reverses_undo: Option<UndoTransactionId>,
) {
    state.next_switch_id += 1;
    let id = state.next_switch_id;
    let prior_placements = plan
        .park
        .iter()
        .filter_map(|window_id| {
            let managed = state.inventory.get(window_id)?;
            let display_id = managed.window.display_id;
            let bounds = state
                .windows
                .get(window_id)
                .map(|placement| placement.bounds)
                .unwrap_or(managed.window.bounds);
            Some((*window_id, display_id, bounds))
        })
        .collect();
    tracing::info!(
        transaction = id,
        display = plan.display_id.0,
        target = %plan.target,
        parking = plan.park.len(),
        restoring = plan.restore.len(),
        "workspace switch started"
    );
    state.switch = Some(WorkspaceSwitchTransaction {
        id,
        display_id: plan.display_id,
        target: plan.target,
        outgoing: plan.outgoing,
        phase: WorkspaceSwitchPhase::Recording,
        recording: Vec::new(),
        in_flight: Vec::new(),
        queued: plan.restore,
        parked: Vec::new(),
        restored: Vec::new(),
        stranded: Vec::new(),
        failure: None,
        prior_placements,
        reverses_undo,
    });
    request_switch_parks(state, plan.park);
    // Nothing to park means the parking phase is already settled, and a
    // switch whose restore queue is empty too commits immediately.
    advance_switch(state);
}

/// Records recovery data for every window the switch is about to park.
///
/// A window that cannot be drafted -- it left management, or its display
/// is gone -- is simply not parked: there is nothing on screen of it to
/// hide, and nothing to put back.
fn request_switch_parks(state: &mut EngineState, windows: Vec<WindowId>) {
    let Some(transaction_id) = state.switch.as_ref().map(|switch| switch.id) else {
        return;
    };
    for window_id in windows {
        if !state.inventory.contains_key(&window_id) {
            // The window closed between preflight and here. There is
            // nothing of it on screen to hide and nothing to put back,
            // so the switch carries on without it.
            tracing::info!(
                ?window_id,
                "window left management before it could park; the switch carries on without it"
            );
            continue;
        }
        // Preflight authorised every one of these; a refusal now means
        // the window changed underneath the switch, and carrying on
        // would commit with it still on screen under another workspace.
        let draft = match plan_parking_authorization(state, window_id) {
            Ok(draft) => draft,
            Err(refusal) => {
                tracing::warn!(
                    ?window_id,
                    %refusal,
                    "a window stopped being parkable mid-switch; cancelling"
                );
                begin_switch_compensation(state, refusal.to_string());
                return;
            }
        };
        let token = request_recovery_entry(state, window_id, draft, Some(transaction_id));
        if let Some(switch) = state.switch.as_mut() {
            switch.recording.push(token);
        }
    }
}

/// Moves the transaction on when the current phase has drained.
///
/// Called after every park and restore outcome rather than at a few
/// chosen points, so there is exactly one place that decides what a
/// settled phase means and what follows it.
fn advance_switch(state: &mut EngineState) {
    loop {
        // One mutable borrow per turn of the loop, taken once and
        // released before anything is emitted. Nothing here re-reaches
        // for `state.switch`, so no step can panic on a transaction
        // another step has already taken.
        let Some(switch) = state.switch.as_mut() else {
            return;
        };
        if !switch.phase_is_settled() {
            return;
        }
        let windows = match switch.phase {
            WorkspaceSwitchPhase::Recording | WorkspaceSwitchPhase::Parking => {
                // Everything that had to leave the screen has. Bring the
                // target workspace's parked windows back.
                let restore = std::mem::take(&mut switch.queued);
                if restore.is_empty() {
                    commit_switch(state);
                    return;
                }
                switch.phase = WorkspaceSwitchPhase::Restoring;
                switch.in_flight.clone_from(&restore);
                restore
            }
            WorkspaceSwitchPhase::Restoring => {
                commit_switch(state);
                return;
            }
            WorkspaceSwitchPhase::CompensatingPark => {
                // Everything this transaction had restored is back at the
                // parking site. Now put back everything it parked.
                let parked = switch.parked.clone();
                switch.phase = WorkspaceSwitchPhase::CompensatingRestore;
                if parked.is_empty() {
                    // Nothing to undo on this side; loop round and finish.
                    continue;
                }
                switch.in_flight.clone_from(&parked);
                parked
            }
            WorkspaceSwitchPhase::CompensatingRestore => {
                finish_compensation(state);
                return;
            }
        };
        emit_switch_restores(state, windows);
        return;
    }
}

/// Emits one restore effect per window, in the order the phase queued
/// them, so a test sees a deterministic effect log.
fn emit_switch_restores(state: &mut EngineState, windows: Vec<WindowId>) {
    for window_id in windows {
        let Some(entry_id) = state.parked_windows.get(&window_id).copied() else {
            // Not parked after all -- already where the switch wants it.
            switch_window_settled(state, window_id, None);
            continue;
        };
        state.effects.push(EngineEffect::RestoreWindow {
            window_id,
            entry_id,
        });
    }
}

/// Records that the adapter has answered for `window_id`, and whether it
/// could not carry the move out.
///
/// Every park and restore outcome the transaction is waiting on comes
/// through here, so the accounting of what has moved -- and therefore
/// what compensation owes -- lives in one place.
fn switch_window_settled(state: &mut EngineState, window_id: WindowId, failure: Option<&str>) {
    let Some(switch) = state.switch.as_mut() else {
        return;
    };
    if !switch.in_flight.contains(&window_id) {
        return;
    }
    switch.in_flight.retain(|id| *id != window_id);
    let phase = switch.phase;
    match (phase, failure) {
        (WorkspaceSwitchPhase::Recording | WorkspaceSwitchPhase::Parking, None) => {
            switch.parked.push(window_id);
        }
        (WorkspaceSwitchPhase::Restoring, None) => switch.restored.push(window_id),
        (WorkspaceSwitchPhase::CompensatingPark, None) => {
            // Back at the parking site, so it no longer owes a re-park.
            switch.restored.retain(|id| *id != window_id);
        }
        (WorkspaceSwitchPhase::CompensatingRestore, None) => {
            switch.parked.retain(|id| *id != window_id);
        }
        (phase, Some(reason)) if phase.is_compensating() => {
            // Compensation itself failed: this window is where neither
            // the switch nor the user put it, and only the explicit
            // restore path can reconcile it.
            tracing::error!(
                ?window_id,
                reason,
                "compensation failed; the window is stranded"
            );
            if !switch.stranded.contains(&window_id) {
                switch.stranded.push(window_id);
            }
            switch.restored.retain(|id| *id != window_id);
            switch.parked.retain(|id| *id != window_id);
        }
        (_, Some(reason)) => {
            let reason = reason.to_owned();
            begin_switch_compensation(state, reason);
            return;
        }
    }
    advance_switch(state);
}

/// Abandons the switch and starts putting back everything it has moved.
///
/// The two compensating phases run in the mirror order of the forward
/// ones: windows this transaction restored go back to the parking site
/// before windows it parked come back on screen, so the outgoing and
/// target workspaces are never on the monitor at once.
fn begin_switch_compensation(state: &mut EngineState, reason: String) {
    let Some(switch) = state.switch.as_mut() else {
        return;
    };
    if switch.phase.is_compensating() {
        return;
    }
    tracing::warn!(
        transaction = switch.id,
        %reason,
        parked = switch.parked.len(),
        restored = switch.restored.len(),
        "workspace switch failed; compensating"
    );
    switch.failure = Some(reason);
    switch.phase = WorkspaceSwitchPhase::CompensatingPark;
    // Anything queued but never started is simply dropped: it has not
    // moved, so there is nothing of it to put back.
    switch.queued.clear();
    switch.recording.clear();
    switch.in_flight.clear();
    let to_repark = switch.restored.clone();
    state
        .pending_parking
        .retain(|pending| pending.transaction.is_none());
    if to_repark.is_empty() {
        advance_switch(state);
        return;
    }
    if let Some(switch) = state.switch.as_mut() {
        switch.in_flight = to_repark.clone();
    }
    request_switch_reparks(state, to_repark);
}

/// Records fresh recovery data for windows compensation must park again.
///
/// The entries the forward switch used were consumed when the windows
/// came back, so these are new ones; a window whose entry cannot be
/// recorded is stranded rather than moved without a way back.
fn request_switch_reparks(state: &mut EngineState, windows: Vec<WindowId>) {
    let Some(transaction_id) = state.switch.as_ref().map(|switch| switch.id) else {
        return;
    };
    for window_id in windows {
        let draft = match plan_parking_authorization(state, window_id) {
            Ok(_) => parking_draft_for(state, window_id),
            Err(refusal) => {
                tracing::error!(
                    ?window_id,
                    %refusal,
                    "compensation cannot park the window again; it is stranded"
                );
                None
            }
        };
        let Some(draft) = draft else {
            switch_window_settled(
                state,
                window_id,
                Some("recovery data could not be recorded"),
            );
            continue;
        };
        let token = request_recovery_entry(state, window_id, draft, Some(transaction_id));
        if let Some(switch) = state.switch.as_mut() {
            switch.recording.push(token);
        }
    }
}

/// Commits the switch: the assignment changes, the trees are exchanged,
/// and the whole thing becomes one reversible transaction.
///
/// This is the only place the displayed assignment moves, and it runs
/// only once every native move has landed, which is what "all-or-nothing"
/// means here.
fn commit_switch(state: &mut EngineState) {
    let Some(switch) = state.switch.take() else {
        return;
    };
    let display_id = switch.display_id;
    let target = switch.target.clone();
    let replaced = switch.outgoing.clone();
    // Undo is not itself undoable, so a switch reversing a recorded
    // transaction records nothing new.
    if switch.reverses_undo.is_none() {
        open_undo_scope(state, &format!("workspace-focus {target}"));
        capture_assignment_for_undo(state, display_id);
        for (window_id, prior_display, prior_bounds) in &switch.prior_placements {
            record_undo_member(state, *window_id, Some((*prior_display, *prior_bounds)));
        }
    }
    if replaced.is_some() {
        stash_display_tree(state, display_id);
    }
    state.workspaces.display(&target, display_id);
    adopt_workspace_tree(state, &target, display_id);
    state.focused_display = Some(display_id);
    reconcile_arrangements(state);
    persist_workspaces(state);
    if switch.reverses_undo.is_none() {
        close_undo_scope(state);
    }
    // The point of attention comes back with the workspace. When the
    // window it last had is gone, focus falls back to a member, which is
    // the same choice `plan_workspace_focus` already makes for a
    // workspace that is displayed elsewhere -- see
    // `last_focused_is_forgotten_when_the_window_closes_and_focus_falls_back_to_a_member`.
    // Deliberately not narrowed here: a switch and a focus disagreeing
    // about what to raise would be worse than either rule alone.
    let focused = state
        .workspaces
        .get(&target)
        .and_then(|workspace| workspace.last_focused)
        .filter(|window_id| state.inventory.contains_key(window_id))
        .or_else(|| {
            state
                .workspaces
                .members_of(&target)
                .into_iter()
                .find(|window_id| state.inventory.contains_key(window_id))
        });
    if let Some(window_id) = focused {
        state.effects.push(EngineEffect::FocusWindow { window_id });
    }
    tracing::info!(
        transaction = switch.id,
        display = display_id.0,
        target = %target,
        "workspace switch committed"
    );
    let result = WorkspaceCommandResult::Focused(WorkspaceFocusApplied::Displayed {
        name: target,
        display_id,
        replaced,
    });
    state.last_switch_result = Some(result.clone());
    state.last_workspace_result = Some(result);
    state.revision += 1;
}

/// Closes a compensated switch, either cleanly or as a degraded one.
///
/// A clean compensation leaves published state exactly as the switch found
/// it. One that could not reach every window records the condition and
/// blocks further switching until the explicit restore path clears it.
fn finish_compensation(state: &mut EngineState) {
    let Some(switch) = state.switch.take() else {
        return;
    };
    let reason = switch
        .failure
        .clone()
        .unwrap_or_else(|| "the switch was cancelled".to_owned());
    let failed = WorkspaceSwitchFailed {
        name: switch.target.clone(),
        display_id: switch.display_id,
        reason: reason.clone(),
        compensated: switch.stranded.is_empty(),
        stranded_windows: switch.stranded.clone(),
    };
    if switch.stranded.is_empty() {
        tracing::warn!(
            transaction = switch.id,
            %reason,
            "workspace switch cancelled; every moved window is back"
        );
    } else {
        tracing::error!(
            transaction = switch.id,
            %reason,
            stranded = switch.stranded.len(),
            "workspace switch compensation incomplete; switching is blocked"
        );
        state.switch_degraded = Some(WorkspaceSwitchDegraded {
            display_id: switch.display_id,
            target: switch.target.clone(),
            outgoing: switch.outgoing.clone(),
            stranded_windows: switch.stranded.clone(),
            reason,
        });
    }
    // The displayed assignment never changed, so nothing is reflowed
    // here: the windows are back where the arrangement already expects
    // them.
    let result = WorkspaceCommandResult::SwitchFailed(failed);
    state.last_switch_result = Some(result.clone());
    state.last_workspace_result = Some(result);
    state.revision += 1;
}

/// Which way a stranded window has to move to reach the place its
/// workspace says it belongs.
///
/// Being stranded says nothing about where the window currently is: a
/// failed re-park leaves one visible and a failed restore leaves one
/// parked. Reconciling therefore means moving it in whichever direction
/// its workspace requires, which is why this is a direction and not a
/// yes-or-no.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrandedMove {
    Park,
    Restore,
}

/// The move `window_id` still owes, or `None` when it is already where
/// its workspace says it belongs.
fn stranded_move_for(state: &EngineState, window_id: WindowId) -> Option<StrandedMove> {
    if !state.inventory.contains_key(&window_id) {
        // The window is gone. Nothing to move, and nothing to report:
        // a closed window is accounted for by having closed.
        return None;
    }
    let hidden = state
        .workspaces
        .workspace_of(window_id)
        .is_some_and(|name| !state.workspaces.is_displayed(name));
    let parked = state.parked_windows.contains_key(&window_id);
    match (hidden, parked) {
        (true, false) => Some(StrandedMove::Park),
        (false, true) => Some(StrandedMove::Restore),
        _ => None,
    }
}

/// Decides what an explicit `restore-switch` would do about a degraded
/// switch, without doing it.
///
/// The answer names every stranded window that is not where its workspace
/// says it belongs -- in either direction. A window whose re-park failed
/// is sitting on a monitor showing another workspace, and putting that
/// right means parking it, not restoring it; answering "reconciled" and
/// unblocking would leave two workspaces mixed on one screen.
pub fn plan_workspace_switch_restore(state: &EngineState) -> WorkspaceSwitchRestoreResult {
    let Some(degraded) = &state.switch_degraded else {
        return WorkspaceSwitchRestoreResult::NotDegraded;
    };
    let windows: Vec<WindowId> = degraded
        .stranded_windows
        .iter()
        .copied()
        .filter(|window_id| stranded_move_for(state, *window_id).is_some())
        .collect();
    if windows.is_empty() {
        return WorkspaceSwitchRestoreResult::Reconciled {
            restored_windows: degraded.stranded_windows.clone(),
        };
    }
    WorkspaceSwitchRestoreResult::Requested { windows }
}

/// Records that `window_id` could not be moved to the side of the
/// boundary its workspace requires, outside any switch transaction.
///
/// Reapplying a stored assignment, or parking a window a rule sent to a
/// hidden workspace, moves one window at a time: putting the others back
/// on a monitor showing a different workspace would mix two workspaces
/// rather than un-mix them, so there is nothing to compensate. What the
/// same safety rules do demand is that a half-applied assignment is never
/// silent -- so the window is recorded as stranded, switching is blocked,
/// and `restore-switch` is the way out, exactly as after a failed switch.
///
/// An all-or-nothing batch would have to restore the windows it had
/// already parked, and two workspaces are never intentionally left mixed;
/// stranding one window mixes less than un-parking the rest.
fn strand_window(state: &mut EngineState, window_id: WindowId, reason: String) {
    let Some(name) = state.workspaces.workspace_of(window_id).cloned() else {
        return;
    };
    if state.workspaces.is_displayed(&name) {
        return;
    }
    let Some(display_id) = state
        .inventory
        .get(&window_id)
        .map(|managed| managed.window.display_id)
    else {
        return;
    };
    match state.switch_degraded.as_mut() {
        Some(degraded) => {
            if !degraded.stranded_windows.contains(&window_id) {
                degraded.stranded_windows.push(window_id);
            }
        }
        None => {
            tracing::error!(
                ?window_id,
                workspace = %name,
                %reason,
                "a window of a hidden workspace could not leave the screen; switching is blocked"
            );
            state.switch_degraded = Some(WorkspaceSwitchDegraded {
                display_id,
                target: name,
                outgoing: state.workspaces.displayed_on(display_id).cloned(),
                stranded_windows: vec![window_id],
                reason,
            });
        }
    }
}

/// Drops `window_id` from the degraded condition once it has reached the
/// place its workspace says it belongs, and clears the condition entirely
/// when nothing is left stranded.
///
/// Called whenever a window lands, in either direction and from any path:
/// the explicit reconcile, `restore-windows`, a later switch, or the
/// window's own application moving it. A landing that is still the wrong
/// side of the boundary settles nothing, so switching unblocks exactly
/// when the condition stops being true and not before.
fn settle_stranded_window(state: &mut EngineState, window_id: WindowId) {
    let still_owed = stranded_move_for(state, window_id).is_some();
    let Some(degraded) = state.switch_degraded.as_mut() else {
        return;
    };
    if !degraded.stranded_windows.contains(&window_id) || still_owed {
        return;
    }
    degraded.stranded_windows.retain(|id| *id != window_id);
    if degraded.stranded_windows.is_empty() {
        tracing::info!(
            "every stranded window is where its workspace says it belongs; \
             workspace switching is unblocked"
        );
        state.switch_degraded = None;
    }
    state.revision += 1;
}

/// Records `display_id`'s displayed workspace in the open scope, as it
/// stands now, before the switch changes it.
fn capture_assignment_for_undo(state: &mut EngineState, display_id: DisplayId) {
    let displayed = state.workspaces.displayed_on(display_id).cloned();
    if let Some(scope) = state.undo_scope.as_mut() {
        if !scope
            .prior_assignments
            .iter()
            .any(|(id, _)| *id == display_id)
        {
            scope.prior_assignments.push((display_id, displayed));
        }
    }
}

/// Decides whether `window_id` may be parked, and with what recovery data,
/// without recording anything.
///
/// Every refusal comes before the draft exists: a degraded state
/// database, an unverified or refused parking site, a full-screen
/// window, or a window the topology cannot place. The draft's process
/// creation time is left for the agent, which has platform access, to
/// fill in before the entry is written.
pub fn plan_parking_authorization(
    state: &EngineState,
    window_id: WindowId,
) -> Result<RecoveryDraft, ParkingRefusal> {
    let managed = state
        .inventory
        .get(&window_id)
        .ok_or(ParkingRefusal::NotManaged { window_id })?;
    if matches!(state.persistence_health, PersistenceHealth::Degraded { .. }) {
        return Err(ParkingRefusal::PersistenceDegraded);
    }
    match &state.parking_capability {
        ParkingCapability::Verified => {}
        ParkingCapability::Unverified => return Err(ParkingRefusal::ParkingCapabilityUnverified),
        ParkingCapability::Refused { reason } => {
            return Err(ParkingRefusal::ParkingRefused {
                reason: reason.clone(),
            })
        }
    }
    if managed.window.lifecycle == WindowLifecycle::Fullscreen {
        return Err(ParkingRefusal::Fullscreen { window_id });
    }
    if managed.window.lifecycle == WindowLifecycle::Minimized {
        return Err(ParkingRefusal::Minimized { window_id });
    }
    if state
        .pending_parking
        .iter()
        .any(|pending| pending.window_id == window_id)
    {
        return Err(ParkingRefusal::AlreadyPending { window_id });
    }
    parking_draft_for(state, window_id).ok_or(ParkingRefusal::UnknownDisplay { window_id })
}

/// Registers a parking request for `window_id` and returns its token.
///
/// The single place a window starts waiting to be parked, so the ordering
/// -- the ledger entry recorded first, the parking effect only once it is
/// acknowledged durable -- is written once instead of being got right in
/// three places and wrong in a fourth.
fn request_recovery_entry(
    state: &mut EngineState,
    window_id: WindowId,
    draft: RecoveryDraft,
    transaction: Option<u64>,
) -> u64 {
    state.next_parking_token += 1;
    let token = state.next_parking_token;
    state.pending_parking.push(PendingParking {
        token,
        window_id,
        transaction,
    });
    state
        .persistence_intents
        .push(PersistenceIntent::RecordRecovery {
            token,
            draft: Box::new(draft),
        });
    token
}

/// The recovery data for `window_id` as the inventory and the topology
/// describe it, or `None` when the window is unmanaged or sits on a
/// display that is not connected.
///
/// Shared by the explicit park command and the switch transaction, so a
/// window parked either way is recorded identically and comes back the
/// same.
fn parking_draft_for(state: &EngineState, window_id: WindowId) -> Option<RecoveryDraft> {
    let managed = state.inventory.get(&window_id)?;
    let fingerprint = display_fingerprint_of(state, managed.window.display_id)?;
    // The normal bounds are the placement the reducer intends when the
    // window is not maximised; a maximised window's `bounds` is its
    // maximised extent, so the last intended placement stands in.
    let normal_bounds = state
        .windows
        .get(&window_id)
        .map(|placement| placement.bounds)
        .unwrap_or(managed.window.bounds);
    Some(RecoveryDraft::capture(
        &managed.window,
        &state.session_id,
        &fingerprint,
        normal_bounds,
        now_unix(),
    ))
}

/// Decides what creating a workspace would do, without doing it.
pub fn plan_workspace_create(
    state: &EngineState,
    name: &str,
) -> Result<WorkspaceCreateApplied, WorkspaceRefusal> {
    let name =
        WorkspaceName::new(name).map_err(|reason| WorkspaceRefusal::InvalidName { reason })?;
    if let Some(existing) = state.workspaces.resolve(name.as_str()) {
        return Err(WorkspaceRefusal::AlreadyExists { name: existing });
    }
    Ok(WorkspaceCreateApplied { name })
}

/// Decides what deleting a workspace would do, without doing it.
pub fn plan_workspace_delete(
    state: &EngineState,
    name: &str,
) -> Result<WorkspaceDeleteApplied, WorkspaceRefusal> {
    state
        .workspaces
        .check_delete(name)
        .map(|name| WorkspaceDeleteApplied { name })
}

/// Decides what displaying the hidden workspace `name` on the focused
/// monitor would move, and refuses before anything does.
///
/// Everything the transaction depends on is checked here: that no other
/// switch is in flight, that an earlier one did not leave windows
/// unaccounted for, that the topology has a monitor to switch on, that
/// every window it would move can be moved, and -- once it is known that
/// something would move -- that experimental switching is authorised and
/// recovery data would be durable. A switch that moves nothing is pure
/// bookkeeping and skips the last two: it needs no parking site, so
/// demanding one would refuse a change the desktop never sees.
pub fn plan_workspace_switch(
    state: &EngineState,
    name: &str,
) -> Result<WorkspaceSwitchPlan, WorkspaceRefusal> {
    let display_id = state
        .focused_display
        .filter(|display_id| {
            state
                .displays
                .iter()
                .any(|display| display.id == *display_id)
        })
        .ok_or(WorkspaceRefusal::NoFocusedDisplay)?;
    plan_workspace_switch_on(state, display_id, name)
}

/// Like [`plan_workspace_switch`], for a named monitor rather than the
/// focused one.
///
/// Undo needs this: it reverses the switch on the display the transaction
/// recorded, which may not be the one focus happens to be on now.
pub fn plan_workspace_switch_on(
    state: &EngineState,
    display_id: DisplayId,
    name: &str,
) -> Result<WorkspaceSwitchPlan, WorkspaceRefusal> {
    let target = state.workspaces.require(name)?;
    if state.paused {
        return Err(WorkspaceRefusal::Paused);
    }
    // A switch over windows whose real position is unknown would compound
    // the problem rather than fix it, so the degraded condition blocks
    // every switch until the explicit restore path clears it.
    if let Some(degraded) = &state.switch_degraded {
        return Err(WorkspaceRefusal::SwitchDegraded {
            stranded_windows: degraded.stranded_windows.len(),
        });
    }
    if let Some(switch) = &state.switch {
        return Err(WorkspaceRefusal::SwitchInFlight {
            display_id: switch.display_id,
        });
    }
    if !state
        .displays
        .iter()
        .any(|display| display.id == display_id)
    {
        return Err(WorkspaceRefusal::UnknownDisplay { display_id });
    }
    let outgoing = state.workspaces.displayed_on(display_id).cloned();

    // What has to leave the screen: the outgoing workspace's live members
    // that are actually on it. A minimized member occupies no screen and
    // is left as it is, and one already parked is where the switch wants
    // it.
    let mut park = Vec::new();
    if let Some(outgoing) = &outgoing {
        for window_id in state.workspaces.members_of(outgoing) {
            let Some(managed) = state.inventory.get(&window_id) else {
                continue;
            };
            if state.parked_windows.contains_key(&window_id) {
                continue;
            }
            if managed.window.lifecycle == WindowLifecycle::Fullscreen {
                return Err(WorkspaceRefusal::FullscreenMember {
                    name: outgoing.clone(),
                    window_id,
                });
            }
            if managed.window.lifecycle == WindowLifecycle::Minimized {
                continue;
            }
            park.push(window_id);
        }
    }
    park.sort_by_key(|window_id| window_id.0);

    // What comes back: the target workspace's windows this session parked.
    let mut restore: Vec<WindowId> = state
        .workspaces
        .members_of(&target)
        .into_iter()
        .filter(|window_id| state.parked_windows.contains_key(window_id))
        .collect();
    restore.sort_by_key(|window_id| window_id.0);

    let plan = WorkspaceSwitchPlan {
        display_id,
        target,
        outgoing,
        park,
        restore,
    };
    if plan.moves_windows() {
        // The whole-topology conditions first, so a refused parking site
        // or a degraded database is reported as itself rather than as a
        // property of whichever window happens to be checked first.
        let status = state.workspace_switching_status();
        if status != WorkspaceSwitchingStatus::Experimental {
            return Err(WorkspaceRefusal::SwitchingNotAuthorised {
                status: status.code().to_owned(),
                reason: switching_status_reason(&status),
            });
        }
        if matches!(state.persistence_health, PersistenceHealth::Degraded { .. }) {
            return Err(WorkspaceRefusal::PersistenceDegraded);
        }
        // Then every window the switch would move, through the same
        // authorisation the move itself will ask for. Without this the
        // switch discovers a window it cannot park only after other
        // windows have already left the screen.
        let outgoing = plan.outgoing.clone().unwrap_or_else(|| plan.target.clone());
        for window_id in &plan.park {
            if let Err(reason) = plan_parking_authorization(state, *window_id) {
                return Err(WorkspaceRefusal::MemberNotParkable {
                    name: outgoing,
                    window_id: *window_id,
                    reason,
                });
            }
        }
    }
    Ok(plan)
}

/// The human-readable reason behind a switching status that is not
/// `experimental`, for a refusal that has to say why.
fn switching_status_reason(status: &WorkspaceSwitchingStatus) -> Option<String> {
    match status {
        WorkspaceSwitchingStatus::Disabled => {
            Some("no matched topology profile requests it, and base config cannot".to_owned())
        }
        WorkspaceSwitchingStatus::Requested { pending } => Some(pending.code().to_owned()),
        WorkspaceSwitchingStatus::Unavailable { reason } => Some(reason.to_string()),
        WorkspaceSwitchingStatus::Experimental => None,
    }
}

/// Decides what focusing a workspace would do, without doing it. The same
/// function answers the IPC preflight and drives the reducer, so a caller
/// is never told something different from what happens.
pub fn plan_workspace_focus(
    state: &EngineState,
    name: &str,
) -> Result<WorkspaceFocusApplied, WorkspaceRefusal> {
    let name = state.workspaces.require(name)?;
    if let Some(display_id) = state.workspaces.display_of(&name) {
        let last_focused = state
            .workspaces
            .get(&name)
            .and_then(|workspace| workspace.last_focused)
            .filter(|window_id| state.inventory.contains_key(window_id));
        let focused_window = last_focused.or_else(|| {
            state
                .workspaces
                .members_of(&name)
                .into_iter()
                .find(|window_id| state.inventory.contains_key(window_id))
        });
        return Ok(WorkspaceFocusApplied::FocusedExisting {
            name,
            display_id,
            focused_window,
        });
    }
    // A hidden workspace is displayed by a switch transaction, so the
    // switch's own preflight is what decides here: one answer, whether it
    // is reached through IPC's preflight or through the reducer.
    let plan = plan_workspace_switch(state, name.as_str())?;
    if plan.moves_windows() {
        return Ok(WorkspaceFocusApplied::SwitchStarted {
            name,
            display_id: plan.display_id,
            replaced: plan.outgoing,
            parking: plan.park.len(),
            restoring: plan.restore.len(),
        });
    }
    Ok(WorkspaceFocusApplied::Displayed {
        name,
        display_id: plan.display_id,
        replaced: plan.outgoing,
    })
}

/// Decides what moving a displayed workspace to `display_id` would do,
/// without doing it.
pub fn plan_workspace_move(
    state: &EngineState,
    name: &str,
    display_id: DisplayId,
) -> Result<WorkspaceMoveApplied, WorkspaceRefusal> {
    let name = state.workspaces.require(name)?;
    if state.paused {
        return Err(WorkspaceRefusal::Paused);
    }
    // A move exchanges what two monitors show. Doing that under a switch
    // would leave the transaction compensating onto a display that no
    // longer shows what it started from.
    if let Some(switch) = &state.switch {
        return Err(WorkspaceRefusal::SwitchInFlight {
            display_id: switch.display_id,
        });
    }
    let from_display_id = state
        .workspaces
        .display_of(&name)
        .ok_or_else(|| WorkspaceRefusal::NotDisplayed { name: name.clone() })?;
    if !state
        .displays
        .iter()
        .any(|display| display.id == display_id)
    {
        return Err(WorkspaceRefusal::UnknownDisplay { display_id });
    }
    if from_display_id == display_id {
        return Err(WorkspaceRefusal::AlreadyDisplayedThere { name, display_id });
    }
    Ok(WorkspaceMoveApplied {
        name,
        from_display_id,
        to_display_id: display_id,
        swapped_with: state.workspaces.displayed_on(display_id).cloned(),
    })
}

/// Updates visual order and emits one final Balanced-grid plan per
/// affected display. It is intentionally a no-op in manual mode, leaving
/// current bounds untouched on profile deactivation. Reflows every
/// display's tiled windows using whichever arrangement the resolved
/// configuration selects.
///
/// The two arrangements agree on *which* windows are tiled and disagree
/// only on where they go, so membership is computed once by
/// [`refresh_tiling_sets`] and each planner is handed the same answer.
fn reconcile_arrangements(state: &mut EngineState) {
    match state.resolved_config.tiling_mode {
        TilingMode::Balanced => reconcile_balanced_grids(state),
        TilingMode::Tree => reconcile_container_trees(state),
    }
}

/// Brings each display's visual window order up to date and returns the
/// windows each one should currently arrange, with its work area.
///
/// Shared by both planners so a window can never be tiled under one
/// arrangement and forgotten under the other.
fn refresh_tiling_sets(state: &mut EngineState) -> Vec<(DisplayId, Rect, Vec<WindowId>)> {
    // A window is arranged where its workspace is displayed, which is its
    // physical display except while a workspace focus is carrying it to
    // the focused display, and nowhere while its workspace is hidden.
    let arrangement: HashMap<WindowId, Option<DisplayId>> = state
        .inventory
        .keys()
        .map(|id| (*id, arrangement_display_of(state, *id)))
        .collect();
    for (display_id, order) in &mut state.visual_window_order {
        order.retain(|id| {
            state.inventory.get(id).is_some_and(|managed| {
                (managed.action == ManageAction::Tile || state.session_tiled.contains(id))
                    && arrangement.get(id).copied().flatten() == Some(*display_id)
            })
        });
    }
    let mut candidates: Vec<_> = state
        .inventory
        .values()
        .filter(|managed| {
            managed.action == ManageAction::Tile || state.session_tiled.contains(&managed.window.id)
        })
        .filter_map(|managed| {
            let display_id = arrangement.get(&managed.window.id).copied().flatten()?;
            Some((display_id, managed.window.id, managed.window.bounds))
        })
        .collect();
    candidates.sort_by_key(|(display, id, bounds)| (display.0, bounds.y, bounds.x, id.0));
    for (display_id, window_id, _) in candidates {
        let order = state.visual_window_order.entry(display_id).or_default();
        if !order.contains(&window_id) {
            order.push(window_id);
        }
    }

    state
        .displays
        .iter()
        .map(|display| {
            let ids = state
                .visual_window_order
                .get(&display.id)
                .cloned()
                .unwrap_or_default();
            let active: Vec<_> = ids
                .into_iter()
                .filter(|id| {
                    state.inventory.get(id).is_some_and(|managed| {
                        arrangement.get(id).copied().flatten() == Some(display.id)
                            && (managed.action == ManageAction::Tile
                                || state.session_tiled.contains(id))
                            && managed.eligibility == EligibilityReason::Eligible
                    })
                })
                .collect();
            (display.id, display.work_area, active)
        })
        .collect()
}

/// Whether this display's reflow must wait for a drag to finish.
fn defer_reflow(state: &mut EngineState, display_id: DisplayId) -> bool {
    if state
        .interactive_placement
        .is_some_and(|session| session.display_id == display_id)
    {
        state.deferred_reflow_displays.insert(display_id);
        return true;
    }
    false
}

fn reconcile_balanced_grids(state: &mut EngineState) {
    if !state.automatic_tiling_active || state.paused {
        return;
    }
    state.constraint_overflow.clear();

    for (display_id, work_area, active) in refresh_tiling_sets(state) {
        if defer_reflow(state, display_id) {
            continue;
        }
        let cells = plan_balanced_grid(work_area, active.len());
        for (window_id, raw_bounds) in active.into_iter().zip(cells) {
            let bounds = apply_gaps(raw_bounds, work_area, state.resolved_config.gaps);
            let unchanged = state.windows.get(&window_id).is_some_and(|placement| {
                placement.display_id == display_id && placement.bounds == bounds
            });
            if !unchanged {
                place_window(state, window_id, display_id, bounds, None);
            }
        }
    }
}

/// Reflows every display's container tree, growing and shrinking each tree
/// to match the windows currently tiled there.
///
/// The tree is reconciled against the active set rather than mutated by
/// each event that could affect it. A structure that survives restarts has
/// to be able to re-derive itself from observed reality; rebuilding the
/// delta here is what makes "the window closed while the agent was not
/// running" the same code path as "the window closed just now".
fn reconcile_container_trees(state: &mut EngineState) {
    if !state.automatic_tiling_active || state.paused {
        return;
    }

    let live_displays: HashSet<DisplayId> =
        state.displays.iter().map(|display| display.id).collect();
    state
        .trees
        .retain(|display_id, _| live_displays.contains(display_id));
    state
        .constraint_overflow
        .retain(|display_id, _| live_displays.contains(display_id));

    for (display_id, work_area, active) in refresh_tiling_sets(state) {
        if defer_reflow(state, display_id) {
            continue;
        }
        let focused = state.focused_window;
        let fingerprint = display_fingerprint_of(state, display_id);

        // Taken out of the map for the duration, so the matcher below can
        // read the rest of the state freely.
        let mut tree = state.trees.remove(&display_id).unwrap_or_default();

        // The stored arrangement replaces whatever insertion has built so
        // far, keeping only the leaves whose windows are confidently
        // identified. It is not conditional on the tree being empty: the
        // database's answer normally arrives *after* the first windows are
        // observed, so by then a default arrangement already exists, and
        // the user's saved one is the arrangement they asked for. Windows
        // the stored tree cannot account for are re-inserted below.
        //
        // `pending_trees` is a startup payload and is consumed here, so
        // this happens once and the reducer is the authority afterwards.
        let now = now_unix();
        let workspace = state.workspaces.displayed_on(display_id).cloned();
        // A displayed workspace's own stored tree comes first. Failing
        // that, the display-anchored tree written before workspaces
        // existed is adopted by the workspace now displayed there, so an
        // upgrade keeps the arrangement the user had.
        let stored = workspace
            .as_ref()
            .and_then(|name| state.pending_workspace_trees.remove(name))
            .or_else(|| {
                fingerprint
                    .as_ref()
                    .and_then(|fingerprint| state.pending_trees.remove(fingerprint))
            });
        if let Some(stored) = stored {
            tree = restore_tree(state, &stored, &active, now);
        }

        // A window that has closed leaves its slot dormant rather than
        // giving it up, so a confident return can reclaim it (spec user
        // stories 38 and 39). The evidence was captured at the last
        // reflow, because a closed window is already gone from the
        // inventory by now. A window that is still open but no longer
        // tiled here -- floated, minimized, or moved to another display
        // -- simply leaves the tree: it is not lost, and a slot kept for
        // a window the user can see would be a ghost. A leaf nothing could
        // ever recognise is removed instead of holding space open forever.
        let departed: Vec<WindowId> = tree
            .windows()
            .into_iter()
            .filter(|window_id| !active.contains(window_id))
            .copied()
            .collect();
        for window_id in departed {
            let evidence = state.leaf_evidence.remove(&window_id);
            match evidence {
                Some(evidence) if !state.inventory.contains_key(&window_id) => {
                    tree.make_dormant(
                        &window_id,
                        DormantPosition {
                            evidence,
                            since_unix: now,
                        },
                    );
                }
                _ => {
                    tree.remove(&window_id);
                }
            }
        }
        tree.prune_dormant(now);

        // A returning window takes its old slot back only on a confident
        // match; anything less inserts it fresh and leaves the slot for a
        // better candidate. Slots are offered in visual order and each
        // window is claimed at most once, so the outcome does not depend
        // on enumeration order.
        let mut unplaced: Vec<WindowId> = active
            .iter()
            .filter(|window_id| !tree.contains(window_id))
            .copied()
            .collect();
        for (position, dormant) in tree
            .dormant_positions()
            .into_iter()
            .map(|(position, dormant)| (position, dormant.clone()))
            .collect::<Vec<_>>()
        {
            let candidates: Vec<&Window> = unplaced
                .iter()
                .filter_map(|window_id| state.inventory.get(window_id))
                .map(|managed| &managed.window)
                .collect();
            let Some(window_id) = match_window_with_order(
                &dormant.evidence,
                candidates.iter().copied(),
                |window| display_fingerprint_of(state, window.display_id),
                |window| Some(launch_order_of(state, window.id)),
            )
            .confident_window() else {
                continue;
            };
            if tree.reclaim(position, window_id) {
                unplaced.retain(|id| *id != window_id);
            }
        }

        // New windows arrive in visual order, so the arrangement a set of
        // windows produces does not depend on which order they were
        // observed in. A tree of only dormant slots has no window to
        // split, so the newcomer goes beside the whole arrangement and
        // has the display to itself until a slot is reclaimed.
        for window_id in &unplaced {
            if tree.insert_first(*window_id) {
                continue;
            }
            if !tree.has_windows() {
                tree.split_root(longer_axis_of(work_area), *window_id);
                continue;
            }
            let placements = plan_tree_raw(&tree, work_area);
            let Some(insertion) = choose_insertion(&placements, focused) else {
                continue;
            };
            tree.split_leaf(&insertion.target, insertion.axis, *window_id);
        }

        // Refresh what would recognise each window, now that it is where
        // the tree put it. This is what a later dormancy is built from.
        for window_id in tree.windows() {
            if let Some(evidence) = evidence_for(state, *window_id) {
                state.leaf_evidence.insert(*window_id, evidence);
            }
        }

        // Only a structural change is worth a write. A reflow that moved
        // windows without reshaping the tree -- a display resizing, say --
        // has nothing new to store.
        if state.saved_trees.get(&display_id) != Some(&tree) {
            // A display with a workspace stores its tree under the
            // workspace, written by `persist_workspaces` once the tree is
            // in place below; only a display the pool could not fill
            // keeps the display-anchored row.
            if workspace.is_none() {
                if let Some(fingerprint) = fingerprint {
                    let durable = durable_tree(state, &tree);
                    state
                        .persistence_intents
                        .push(PersistenceIntent::SaveContainerTree {
                            display_fingerprint: fingerprint,
                            tree: Box::new(durable),
                        });
                }
            }
            state.saved_trees.insert(display_id, tree.clone());
        }

        // A window the display cannot fit at its minimum size is left
        // where it is rather than squeezed: it keeps its leaf and its
        // management, and comes back the moment the tree can hold it.
        // Nothing here floats it by choice, so the overflow set is
        // recomputed from scratch every reflow.
        let planned = plan_tree_constrained(&tree, work_area, state.resolved_config.gaps, |id| {
            state
                .inventory
                .get(&id)
                .and_then(|managed| managed.window.minimum_size)
        });
        state.trees.insert(display_id, tree);
        if planned.overflow.is_empty() {
            state.constraint_overflow.remove(&display_id);
        } else {
            state
                .constraint_overflow
                .insert(display_id, planned.overflow);
        }
        for (window_id, bounds) in planned.placements {
            let unchanged = state.windows.get(&window_id).is_some_and(|placement| {
                placement.display_id == display_id && placement.bounds == bounds
            });
            if !unchanged {
                place_window(state, window_id, display_id, bounds, None);
            }
        }
    }
    // Evidence is only ever needed for a window that could still close
    // out of a tree; anything the inventory no longer holds has already
    // gone dormant or been dropped above.
    state
        .leaf_evidence
        .retain(|window_id, _| state.inventory.contains_key(window_id));
    persist_workspaces(state);
}

/// Turns a stored arrangement back into a live one.
///
/// Each leaf's evidence is matched against the windows currently tiled on
/// that display. A leaf that cannot be identified confidently is dropped
/// rather than guessed at, and no live window is claimed by two leaves --
/// the same refusal-first rule undo follows, applied to structure.
fn restore_tree(
    state: &EngineState,
    stored: &PersistedTree,
    active: &[WindowId],
    now_unix: i64,
) -> ContainerTree {
    let candidates: Vec<&Window> = active
        .iter()
        .filter_map(|window_id| state.inventory.get(window_id))
        .map(|managed| &managed.window)
        .collect();
    let mut claimed: HashSet<WindowId> = HashSet::new();
    let mut identify = |evidence: &WindowEvidence| {
        match_window_with_order(
            evidence,
            candidates.iter().copied(),
            |window| display_fingerprint_of(state, window.display_id),
            |window| Some(launch_order_of(state, window.id)),
        )
        .confident_window()
        .filter(|window_id| claimed.insert(*window_id))
    };
    // A stored slot whose window is not here becomes dormant from now: the
    // structure is kept for a later confident return rather than dropped.
    // A slot that was already dormant keeps the time it went dormant, so
    // retention counts from the right moment, and is pruned here if that
    // moment is far enough back.
    let mut restored = stored.convert_leaves(&mut |leaf| match &leaf.occupant {
        Occupant::Live(evidence) => match identify(evidence) {
            Some(window_id) => LeafFate::Live(window_id),
            None => LeafFate::Dormant(DormantPosition {
                evidence: evidence.clone(),
                since_unix: now_unix,
            }),
        },
        Occupant::Dormant(position) => match identify(&position.evidence) {
            Some(window_id) => LeafFate::Live(window_id),
            None => LeafFate::Dormant(position.clone()),
        },
    });
    restored.prune_dormant(now_unix);
    restored
}

/// What would recognise `window_id` in a later session, if the inventory
/// can describe it.
fn evidence_for(state: &EngineState, window_id: WindowId) -> Option<WindowEvidence> {
    let managed = state.inventory.get(&window_id)?;
    let fingerprint = display_fingerprint_of(state, managed.window.display_id)?;
    Some(WindowEvidence::capture(
        &managed.window,
        launch_order_of(state, window_id),
        &fingerprint,
    ))
}

/// The durable form of a live tree: the same structure with each window
/// replaced by evidence that can find it again, and each dormant slot
/// kept as it is. A window the inventory cannot describe is dropped,
/// because a leaf nothing could ever match would only hold space open
/// forever.
fn durable_tree(state: &EngineState, tree: &ContainerTree) -> PersistedTree {
    tree.filter_map_windows(&mut |window_id| evidence_for(state, *window_id))
}

/// Splitting "on the longer axis" of a work area: side by side when it is
/// wide, stacked when it is tall.
const fn longer_axis_of(area: Rect) -> SplitAxis {
    if area.width >= area.height {
        SplitAxis::Horizontal
    } else {
        SplitAxis::Vertical
    }
}

/// The resolved config that should be active for `displays`' current
/// topology: whichever profile in `config_set.profiles` has a
/// `fingerprint` matching [`topology_fingerprint`] of `displays`, or
/// `config_set.base` if none does. Profiles are opt-in overrides, never
/// auto-created, so "no match" is an ordinary outcome, not an error.
/// Shared by [`Event::ConfigChanged`] and
/// [`Event::DisplayTopologyChanged`] so a topology already seen before
/// always re-selects the same profile it matched last time.
fn select_resolved_config(config_set: &ResolvedConfigSet, displays: &[Display]) -> ResolvedConfig {
    let fingerprint = topology_fingerprint(displays);
    config_set
        .profiles
        .iter()
        .find(|profile| profile.fingerprint == fingerprint)
        .map(|profile| profile.config.clone())
        .unwrap_or_else(|| config_set.base.clone())
}

/// The work area of the display with `id`, if it's still in `displays`.
fn work_area_of(displays: &[Display], id: DisplayId) -> Option<Rect> {
    displays
        .iter()
        .find(|display| display.id == id)
        .map(|display| display.work_area)
}

/// The windows in `current` that need a real `SetWindowPos` call to catch
/// up to the reducer -- new since `previous`, or moved/resized since
/// `previous`. Scoped down to what a poll-driven executor needs: it does
/// no transaction planning, just "what changed since I last looked".
///
/// A window present in `previous` but missing from `current` needs no
/// call -- there's nothing sensible to move it to, and the engine never
/// removes tracked windows today anyway.
///
/// Windows whose circuit breaker is open are excluded from the diff so the
/// executor never issues a `SetWindowPos` call for them -- they will
/// re-appear in the diff automatically once the user resets the breaker
/// via a zone-snap command.
pub fn diff_placements(
    previous: &HashMap<WindowId, WindowPlacement>,
    current: &HashMap<WindowId, WindowPlacement>,
) -> Vec<(WindowId, DisplayId, Rect)> {
    current
        .iter()
        .filter(|(_, placement)| !placement.circuit_open())
        .filter(|(window_id, placement)| {
            previous
                .get(window_id)
                .map(|prior| {
                    prior.display_id != placement.display_id || prior.bounds != placement.bounds
                })
                .unwrap_or(true)
        })
        .map(|(window_id, placement)| (*window_id, placement.display_id, placement.bounds))
        .collect()
}

/// Decides what undoing the newest transaction would do, without doing it.
///
/// Every refusal path returns before a single placement is planned, which
/// is what makes undo all-or-nothing: there is no state in which some
/// members have moved and a later one turns out to be unresolvable. The
/// same function answers the IPC preflight and drives the reducer, so a
/// caller is never told something different from what happens. Whether
/// automatic tiling is currently producing container trees, which is what
/// every tree command requires.
fn tree_mode_active(state: &EngineState) -> bool {
    state.resolved_config.tiling_mode == TilingMode::Tree && state.automatic_tiling_active
}

/// The windows currently eligible to occupy leaves of `display_id`'s tree:
/// tiled, on that display, and not temporarily ineligible.
fn active_tiled_windows_on(state: &EngineState, display_id: DisplayId) -> Vec<WindowId> {
    let mut active: Vec<WindowId> = state
        .inventory
        .values()
        .filter(|managed| {
            managed.window.display_id == display_id
                && (managed.action == ManageAction::Tile
                    || state.session_tiled.contains(&managed.window.id))
                && managed.eligibility == EligibilityReason::Eligible
        })
        .map(|managed| managed.window.id)
        .collect();
    active.sort_unstable_by_key(|id| id.0);
    active
}

/// A resize the reducer would apply: the typed outcome to report, and the
/// reshaped tree to adopt.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeResizePlan {
    pub applied: TreeResizeApplied,
    pub tree: ContainerTree,
}

/// What resizing the focused window's tree in `direction` would do
/// against `state` right now, or the typed reason it would do nothing.
///
/// Pure over `state`, so the IPC handler and the reducer reach the same
/// Pure over `state`, so the IPC handler and the reducer reach the same
/// verdict from the same state. The full five-point step is taken when
/// every arranged window stays at or above its minimum size with the gaps
/// it has; otherwise the largest smaller whole step that does is taken,
/// and if not even one point is legal the command is refused without
/// touching the tree. "Legal" means the resize neither overflows a window
/// that was arranged nor costs any decoration: a resize is a request about
/// weights, and it must not be answered by degrading the arrangement.
pub fn plan_tree_resize(
    state: &EngineState,
    direction: CardinalDirection,
) -> Result<TreeResizePlan, TreeResizeRefusal> {
    let command = resize_command(direction).to_owned();
    if state.paused {
        return Err(TreeResizeRefusal::Paused);
    }
    if !tree_mode_active(state) {
        return Err(TreeResizeRefusal::NotTreeMode);
    }
    let window_id = state
        .focused_window
        .ok_or(TreeResizeRefusal::NoFocusedWindow)?;
    let display_id = state
        .inventory
        .get(&window_id)
        .map(|managed| managed.window.display_id)
        .ok_or(TreeResizeRefusal::NoFocusedWindow)?;
    let work_area = work_area_of(&state.displays, display_id)
        .ok_or(TreeResizeRefusal::DisplayUnavailable { display_id })?;
    if !arranged_in_tree(state, display_id, window_id) {
        return Err(TreeResizeRefusal::NotArranged { window_id });
    }
    let tree = state
        .trees
        .get(&display_id)
        .ok_or(TreeResizeRefusal::NotArranged { window_id })?;
    let (axis, toward) = divider_for(direction);
    if !tree.has_divider_toward(&window_id, axis, toward) {
        return Err(TreeResizeRefusal::NoDivider { command });
    }

    let minimum_size = |id: WindowId| {
        state
            .inventory
            .get(&id)
            .and_then(|managed| managed.window.minimum_size)
    };
    let gaps = state.resolved_config.gaps;
    let current = plan_tree_constrained(tree, work_area, gaps, minimum_size);

    for points in (1..=TREE_RESIZE_STEP_PERCENT).rev() {
        let mut candidate = tree.clone();
        let Some(change) =
            candidate.resize_toward(&window_id, axis, toward, f64::from(points) / 100.0)
        else {
            continue;
        };
        let planned = plan_tree_constrained(&candidate, work_area, gaps, minimum_size);
        let no_new_overflow = planned
            .overflow
            .iter()
            .all(|id| current.overflow.contains(id));
        let no_lost_decoration =
            planned.gaps.outer >= current.gaps.outer && planned.gaps.inner >= current.gaps.inner;
        if no_new_overflow && no_lost_decoration {
            return Ok(TreeResizePlan {
                applied: TreeResizeApplied {
                    command,
                    display_id,
                    window_id,
                    percentage_points: points,
                    grew: change.grew,
                    shrank: change.shrank,
                    weights_before: change.weights_before,
                    weights_after: change.weights_after,
                },
                tree: candidate,
            });
        }
    }
    Err(TreeResizeRefusal::MinimumSizeReached { command })
}

/// What swapping the focused window in `direction` would do against
/// `state` right now, or the typed reason it would do nothing.
///
/// The neighbor is exactly the one [`Event::DirectionalFocusRequested`]
/// would focus, because both go through [`directional_neighbor`]: a
/// direction has one spatial meaning. Pure over `state`, so the IPC
/// handler and the reducer reach the same verdict from the same state.
pub fn plan_directional_swap(
    state: &EngineState,
    direction: CardinalDirection,
) -> Result<DirectionalSwapApplied, DirectionalSwapRefusal> {
    let command = swap_command(direction).to_owned();
    if state.paused {
        return Err(DirectionalSwapRefusal::Paused);
    }
    let window_id = state
        .focused_window
        .ok_or(DirectionalSwapRefusal::NoFocusedWindow)?;
    let display_id = state
        .inventory
        .get(&window_id)
        .map(|managed| managed.window.display_id)
        .ok_or(DirectionalSwapRefusal::NoFocusedWindow)?;
    if work_area_of(&state.displays, display_id).is_none() {
        return Err(DirectionalSwapRefusal::DisplayUnavailable { display_id });
    }
    if !directional_endpoint(state, display_id, window_id) {
        return Err(DirectionalSwapRefusal::NotArranged { window_id });
    }
    let neighbor_id =
        directional_neighbor(state, direction).ok_or(DirectionalSwapRefusal::NoNeighbor {
            command: command.clone(),
        })?;
    Ok(DirectionalSwapApplied {
        command,
        display_id,
        window_id,
        neighbor_id,
    })
}

/// What removing dormant slot `position` from `display_id`'s tree would
/// do against `state` right now, or the typed reason it would do nothing.
pub fn plan_remove_position(
    state: &EngineState,
    display_id: DisplayId,
    position: u64,
) -> Result<RemovePositionApplied, RemovePositionRefusal> {
    if state.paused {
        return Err(RemovePositionRefusal::Paused);
    }
    if !tree_mode_active(state) {
        return Err(RemovePositionRefusal::NotTreeMode);
    }
    if work_area_of(&state.displays, display_id).is_none() {
        return Err(RemovePositionRefusal::DisplayUnavailable { display_id });
    }
    let dormant = state
        .trees
        .get(&display_id)
        .and_then(|tree| {
            tree.dormant_positions()
                .into_iter()
                .find(|(number, _)| *number == position)
                .map(|(_, dormant)| dormant.clone())
        })
        .ok_or(RemovePositionRefusal::UnknownPosition {
            display_id,
            position,
        })?;
    Ok(RemovePositionApplied {
        display_id,
        position,
        application: dormant.evidence.application_id.0,
    })
}

pub fn plan_undo(state: &EngineState) -> UndoResult {
    let Some(transaction) = state.newest_undo.as_ref() else {
        return UndoResult::Refused(UndoRefusal::NothingToUndo);
    };

    // Undoing without being able to consume the transaction would leave it
    // available to apply a second time, so a degraded database refuses
    // rather than risking a double undo.
    if let PersistenceHealth::Degraded { reason, .. } = &state.persistence_health {
        return UndoResult::Refused(UndoRefusal::PersistenceDegraded {
            transaction_id: transaction.id,
            reason: reason.code().to_owned(),
        });
    }

    let current_fingerprint = mosaix_domain::topology_fingerprint(&state.displays);
    if current_fingerprint != transaction.topology_fingerprint {
        return UndoResult::Refused(UndoRefusal::TopologyChanged {
            transaction_id: transaction.id,
            recorded_fingerprint: transaction.topology_fingerprint.clone(),
            current_fingerprint,
        });
    }

    let candidates: Vec<&Window> = state
        .inventory
        .values()
        .map(|managed| &managed.window)
        .collect();
    let targets: Vec<UndoTargetOutcome> = transaction
        .members
        .iter()
        .map(|member| UndoTargetOutcome {
            ordinal: member.ordinal,
            application: member.evidence.application_id.0.clone(),
            outcome: match_window_with_order(
                &member.evidence,
                candidates.iter().copied(),
                |window| display_fingerprint_of(state, window.display_id),
                |window| Some(launch_order_of(state, window.id)),
            ),
        })
        .collect();

    if targets.iter().any(|target| !target.is_resolved()) {
        return UndoResult::Refused(UndoRefusal::TargetsUnresolved {
            transaction_id: transaction.id,
            targets,
        });
    }

    // The structure the undo puts back is preflighted too. A leaf whose
    // window is gone is fine -- it comes back dormant, which is exactly
    // what its slot would be by now -- but a leaf that could be either of
    // two windows is refused, because restoring it would guess. The same
    // matcher decides here and at apply time.
    let mut structure: Vec<UndoTargetOutcome> = Vec::new();
    for snapshot in &transaction.prior_trees {
        let Some(display_id) = display_id_of(state, &snapshot.display_fingerprint) else {
            continue;
        };
        let active = active_tiled_windows_on(state, display_id);
        let on_display: Vec<&Window> = active
            .iter()
            .filter_map(|window_id| state.inventory.get(window_id))
            .map(|managed| &managed.window)
            .collect();
        for evidence in snapshot.tree.windows() {
            structure.push(UndoTargetOutcome {
                ordinal: (targets.len() + structure.len()) as u32,
                application: evidence.application_id.0.clone(),
                outcome: match_window_with_order(
                    evidence,
                    on_display.iter().copied(),
                    |window| display_fingerprint_of(state, window.display_id),
                    |window| Some(launch_order_of(state, window.id)),
                ),
            });
        }
    }
    if structure
        .iter()
        .any(|target| matches!(target.outcome, MatchOutcome::Ambiguous { .. }))
    {
        let mut all = targets;
        all.extend(structure);
        return UndoResult::Refused(UndoRefusal::TargetsUnresolved {
            transaction_id: transaction.id,
            targets: all,
        });
    }

    // Two members claiming one window means at least one match is wrong,
    // and there is no way to tell which -- so neither moves.
    let mut claims: HashMap<WindowId, Vec<u32>> = HashMap::new();
    for target in &targets {
        if let Some(window_id) = target.outcome.confident_window() {
            claims.entry(window_id).or_default().push(target.ordinal);
        }
    }
    if let Some((window_id, ordinals)) = claims
        .into_iter()
        .filter(|(_, ordinals)| ordinals.len() > 1)
        .min_by_key(|(window_id, _)| window_id.0)
    {
        let mut ordinals = ordinals;
        ordinals.sort_unstable();
        return UndoResult::Refused(UndoRefusal::TargetsCollide {
            transaction_id: transaction.id,
            window_id,
            ordinals,
        });
    }

    let mut restored = Vec::with_capacity(transaction.members.len());
    for (member, target) in transaction.members.iter().zip(&targets) {
        let Some(display_id) = display_id_of(state, &member.prior_display_fingerprint) else {
            // The whole-topology fingerprint matched, so this should not
            // happen; treating it as a topology change is the honest
            // reading if it ever does.
            return UndoResult::Refused(UndoRefusal::TopologyChanged {
                transaction_id: transaction.id,
                recorded_fingerprint: transaction.topology_fingerprint.clone(),
                current_fingerprint,
            });
        };
        restored.push(UndoRestoredWindow {
            ordinal: member.ordinal,
            window_id: target
                .outcome
                .confident_window()
                .expect("every target is resolved by this point"),
            display_id,
            placement: member.prior_placement,
        });
    }

    // Reversing a workspace switch means switching back, and that switch
    // has to be authorised on its own terms: undo never moves a window out
    // of a workspace without the same guarantees the forward switch had.
    if let Err(reason) = plan_undo_switch_back(state, transaction) {
        return UndoResult::Refused(UndoRefusal::WorkspaceSwitchRefused {
            transaction_id: transaction.id,
            reason,
        });
    }

    UndoResult::Applied(UndoApplied {
        transaction_id: transaction.id,
        command: transaction.command.clone(),
        restored,
    })
}

/// The switch that would put `transaction`'s displayed assignment back,
/// or the reason it cannot be made.
///
/// `Ok(None)` means there is nothing to switch: either the transaction
/// changed no assignment, or the assignment it changed already stands.
fn plan_undo_switch_back(
    state: &EngineState,
    transaction: &UndoTransaction,
) -> Result<Option<WorkspaceSwitchPlan>, WorkspaceRefusal> {
    for assignment in &transaction.prior_assignments {
        let Some(display_id) = display_id_of(state, &assignment.display_fingerprint) else {
            continue;
        };
        let Some(name) = &assignment.workspace else {
            // The display showed no workspace at all. Nothing displays
            // "nothing": every switch names a workspace to show, so this
            // is refused rather than approximated with a guess about
            // which workspace should take its place.
            return Err(WorkspaceRefusal::CannotHideWithoutReplacement {
                display_fingerprint: assignment.display_fingerprint.clone(),
            });
        };
        if state
            .workspaces
            .displayed_on(display_id)
            .is_some_and(|displayed| displayed.as_str() == name)
        {
            continue;
        }
        return plan_workspace_switch_on(state, display_id, name).map(Some);
    }
    Ok(None)
}

fn display_fingerprint_of(state: &EngineState, display_id: DisplayId) -> Option<String> {
    state
        .displays
        .iter()
        .find(|display| display.id == display_id)
        .map(|display| display.stable_fingerprint.clone())
}

fn display_id_of(state: &EngineState, fingerprint: &str) -> Option<DisplayId> {
    state
        .displays
        .iter()
        .find(|display| display.stable_fingerprint == fingerprint)
        .map(|display| display.id)
}

/// A window's ordinal among its own application's managed windows.
///
/// Two windows of one application agree on every other durable signal, so
/// without this the matcher could only ever call them ambiguous. Ordered by
/// native handle, which is allocation-ordered in practice and, more to the
/// point, does not depend on hash iteration order.
fn launch_order_of(state: &EngineState, window_id: WindowId) -> u32 {
    let Some(managed) = state.inventory.get(&window_id) else {
        return 0;
    };
    let mut siblings: Vec<WindowId> = state
        .inventory
        .values()
        .filter(|other| other.window.application_id == managed.window.application_id)
        .map(|other| other.window.id)
        .collect();
    siblings.sort_unstable_by_key(|id| id.0);
    siblings.iter().position(|id| *id == window_id).unwrap_or(0) as u32
}

/// Opens an undo scope for `command`.
///
/// Everything [`place_window`] moves before the matching
/// [`close_undo_scope`] becomes one reversible transaction, including the
/// automatic reflow an explicit command provokes.
fn open_undo_scope(state: &mut EngineState, command: &str) {
    state.undo_scope = Some(UndoScope {
        command: command.to_owned(),
        members: Vec::new(),
        claimed: HashSet::new(),
        prior_trees: Vec::new(),
        prior_assignments: Vec::new(),
    });
}

/// Records `display_id`'s container tree in the open scope, as it stands
/// now, before the command reshapes it. Call it before the mutation; a
/// second call for the same display is ignored.
fn capture_tree_for_undo(state: &mut EngineState, display_id: DisplayId) {
    let tree = state.trees.get(&display_id).cloned().unwrap_or_default();
    if let Some(scope) = state.undo_scope.as_mut() {
        if !scope.prior_trees.iter().any(|(id, _)| *id == display_id) {
            scope.prior_trees.push((display_id, tree));
        }
    }
}

/// Closes the open scope, recording a transaction if it changed anything.
///
/// A command that moved nothing and reshaped nothing -- refused,
/// suppressed by a circuit breaker, or a no-op -- records nothing, so undo
/// never offers to reverse something the user never saw happen. A tree
/// captured but left as it was is not a change either.
fn close_undo_scope(state: &mut EngineState) {
    let Some(scope) = state.undo_scope.take() else {
        return;
    };
    let prior_trees: Vec<UndoTreeSnapshot> = scope
        .prior_trees
        .iter()
        .filter(|(display_id, prior)| {
            state.trees.get(display_id).unwrap_or(&ContainerTree::new()) != prior
        })
        .filter_map(|(display_id, prior)| {
            Some(UndoTreeSnapshot {
                display_fingerprint: display_fingerprint_of(state, *display_id)?,
                tree: durable_tree(state, prior),
            })
        })
        .collect();
    let prior_assignments: Vec<UndoAssignment> = scope
        .prior_assignments
        .iter()
        .filter(|(display_id, prior)| state.workspaces.displayed_on(*display_id) != prior.as_ref())
        .filter_map(|(display_id, prior)| {
            Some(UndoAssignment {
                display_fingerprint: display_fingerprint_of(state, *display_id)?,
                workspace: prior.as_ref().map(|name| name.as_str().to_owned()),
            })
        })
        .collect();
    if scope.members.is_empty() && prior_trees.is_empty() && prior_assignments.is_empty() {
        return;
    }
    state
        .persistence_intents
        .push(PersistenceIntent::RecordUndoTransaction(
            UndoTransactionDraft {
                command: scope.command,
                recorded_at_unix: now_unix(),
                topology_fingerprint: mosaix_domain::topology_fingerprint(&state.displays),
                durable_revision: state.revision,
                members: scope.members,
                prior_trees,
                prior_assignments,
            },
        ));
}

/// Adds `window_id` to the open scope, if there is one and it is not
/// already captured. `previous` is where the window sat before this
/// placement; without it there is nothing for undo to restore.
fn record_undo_member(
    state: &mut EngineState,
    window_id: WindowId,
    previous: Option<(DisplayId, Rect)>,
) {
    let (already_claimed, ordinal) = match state.undo_scope.as_ref() {
        Some(scope) => (
            scope.claimed.contains(&window_id),
            scope.members.len() as u32,
        ),
        None => return,
    };
    if already_claimed {
        return;
    }
    let Some((previous_display, previous_bounds)) = previous else {
        return;
    };
    let Some(prior_display_fingerprint) = display_fingerprint_of(state, previous_display) else {
        return;
    };
    let Some(managed) = state.inventory.get(&window_id) else {
        return;
    };
    // Evidence describes where the window is *now*, because that is where
    // a later session has to find it. The prior placement is what undo
    // restores it to.
    let Some(current_fingerprint) = display_fingerprint_of(state, managed.window.display_id) else {
        return;
    };
    let member = UndoMember {
        ordinal,
        prior_placement: previous_bounds,
        prior_display_fingerprint,
        evidence: WindowEvidence::capture(
            &managed.window,
            launch_order_of(state, window_id),
            &current_fingerprint,
        ),
    };
    if let Some(scope) = state.undo_scope.as_mut() {
        scope.claimed.insert(window_id);
        scope.members.push(member);
    }
}

/// Records `bounds` as `window_id`'s current placement on `display_id`,
/// stashing wherever it was before (if it was already tracked) as the one
/// step [`Event::WindowRestoreRequested`] can undo, and bumps the
/// revision.
///
/// `cycle_step` is the placement's new cycle-step state. Pass `None` to
/// carry forward whatever the window already had (`None` if it wasn't
/// tracked yet) unchanged -- every caller does this except
/// [`Event::ZoneSnapRequested`]'s left/right handling, which passes
/// `Some((direction, step))` to record the cycle position it just resolved.
///
/// Returns `false` if the placement was suppressed because the window's
/// circuit breaker is open. The caller is free to ignore this return value
/// -- it's informational only; the event has already been handled (by
/// doing nothing).
fn place_window(
    state: &mut EngineState,
    window_id: WindowId,
    display_id: DisplayId,
    bounds: Rect,
    cycle_step: Option<(HorizontalDirection, CycleStep)>,
) -> bool {
    let existing = state.windows.get(&window_id);
    // Circuit breaker: if the window is in open-circuit state (repeated
    // rejections), suppress this placement without modifying state.
    if existing.is_some_and(|p| p.circuit_open()) {
        tracing::debug!(
            ?window_id,
            "placement suppressed: circuit breaker is open for this window"
        );
        return false;
    }
    let previous_placement = existing.map(|placement| (placement.display_id, placement.bounds));
    let cycle_step = cycle_step.or_else(|| existing.and_then(|placement| placement.cycle_step));
    // Preserve rejection_count across placements so the breaker state
    // survives non-user-initiated placements (e.g. throw-to-display). Only
    // an explicit zone-snap command resets it (handled in
    // Event::ZoneSnapRequested above).
    let rejection_count = existing.map_or(0, |p| p.rejection_count);
    state.windows.insert(
        window_id,
        WindowPlacement {
            display_id,
            bounds,
            // Assume the placement lands; `WindowBoundsObserved` corrects
            // this the moment the OS says otherwise.
            observed_bounds: bounds,
            previous_placement,
            cycle_step,
            rejection_count,
        },
    );
    if let Some(managed) = state.inventory.get_mut(&window_id) {
        managed.window.display_id = display_id;
        managed.window.bounds = bounds;
    }
    // After the inventory is updated, so captured evidence describes where
    // the window now is rather than where it was.
    record_undo_member(state, window_id, previous_placement);
    state.effects.push(EngineEffect::PlaceWindow {
        window_id,
        display_id,
        bounds,
    });
    state.revision += 1;
    true
}

/// Migrate windows whose current `display_id` is absent from
/// `new_displays` to the nearest surviving display, preserving their
/// normalized position via [`throw_preserving_ratio`]. Called *before*
/// `state.displays` is updated so the old topology is still available to
/// compute ratios from.
///
/// This is the shared implementation for both
/// [`Event::DisplayTopologyChanged`] (hotplug) and
/// [`Event::WakeReconciliation`] (sleep/wake).
fn migrate_orphaned_windows(state: &mut EngineState, new_displays: &[Display]) {
    if new_displays.is_empty() {
        // No surviving displays -- nothing sensible to migrate to. Leave
        // windows untouched; they'll be reconciled when a display comes
        // back.
        tracing::warn!(
            "all displays disappeared; deferring window migration until a display returns"
        );
        return;
    }

    let mut migrated = 0usize;
    let mut effects = Vec::new();
    // Collect the set of display IDs that are *leaving* the topology.
    let vanished_ids: Vec<DisplayId> = state
        .displays
        .iter()
        .filter(|d| !new_displays.iter().any(|nd| nd.id == d.id))
        .map(|d| d.id)
        .collect();

    if vanished_ids.is_empty() {
        return;
    }

    tracing::info!(
        vanished_count = vanished_ids.len(),
        "migrating windows from vanished displays"
    );

    // For each window on a vanished display, find the nearest surviving
    // display by comparing display center-points, then throw the window to it.
    for (window_id, placement) in state.windows.iter_mut() {
        if !vanished_ids.contains(&placement.display_id) {
            continue;
        }

        // Find the old display's geometry.
        let Some(old_display) = state.displays.iter().find(|d| d.id == placement.display_id) else {
            continue;
        };

        // Find the nearest new display by Euclidean distance between centers.
        let old_cx = old_display.full_bounds.x + old_display.full_bounds.width / 2;
        let old_cy = old_display.full_bounds.y + old_display.full_bounds.height / 2;

        let Some(nearest) = new_displays.iter().min_by_key(|nd| {
            let cx = nd.full_bounds.x + nd.full_bounds.width / 2;
            let cy = nd.full_bounds.y + nd.full_bounds.height / 2;
            let dx = (cx - old_cx) as i64;
            let dy = (cy - old_cy) as i64;
            dx * dx + dy * dy
        }) else {
            continue;
        };

        // Preserve the window's normalized position on the new display.
        let new_bounds =
            throw_preserving_ratio(placement.bounds, old_display.work_area, nearest.work_area);

        tracing::info!(
            ?placement.display_id,
            new_display_id = ?nearest.id,
            "migrating window from vanished display"
        );

        placement.display_id = nearest.id;
        placement.bounds = new_bounds;
        effects.push(EngineEffect::PlaceWindow {
            window_id: *window_id,
            display_id: nearest.id,
            bounds: new_bounds,
        });
        migrated += 1;
    }

    for effect in &effects {
        let EngineEffect::PlaceWindow {
            window_id,
            display_id,
            bounds,
        } = effect
        else {
            continue;
        };
        if let Some(managed) = state.inventory.get_mut(window_id) {
            managed.window.display_id = *display_id;
            managed.window.bounds = *bounds;
        }
    }
    state.effects.extend(effects);

    if migrated > 0 {
        tracing::info!(migrated, "window migration complete");
    }
}

fn migrate_focused_display(state: &mut EngineState, new_displays: &[Display]) {
    let Some(focused_id) = state.focused_display else {
        return;
    };
    if new_displays.iter().any(|display| display.id == focused_id) {
        return;
    }
    let Some(previous) = state
        .displays
        .iter()
        .find(|display| display.id == focused_id)
    else {
        state.focused_display = None;
        return;
    };
    let center_x = previous.full_bounds.x + previous.full_bounds.width / 2;
    let center_y = previous.full_bounds.y + previous.full_bounds.height / 2;
    state.focused_display = new_displays
        .iter()
        .min_by_key(|candidate| {
            let candidate_x = candidate.full_bounds.x + candidate.full_bounds.width / 2;
            let candidate_y = candidate.full_bounds.y + candidate.full_bounds.height / 2;
            let dx = (candidate_x - center_x) as i64;
            let dy = (candidate_y - center_y) as i64;
            (dx * dx + dy * dy, !candidate.is_primary, candidate.id.0)
        })
        .map(|display| display.id);
}

/// The queue actually carries this, not `Event` directly, so [`stop`] can
/// terminate the reducer with an explicit poison pill rather than by
/// waiting for every sender to be dropped -- callers are expected to hand
/// out cloned [`EventSender`]s to multiple producer threads, so those
/// threads' lifetimes, not the last sender's, would otherwise decide when
/// `stop` can return.
///
/// [`stop`]: EngineHandle::stop
enum Message {
    Event(Event),
    Shutdown,
}

/// A cloneable handle for sending events into the reducer's queue. Meant
/// to be handed to every event producer (platform adapters, commands,
/// timers); the queue only actually closes when [`EngineHandle::stop`] is
/// called, not when the last `EventSender` is dropped.
#[derive(Clone)]
pub struct EventSender {
    inner: SyncSender<Message>,
}

impl EventSender {
    /// Enqueues `event`, blocking the caller while the queue is full --
    /// that backpressure is intentional, because the event queue is
    /// bounded. Fails, returning the event back, once the reducer has
    /// stopped.
    pub fn send(&self, event: Event) -> std::result::Result<(), Event> {
        self.inner
            .send(Message::Event(event))
            .map_err(|err| match err.0 {
                Message::Event(event) => event,
                Message::Shutdown => unreachable!("only EngineHandle::stop sends Shutdown"),
            })
    }
}

/// A running reducer, owned by a dedicated thread. Dropping (or
/// [`stop`](Self::stop)) signals the thread to exit and joins it.
pub struct EngineHandle {
    events: EventSender,
    state: Arc<Mutex<EngineState>>,
    join_handle: Option<JoinHandle<()>>,
}

impl EngineHandle {
    /// A cloneable sender for feeding events into the reducer's queue.
    pub fn events(&self) -> EventSender {
        self.events.clone()
    }

    /// The latest committed state. Never blocks on the reducer; readers
    /// see a consistent snapshot as of the most recently applied event.
    pub fn snapshot(&self) -> EngineState {
        self.state
            .lock()
            .expect("engine state mutex poisoned")
            .clone()
    }

    /// A cloneable handle for reading committed state from another thread
    /// (e.g. a platform executor polling for placements to apply), without
    /// needing the [`EngineHandle`] itself.
    pub fn state_reader(&self) -> StateReader {
        StateReader {
            state: Arc::clone(&self.state),
        }
    }

    /// Signals the reducer thread to exit and waits for it to do so.
    /// Terminates the thread even if other [`EventSender`] clones (e.g.
    /// held by producer threads) are still alive; callers that want those
    /// producers to stop cleanly too should stop them separately.
    pub fn stop(mut self) {
        self.request_stop();
    }

    fn request_stop(&mut self) {
        if let Some(join_handle) = self.join_handle.take() {
            // Best-effort: if the reducer already exited on its own, the
            // channel is disconnected and this returns an error we don't
            // care about.
            let _ = self.events.inner.send(Message::Shutdown);
            let _ = join_handle.join();
        }
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// A cloneable handle for reading the engine's latest committed state.
/// See [`EngineHandle::state_reader`].
#[derive(Clone)]
pub struct StateReader {
    state: Arc<Mutex<EngineState>>,
}

impl StateReader {
    /// The latest committed state. Never blocks on the reducer; readers
    /// see a consistent snapshot as of the most recently applied event.
    pub fn snapshot(&self) -> EngineState {
        self.state
            .lock()
            .expect("engine state mutex poisoned")
            .clone()
    }

    /// Just the current revision, without cloning the whole state.
    ///
    /// [`snapshot`](Self::snapshot) deep-clones every window, display, and
    /// rule, which is far too costly for a reader that polls to find out
    /// *whether* anything changed. Such readers -- the focus-border
    /// controller is the first -- watch this and take a snapshot only when
    /// it advances.
    pub fn revision(&self) -> u64 {
        self.state
            .lock()
            .expect("engine state mutex poisoned")
            .revision
    }
}

/// Spawns the reducer thread with `initial_displays` and `initial_config_set`
/// as the starting state, using [`DEFAULT_QUEUE_CAPACITY`]. The active
/// `resolved_config` starts pre-selected against `initial_displays` (the
/// same selection [`Event::DisplayTopologyChanged`] would perform), so a
/// topology already matching a saved profile at startup takes effect
/// immediately -- without waiting for a subsequent topology-change event
/// that may never come.
pub fn spawn_engine(
    initial_displays: Vec<Display>,
    initial_config_set: ResolvedConfigSet,
) -> EngineHandle {
    spawn_engine_with_capacity(initial_displays, initial_config_set, DEFAULT_QUEUE_CAPACITY)
}

/// Like [`spawn_engine`], with an explicit event-queue bound.
/// The compiled form of a resolved config's `[[rules]]` entries.
///
/// `mosaix_config::validate` has already proved every pattern compiles by
/// the time a `ResolvedConfig` exists, so an error here means one reached
/// the reducer without passing through validation. The offending rule is
/// dropped and named rather than passed on: a rule that cannot compile
/// has no matcher, and keeping it would mean a rule the user wrote that
/// never matches and never says why.
fn compile_rules(resolved: &ResolvedConfig) -> Vec<Rule> {
    resolved
        .rules
        .iter()
        .filter_map(|config| match Rule::try_from(config.clone()) {
            Ok(rule) => Some(rule),
            Err(error) => {
                tracing::error!(
                    %error,
                    id = %config.id,
                    "a rule reached the reducer without passing validation; ignoring it"
                );
                None
            }
        })
        .collect()
}

pub fn spawn_engine_with_capacity(
    initial_displays: Vec<Display>,
    initial_config_set: ResolvedConfigSet,
    capacity: usize,
) -> EngineHandle {
    let (tx, rx) = sync_channel::<Message>(capacity);
    let initial_resolved_config = select_resolved_config(&initial_config_set, &initial_displays);
    let initial_automatic_tiling_active = initial_resolved_config.automatic_tiling_enabled;
    let initial_focused_display = initial_displays.first().map(|display| display.id);
    let initial_state = EngineState {
        revision: 0,
        persistence_health: PersistenceHealth::Healthy {
            last_durable_revision: 0,
        },
        persistence_intents: Vec::new(),
        newest_undo: None,
        last_undo_result: None,
        undo_scope: None,
        trees: HashMap::new(),
        pending_trees: HashMap::new(),
        saved_trees: HashMap::new(),
        leaf_evidence: HashMap::new(),
        constraint_overflow: HashMap::new(),
        workspaces: WorkspacePool::new(),
        pending_workspace_trees: HashMap::new(),
        pending_displayed: HashMap::new(),
        saved_workspaces: HashMap::new(),
        last_workspace_result: None,
        rule_workspace_refusals: Vec::new(),
        switching_unavailable: None,
        parking_capability: ParkingCapability::Unverified,
        // One identity per process start: a restarted agent has a new
        // one, so a previous session's ledger entries are never mistaken
        // for this session's own.
        session_id: format!("{}-{}", std::process::id(), now_unix()),
        pending_parking: Vec::new(),
        next_parking_token: 0,
        parked_windows: HashMap::new(),
        switch: None,
        next_switch_id: 0,
        switch_degraded: None,
        last_switch_result: None,
        last_parking_refusal: None,
        last_parking_failure: None,
        recovery_outcomes: Vec::new(),
        displays: initial_displays,
        windows: HashMap::new(),
        inventory: HashMap::new(),
        observed_windows: HashMap::new(),
        rules: compile_rules(&initial_resolved_config),
        visual_window_order: HashMap::new(),
        session_floating: HashSet::new(),
        session_tiled: HashSet::new(),
        focused_window: None,
        focused_display: initial_focused_display,
        resolved_config: initial_resolved_config,
        config_set: initial_config_set,
        paused: false,
        automatic_tiling_active: initial_automatic_tiling_active,
        automatic_tiling_suspended: false,
        interactive_placement: None,
        deferred_reflow_displays: HashSet::new(),
        effects: Vec::new(),
        last_applied_layouts: HashMap::new(),
        hotkey_capture_suspended: false,
        capture_holds: 0,
        unregistered_bindings: Vec::new(),
    };
    let mut initial_state = initial_state;
    // The pool starts from what configuration declares, and every display
    // that can be given a workspace gets one before the first window is
    // observed, so the first observation already has somewhere to belong.
    sync_workspaces_from_config(&mut initial_state);
    fill_empty_displays(&mut initial_state);
    apply_switching_mapping(&mut initial_state);
    let state = Arc::new(Mutex::new(initial_state.clone()));

    let published_state = Arc::clone(&state);
    let join_handle = thread::spawn(move || {
        let mut local = initial_state;
        while let Ok(Message::Event(event)) = rx.recv() {
            apply(&mut local, event);
            *published_state.lock().expect("engine state mutex poisoned") = local.clone();
        }
        tracing::info!("reducer thread exiting");
    });

    EngineHandle {
        events: EventSender { inner: tx },
        state,
        join_handle: Some(join_handle),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mosaix_config::ResolvedProfile;
    use mosaix_domain::{DisplayId, Rect, Rotation};
    use std::time::{Duration, Instant};

    fn display(id: isize, fingerprint: &str, x: i32) -> Display {
        Display {
            id: DisplayId(id),
            stable_fingerprint: fingerprint.to_string(),
            full_bounds: Rect {
                x,
                y: 0,
                width: 1920,
                height: 1080,
            },
            work_area: Rect {
                x,
                y: 0,
                width: 1920,
                height: 1080,
            },
            scale_factor: 1.0,
            rotation: Rotation::Landscape,
            is_primary: x == 0,
        }
    }

    fn wait_for(mut condition: impl FnMut() -> bool, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            thread::sleep(Duration::from_millis(1));
        }
        condition()
    }

    #[test]
    fn apply_bumps_revision_on_genuine_topology_change() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );

        assert_eq!(state.revision, 1);
        assert_eq!(state.displays.len(), 1);
    }

    #[test]
    fn apply_ignores_a_repeated_topology_hint() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );

        assert_eq!(
            state.revision, 1,
            "an identical re-enumeration must not bump the revision"
        );
    }

    #[test]
    fn apply_bumps_revision_again_when_topology_actually_changes() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0), display(2, "MON-B", 1920)]),
        );

        assert_eq!(state.revision, 2);
        assert_eq!(state.displays.len(), 2);
    }

    #[test]
    fn work_area_change_reflows_without_clearing_tiling_suspension() {
        let mut state = EngineState {
            displays: vec![display(1, "primary", 0)],
            resolved_config: ResolvedConfig {
                automatic_tiling_enabled: true,
                ..ResolvedConfig::default()
            },
            automatic_tiling_suspended: true,
            automatic_tiling_active: false,
            ..EngineState::default()
        };
        let mut changed = state.displays[0].clone();
        changed.work_area = Rect::new(0, 40, 1920, 1040);

        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![changed.clone()]),
        );

        assert_eq!(state.displays, vec![changed]);
        assert!(state.automatic_tiling_suspended);
        assert!(!state.automatic_tiling_active);
    }

    #[test]
    fn apply_window_placed_tracks_the_window_with_no_previous_bounds() {
        let mut state = EngineState::default();
        let bounds = Rect::new(0, 0, 960, 1080);
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds,
            },
        );

        let placement = state
            .windows
            .get(&WindowId(1))
            .expect("window should be tracked");
        assert_eq!(placement.display_id, DisplayId(1));
        assert_eq!(placement.bounds, bounds);
        assert_eq!(placement.previous_placement, None);
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn apply_window_placed_again_remembers_the_prior_display_and_bounds() {
        let mut state = EngineState::default();
        let first = Rect::new(0, 0, 960, 1080);
        let second = Rect::new(0, 0, 1920, 1080);
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: first,
            },
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: second,
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(placement.bounds, second);
        assert_eq!(placement.previous_placement, Some((DisplayId(1), first)));
        assert_eq!(state.revision, 2);
    }

    #[test]
    fn apply_restore_reverts_to_the_previous_bounds_and_clears_it() {
        let mut state = EngineState::default();
        let first = Rect::new(0, 0, 960, 1080);
        let second = Rect::new(0, 0, 1920, 1080);
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: first,
            },
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: second,
            },
        );
        apply(
            &mut state,
            Event::WindowRestoreRequested {
                window_id: WindowId(1),
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.bounds, first,
            "restore should return to the bounds before the last placement"
        );
        assert_eq!(placement.display_id, DisplayId(1));
        assert_eq!(
            placement.previous_placement, None,
            "restore is a single step, not a stack"
        );
        assert_eq!(state.revision, 3);
    }

    #[test]
    fn apply_restore_after_a_cross_display_throw_reverts_the_display_too() {
        // Regression test: a naive implementation that remembers only
        // `bounds` (not which display they belonged to) would restore
        // source-display-relative coordinates while leaving `display_id`
        // at the post-throw target display.
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0), display(2, "MON-B", 1920)]),
        );
        let original_bounds = Rect::new(0, 0, 960, 1080);
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: original_bounds,
            },
        );
        apply(
            &mut state,
            Event::WindowThrowToDisplayRequested {
                window_id: WindowId(1),
                direction: DisplayDirection::Next,
            },
        );
        apply(
            &mut state,
            Event::WindowRestoreRequested {
                window_id: WindowId(1),
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.display_id,
            DisplayId(1),
            "restore must move the window back to its source display"
        );
        assert_eq!(placement.bounds, original_bounds);
    }

    #[test]
    fn apply_restore_twice_only_undoes_one_step() {
        let mut state = EngineState::default();
        let first = Rect::new(0, 0, 960, 1080);
        let second = Rect::new(0, 0, 1920, 1080);
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: first,
            },
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: second,
            },
        );
        apply(
            &mut state,
            Event::WindowRestoreRequested {
                window_id: WindowId(1),
            },
        );
        let revision_after_first_restore = state.revision;
        apply(
            &mut state,
            Event::WindowRestoreRequested {
                window_id: WindowId(1),
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.bounds, first,
            "a second restore with nothing remembered must be a no-op"
        );
        assert_eq!(
            state.revision, revision_after_first_restore,
            "a no-op restore must not bump the revision"
        );
    }

    #[test]
    fn apply_restore_is_a_noop_for_an_untracked_window() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::WindowRestoreRequested {
                window_id: WindowId(1),
            },
        );

        assert_eq!(state.revision, 0);
        assert!(state.windows.is_empty());
    }

    #[test]
    fn apply_throw_moves_the_window_to_the_next_display_preserving_its_ratio() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0), display(2, "MON-B", 1920)]),
        );
        let left_half = Rect::new(0, 0, 960, 1080);
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: left_half,
            },
        );
        apply(
            &mut state,
            Event::WindowThrowToDisplayRequested {
                window_id: WindowId(1),
                direction: DisplayDirection::Next,
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(placement.display_id, DisplayId(2));
        assert_eq!(
            placement.bounds,
            Rect::new(1920, 0, 960, 1080),
            "half-width, full-height should be preserved on the target display"
        );
        assert_eq!(
            placement.previous_placement,
            Some((DisplayId(1), left_half)),
            "a throw should itself be restorable"
        );
    }

    #[test]
    fn apply_throw_prev_wraps_around_to_the_last_display() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0), display(2, "MON-B", 1920)]),
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );
        apply(
            &mut state,
            Event::WindowThrowToDisplayRequested {
                window_id: WindowId(1),
                direction: DisplayDirection::Prev,
            },
        );

        assert_eq!(
            state.windows.get(&WindowId(1)).unwrap().display_id,
            DisplayId(2)
        );
    }

    #[test]
    fn apply_throw_is_a_noop_for_an_untracked_window() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0), display(2, "MON-B", 1920)]),
        );
        apply(
            &mut state,
            Event::WindowThrowToDisplayRequested {
                window_id: WindowId(1),
                direction: DisplayDirection::Next,
            },
        );

        assert_eq!(
            state.revision, 1,
            "only the topology change should have bumped the revision"
        );
        assert!(state.windows.is_empty());
    }

    #[test]
    fn apply_throw_is_a_noop_with_only_one_display() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        let bounds = Rect::new(0, 0, 960, 1080);
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds,
            },
        );
        let revision_before_throw = state.revision;
        apply(
            &mut state,
            Event::WindowThrowToDisplayRequested {
                window_id: WindowId(1),
                direction: DisplayDirection::Next,
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.bounds, bounds,
            "with no other display, the window must not move"
        );
        assert_eq!(placement.display_id, DisplayId(1));
        assert_eq!(state.revision, revision_before_throw);
    }

    #[test]
    fn apply_throw_is_a_noop_when_the_windows_display_left_the_topology() {
        // Regression test: when a monitor unplugs, the window is
        // *migrated* to the surviving display by `DisplayTopologyChanged`.
        // A subsequent throw then operates on that surviving display --
        // but with only one display remaining there is no adjacent display
        // to throw to, so the throw is still a no-op. The key assertion is
        // that the window ends up on the surviving display, not stranded
        // on the vanished one.
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0), display(2, "MON-B", 1920)]),
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 960, 1080),
            },
        );
        // Display 1 (the window's display) unplugs, leaving only display 2.
        // `migrate_orphaned_windows` moves the window to display 2 as part of
        // the topology-change handler.
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(2, "MON-B", 1920)]),
        );

        // The window must have been migrated to display 2.
        assert_eq!(
            state.windows.get(&WindowId(1)).unwrap().display_id,
            DisplayId(2),
            "the window must be migrated to the surviving display on hotplug"
        );

        let revision_before_throw = state.revision;
        apply(
            &mut state,
            Event::WindowThrowToDisplayRequested {
                window_id: WindowId(1),
                direction: DisplayDirection::Next,
            },
        );

        // Only one display remains, so the throw is still a no-op.
        assert_eq!(
            state.windows.get(&WindowId(1)).unwrap().display_id,
            DisplayId(2),
            "with no adjacent display, the throw must be a no-op on the migrated display"
        );
        assert_eq!(
            state.revision, revision_before_throw,
            "throw with no adjacent display must not bump the revision"
        );
    }

    #[test]
    fn apply_focused_sets_the_focused_window_and_bumps_revision() {
        let mut state = EngineState::default();
        assert_eq!(state.focused_window, None);
        let bounds = Rect::new(0, 0, 1920, 1080);

        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds,
            },
        );

        assert_eq!(state.focused_window, Some(WindowId(1)));
        assert_eq!(state.revision, 1);
        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(placement.display_id, DisplayId(1));
        assert_eq!(placement.bounds, bounds);
        assert_eq!(placement.previous_placement, None);
    }

    #[test]
    fn apply_focused_again_with_a_different_window_replaces_it() {
        let mut state = EngineState::default();
        let bounds = Rect::new(0, 0, 1920, 1080);
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds,
            },
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(2),
                display_id: DisplayId(1),
                bounds,
            },
        );

        assert_eq!(state.focused_window, Some(WindowId(2)));
        assert_eq!(state.revision, 2);
    }

    #[test]
    fn apply_zone_snap_left_on_first_press_snaps_to_left_half() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );

        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(placement.bounds, Rect::new(0, 0, 960, 1080));
        assert_eq!(
            placement.cycle_step,
            Some((HorizontalDirection::Left, CycleStep::Half))
        );
    }

    #[test]
    fn zone_snap_emits_one_platform_neutral_placement_effect() {
        let mut state = EngineState {
            displays: vec![display(1, "primary", 0)],
            ..EngineState::default()
        };
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(7),
                display_id: DisplayId(1),
                bounds: Rect::new(10, 10, 500, 500),
            },
        );
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );

        assert_eq!(
            state.effects,
            vec![EngineEffect::PlaceWindow {
                window_id: WindowId(7),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 960, 1080),
            }]
        );
    }

    #[test]
    fn apply_zone_snap_left_repeated_advances_half_third_two_thirds_then_wraps() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );

        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.bounds,
            Rect::new(0, 0, 640, 1080),
            "second press should shrink to the left third"
        );
        assert_eq!(
            placement.cycle_step,
            Some((HorizontalDirection::Left, CycleStep::Third))
        );

        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.bounds,
            Rect::new(0, 0, 1280, 1080),
            "third press should expand to the left two-thirds"
        );
        assert_eq!(
            placement.cycle_step,
            Some((HorizontalDirection::Left, CycleStep::TwoThirds))
        );

        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.bounds,
            Rect::new(0, 0, 960, 1080),
            "fourth press should wrap back to half"
        );
        assert_eq!(
            placement.cycle_step,
            Some((HorizontalDirection::Left, CycleStep::Half))
        );
    }

    #[test]
    fn apply_zone_snap_right_runs_its_own_independent_cycle_from_half() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );

        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Right,
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.bounds,
            Rect::new(960, 0, 960, 1080),
            "switching direction should start a fresh cycle at half, not continue the other direction's step"
        );
        assert_eq!(
            placement.cycle_step,
            Some((HorizontalDirection::Right, CycleStep::Half))
        );
    }

    #[test]
    fn apply_zone_snap_top_always_resolves_to_top_half_and_never_cycles() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );

        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Top,
            },
        );
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Top,
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.bounds,
            Rect::new(0, 0, 1920, 540),
            "repeated top presses must stay at half, never cycle"
        );
        assert_eq!(
            placement.cycle_step, None,
            "top/bottom must not write cycle-step state"
        );
    }

    #[test]
    fn apply_zone_snap_bottom_always_resolves_to_bottom_half_and_leaves_cycle_step_untouched() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );

        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Bottom,
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(placement.bounds, Rect::new(0, 540, 1920, 540));
        assert_eq!(
            placement.cycle_step,
            Some((HorizontalDirection::Left, CycleStep::Half)),
            "top/bottom must not read or write cycle-step state, so left's cycle position survives underneath"
        );
    }

    #[test]
    fn apply_zone_snap_applies_the_active_configs_gaps_to_a_half_snap() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::ConfigChanged(Box::new(config_set_with_gaps(mosaix_domain::Gaps::new(
                10, 4,
            )))),
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );

        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.bounds,
            Rect::new(10, 10, 960 - 10 - 4, 1080 - 10 - 10),
            "left half's outer edges get the outer gap, its interior right edge gets the inner gap"
        );
    }

    #[test]
    fn apply_zone_snap_applies_the_active_configs_gaps_to_a_cycled_third_step() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::ConfigChanged(Box::new(config_set_with_gaps(mosaix_domain::Gaps::new(
                10, 4,
            )))),
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );

        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.bounds,
            Rect::new(10, 10, 640 - 10 - 4, 1080 - 10 - 10),
            "the left third's interior right edge still gets the inner gap after cycling"
        );
    }

    #[test]
    fn apply_zone_snap_reflects_a_gap_change_on_the_very_next_placement_with_no_restart() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::ConfigChanged(Box::new(config_set_with_gaps(mosaix_domain::Gaps::new(
                10, 4,
            )))),
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        assert_eq!(
            state.windows.get(&WindowId(1)).unwrap().bounds,
            Rect::new(10, 10, 960 - 10 - 4, 1080 - 10 - 10)
        );

        // A hot-edited config (or a topology-triggered profile switch)
        // delivers new gap values with no restart.
        apply(
            &mut state,
            Event::ConfigChanged(Box::new(config_set_with_gaps(mosaix_domain::Gaps::new(
                20, 8,
            )))),
        );
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.bounds,
            Rect::new(20, 20, 640 - 20 - 8, 1080 - 20 - 20),
            "the cycled third step must use the newly-configured gaps, not the ones active at the first press"
        );
    }

    #[test]
    fn apply_zone_snap_is_a_noop_with_no_focused_window() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        let bounds = Rect::new(0, 0, 1920, 1080);
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds,
            },
        );
        let revision_before = state.revision;

        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );

        assert_eq!(
            state.revision, revision_before,
            "no focused window must be a no-op"
        );
        assert_eq!(state.windows.get(&WindowId(1)).unwrap().bounds, bounds);
    }

    /// Reproduces the exact event sequence the real agent produces for
    /// "focus a window you've never snapped before, then press a snap
    /// hotkey": `mosaix-agent`'s focus forwarder (main.rs) sends
    /// `WindowFocused` carrying the window's OS-observed placement, and
    /// nothing else ever sends `WindowPlaced` for a window Mosaix didn't
    /// itself just place. `WindowFocused` must therefore register the
    /// window itself, or every snap hotkey silently no-ops on first use.
    #[test]
    fn zone_snap_on_a_freshly_focused_never_placed_window_snaps_it_using_the_observed_bounds() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(100, 100, 800, 600),
            },
        );

        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.bounds,
            Rect::new(0, 0, 960, 1080),
            "a snap hotkey on a freshly-focused, never-explicitly-placed window must snap it, \
             not silently no-op"
        );
    }

    #[test]
    fn apply_zone_snap_on_migrated_window_works_on_surviving_display() {
        // When a monitor unplugs, `migrate_orphaned_windows` moves any
        // window that was on it to the nearest surviving display. A
        // subsequent zone-snap must therefore operate on the window's
        // *new* display, not fail because the original display is gone.
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0), display(2, "MON-B", 1920)]),
        );
        let original_bounds = Rect::new(0, 0, 960, 1080);
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: original_bounds,
            },
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: original_bounds,
            },
        );
        // Display 1 (the focused window's display) unplugs, leaving only display 2.
        // `migrate_orphaned_windows` runs as part of the topology-change handler,
        // moving the window to display 2.
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(2, "MON-B", 1920)]),
        );

        // Confirm migration happened.
        assert_eq!(
            state.windows.get(&WindowId(1)).unwrap().display_id,
            DisplayId(2),
            "window must have been migrated to display 2"
        );

        let revision_before = state.revision;
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );

        // Zone snap must now succeed, using display 2's work area.
        assert!(
            state.revision > revision_before,
            "zone-snap on a migrated window must produce a new placement (revision must bump)"
        );
        // The window must now be on display 2 with a valid left-half snap.
        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.display_id,
            DisplayId(2),
            "window must remain on display 2 after snap"
        );
        // Left-half of display 2's 1920x1080 work area (starts at x=1920).
        assert_eq!(
            placement.bounds,
            Rect::new(1920, 0, 960, 1080),
            "snap must produce the left half of display 2's work area"
        );
    }

    #[test]
    fn apply_bounds_observed_matching_the_last_placement_leaves_cycle_state_alone() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        let revision_before = state.revision;

        // The OS echoes back exactly the bounds/display Mosaix's own
        // placement just produced.
        apply(
            &mut state,
            Event::WindowBoundsObserved {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 960, 1080),
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.cycle_step,
            Some((HorizontalDirection::Left, CycleStep::Half)),
            "a matching observation must leave cycle state untouched"
        );
        assert_eq!(
            state.revision, revision_before,
            "a matching observation must not bump the revision"
        );
    }

    #[test]
    fn apply_bounds_observed_not_matching_the_last_placement_resets_cycle_step() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        let revision_before = state.revision;

        // A manual drag left the window somewhere Mosaix's own last
        // placement (the left third) didn't put it.
        apply(
            &mut state,
            Event::WindowBoundsObserved {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(100, 100, 400, 400),
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.cycle_step, None,
            "a non-matching observation must reset cycle step to step 1"
        );
        assert_eq!(
            placement.bounds,
            Rect::new(0, 0, 640, 1080),
            "tracked placement bounds must not be altered by an observation"
        );
        assert_eq!(state.revision, revision_before + 1);
    }

    #[test]
    fn apply_bounds_observed_mismatch_then_zone_snap_restarts_the_cycle_at_half() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        apply(
            &mut state,
            Event::WindowBoundsObserved {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(100, 100, 400, 400),
            },
        );

        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.bounds,
            Rect::new(0, 0, 960, 1080),
            "the next same-direction press after an external move must start back at half"
        );
        assert_eq!(
            placement.cycle_step,
            Some((HorizontalDirection::Left, CycleStep::Half))
        );
    }

    #[test]
    fn apply_bounds_observed_is_a_noop_for_an_untracked_window() {
        let mut state = EngineState::default();

        apply(
            &mut state,
            Event::WindowBoundsObserved {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 100, 100),
            },
        );

        assert_eq!(state.revision, 0);
        assert!(state.windows.is_empty());
    }

    #[test]
    fn apply_bounds_observed_mismatch_with_no_cycle_step_records_the_move() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            },
        );
        let revision_before = state.revision;

        apply(
            &mut state,
            Event::WindowBoundsObserved {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(100, 100, 400, 400),
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(placement.cycle_step, None);
        assert_eq!(
            placement.bounds,
            Rect::new(0, 0, 1920, 1080),
            "the intended placement is unchanged by an observation"
        );
        assert_eq!(
            placement.observed_bounds,
            Rect::new(100, 100, 400, 400),
            "but where the window actually is must be recorded"
        );
        assert!(
            state.revision > revision_before,
            "the window really moved, so watchers of the revision -- the focus \
             border among them -- must be able to notice"
        );
    }

    fn resolved_config_with_left_binding(combo: &str) -> ResolvedConfig {
        let mut hotkeys = std::collections::BTreeMap::new();
        hotkeys.insert(
            mosaix_config::Command::SnapLeft,
            mosaix_config::KeyCombo::parse(combo).unwrap(),
        );
        ResolvedConfig {
            hotkeys,
            gaps: mosaix_domain::Gaps::default(),
            ..ResolvedConfig::default()
        }
    }

    fn config_set_with_left_binding(combo: &str) -> ResolvedConfigSet {
        ResolvedConfigSet {
            base: resolved_config_with_left_binding(combo),
            profiles: Vec::new(),
        }
    }

    fn config_set_with_gaps(gaps: mosaix_domain::Gaps) -> ResolvedConfigSet {
        ResolvedConfigSet {
            base: ResolvedConfig {
                gaps,
                ..ResolvedConfig::default()
            },
            profiles: Vec::new(),
        }
    }

    fn observed_window(
        id: isize,
        role: mosaix_domain::WindowRole,
        lifecycle: WindowLifecycle,
    ) -> Window {
        Window {
            id: WindowId(id),
            process_id: 1,
            application_id: mosaix_domain::ApplicationId("test".to_string()),
            executable_path: None,
            title: "non-sensitive-test-title".to_string(),
            native_class: None,
            role,
            bounds: Rect::new(0, 0, 400, 300),
            display_id: DisplayId(1),
            capabilities: mosaix_domain::WindowCapabilities {
                can_move: true,
                can_resize: true,
                can_minimize: true,
                can_maximize: true,
            },
            elevated: false,
            lifecycle,
            minimum_size: None,
        }
    }

    #[test]
    fn observed_windows_publish_tile_float_and_exclude_rule_outcomes() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    observed_window(
                        1,
                        mosaix_domain::WindowRole::Normal,
                        WindowLifecycle::Active,
                    ),
                    observed_window(
                        2,
                        mosaix_domain::WindowRole::Dialog,
                        WindowLifecycle::Active,
                    ),
                    observed_window(3, mosaix_domain::WindowRole::Popup, WindowLifecycle::Active),
                ],
            },
        );

        assert_eq!(state.inventory.len(), 2);
        assert_eq!(state.inventory[&WindowId(1)].action, ManageAction::Tile);
        assert_eq!(
            state.inventory[&WindowId(1)].eligibility,
            EligibilityReason::Eligible
        );
        assert_eq!(state.inventory[&WindowId(2)].action, ManageAction::Float);
        assert_eq!(
            state.inventory[&WindowId(2)].eligibility,
            EligibilityReason::FloatingRule
        );
        assert!(!state.inventory.contains_key(&WindowId(3)));
    }

    /// Rules were unreachable from configuration before this: nothing
    /// ever sent `Event::RulesChanged`, so `state.rules` stayed empty
    /// however many `[[rules]]` the user wrote. Configuration is now the
    /// source, and a config change is what delivers them.
    #[test]
    fn rules_written_in_configuration_reach_the_evaluator() {
        let mut state = EngineState::default();

        apply(
            &mut state,
            Event::ConfigChanged(Box::new(ResolvedConfigSet {
                base: ResolvedConfig {
                    rules: vec![mosaix_rules::RuleConfig {
                        id: "float-the-test-window".to_owned(),
                        // Above the built-ins, which sit at -100.
                        priority: 10,
                        enabled: true,
                        matcher: mosaix_rules::MatcherConfig {
                            title_regex: Some("^non-sensitive".to_owned()),
                            ..Default::default()
                        },
                        actions: mosaix_rules::ActionConfig {
                            manage: ManageAction::Float,
                            workspace: None,
                        },
                    }],
                    ..ResolvedConfig::default()
                },
                profiles: Vec::new(),
            })),
        );

        assert_eq!(state.rules.len(), 1, "configuration is a source of rules");
        assert_eq!(state.rules[0].id, "float-the-test-window");

        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![observed_window(
                    1,
                    mosaix_domain::WindowRole::Normal,
                    WindowLifecycle::Active,
                )],
            },
        );

        assert_eq!(
            state.inventory[&WindowId(1)].action,
            ManageAction::Float,
            "a normal window the user's rule matched floats instead of tiling"
        );
        assert_eq!(
            state.inventory[&WindowId(1)].eligibility,
            EligibilityReason::FloatingRule
        );
    }

    #[test]
    fn elevated_windows_remain_in_inventory_with_an_ineligible_reason() {
        let mut state = EngineState::default();
        let mut elevated = observed_window(
            8,
            mosaix_domain::WindowRole::Normal,
            WindowLifecycle::Active,
        );
        elevated.elevated = true;

        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![elevated],
            },
        );

        assert_eq!(
            state.inventory[&WindowId(8)].eligibility,
            EligibilityReason::Elevated
        );
    }

    #[test]
    fn active_profile_reflows_eligible_inventory_into_balanced_grid_effects() {
        let mut state = EngineState {
            displays: vec![display(1, "primary", 0)],
            automatic_tiling_active: true,
            ..EngineState::default()
        };
        let mut left = observed_window(
            1,
            mosaix_domain::WindowRole::Normal,
            WindowLifecycle::Active,
        );
        left.bounds = Rect::new(100, 200, 400, 300);
        let mut right = observed_window(
            2,
            mosaix_domain::WindowRole::Normal,
            WindowLifecycle::Active,
        );
        right.bounds = Rect::new(800, 200, 400, 300);

        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![right, left],
            },
        );

        assert_eq!(
            state.visual_window_order[&DisplayId(1)],
            vec![WindowId(1), WindowId(2)]
        );
        assert_eq!(
            state.effects,
            vec![
                EngineEffect::PlaceWindow {
                    window_id: WindowId(1),
                    display_id: DisplayId(1),
                    bounds: Rect::new(0, 0, 960, 1080)
                },
                EngineEffect::PlaceWindow {
                    window_id: WindowId(2),
                    display_id: DisplayId(1),
                    bounds: Rect::new(960, 0, 960, 1080)
                },
            ]
        );
    }

    #[test]
    fn temporary_lifecycle_ineligibility_keeps_visual_order_and_reflows_once() {
        let mut state = EngineState {
            displays: vec![display(1, "primary", 0)],
            automatic_tiling_active: true,
            ..EngineState::default()
        };
        let windows = vec![
            observed_window(
                1,
                mosaix_domain::WindowRole::Normal,
                WindowLifecycle::Active,
            ),
            observed_window(
                2,
                mosaix_domain::WindowRole::Normal,
                WindowLifecycle::Active,
            ),
            observed_window(
                3,
                mosaix_domain::WindowRole::Normal,
                WindowLifecycle::Active,
            ),
        ];
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: windows.clone(),
            },
        );
        state.effects.clear();

        let mut minimized = windows.clone();
        minimized[1].lifecycle = WindowLifecycle::Minimized;
        apply(&mut state, Event::WindowsObserved { windows: minimized });

        assert_eq!(
            state.visual_window_order[&DisplayId(1)],
            vec![WindowId(1), WindowId(2), WindowId(3)]
        );
        assert_eq!(
            state.inventory[&WindowId(2)].eligibility,
            EligibilityReason::Minimized
        );
        assert_eq!(
            state.effects.len(),
            2,
            "only the two active members receive the settled plan"
        );
    }

    #[test]
    fn apply_config_changed_updates_resolved_config_and_bumps_revision() {
        let mut state = EngineState::default();
        assert_eq!(state.resolved_config, ResolvedConfig::default());

        let new_config_set = config_set_with_left_binding("ctrl+alt+left");
        apply(
            &mut state,
            Event::ConfigChanged(Box::new(new_config_set.clone())),
        );

        assert_eq!(state.resolved_config, new_config_set.base);
        assert_eq!(state.config_set, new_config_set);
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn apply_config_changed_ignores_an_identical_resolved_config() {
        let mut state = EngineState::default();
        let config_set = config_set_with_left_binding("ctrl+alt+left");
        apply(
            &mut state,
            Event::ConfigChanged(Box::new(config_set.clone())),
        );
        let revision_after_first = state.revision;

        apply(&mut state, Event::ConfigChanged(Box::new(config_set)));

        assert_eq!(
            state.revision, revision_after_first,
            "re-delivering an identical resolved config must not bump the revision"
        );
    }

    #[test]
    fn apply_config_changed_again_with_different_settings_replaces_it() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::ConfigChanged(Box::new(config_set_with_left_binding("ctrl+alt+left"))),
        );
        let revision_after_first = state.revision;

        let second_config_set = config_set_with_left_binding("ctrl+shift+left");
        apply(
            &mut state,
            Event::ConfigChanged(Box::new(second_config_set.clone())),
        );

        assert_eq!(state.resolved_config, second_config_set.base);
        assert_eq!(state.revision, revision_after_first + 1);
    }

    #[test]
    fn apply_topology_change_selects_a_profile_matching_the_new_topology() {
        let mut state = EngineState::default();
        let displays = vec![display(1, "MON-A", 0)];
        let profile = ResolvedProfile {
            fingerprint: topology_fingerprint(&displays),
            config: resolved_config_with_left_binding("ctrl+shift+left"),
        };
        let config_set = ResolvedConfigSet {
            base: ResolvedConfig::default(),
            profiles: vec![profile.clone()],
        };
        apply(&mut state, Event::ConfigChanged(Box::new(config_set)));
        assert_eq!(
            state.resolved_config,
            ResolvedConfig::default(),
            "no display observed yet, so no profile should match"
        );

        apply(&mut state, Event::DisplayTopologyChanged(displays));

        assert_eq!(
            state.resolved_config, profile.config,
            "the topology change should re-select the profile matching its fingerprint"
        );
    }

    #[test]
    fn moving_to_a_desk_with_a_profile_swaps_in_that_profiles_saved_layouts() {
        // Per-topology saved layouts ride the same profile selection every
        // other setting does, so a docked desk and a laptop alone can offer
        // different layout sets with no restart and no new mechanism.
        let mut state = EngineState::default();
        let laptop = vec![display(1, "MON-A", 0)];
        let desk = vec![display(1, "MON-A", 0), display(2, "MON-B", 1920)];
        let mut docked_layouts = std::collections::BTreeMap::new();
        docked_layouts.insert(
            "docked".to_owned(),
            mosaix_config::SavedLayout {
                cells: vec![mosaix_domain::NormalizedRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                }],
            },
        );
        let mut base_layouts = std::collections::BTreeMap::new();
        base_layouts.insert("writing".to_owned(), mosaix_config::SavedLayout::default());
        let config_set = ResolvedConfigSet {
            base: ResolvedConfig {
                layouts: base_layouts,
                ..ResolvedConfig::default()
            },
            profiles: vec![ResolvedProfile {
                fingerprint: topology_fingerprint(&desk),
                config: ResolvedConfig {
                    layouts: docked_layouts,
                    ..ResolvedConfig::default()
                },
            }],
        };
        apply(&mut state, Event::ConfigChanged(Box::new(config_set)));
        apply(&mut state, Event::DisplayTopologyChanged(laptop));

        assert_eq!(
            state.resolved_config.layouts.keys().collect::<Vec<_>>(),
            vec!["writing"],
            "a topology matching no profile offers the base layouts"
        );

        apply(&mut state, Event::DisplayTopologyChanged(desk));

        assert_eq!(
            state.resolved_config.layouts.keys().collect::<Vec<_>>(),
            vec!["docked"],
            "docking makes the matched profile's layouts the ones on offer"
        );
    }

    #[test]
    fn apply_topology_change_with_no_matching_profile_falls_back_to_base() {
        let mut state = EngineState::default();
        let base = resolved_config_with_left_binding("ctrl+alt+left");
        let profile = ResolvedProfile {
            fingerprint: "a fingerprint no display will ever produce".to_string(),
            config: resolved_config_with_left_binding("ctrl+shift+left"),
        };
        let config_set = ResolvedConfigSet {
            base: base.clone(),
            profiles: vec![profile],
        };
        apply(&mut state, Event::ConfigChanged(Box::new(config_set)));

        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );

        assert_eq!(
            state.resolved_config, base,
            "an unrecognized topology must fall back to base config, not error or invent a profile"
        );
    }

    #[test]
    fn apply_topology_round_trip_between_two_known_topologies_reselects_the_same_profile() {
        let mut state = EngineState::default();
        let displays_a = vec![display(1, "MON-A", 0)];
        let displays_b = vec![display(2, "MON-B", 0)];
        let profile_a = ResolvedProfile {
            fingerprint: topology_fingerprint(&displays_a),
            config: resolved_config_with_left_binding("ctrl+shift+left"),
        };
        let profile_b = ResolvedProfile {
            fingerprint: topology_fingerprint(&displays_b),
            config: resolved_config_with_left_binding("ctrl+alt+left"),
        };
        let config_set = ResolvedConfigSet {
            base: ResolvedConfig::default(),
            profiles: vec![profile_a.clone(), profile_b.clone()],
        };
        apply(&mut state, Event::ConfigChanged(Box::new(config_set)));

        apply(
            &mut state,
            Event::DisplayTopologyChanged(displays_a.clone()),
        );
        assert_eq!(state.resolved_config, profile_a.config);

        apply(&mut state, Event::DisplayTopologyChanged(displays_b));
        assert_eq!(state.resolved_config, profile_b.config);

        apply(&mut state, Event::DisplayTopologyChanged(displays_a));
        assert_eq!(
            state.resolved_config, profile_a.config,
            "switching back to a previously-seen topology must re-select the same profile"
        );
    }

    #[test]
    fn spawned_engine_starts_with_initial_state_and_applies_events() {
        let handle = spawn_engine(vec![display(1, "MON-A", 0)], ResolvedConfigSet::default());
        assert_eq!(handle.snapshot().revision, 0);
        assert_eq!(handle.snapshot().displays.len(), 1);

        handle
            .events()
            .send(Event::DisplayTopologyChanged(vec![
                display(1, "MON-A", 0),
                display(2, "MON-B", 1920),
            ]))
            .expect("reducer should still be running");

        assert!(
            wait_for(|| handle.snapshot().revision == 1, Duration::from_secs(2)),
            "expected the reducer to apply the queued event"
        );
        assert_eq!(handle.snapshot().displays.len(), 2);

        handle.stop();
    }

    #[test]
    fn stopping_the_engine_closes_the_queue_and_joins_cleanly() {
        let handle = spawn_engine(Vec::new(), ResolvedConfigSet::default());
        let events = handle.events();
        handle.stop();

        // The reducer thread is gone; the queue is closed, so further
        // sends must fail rather than hang.
        assert!(events
            .send(Event::DisplayTopologyChanged(Vec::new()))
            .is_err());
    }

    #[test]
    fn diff_placements_is_empty_when_nothing_changed() {
        let mut windows = HashMap::new();
        windows.insert(
            WindowId(1),
            WindowPlacement {
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 960, 1080),
                observed_bounds: Rect::new(0, 0, 960, 1080),
                previous_placement: None,
                cycle_step: None,
                rejection_count: 0,
            },
        );

        assert!(diff_placements(&windows, &windows.clone()).is_empty());
    }

    #[test]
    fn diff_placements_reports_new_and_moved_windows_but_not_unchanged_ones() {
        let unchanged = WindowPlacement {
            display_id: DisplayId(1),
            bounds: Rect::new(0, 0, 960, 1080),
            observed_bounds: Rect::new(0, 0, 960, 1080),
            previous_placement: None,
            cycle_step: None,
            rejection_count: 0,
        };
        let mut previous = HashMap::new();
        previous.insert(WindowId(1), unchanged);
        previous.insert(
            WindowId(2),
            WindowPlacement {
                bounds: Rect::new(0, 0, 1920, 1080),
                ..unchanged
            },
        );

        let mut current = HashMap::new();
        current.insert(WindowId(1), unchanged); // untouched
        current.insert(
            WindowId(2),
            WindowPlacement {
                bounds: Rect::new(960, 0, 960, 1080),
                ..unchanged
            }, // moved
        );
        current.insert(
            WindowId(3),
            WindowPlacement {
                bounds: Rect::new(0, 0, 640, 1080),
                ..unchanged
            }, // new
        );

        let mut diff = diff_placements(&previous, &current);
        diff.sort_by_key(|(window_id, _, _)| window_id.0);

        assert_eq!(
            diff,
            vec![
                (WindowId(2), DisplayId(1), Rect::new(960, 0, 960, 1080)),
                (WindowId(3), DisplayId(1), Rect::new(0, 0, 640, 1080)),
            ],
            "unchanged WindowId(1) must be excluded; moved WindowId(2) and new WindowId(3) must both be reported"
        );
    }

    /// The seam a platform executor polls: run the exact event sequence a
    /// real snap hotkey produces through a real spawned engine, and confirm
    /// `diff_placements` against successive [`StateReader`] snapshots
    /// reports the window that now needs a real `SetWindowPos` call. This
    /// is what closes the gap in "the engine computes a new placement but
    /// nothing ever moves the real window" -- the executor only has to
    /// react to what this reports.
    #[test]
    fn state_reader_diff_reports_the_window_a_snap_hotkey_just_placed() {
        let handle = spawn_engine(vec![display(1, "MON-A", 0)], ResolvedConfigSet::default());
        let reader = handle.state_reader();
        let before = reader.snapshot().windows;

        handle
            .events()
            .send(Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(100, 100, 800, 600),
            })
            .expect("reducer should still be running");
        handle
            .events()
            .send(Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            })
            .expect("reducer should still be running");

        assert!(
            wait_for(
                || reader.snapshot().windows.contains_key(&WindowId(1)),
                Duration::from_secs(2)
            ),
            "expected the snap to be committed"
        );
        let after = reader.snapshot().windows;

        assert_eq!(
            diff_placements(&before, &after),
            vec![(WindowId(1), DisplayId(1), Rect::new(0, 0, 960, 1080))],
            "an executor polling before/after this snap must be told to move WindowId(1)"
        );

        handle.stop();
    }

    #[test]
    fn apply_pause_sets_paused_and_bumps_revision() {
        let mut state = EngineState::default();
        apply(&mut state, Event::PauseRequested);
        assert!(state.paused);
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn apply_pause_twice_is_idempotent() {
        let mut state = EngineState::default();
        apply(&mut state, Event::PauseRequested);
        apply(&mut state, Event::PauseRequested);
        assert!(state.paused);
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn apply_resume_clears_paused_and_bumps_revision() {
        let mut state = EngineState {
            paused: true,
            ..Default::default()
        };
        apply(&mut state, Event::ResumeRequested);
        assert!(!state.paused);
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn apply_resume_when_not_paused_is_idempotent() {
        let mut state = EngineState::default();
        apply(&mut state, Event::ResumeRequested);
        assert!(!state.paused);
        assert_eq!(state.revision, 0);
    }

    #[test]
    fn apply_zone_snap_is_suppressed_while_paused() {
        let mut state = EngineState {
            paused: true,
            focused_window: Some(WindowId(1)),
            ..Default::default()
        };
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        assert_eq!(state.revision, 0);
    }

    #[test]
    fn apply_window_placed_is_suppressed_while_paused() {
        let mut state = EngineState {
            paused: true,
            ..Default::default()
        };
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 100, 100),
            },
        );
        assert_eq!(state.revision, 0);
    }

    #[test]
    fn apply_throw_is_suppressed_while_paused() {
        let mut state = EngineState {
            paused: true,
            ..Default::default()
        };
        apply(
            &mut state,
            Event::WindowThrowToDisplayRequested {
                window_id: WindowId(1),
                direction: mosaix_layout::DisplayDirection::Next,
            },
        );
        assert_eq!(state.revision, 0);
    }

    #[test]
    fn apply_display_topology_still_processes_while_paused() {
        let mut state = EngineState {
            paused: true,
            ..Default::default()
        };
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn apply_focus_still_processes_while_paused() {
        let mut state = EngineState {
            paused: true,
            ..Default::default()
        };
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 100, 100),
            },
        );
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn bounds_observed_updates_observed_bounds_but_leaves_the_intended_placement() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 100, 100),
            },
        );
        let revision_before = state.revision;

        // Something other than Mosaix moved the window.
        apply(
            &mut state,
            Event::WindowBoundsObserved {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(400, 300, 100, 100),
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.observed_bounds,
            Rect::new(400, 300, 100, 100),
            "observed bounds must track where the window actually is"
        );
        assert_eq!(
            placement.bounds,
            Rect::new(0, 0, 100, 100),
            "the intended placement stays put -- comparing the two is how an \
             external move is detected"
        );
        assert!(
            state.revision > revision_before,
            "readers watching the revision must be able to notice the move"
        );
    }

    #[test]
    fn apply_config_still_processes_while_paused() {
        let mut state = EngineState {
            paused: true,
            ..Default::default()
        };
        let config_set = ResolvedConfigSet {
            base: ResolvedConfig::default(),
            profiles: vec![],
        };
        apply(&mut state, Event::ConfigChanged(Box::new(config_set)));
        assert!(state.paused);
    }

    // ---- Startup reconciliation ---------------------------------------

    #[test]
    fn startup_reconciliation_registers_untracked_windows() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        let windows = vec![
            (WindowId(10), DisplayId(1), Rect::new(0, 0, 960, 1080)),
            (WindowId(20), DisplayId(1), Rect::new(960, 0, 960, 1080)),
        ];
        apply(&mut state, Event::StartupReconciliation { windows });

        assert!(
            state.windows.contains_key(&WindowId(10)),
            "window 10 must be registered by reconciliation"
        );
        assert!(
            state.windows.contains_key(&WindowId(20)),
            "window 20 must be registered by reconciliation"
        );
        assert_eq!(
            state.windows.get(&WindowId(10)).unwrap().display_id,
            DisplayId(1)
        );
    }

    #[test]
    fn startup_reconciliation_does_not_overwrite_already_tracked_windows() {
        // If a WindowFocused event arrived before StartupReconciliation
        // (e.g. due to event ordering), the existing placement must win.
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        let focused_bounds = Rect::new(100, 100, 800, 600);
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(10),
                display_id: DisplayId(1),
                bounds: focused_bounds,
            },
        );
        // Reconciliation reports different bounds for the same window.
        apply(
            &mut state,
            Event::StartupReconciliation {
                windows: vec![(WindowId(10), DisplayId(1), Rect::new(0, 0, 960, 1080))],
            },
        );

        // The focused-event placement must be preserved.
        assert_eq!(
            state.windows.get(&WindowId(10)).unwrap().bounds,
            focused_bounds,
            "an already-tracked window's placement must not be overwritten by reconciliation"
        );
    }

    #[test]
    fn startup_reconciliation_with_empty_list_is_a_noop() {
        let mut state = EngineState::default();
        apply(&mut state, Event::StartupReconciliation { windows: vec![] });
        assert_eq!(
            state.revision, 0,
            "empty reconciliation must not bump the revision"
        );
        assert!(state.windows.is_empty());
    }

    // ---- Display migration (hotplug and wake) -------------------------

    #[test]
    fn topology_change_migrates_window_to_nearest_surviving_display() {
        // The window starts on MON-A (left monitor). When MON-A unplugs,
        // the window must be migrated to MON-B (the only survivor).
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0), display(2, "MON-B", 1920)]),
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 960, 1080),
            },
        );
        // MON-A unplugs.
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(2, "MON-B", 1920)]),
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.display_id,
            DisplayId(2),
            "window must be migrated to the surviving display"
        );
        // Bounds must be within display 2's work area (x in 1920..3840).
        assert!(
            placement.bounds.x >= 1920,
            "migrated bounds must be on display 2's x range"
        );
    }

    #[test]
    fn topology_change_with_no_displays_does_not_migrate() {
        // If all displays disappear (USB dock fully unplugged), we leave windows
        // in place rather than crashing or migrating to nothing.
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 960, 1080),
            },
        );
        let bounds_before = state.windows.get(&WindowId(1)).unwrap().bounds;

        // Topology fingerprints differ when the display list changes, so even an
        // empty list is processed (the fingerprint of [] != fingerprint of [MON-A]).
        // But migration must gracefully handle the empty-display case.
        apply(&mut state, Event::DisplayTopologyChanged(vec![]));

        assert_eq!(
            state.windows.get(&WindowId(1)).unwrap().bounds,
            bounds_before,
            "with no surviving displays, window bounds must be left intact"
        );
    }

    #[test]
    fn wake_reconciliation_updates_topology_and_registers_new_windows() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 960, 1080),
            },
        );

        // After wake: a new display appears and a new window opened while asleep.
        let new_displays = vec![display(1, "MON-A", 0), display(2, "MON-B", 1920)];
        let mut first = observed_window(
            1,
            mosaix_domain::WindowRole::Normal,
            WindowLifecycle::Active,
        );
        first.bounds = Rect::new(0, 0, 960, 1080);
        let mut second = observed_window(
            99,
            mosaix_domain::WindowRole::Normal,
            WindowLifecycle::Active,
        );
        second.display_id = DisplayId(2);
        second.bounds = Rect::new(1920, 0, 960, 1080);
        let new_windows = vec![first, second];
        apply(
            &mut state,
            Event::WakeReconciliation {
                displays: new_displays,
                windows: Some(new_windows),
            },
        );

        assert_eq!(
            state.displays.len(),
            2,
            "topology must include the new display"
        );
        assert!(
            state.windows.contains_key(&WindowId(99)),
            "new window observed after wake must be registered"
        );
        // Window 1's placement must not be overwritten.
        assert_eq!(
            state.windows.get(&WindowId(1)).unwrap().bounds,
            Rect::new(0, 0, 960, 1080)
        );
    }

    // ---- Per-window circuit breaker -----------------------------------

    #[test]
    fn placement_rejected_increments_rejection_count() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 960, 1080),
            },
        );
        apply(
            &mut state,
            Event::PlacementRejected {
                window_id: WindowId(1),
            },
        );
        apply(
            &mut state,
            Event::PlacementRejected {
                window_id: WindowId(1),
            },
        );

        assert_eq!(state.windows.get(&WindowId(1)).unwrap().rejection_count, 2);
        // Not yet at threshold; circuit must still be closed.
        assert!(
            !state.windows.get(&WindowId(1)).unwrap().circuit_open(),
            "circuit must not open before threshold is reached"
        );
    }

    #[test]
    fn accepted_placement_breaks_a_rejection_streak() {
        let mut state = EngineState::default();
        state.windows.insert(
            WindowId(1),
            WindowPlacement {
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 100, 100),
                observed_bounds: Rect::new(0, 0, 100, 100),
                previous_placement: None,
                cycle_step: None,
                rejection_count: 2,
            },
        );

        apply(
            &mut state,
            Event::PlacementAccepted {
                window_id: WindowId(1),
            },
        );

        assert_eq!(state.windows[&WindowId(1)].rejection_count, 0);
    }

    #[test]
    fn circuit_breaker_opens_at_threshold_and_suppresses_placements() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 960, 1080),
            },
        );
        // Drive to threshold.
        for _ in 0..CIRCUIT_BREAKER_THRESHOLD {
            apply(
                &mut state,
                Event::PlacementRejected {
                    window_id: WindowId(1),
                },
            );
        }

        assert!(
            state.windows.get(&WindowId(1)).unwrap().circuit_open(),
            "circuit must be open after reaching the threshold"
        );

        // A new WindowPlaced must be suppressed by place_window.
        let revision_before = state.revision;
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(100, 100, 400, 300),
            },
        );
        assert_eq!(
            state.revision, revision_before,
            "placement must be suppressed while circuit is open"
        );
        assert_eq!(
            state.windows.get(&WindowId(1)).unwrap().bounds,
            Rect::new(0, 0, 960, 1080),
            "window bounds must remain unchanged while circuit is open"
        );
    }

    #[test]
    fn zone_snap_resets_circuit_breaker_and_places_the_window() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 960, 1080),
            },
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 960, 1080),
            },
        );
        // Open the circuit.
        for _ in 0..CIRCUIT_BREAKER_THRESHOLD {
            apply(
                &mut state,
                Event::PlacementRejected {
                    window_id: WindowId(1),
                },
            );
        }
        assert!(state.windows.get(&WindowId(1)).unwrap().circuit_open());

        // Zone-snap must reset the breaker and snap the window.
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(
            placement.rejection_count, 0,
            "zone-snap must reset the rejection count to zero"
        );
        assert!(
            !placement.circuit_open(),
            "circuit must be closed after zone-snap"
        );
        // The window must have actually been snapped.
        assert_eq!(
            placement.bounds,
            Rect::new(0, 0, 960, 1080),
            "left-half snap on display 1"
        );
    }

    #[test]
    fn circuit_breaker_count_reports_open_circuit_windows() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        for id in [1, 2, 3] {
            apply(
                &mut state,
                Event::WindowPlaced {
                    window_id: WindowId(id),
                    display_id: DisplayId(1),
                    bounds: Rect::new(0, 0, 960, 1080),
                },
            );
        }
        assert_eq!(state.circuit_breaker_count(), 0);

        // Open circuit on window 1 and 3, but not 2.
        for _ in 0..CIRCUIT_BREAKER_THRESHOLD {
            apply(
                &mut state,
                Event::PlacementRejected {
                    window_id: WindowId(1),
                },
            );
            apply(
                &mut state,
                Event::PlacementRejected {
                    window_id: WindowId(3),
                },
            );
        }

        assert_eq!(
            state.circuit_breaker_count(),
            2,
            "two windows should have open circuits"
        );
    }

    #[test]
    fn rearrange_resets_open_circuits_and_reflows_healthy_windows_once() {
        let mut state = EngineState {
            displays: vec![display(1, "primary", 0)],
            automatic_tiling_active: true,
            ..EngineState::default()
        };
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    observed_window(
                        1,
                        mosaix_domain::WindowRole::Normal,
                        WindowLifecycle::Active,
                    ),
                    observed_window(
                        2,
                        mosaix_domain::WindowRole::Normal,
                        WindowLifecycle::Active,
                    ),
                ],
            },
        );
        for _ in 0..CIRCUIT_BREAKER_THRESHOLD {
            apply(
                &mut state,
                Event::PlacementRejected {
                    window_id: WindowId(1),
                },
            );
        }
        state.effects.clear();

        apply(&mut state, Event::RearrangeRequested);

        assert_eq!(state.effects, vec![EngineEffect::ReconcileWindows]);
        state.effects.clear();
        apply(
            &mut state,
            Event::RearrangeReconciliationComplete {
                windows: vec![
                    observed_window(
                        1,
                        mosaix_domain::WindowRole::Normal,
                        WindowLifecycle::Active,
                    ),
                    observed_window(
                        2,
                        mosaix_domain::WindowRole::Normal,
                        WindowLifecycle::Active,
                    ),
                ],
            },
        );

        assert_eq!(state.circuit_breaker_count(), 0);
        assert_eq!(
            state.inventory[&WindowId(1)].eligibility,
            EligibilityReason::Eligible
        );
        assert_eq!(
            state.effects,
            vec![EngineEffect::PlaceWindow {
                window_id: WindowId(2),
                display_id: DisplayId(1),
                bounds: Rect::new(960, 0, 960, 1080),
            }],
            "one reflow should restore the healthy window's balanced cell"
        );
    }

    #[test]
    fn automatic_tiling_suspension_preserves_manual_placement_and_clears_on_topology_change() {
        let mut state = EngineState {
            displays: vec![display(1, "primary", 0)],
            resolved_config: ResolvedConfig {
                automatic_tiling_enabled: true,
                ..ResolvedConfig::default()
            },
            automatic_tiling_active: true,
            ..EngineState::default()
        };

        apply(&mut state, Event::ToggleAutomaticTilingRequested);
        assert!(state.automatic_tiling_suspended);
        assert!(!state.automatic_tiling_active);

        apply(
            &mut state,
            Event::WindowPlaced {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 400, 300),
            },
        );
        assert!(state.windows.contains_key(&WindowId(1)));

        state.config_set.base = state.resolved_config.clone();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(2, "secondary", 1920)]),
        );
        assert!(!state.automatic_tiling_suspended);
        assert!(state.automatic_tiling_active);
    }

    #[test]
    fn zone_snap_session_floats_the_focused_window_and_reflows_the_remainder() {
        let mut state = EngineState {
            displays: vec![display(1, "primary", 0)],
            automatic_tiling_active: true,
            ..EngineState::default()
        };
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    observed_window(
                        1,
                        mosaix_domain::WindowRole::Normal,
                        WindowLifecycle::Active,
                    ),
                    observed_window(
                        2,
                        mosaix_domain::WindowRole::Normal,
                        WindowLifecycle::Active,
                    ),
                ],
            },
        );
        state.effects.clear();
        state.focused_window = Some(WindowId(1));

        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );

        assert!(state.session_floating.contains(&WindowId(1)));
        assert_eq!(
            state.inventory[&WindowId(1)].eligibility,
            EligibilityReason::SessionFloating
        );
        assert_eq!(
            state.windows[&WindowId(2)].bounds,
            Rect::new(0, 0, 1920, 1080)
        );
    }

    #[test]
    fn directional_focus_emits_the_nearest_display_local_neighbor() {
        let mut state = EngineState {
            displays: vec![display(1, "primary", 0), display(2, "secondary", 1920)],
            automatic_tiling_active: true,
            ..EngineState::default()
        };
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    observed_window(
                        1,
                        mosaix_domain::WindowRole::Normal,
                        WindowLifecycle::Active,
                    ),
                    observed_window(
                        2,
                        mosaix_domain::WindowRole::Normal,
                        WindowLifecycle::Active,
                    ),
                ],
            },
        );
        state.focused_window = Some(WindowId(1));
        state.effects.clear();

        apply(
            &mut state,
            Event::DirectionalFocusRequested {
                direction: CardinalDirection::Right,
            },
        );

        assert_eq!(
            state.effects,
            vec![EngineEffect::FocusWindow {
                window_id: WindowId(2)
            }]
        );
    }

    #[test]
    fn directional_swap_exchanges_visual_order_and_reflows_changed_cells() {
        let mut state = EngineState {
            displays: vec![display(1, "primary", 0)],
            automatic_tiling_active: true,
            ..EngineState::default()
        };
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    observed_window(
                        1,
                        mosaix_domain::WindowRole::Normal,
                        WindowLifecycle::Active,
                    ),
                    observed_window(
                        2,
                        mosaix_domain::WindowRole::Normal,
                        WindowLifecycle::Active,
                    ),
                ],
            },
        );
        state.focused_window = Some(WindowId(1));
        state.effects.clear();

        apply(
            &mut state,
            Event::DirectionalSwapRequested {
                direction: CardinalDirection::Right,
            },
        );

        assert_eq!(
            state.visual_window_order[&DisplayId(1)],
            vec![WindowId(2), WindowId(1)]
        );
        assert_eq!(state.effects.len(), 2);
    }

    #[test]
    fn interactive_placement_defers_its_display_until_session_end() {
        let mut state = EngineState {
            displays: vec![display(1, "primary", 0)],
            automatic_tiling_active: true,
            ..EngineState::default()
        };
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![observed_window(
                    1,
                    mosaix_domain::WindowRole::Normal,
                    WindowLifecycle::Active,
                )],
            },
        );
        apply(
            &mut state,
            Event::InteractivePlacementStarted {
                window_id: WindowId(1),
            },
        );
        state.effects.clear();
        let mut second = observed_window(
            2,
            mosaix_domain::WindowRole::Normal,
            WindowLifecycle::Active,
        );
        second.bounds = Rect::new(100, 100, 800, 600);
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    observed_window(
                        1,
                        mosaix_domain::WindowRole::Normal,
                        WindowLifecycle::Active,
                    ),
                    second,
                ],
            },
        );
        assert!(state.effects.is_empty());

        apply(
            &mut state,
            Event::InteractivePlacementEnded {
                window_id: WindowId(1),
                committed_manual_placement: false,
            },
        );

        assert_eq!(state.effects.len(), 2);
        assert!(state.interactive_placement.is_none());
    }

    #[test]
    fn explicit_maximize_session_floats_until_the_user_toggles_it_back() {
        let mut state = EngineState {
            displays: vec![display(1, "primary", 0)],
            automatic_tiling_active: true,
            ..EngineState::default()
        };
        let active = observed_window(
            1,
            mosaix_domain::WindowRole::Normal,
            WindowLifecycle::Active,
        );
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![active.clone()],
            },
        );

        let mut maximized = active.clone();
        maximized.lifecycle = WindowLifecycle::Maximized;
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![maximized],
            },
        );
        assert!(state.session_floating.contains(&WindowId(1)));
        assert_eq!(
            state.inventory[&WindowId(1)].eligibility,
            EligibilityReason::SessionFloating
        );

        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![active],
            },
        );
        assert_eq!(
            state.inventory[&WindowId(1)].eligibility,
            EligibilityReason::SessionFloating,
            "restoring a maximized window must not silently return it to the grid"
        );
    }

    #[test]
    fn authoritative_observation_removes_closed_windows_and_session_state() {
        let mut state = EngineState {
            displays: vec![display(1, "primary", 0)],
            automatic_tiling_active: true,
            ..EngineState::default()
        };
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![observed_window(
                    1,
                    mosaix_domain::WindowRole::Normal,
                    WindowLifecycle::Active,
                )],
            },
        );
        state.focused_window = Some(WindowId(1));
        state.session_floating.insert(WindowId(1));

        apply(
            &mut state,
            Event::WindowsObserved {
                windows: Vec::new(),
            },
        );

        assert!(!state.inventory.contains_key(&WindowId(1)));
        assert!(!state.windows.contains_key(&WindowId(1)));
        assert!(!state.session_floating.contains(&WindowId(1)));
        assert_eq!(state.focused_window, None);
        assert!(state.visual_window_order[&DisplayId(1)].is_empty());
    }

    #[test]
    fn resuming_global_pause_reflows_the_current_active_set() {
        let mut state = EngineState {
            displays: vec![display(1, "primary", 0)],
            automatic_tiling_active: true,
            paused: true,
            ..EngineState::default()
        };
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![observed_window(
                    1,
                    mosaix_domain::WindowRole::Normal,
                    WindowLifecycle::Active,
                )],
            },
        );
        assert!(state.effects.is_empty());

        apply(&mut state, Event::ResumeRequested);

        assert_eq!(
            state.effects,
            vec![EngineEffect::PlaceWindow {
                window_id: WindowId(1),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 1920, 1080),
            }]
        );
    }

    #[test]
    fn diff_placements_excludes_open_circuit_windows() {
        // Windows whose circuit is open must not appear in the diff so the
        // executor never tries to `SetWindowPos` them again.
        let mut previous: HashMap<WindowId, WindowPlacement> = HashMap::new();
        let open_circuit = WindowPlacement {
            display_id: DisplayId(1),
            bounds: Rect::new(0, 0, 100, 100),
            observed_bounds: Rect::new(0, 0, 100, 100),
            previous_placement: None,
            cycle_step: None,
            rejection_count: CIRCUIT_BREAKER_THRESHOLD,
        };
        let healthy = WindowPlacement {
            display_id: DisplayId(1),
            bounds: Rect::new(100, 100, 200, 200),
            observed_bounds: Rect::new(100, 100, 200, 200),
            previous_placement: None,
            cycle_step: None,
            rejection_count: 0,
        };
        previous.insert(WindowId(1), open_circuit);
        previous.insert(WindowId(2), healthy);

        // In `current`, both windows moved.
        let mut current = previous.clone();
        current.get_mut(&WindowId(1)).unwrap().bounds = Rect::new(50, 50, 100, 100);
        current.get_mut(&WindowId(2)).unwrap().bounds = Rect::new(200, 200, 200, 200);

        let diff = diff_placements(&previous, &current);
        let ids: Vec<WindowId> = diff.iter().map(|(id, _, _)| *id).collect();

        assert!(
            !ids.contains(&WindowId(1)),
            "open-circuit window must not appear in the placement diff"
        );
        assert!(
            ids.contains(&WindowId(2)),
            "healthy window with changed bounds must appear in the diff"
        );
    }

    fn saved_layout(cells: &[(f64, f64, f64, f64)]) -> mosaix_config::SavedLayout {
        mosaix_config::SavedLayout {
            cells: cells
                .iter()
                .map(|(x, y, width, height)| mosaix_domain::NormalizedRect {
                    x: *x,
                    y: *y,
                    width: *width,
                    height: *height,
                })
                .collect(),
        }
    }

    /// One 1920x1080 display at the origin carrying a saved layout under
    /// `name`, with nothing observed and nothing focused yet.
    fn state_with_saved_layout(name: &str, cells: &[(f64, f64, f64, f64)]) -> EngineState {
        let mut layouts = std::collections::BTreeMap::new();
        layouts.insert(name.to_owned(), saved_layout(cells));
        EngineState {
            displays: vec![display(1, "primary", 0)],
            resolved_config: ResolvedConfig {
                layouts,
                ..ResolvedConfig::default()
            },
            ..EngineState::default()
        }
    }

    fn window_at(id: isize, display_id: isize, bounds: Rect) -> Window {
        let mut window = observed_window(
            id,
            mosaix_domain::WindowRole::Normal,
            WindowLifecycle::Active,
        );
        window.display_id = DisplayId(display_id);
        window.bounds = bounds;
        window
    }

    /// [`window_at`] belonging to a named application, for the cases where
    /// two windows have to be genuinely distinguishable.
    fn app_window_at(id: isize, application: &str, display_id: isize, bounds: Rect) -> Window {
        let mut window = window_at(id, display_id, bounds);
        window.application_id = mosaix_domain::ApplicationId(application.to_owned());
        window
    }

    fn placements(state: &EngineState) -> Vec<(WindowId, DisplayId, Rect)> {
        state
            .effects
            .iter()
            .filter_map(|effect| match effect {
                EngineEffect::PlaceWindow {
                    window_id,
                    display_id,
                    bounds,
                } => Some((*window_id, *display_id, *bounds)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn applying_a_saved_layout_places_the_displays_windows_into_its_cells() {
        let mut state =
            state_with_saved_layout("writing", &[(0.0, 0.0, 0.6, 1.0), (0.6, 0.0, 0.4, 1.0)]);
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    window_at(1, 1, Rect::new(0, 0, 400, 300)),
                    window_at(2, 1, Rect::new(900, 0, 400, 300)),
                ],
            },
        );
        state.focused_window = Some(WindowId(1));
        state.effects.clear();

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "writing".to_owned(),
            },
        );

        assert_eq!(
            placements(&state),
            vec![
                (WindowId(1), DisplayId(1), Rect::new(0, 0, 1152, 1080)),
                (WindowId(2), DisplayId(1), Rect::new(1152, 0, 768, 1080)),
            ]
        );
    }

    #[test]
    fn a_saved_layout_fills_its_cells_in_visual_window_order() {
        let mut state =
            state_with_saved_layout("stack", &[(0.0, 0.0, 1.0, 0.5), (0.0, 0.5, 1.0, 0.5)]);
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    // Observed first, but sitting lower on screen, so
                    // visual window order puts it in the second cell.
                    window_at(9, 1, Rect::new(0, 600, 400, 300)),
                    window_at(4, 1, Rect::new(0, 100, 400, 300)),
                ],
            },
        );
        state.focused_window = Some(WindowId(9));
        state.effects.clear();

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "stack".to_owned(),
            },
        );

        assert_eq!(
            placements(&state),
            vec![
                (WindowId(4), DisplayId(1), Rect::new(0, 0, 1920, 540)),
                (WindowId(9), DisplayId(1), Rect::new(0, 540, 1920, 540)),
            ]
        );
    }

    #[test]
    fn a_saved_layout_targets_the_focused_windows_display_and_leaves_the_others_alone() {
        let mut state = state_with_saved_layout("half", &[(0.0, 0.0, 0.5, 1.0)]);
        state.displays.push(display(2, "secondary", 1920));
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    window_at(1, 1, Rect::new(0, 0, 400, 300)),
                    window_at(2, 2, Rect::new(1920, 0, 400, 300)),
                ],
            },
        );
        state.focused_window = Some(WindowId(2));
        state.effects.clear();

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "half".to_owned(),
            },
        );

        assert_eq!(
            placements(&state),
            vec![(WindowId(2), DisplayId(2), Rect::new(1920, 0, 960, 1080))],
            "only the focused window's display is rearranged"
        );
    }

    #[test]
    fn applying_a_saved_layout_with_no_focused_display_is_rejected_and_places_nothing() {
        let mut state = state_with_saved_layout("half", &[(0.0, 0.0, 0.5, 1.0)]);
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![window_at(1, 1, Rect::new(0, 0, 400, 300))],
            },
        );
        state.effects.clear();
        let revision = state.revision;

        assert_eq!(
            plan_saved_layout(&state, "half"),
            Err(SavedLayoutRejection::NoFocusedDisplay)
        );

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "half".to_owned(),
            },
        );

        assert!(placements(&state).is_empty());
        assert_eq!(
            state.revision, revision,
            "a rejected command commits nothing"
        );
    }

    #[test]
    fn unmanaged_focus_without_a_display_target_is_not_a_layout_target() {
        let mut state = state_with_saved_layout("half", &[(0.0, 0.0, 0.5, 1.0)]);
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![window_at(1, 1, Rect::new(0, 0, 400, 300))],
            },
        );
        // An excluded window -- a popup, say -- can hold OS focus without
        // ever entering the managed inventory.
        state.focused_window = Some(WindowId(77));

        assert_eq!(
            plan_saved_layout(&state, "half"),
            Err(SavedLayoutRejection::NoFocusedDisplay)
        );
    }

    #[test]
    fn applying_an_unknown_saved_layout_is_rejected_naming_it() {
        let mut state = state_with_saved_layout("writing", &[(0.0, 0.0, 1.0, 1.0)]);
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![window_at(1, 1, Rect::new(0, 0, 400, 300))],
            },
        );
        state.focused_window = Some(WindowId(1));
        state.effects.clear();

        let rejection = plan_saved_layout(&state, "wrtiing").unwrap_err();

        assert_eq!(
            rejection,
            SavedLayoutRejection::UnknownLayout {
                name: "wrtiing".to_owned()
            }
        );
        assert!(rejection.to_string().contains("wrtiing"));

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "wrtiing".to_owned(),
            },
        );
        assert!(placements(&state).is_empty());
    }

    #[test]
    fn a_saved_layout_with_more_windows_than_cells_places_only_what_fits() {
        let mut state = state_with_saved_layout("single", &[(0.0, 0.0, 1.0, 1.0)]);
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    window_at(1, 1, Rect::new(0, 0, 400, 300)),
                    window_at(2, 1, Rect::new(0, 500, 400, 300)),
                ],
            },
        );
        state.focused_window = Some(WindowId(1));
        state.effects.clear();

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "single".to_owned(),
            },
        );

        assert_eq!(
            placements(&state),
            vec![(WindowId(1), DisplayId(1), Rect::new(0, 0, 1920, 1080))]
        );
    }

    #[test]
    fn a_saved_layout_with_more_cells_than_windows_leaves_the_surplus_empty() {
        let mut state = state_with_saved_layout(
            "three-up",
            &[
                (0.0, 0.0, 0.34, 1.0),
                (0.34, 0.0, 0.33, 1.0),
                (0.67, 0.0, 0.33, 1.0),
            ],
        );
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![window_at(1, 1, Rect::new(0, 0, 400, 300))],
            },
        );
        state.focused_window = Some(WindowId(1));
        state.effects.clear();

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "three-up".to_owned(),
            },
        );

        assert_eq!(placements(&state).len(), 1);
    }

    #[test]
    fn a_minimized_window_is_not_given_a_cell() {
        let mut state =
            state_with_saved_layout("pair", &[(0.0, 0.0, 0.5, 1.0), (0.5, 0.0, 0.5, 1.0)]);
        let mut minimized = window_at(2, 1, Rect::new(0, 500, 400, 300));
        minimized.lifecycle = WindowLifecycle::Minimized;
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![window_at(1, 1, Rect::new(0, 0, 400, 300)), minimized],
            },
        );
        state.focused_window = Some(WindowId(1));
        state.effects.clear();

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "pair".to_owned(),
            },
        );

        assert_eq!(
            placements(&state),
            vec![(WindowId(1), DisplayId(1), Rect::new(0, 0, 960, 1080))]
        );
    }

    #[test]
    fn applying_a_saved_layout_while_paused_is_rejected() {
        let mut state = state_with_saved_layout("half", &[(0.0, 0.0, 0.5, 1.0)]);
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![window_at(1, 1, Rect::new(0, 0, 400, 300))],
            },
        );
        state.focused_window = Some(WindowId(1));
        apply(&mut state, Event::PauseRequested);
        state.effects.clear();

        assert_eq!(
            plan_saved_layout(&state, "half"),
            Err(SavedLayoutRejection::Paused)
        );

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "half".to_owned(),
            },
        );
        assert!(placements(&state).is_empty());
    }

    /// [`state_with_saved_layout`] plus gaps, for the gap post-processing
    /// step.
    fn state_with_saved_layout_and_gaps(
        name: &str,
        cells: &[(f64, f64, f64, f64)],
        gaps: mosaix_domain::Gaps,
    ) -> EngineState {
        let mut state = state_with_saved_layout(name, cells);
        state.resolved_config.gaps = gaps;
        state
    }

    /// [`state_with_saved_layout`] with automatic tiling running, so an
    /// applied layout has an active tiling set to be pulled out of.
    fn tiling_state_with_saved_layout(name: &str, cells: &[(f64, f64, f64, f64)]) -> EngineState {
        let mut state = state_with_saved_layout(name, cells);
        state.resolved_config.automatic_tiling_enabled = true;
        state.automatic_tiling_active = true;
        state
    }

    #[test]
    fn gaps_inset_a_restored_layouts_rectangles_the_same_way_they_inset_the_grid() {
        let mut state = state_with_saved_layout_and_gaps(
            "writing",
            &[(0.0, 0.0, 0.5, 1.0), (0.5, 0.0, 0.5, 1.0)],
            mosaix_domain::Gaps::new(10, 4),
        );
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    window_at(1, 1, Rect::new(0, 0, 400, 300)),
                    window_at(2, 1, Rect::new(900, 0, 400, 300)),
                ],
            },
        );
        state.focused_window = Some(WindowId(1));
        state.effects.clear();

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "writing".to_owned(),
            },
        );

        // Outer gap on the three work-area edges each cell touches, inner
        // gap on the seam between them -- byte for byte what `apply_gaps`
        // does to a two-cell balanced grid on this display.
        let work_area = state.displays[0].work_area;
        let expected: Vec<Rect> = plan_balanced_grid(work_area, 2)
            .into_iter()
            .map(|raw| apply_gaps(raw, work_area, mosaix_domain::Gaps::new(10, 4)))
            .collect();
        assert_eq!(
            placements(&state),
            vec![
                (WindowId(1), DisplayId(1), expected[0]),
                (WindowId(2), DisplayId(1), expected[1]),
            ]
        );
    }

    #[test]
    fn the_cells_to_rectangles_function_itself_stays_gap_unaware() {
        let work_area = Rect::new(0, 0, 1920, 1080);
        let cells = [mosaix_domain::NormalizedRect {
            x: 0.0,
            y: 0.0,
            width: 0.5,
            height: 1.0,
        }];

        // Same input, and gaps live in `resolved_config`, nowhere this
        // call can see -- so the ungapped rectangle is the only thing it
        // can produce.
        assert_eq!(
            resolve_saved_layout(work_area, &cells),
            vec![Rect::new(0, 0, 960, 1080)]
        );
    }

    #[test]
    fn applying_a_layout_under_automatic_tiling_session_floats_what_it_placed() {
        let mut state = tiling_state_with_saved_layout(
            "writing",
            &[(0.0, 0.0, 0.6, 1.0), (0.6, 0.0, 0.4, 1.0)],
        );
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    window_at(1, 1, Rect::new(0, 0, 400, 300)),
                    window_at(2, 1, Rect::new(900, 0, 400, 300)),
                ],
            },
        );
        state.focused_window = Some(WindowId(1));
        state.effects.clear();

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "writing".to_owned(),
            },
        );

        assert!(state.session_floating.contains(&WindowId(1)));
        assert!(state.session_floating.contains(&WindowId(2)));
        assert_eq!(
            state.inventory[&WindowId(1)].eligibility,
            EligibilityReason::SessionFloating
        );
        assert_eq!(
            placements(&state),
            vec![
                (WindowId(1), DisplayId(1), Rect::new(0, 0, 1152, 1080)),
                (WindowId(2), DisplayId(1), Rect::new(1152, 0, 768, 1080)),
            ],
            "the reflow that follows must not overwrite the cells just committed"
        );
    }

    #[test]
    fn the_remaining_tiling_set_reflows_around_a_layout_in_one_pass() {
        // A one-cell layout on a three-window display: window 1 takes the
        // cell and floats, and 2 and 3 -- still tiled -- must end up
        // sharing the whole work area as a two-window grid.
        let mut state = tiling_state_with_saved_layout("solo", &[(0.0, 0.0, 0.5, 1.0)]);
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    window_at(1, 1, Rect::new(0, 0, 400, 300)),
                    window_at(2, 1, Rect::new(0, 400, 400, 300)),
                    window_at(3, 1, Rect::new(0, 800, 400, 300)),
                ],
            },
        );
        state.focused_window = Some(WindowId(1));
        state.effects.clear();

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "solo".to_owned(),
            },
        );

        let committed = placements(&state);
        assert_eq!(
            committed
                .iter()
                .filter(|(window_id, _, _)| *window_id == WindowId(2))
                .count(),
            1,
            "the reflow around the layout must be a single pass, got {committed:?}"
        );
        let grid = plan_balanced_grid(state.displays[0].work_area, 2);
        assert_eq!(
            state.windows[&WindowId(2)].bounds,
            grid[0],
            "the still-tiled windows reflow as a two-window grid"
        );
        assert_eq!(state.windows[&WindowId(3)].bounds, grid[1]);
        assert_eq!(
            state.windows[&WindowId(1)].bounds,
            Rect::new(0, 0, 960, 1080),
            "the window the layout placed keeps its cell"
        );
    }

    #[test]
    fn toggle_floating_returns_a_layout_placed_window_to_the_tiling_set() {
        let mut state = tiling_state_with_saved_layout("solo", &[(0.0, 0.0, 0.5, 1.0)]);
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    window_at(1, 1, Rect::new(0, 0, 400, 300)),
                    window_at(2, 1, Rect::new(0, 400, 400, 300)),
                ],
            },
        );
        state.focused_window = Some(WindowId(1));
        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "solo".to_owned(),
            },
        );
        assert!(state.session_floating.contains(&WindowId(1)));
        state.effects.clear();

        apply(&mut state, Event::ToggleFloatingRequested);

        assert!(!state.session_floating.contains(&WindowId(1)));
        assert_eq!(
            state.inventory[&WindowId(1)].eligibility,
            EligibilityReason::Eligible
        );
        let grid = plan_balanced_grid(state.displays[0].work_area, 2);
        assert_eq!(state.windows[&WindowId(1)].bounds, grid[0]);
        assert_eq!(state.windows[&WindowId(2)].bounds, grid[1]);
    }

    #[test]
    fn applying_a_layout_with_tiling_off_floats_nothing() {
        let mut state = state_with_saved_layout("solo", &[(0.0, 0.0, 0.5, 1.0)]);
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![window_at(1, 1, Rect::new(0, 0, 400, 300))],
            },
        );
        state.focused_window = Some(WindowId(1));

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "solo".to_owned(),
            },
        );

        assert!(
            state.session_floating.is_empty(),
            "with no grid to be pulled out of, there is nothing to float"
        );
    }

    #[test]
    fn more_cells_than_windows_reports_nothing_unplaced_and_succeeds() {
        let mut state = state_with_saved_layout(
            "three-up",
            &[
                (0.0, 0.0, 0.34, 1.0),
                (0.34, 0.0, 0.33, 1.0),
                (0.67, 0.0, 0.33, 1.0),
            ],
        );
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![window_at(1, 1, Rect::new(0, 0, 400, 300))],
            },
        );
        state.focused_window = Some(WindowId(1));

        let plan =
            plan_saved_layout(&state, "three-up").expect("a surplus of cells is not a failure");

        assert_eq!(plan.placements.len(), 1);
        assert_eq!(plan.unplaced, 0, "empty cells are not unplaced windows");
    }

    #[test]
    fn more_windows_than_cells_reports_how_many_it_could_not_place() {
        let mut state = state_with_saved_layout("single", &[(0.0, 0.0, 1.0, 1.0)]);
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    window_at(1, 1, Rect::new(0, 0, 400, 300)),
                    window_at(2, 1, Rect::new(0, 400, 400, 300)),
                    window_at(3, 1, Rect::new(0, 800, 400, 300)),
                ],
            },
        );
        state.focused_window = Some(WindowId(1));
        state.effects.clear();

        let plan =
            plan_saved_layout(&state, "single").expect("a surplus of windows is not a failure");
        assert_eq!(plan.placements.len(), 1);
        assert_eq!(plan.unplaced, 2);

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "single".to_owned(),
            },
        );

        // The two it could not place are left exactly where they were,
        // not dropped, shrunk, or stacked somewhere.
        assert_eq!(placements(&state).len(), 1);
        assert_eq!(
            state.inventory[&WindowId(2)].window.bounds,
            Rect::new(0, 400, 400, 300)
        );
        assert_eq!(
            state.inventory[&WindowId(3)].window.bounds,
            Rect::new(0, 800, 400, 300)
        );
    }

    #[test]
    fn an_open_circuit_breaker_suppresses_a_layout_placement_without_floating_the_window() {
        let mut state =
            tiling_state_with_saved_layout("pair", &[(0.0, 0.0, 0.5, 1.0), (0.5, 0.0, 0.5, 1.0)]);
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    window_at(1, 1, Rect::new(0, 0, 400, 300)),
                    window_at(2, 1, Rect::new(0, 400, 400, 300)),
                ],
            },
        );
        state.focused_window = Some(WindowId(1));
        state.windows.get_mut(&WindowId(2)).unwrap().rejection_count = CIRCUIT_BREAKER_THRESHOLD;
        state.effects.clear();

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "pair".to_owned(),
            },
        );

        let committed = placements(&state);
        assert!(
            committed
                .iter()
                .all(|(window_id, _, _)| *window_id != WindowId(2)),
            "an open circuit still suppresses the placement, got {committed:?}"
        );
        assert!(
            !state.session_floating.contains(&WindowId(2)),
            "a window that was never moved must not be pulled out of the tiling set"
        );
        assert!(state.session_floating.contains(&WindowId(1)));
    }

    #[test]
    fn a_saved_layout_is_rejected_when_the_focused_windows_display_has_gone() {
        let mut state = state_with_saved_layout("half", &[(0.0, 0.0, 0.5, 1.0)]);
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![window_at(1, 1, Rect::new(0, 0, 400, 300))],
            },
        );
        state.focused_window = Some(WindowId(1));
        state.displays.clear();

        assert_eq!(
            plan_saved_layout(&state, "half"),
            Err(SavedLayoutRejection::DisplayUnavailable {
                display_id: DisplayId(1)
            })
        );
    }

    #[test]
    fn hotkey_capture_start_suspends_registration_and_end_restores_it() {
        let mut state = EngineState::default();

        apply(&mut state, Event::HotkeyCaptureStarted);
        assert!(state.hotkey_capture_suspended);
        assert_eq!(state.revision, 1);

        apply(&mut state, Event::HotkeyCaptureEnded);
        assert!(!state.hotkey_capture_suspended);
        assert_eq!(state.revision, 2);
    }

    #[test]
    fn a_second_editor_does_not_re_suspend_what_is_already_suspended() {
        let mut state = EngineState::default();
        apply(&mut state, Event::HotkeyCaptureStarted);

        apply(&mut state, Event::HotkeyCaptureStarted);

        assert!(state.hotkey_capture_suspended);
        assert_eq!(
            state.revision, 1,
            "a second editor window is not a second suspension"
        );
    }

    #[test]
    fn suspension_lasts_until_the_editor_that_closes_last() {
        let mut state = EngineState::default();
        apply(&mut state, Event::HotkeyCaptureStarted);
        apply(&mut state, Event::HotkeyCaptureStarted);

        apply(&mut state, Event::HotkeyCaptureEnded);

        assert!(
            state.hotkey_capture_suspended,
            "one editor closing must not re-register under another whose capture dialog is armed"
        );

        apply(&mut state, Event::HotkeyCaptureEnded);

        assert!(!state.hotkey_capture_suspended);
    }

    #[test]
    fn a_stray_capture_end_cannot_drive_the_hold_count_below_zero() {
        let mut state = EngineState::default();
        apply(&mut state, Event::HotkeyCaptureEnded);
        apply(&mut state, Event::HotkeyCaptureStarted);

        apply(&mut state, Event::HotkeyCaptureEnded);

        assert!(
            !state.hotkey_capture_suspended,
            "the one editor that opened has closed, so nothing holds suspension"
        );
    }

    #[test]
    fn capture_end_without_a_capture_changes_nothing() {
        let mut state = EngineState::default();

        apply(&mut state, Event::HotkeyCaptureEnded);

        assert!(!state.hotkey_capture_suspended);
        assert_eq!(state.revision, 0);
    }

    #[test]
    fn a_config_reload_during_capture_leaves_registration_suspended() {
        let mut state = EngineState::default();
        apply(&mut state, Event::HotkeyCaptureStarted);

        apply(&mut state, Event::ConfigChanged(Box::default()));

        assert!(
            state.hotkey_capture_suspended,
            "a configuration change must not lift the editor's suspension"
        );
    }

    #[test]
    fn a_registration_pass_records_the_bindings_that_did_not_come_back() {
        let mut state = EngineState::default();

        apply(
            &mut state,
            Event::HotkeyRegistrationReported {
                unregistered: vec![Command::SnapLeft],
            },
        );

        assert_eq!(state.unregistered_bindings, vec![Command::SnapLeft]);
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn opening_the_editor_keeps_the_previous_passs_failures_to_report() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::HotkeyRegistrationReported {
                unregistered: vec![Command::SnapLeft],
            },
        );

        apply(&mut state, Event::HotkeyCaptureStarted);

        assert_eq!(
            state.unregistered_bindings,
            vec![Command::SnapLeft],
            "a binding that did not come back is only known after the editor that \
             caused it has closed, so the next editor to open is the only one that \
             can tell the user"
        );
    }

    #[test]
    fn an_unchanged_registration_report_does_not_bump_the_revision() {
        let mut state = EngineState::default();
        apply(
            &mut state,
            Event::HotkeyRegistrationReported {
                unregistered: vec![Command::SnapLeft],
            },
        );
        let revision = state.revision;

        apply(
            &mut state,
            Event::HotkeyRegistrationReported {
                unregistered: vec![Command::SnapLeft],
            },
        );

        assert_eq!(state.revision, revision);
    }

    #[test]
    fn applying_a_layout_records_it_as_that_displays_current_arrangement() {
        let mut state = state_with_saved_layout("half", &[(0.0, 0.0, 0.5, 1.0)]);
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![window_at(1, 1, Rect::new(0, 0, 400, 300))],
            },
        );
        state.focused_window = Some(WindowId(1));

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "half".to_owned(),
            },
        );

        assert_eq!(
            state.last_applied_layouts.get(&DisplayId(1)),
            Some(&"half".to_owned()),
            "machine-readable state has to say what a display is currently arranged as"
        );
    }

    #[test]
    fn a_rejected_layout_apply_records_nothing() {
        let mut state = state_with_saved_layout("half", &[(0.0, 0.0, 0.5, 1.0)]);
        // Nothing focused, so the apply is rejected with a reason and
        // changes nothing.

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "half".to_owned(),
            },
        );

        assert!(
            state.last_applied_layouts.is_empty(),
            "an apply that placed nothing must not claim a display is arranged as it"
        );
    }

    #[test]
    fn a_display_that_leaves_the_topology_takes_its_recorded_layout_with_it() {
        let mut state = state_with_saved_layout("half", &[(0.0, 0.0, 0.5, 1.0)]);
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![window_at(1, 1, Rect::new(0, 0, 400, 300))],
            },
        );
        state.focused_window = Some(WindowId(1));
        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "half".to_owned(),
            },
        );
        assert!(!state.last_applied_layouts.is_empty());

        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(2, "MON-B", 1920)]),
        );

        assert!(
            state.last_applied_layouts.is_empty(),
            "a display nobody can see has no current arrangement to report"
        );
    }
    #[test]
    fn focused_display_survives_desktop_focus_and_moves_when_its_display_disconnects() {
        let mut state = state_with_saved_layout("unused", &[]);
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![
                display(1, "left", -1920),
                display(2, "primary", 0),
            ]),
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(10),
                display_id: DisplayId(1),
                bounds: Rect::new(-1800, 0, 800, 600),
            },
        );
        assert_eq!(state.focused_display, Some(DisplayId(1)));

        apply(&mut state, Event::DesktopFocused);
        assert_eq!(state.focused_window, None);
        assert_eq!(state.focused_display, Some(DisplayId(1)));

        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(2, "primary", 0)]),
        );
        assert_eq!(state.focused_display, Some(DisplayId(2)));
    }

    // ---- Persistent undo ----------------------------------------------

    /// A state with one display, one managed window, and that window
    /// focused -- the smallest situation in which an explicit command has
    /// something to move and therefore something to record.
    fn state_ready_to_snap() -> EngineState {
        let mut state = EngineState {
            displays: vec![display(1, "DISPLAY1", 0)],
            ..EngineState::default()
        };
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![window_at(7, 1, Rect::new(10, 10, 500, 500))],
            },
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(7),
                display_id: DisplayId(1),
                bounds: Rect::new(10, 10, 500, 500),
            },
        );
        state.effects.clear();
        state.persistence_intents.clear();
        state
    }

    fn recorded_drafts(state: &EngineState) -> Vec<&UndoTransactionDraft> {
        state
            .persistence_intents
            .iter()
            .filter_map(|intent| match intent {
                PersistenceIntent::RecordUndoTransaction(draft) => Some(draft),
                PersistenceIntent::ConsumeUndoTransaction(_)
                | PersistenceIntent::SaveContainerTree { .. }
                | PersistenceIntent::SaveWorkspace(_)
                | PersistenceIntent::DeleteWorkspace(_)
                | PersistenceIntent::RecordRecovery { .. }
                | PersistenceIntent::MarkParked(_)
                | PersistenceIntent::MarkRestored(_) => None,
            })
            .collect()
    }

    /// Turns the draft an explicit command produced into the stored
    /// transaction the agent would publish back, which is what undo reads.
    fn stored(draft: &UndoTransactionDraft, id: i64) -> UndoTransaction {
        UndoTransaction {
            id: UndoTransactionId(id),
            command: draft.command.clone(),
            recorded_at_unix: draft.recorded_at_unix,
            topology_fingerprint: draft.topology_fingerprint.clone(),
            durable_revision: draft.durable_revision,
            members: draft.members.clone(),
            prior_trees: draft.prior_trees.clone(),
            prior_assignments: draft.prior_assignments.clone(),
        }
    }

    #[test]
    fn an_explicit_snap_records_one_undo_transaction_with_the_prior_placement() {
        let mut state = state_ready_to_snap();

        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );

        let drafts = recorded_drafts(&state);
        assert_eq!(drafts.len(), 1, "one command records one transaction");
        let draft = drafts[0];
        assert_eq!(draft.command, "snap-left");
        assert_eq!(
            draft.topology_fingerprint,
            mosaix_domain::topology_fingerprint(&state.displays)
        );
        assert_eq!(draft.members.len(), 1);
        let member = &draft.members[0];
        assert_eq!(
            member.prior_placement,
            Rect::new(10, 10, 500, 500),
            "undo restores where the window was before the command"
        );
        assert_eq!(member.prior_display_fingerprint, "DISPLAY1");
        assert_eq!(
            member.evidence.last_placement,
            Rect::new(0, 0, 960, 1080),
            "evidence describes where the window is now, which is where a \
             later session has to find it"
        );
    }

    #[test]
    fn recorded_evidence_never_contains_the_window_title() {
        let mut state = state_ready_to_snap();
        state
            .inventory
            .get_mut(&WindowId(7))
            .expect("the window is managed")
            .window
            .title = "Quarterly salary review.xlsx".to_owned();

        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );

        let recorded = format!("{:?}", recorded_drafts(&state));
        assert!(
            !recorded.contains("Quarterly salary review"),
            "a document name must never reach a durable record: {recorded}"
        );
    }

    #[test]
    fn passive_observation_records_no_undo_transaction() {
        let mut state = state_ready_to_snap();

        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![window_at(7, 1, Rect::new(300, 300, 500, 500))],
            },
        );
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "DISPLAY1", 0)]),
        );

        assert!(
            recorded_drafts(&state).is_empty(),
            "undo must never offer to reverse something the user did not do"
        );
    }

    #[test]
    fn undo_restores_the_prior_placement_and_consumes_the_transaction() {
        let mut state = state_ready_to_snap();
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        let transaction = stored(recorded_drafts(&state)[0], 1);
        state.persistence_intents.clear();
        state.effects.clear();
        apply(
            &mut state,
            Event::UndoHistoryLoaded(Some(Box::new(transaction))),
        );

        apply(&mut state, Event::UndoRequested);

        assert_eq!(
            placements(&state),
            vec![(WindowId(7), DisplayId(1), Rect::new(10, 10, 500, 500))],
            "undo puts the window back where the command found it"
        );
        assert_eq!(
            state.persistence_intents,
            vec![PersistenceIntent::ConsumeUndoTransaction(
                UndoTransactionId(1)
            )],
            "a successful undo consumes its transaction"
        );
        assert!(state.newest_undo.is_none());
        assert!(matches!(
            state.last_undo_result,
            Some(UndoResult::Applied(_))
        ));
    }

    #[test]
    fn undo_records_no_transaction_of_its_own() {
        let mut state = state_ready_to_snap();
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        let transaction = stored(recorded_drafts(&state)[0], 1);
        apply(
            &mut state,
            Event::UndoHistoryLoaded(Some(Box::new(transaction))),
        );
        state.persistence_intents.clear();

        apply(&mut state, Event::UndoRequested);

        assert!(
            recorded_drafts(&state).is_empty(),
            "undo must not become undoable"
        );
    }

    #[test]
    fn undo_refuses_and_keeps_the_transaction_when_the_topology_changed() {
        let mut state = state_ready_to_snap();
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        let transaction = stored(recorded_drafts(&state)[0], 1);
        apply(
            &mut state,
            Event::UndoHistoryLoaded(Some(Box::new(transaction.clone()))),
        );
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![
                display(1, "DISPLAY1", 0),
                display(2, "DISPLAY2", 1920),
            ]),
        );
        state.effects.clear();
        state.persistence_intents.clear();

        apply(&mut state, Event::UndoRequested);

        assert!(
            placements(&state).is_empty(),
            "a refusal must happen before any window moves"
        );
        assert!(state.persistence_intents.is_empty());
        assert_eq!(
            state.newest_undo,
            Some(transaction),
            "a refused transaction is kept for retry"
        );
        let Some(UndoResult::Refused(refusal)) = &state.last_undo_result else {
            panic!("expected a refusal, got {:?}", state.last_undo_result);
        };
        assert_eq!(refusal.code(), "topology_changed");
        assert_eq!(refusal.transaction_id(), Some(UndoTransactionId(1)));
    }

    #[test]
    fn undo_refuses_when_the_target_window_is_gone() {
        let mut state = state_ready_to_snap();
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        let transaction = stored(recorded_drafts(&state)[0], 1);
        apply(
            &mut state,
            Event::UndoHistoryLoaded(Some(Box::new(transaction.clone()))),
        );
        apply(&mut state, Event::WindowsObserved { windows: vec![] });
        state.effects.clear();
        state.persistence_intents.clear();

        apply(&mut state, Event::UndoRequested);

        assert!(placements(&state).is_empty());
        assert_eq!(state.newest_undo, Some(transaction));
        let Some(UndoResult::Refused(refusal)) = &state.last_undo_result else {
            panic!("expected a refusal");
        };
        assert_eq!(refusal.code(), "targets_unresolved");
    }

    #[test]
    fn undo_refuses_rather_than_guessing_between_two_identical_windows() {
        let mut state = state_ready_to_snap();
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        let transaction = stored(recorded_drafts(&state)[0], 1);
        apply(
            &mut state,
            Event::UndoHistoryLoaded(Some(Box::new(transaction.clone()))),
        );
        // A second window of the same application, in the same place, with
        // the same everything. The snapped window is now indistinguishable
        // from its twin.
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    window_at(7, 1, Rect::new(0, 0, 960, 1080)),
                    window_at(8, 1, Rect::new(0, 0, 960, 1080)),
                ],
            },
        );
        state.effects.clear();
        state.persistence_intents.clear();

        apply(&mut state, Event::UndoRequested);

        assert!(
            placements(&state).is_empty(),
            "Mosaix must never move the wrong window"
        );
        assert_eq!(state.newest_undo, Some(transaction));
        let Some(UndoResult::Refused(UndoRefusal::TargetsUnresolved { targets, .. })) =
            &state.last_undo_result
        else {
            panic!(
                "expected an unresolved refusal, got {:?}",
                state.last_undo_result
            );
        };
        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[0].outcome.code(),
            "ambiguous",
            "the refusal must say it was a tie, not that the window vanished"
        );
    }

    #[test]
    fn undo_refuses_while_the_state_database_is_degraded() {
        let mut state = state_ready_to_snap();
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        let transaction = stored(recorded_drafts(&state)[0], 1);
        apply(
            &mut state,
            Event::UndoHistoryLoaded(Some(Box::new(transaction.clone()))),
        );
        apply(
            &mut state,
            Event::PersistenceHealthChanged(PersistenceHealth::Degraded {
                last_durable_revision: 4,
                reason: mosaix_persistence::PersistenceFailure::WriteFailed,
            }),
        );
        state.effects.clear();
        state.persistence_intents.clear();

        apply(&mut state, Event::UndoRequested);

        assert!(
            placements(&state).is_empty(),
            "undo that cannot be consumed could be applied twice"
        );
        assert_eq!(state.newest_undo, Some(transaction));
        let Some(UndoResult::Refused(refusal)) = &state.last_undo_result else {
            panic!("expected a refusal");
        };
        assert_eq!(refusal.code(), "persistence_degraded");
    }

    #[test]
    fn undo_with_empty_history_refuses_without_naming_a_transaction() {
        let mut state = state_ready_to_snap();

        apply(&mut state, Event::UndoRequested);

        assert!(placements(&state).is_empty());
        assert_eq!(
            state.last_undo_result,
            Some(UndoResult::Refused(UndoRefusal::NothingToUndo))
        );
    }

    #[test]
    fn there_is_no_way_to_force_a_refused_undo() {
        // The absence of an override is a contract, so it is asserted
        // rather than left to review: every refusal reached through the
        // public planner leaves the transaction and moves nothing.
        let mut state = state_ready_to_snap();
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        let transaction = stored(recorded_drafts(&state)[0], 1);
        apply(
            &mut state,
            Event::UndoHistoryLoaded(Some(Box::new(transaction.clone()))),
        );
        apply(&mut state, Event::WindowsObserved { windows: vec![] });

        for _ in 0..5 {
            state.effects.clear();
            apply(&mut state, Event::UndoRequested);
            assert!(
                placements(&state).is_empty(),
                "repeating a refused undo must not wear it down into a guess"
            );
            assert_eq!(state.newest_undo, Some(transaction.clone()));
        }
    }

    // ---- Command-level atomic undo ------------------------------------

    /// Two managed windows on one display with a two-cell layout, so an
    /// explicit command has more than one window to move.
    fn state_ready_for_a_two_window_command() -> EngineState {
        let mut state =
            state_with_saved_layout("halves", &[(0.0, 0.0, 0.5, 1.0), (0.5, 0.0, 0.5, 1.0)]);
        state.displays = vec![display(1, "DISPLAY1", 0)];
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    window_at(11, 1, Rect::new(10, 10, 400, 300)),
                    window_at(12, 1, Rect::new(500, 400, 400, 300)),
                ],
            },
        );
        apply(
            &mut state,
            Event::FocusDisplayRequested {
                display_id: DisplayId(1),
            },
        );
        state.effects.clear();
        state.persistence_intents.clear();
        state
    }

    #[test]
    fn one_multi_window_command_records_one_transaction_covering_every_window() {
        let mut state = state_ready_for_a_two_window_command();

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "halves".to_owned(),
            },
        );

        let drafts = recorded_drafts(&state);
        assert_eq!(
            drafts.len(),
            1,
            "two windows moved by one command is still one transaction"
        );
        let mut priors: Vec<Rect> = drafts[0]
            .members
            .iter()
            .map(|member| member.prior_placement)
            .collect();
        priors.sort_by_key(|rect| (rect.x, rect.y));
        assert_eq!(
            priors,
            vec![Rect::new(10, 10, 400, 300), Rect::new(500, 400, 400, 300)],
            "every window's own prior placement is recorded, not just the first"
        );
        let ordinals: Vec<u32> = drafts[0]
            .members
            .iter()
            .map(|member| member.ordinal)
            .collect();
        assert_eq!(ordinals, vec![0, 1], "members are ordinal-addressable");
    }

    #[test]
    fn undoing_a_multi_window_command_restores_every_window_at_once() {
        let mut state = state_ready_for_a_two_window_command();
        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "halves".to_owned(),
            },
        );
        let transaction = stored(recorded_drafts(&state)[0], 5);
        state.persistence_intents.clear();
        state.effects.clear();
        apply(
            &mut state,
            Event::UndoHistoryLoaded(Some(Box::new(transaction))),
        );

        apply(&mut state, Event::UndoRequested);

        let mut restored: Vec<(WindowId, Rect)> = placements(&state)
            .into_iter()
            .map(|(window_id, _, bounds)| (window_id, bounds))
            .collect();
        restored.sort_by_key(|(window_id, _)| window_id.0);
        assert_eq!(
            restored,
            vec![
                (WindowId(11), Rect::new(10, 10, 400, 300)),
                (WindowId(12), Rect::new(500, 400, 400, 300)),
            ],
            "one undo reverses the whole command, not one window of it"
        );
        assert_eq!(
            state.persistence_intents,
            vec![PersistenceIntent::ConsumeUndoTransaction(
                UndoTransactionId(5)
            )],
            "consumption is committed in the same transition as the placements"
        );
    }

    #[test]
    fn one_unresolvable_member_stops_the_whole_multi_window_undo() {
        let mut state = state_ready_for_a_two_window_command();
        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "halves".to_owned(),
            },
        );
        let transaction = stored(recorded_drafts(&state)[0], 5);
        apply(
            &mut state,
            Event::UndoHistoryLoaded(Some(Box::new(transaction.clone()))),
        );
        // One of the two windows has closed. The other is still perfectly
        // identifiable -- and must still not move.
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![window_at(11, 1, Rect::new(0, 0, 960, 1080))],
            },
        );
        state.effects.clear();
        state.persistence_intents.clear();

        apply(&mut state, Event::UndoRequested);

        assert!(
            placements(&state).is_empty(),
            "a partial undo would leave a half-restored layout, so none of it runs"
        );
        assert!(
            state.persistence_intents.is_empty(),
            "a refused transaction is not consumed"
        );
        assert_eq!(state.newest_undo, Some(transaction));
        let Some(UndoResult::Refused(UndoRefusal::TargetsUnresolved { targets, .. })) =
            &state.last_undo_result
        else {
            panic!(
                "expected an unresolved refusal, got {:?}",
                state.last_undo_result
            );
        };
        assert_eq!(
            targets.len(),
            2,
            "both members are reported, not only the failing one"
        );
        assert_eq!(
            targets.iter().filter(|target| target.is_resolved()).count(),
            1,
            "the report distinguishes the member that was found from the one that was not"
        );
    }

    #[test]
    fn a_refused_newest_transaction_is_never_skipped_for_an_older_one() {
        // Two different applications, so "the window is gone" is a fact the
        // matcher can actually establish rather than a tie it has to break.
        let mut state =
            state_with_saved_layout("halves", &[(0.0, 0.0, 0.5, 1.0), (0.5, 0.0, 0.5, 1.0)]);
        state.displays = vec![display(1, "DISPLAY1", 0)];
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    app_window_at(11, "alpha.exe", 1, Rect::new(10, 10, 400, 300)),
                    app_window_at(12, "beta.exe", 1, Rect::new(500, 400, 400, 300)),
                ],
            },
        );
        apply(
            &mut state,
            Event::FocusDisplayRequested {
                display_id: DisplayId(1),
            },
        );
        state.persistence_intents.clear();

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "halves".to_owned(),
            },
        );
        let older = stored(recorded_drafts(&state)[0], 1);
        state.persistence_intents.clear();
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(11),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 960, 1080),
            },
        );
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Right,
            },
        );
        let newest = stored(recorded_drafts(&state)[0], 2);
        assert_ne!(older.id, newest.id);

        // The newest transaction's target has closed; the older one's
        // targets are both still present and identifiable.
        apply(
            &mut state,
            Event::UndoHistoryLoaded(Some(Box::new(newest.clone()))),
        );
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![app_window_at(
                    12,
                    "beta.exe",
                    1,
                    Rect::new(960, 0, 960, 1080),
                )],
            },
        );
        state.effects.clear();
        state.persistence_intents.clear();

        apply(&mut state, Event::UndoRequested);

        assert!(
            placements(&state).is_empty(),
            "undo must not reach past an unavailable newest transaction"
        );
        assert_eq!(
            state.newest_undo,
            Some(newest),
            "the newest transaction stays newest; it is not discarded to reach the older one"
        );
        assert_eq!(
            state
                .last_undo_result
                .as_ref()
                .map(|result| result.is_applied()),
            Some(false)
        );
        assert_ne!(
            state.newest_undo.as_ref().map(|held| held.id),
            Some(older.id)
        );
    }

    #[test]
    fn an_explicit_command_and_the_reflow_it_causes_share_one_transaction() {
        let mut state = tiling_state_with_saved_layout("halves", &[(0.0, 0.0, 0.5, 1.0)]);
        state.displays = vec![display(1, "DISPLAY1", 0)];
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    window_at(21, 1, Rect::new(10, 10, 400, 300)),
                    window_at(22, 1, Rect::new(500, 10, 400, 300)),
                ],
            },
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(21),
                display_id: DisplayId(1),
                bounds: Rect::new(10, 10, 400, 300),
            },
        );
        state.persistence_intents.clear();

        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );

        let drafts = recorded_drafts(&state);
        assert_eq!(
            drafts.len(),
            1,
            "the reflow is part of the command, not a second one"
        );
        let moved: HashSet<WindowId> = placements(&state)
            .into_iter()
            .map(|(window_id, _, _)| window_id)
            .collect();
        assert!(
            moved.len() > 1,
            "this test only means something if the snap provoked a reflow"
        );
        assert_eq!(
            drafts[0].members.len(),
            moved.len(),
            "every window the command moved is reversible with it"
        );
    }

    #[test]
    fn no_passive_event_class_creates_an_undo_transaction() {
        let mut state = state_ready_for_a_two_window_command();

        // An application moving its own window.
        apply(
            &mut state,
            Event::WindowBoundsObserved {
                window_id: WindowId(11),
                display_id: DisplayId(1),
                bounds: Rect::new(77, 77, 400, 300),
            },
        );
        // A window closing.
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![window_at(11, 1, Rect::new(77, 77, 400, 300))],
            },
        );
        // A monitor being unplugged, then replugged.
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(2, "DISPLAY2", 0)]),
        );
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "DISPLAY1", 0)]),
        );
        // Waking up.
        apply(
            &mut state,
            Event::WakeReconciliation {
                displays: vec![display(1, "DISPLAY1", 0)],
                windows: Some(vec![window_at(11, 1, Rect::new(77, 77, 400, 300))]),
            },
        );

        assert!(
            recorded_drafts(&state).is_empty(),
            "undo must never claim it can reverse an application closing or a monitor \
             being unplugged"
        );
    }

    // ---- Undo coverage and retention ----------------------------------

    /// Automatic tiling running over two windows on one display, which is
    /// the situation most explicit commands need in order to move anything.
    fn tiling_state_with_two_windows() -> EngineState {
        let mut state = tiling_state_with_saved_layout("halves", &[(0.0, 0.0, 0.5, 1.0)]);
        state.displays = vec![display(1, "DISPLAY1", 0), display(2, "DISPLAY2", 1920)];
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![
                    app_window_at(31, "alpha.exe", 1, Rect::new(10, 10, 400, 300)),
                    app_window_at(32, "beta.exe", 1, Rect::new(500, 10, 400, 300)),
                ],
            },
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(31),
                display_id: DisplayId(1),
                bounds: Rect::new(10, 10, 400, 300),
            },
        );
        state.effects.clear();
        state.persistence_intents.clear();
        state
    }

    /// One named command to drive against a prepared state.
    type CommandCase = (&'static str, Box<dyn Fn(&mut EngineState)>);

    #[test]
    fn every_explicit_placement_command_records_exactly_one_transaction() {
        // Each entry is a command that moves windows. The contract is one
        // transaction per command -- not none, and not one per window.
        let commands: Vec<CommandCase> = vec![
            (
                "zone snap",
                Box::new(|state: &mut EngineState| {
                    apply(
                        state,
                        Event::ZoneSnapRequested {
                            direction: ZoneSnapDirection::Left,
                        },
                    )
                }),
            ),
            (
                "throw to display",
                Box::new(|state: &mut EngineState| {
                    apply(
                        state,
                        Event::WindowThrowToDisplayRequested {
                            window_id: WindowId(31),
                            direction: DisplayDirection::Next,
                        },
                    )
                }),
            ),
            (
                "directional swap",
                Box::new(|state: &mut EngineState| {
                    apply(
                        state,
                        Event::DirectionalSwapRequested {
                            direction: CardinalDirection::Right,
                        },
                    )
                }),
            ),
            (
                "toggle floating",
                Box::new(|state: &mut EngineState| apply(state, Event::ToggleFloatingRequested)),
            ),
            (
                // Suspending moves nothing; resuming is the half that
                // reflows, so the pair is what has a transaction to record.
                "toggle automatic tiling back on",
                Box::new(|state: &mut EngineState| {
                    apply(state, Event::ToggleAutomaticTilingRequested);
                    for window_id in [WindowId(31), WindowId(32)] {
                        if let Some(placement) = state.windows.get_mut(&window_id) {
                            placement.bounds = Rect::new(5, 5, 100, 100);
                        }
                    }
                    apply(state, Event::ToggleAutomaticTilingRequested);
                }),
            ),
        ];

        for (name, run) in commands {
            let mut state = tiling_state_with_two_windows();
            run(&mut state);
            let drafts = recorded_drafts(&state);
            assert_eq!(
                drafts.len(),
                1,
                "{name} moved windows but recorded {} transactions",
                drafts.len()
            );
            assert!(
                !drafts[0].members.is_empty(),
                "{name} recorded a transaction with nothing in it"
            );
        }
    }

    #[test]
    fn a_rearrange_records_its_reflow_as_one_transaction() {
        let mut state = tiling_state_with_two_windows();

        apply(&mut state, Event::RearrangeRequested);
        // A third window arrived while Mosaix was not looking, so the grid
        // this rearrange recomputes is genuinely different from the one on
        // screen -- otherwise there would be nothing to record.
        apply(
            &mut state,
            Event::RearrangeReconciliationComplete {
                windows: vec![
                    app_window_at(31, "alpha.exe", 1, Rect::new(10, 10, 400, 300)),
                    app_window_at(32, "beta.exe", 1, Rect::new(500, 10, 400, 300)),
                    app_window_at(33, "gamma.exe", 1, Rect::new(900, 10, 400, 300)),
                ],
            },
        );

        let drafts = recorded_drafts(&state);
        assert_eq!(drafts.len(), 1, "one rearrange, one transaction");
        assert_eq!(drafts[0].command, "rearrange");
    }

    #[test]
    fn restore_keeps_its_own_meaning_and_is_still_durably_reversible() {
        let mut state = state_ready_to_snap();
        apply(
            &mut state,
            Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left,
            },
        );
        state.persistence_intents.clear();
        state.effects.clear();

        apply(
            &mut state,
            Event::WindowRestoreRequested {
                window_id: WindowId(7),
            },
        );

        assert_eq!(
            placements(&state),
            vec![(WindowId(7), DisplayId(1), Rect::new(10, 10, 500, 500))],
            "restore still returns the window to its remembered placement"
        );
        let drafts = recorded_drafts(&state);
        assert_eq!(drafts.len(), 1, "restore is an explicit placement command");
        assert_eq!(drafts[0].command, "restore");
        assert_eq!(
            drafts[0].members[0].prior_placement,
            Rect::new(0, 0, 960, 1080),
            "undoing a restore returns the window to where the restore found it"
        );

        // The distinct meaning to preserve: one remembered in-session
        // placement, consumed when it is used.
        state.effects.clear();
        state.persistence_intents.clear();
        apply(
            &mut state,
            Event::WindowRestoreRequested {
                window_id: WindowId(7),
            },
        );
        assert!(
            placements(&state).is_empty(),
            "restore remembers one placement, not a history"
        );
        assert!(recorded_drafts(&state).is_empty());
    }

    #[test]
    fn commands_that_change_no_placement_record_nothing() {
        let mut state = tiling_state_with_two_windows();

        apply(&mut state, Event::PauseRequested);
        apply(&mut state, Event::ResumeRequested);
        apply(
            &mut state,
            Event::DirectionalFocusRequested {
                direction: CardinalDirection::Right,
            },
        );
        apply(
            &mut state,
            Event::FocusDisplayRequested {
                display_id: DisplayId(2),
            },
        );
        apply(&mut state, Event::HotkeyCaptureStarted);
        apply(&mut state, Event::HotkeyCaptureEnded);

        assert!(
            recorded_drafts(&state).is_empty(),
            "pausing, focusing, and opening the hotkey editor move no window, \
             so none of them belong in undo history"
        );
    }

    #[test]
    fn a_command_that_moves_nothing_records_nothing() {
        // A throw with nowhere to throw to, on a single-display topology.
        let mut state = state_ready_to_snap();

        apply(
            &mut state,
            Event::WindowThrowToDisplayRequested {
                window_id: WindowId(7),
                direction: DisplayDirection::Next,
            },
        );

        assert!(
            recorded_drafts(&state).is_empty(),
            "undo must not offer to reverse a command that did nothing"
        );
    }

    // ---- Container tree -----------------------------------------------

    /// Automatic tiling running in tree mode on one 1920x1080 display.
    fn tree_state() -> EngineState {
        EngineState {
            displays: vec![display(1, "DISPLAY1", 0)],
            automatic_tiling_active: true,
            resolved_config: ResolvedConfig {
                automatic_tiling_enabled: true,
                tiling_mode: TilingMode::Tree,
                ..ResolvedConfig::default()
            },
            ..EngineState::default()
        }
    }

    fn observe(state: &mut EngineState, windows: Vec<Window>) {
        apply(state, Event::WindowsObserved { windows });
    }

    /// Where each window currently sits, ordered by window id so the
    /// assertion does not depend on effect ordering.
    fn arrangement(state: &EngineState) -> Vec<(isize, Rect)> {
        let mut placed: Vec<(isize, Rect)> = state
            .windows
            .iter()
            .map(|(window_id, placement)| (window_id.0, placement.bounds))
            .collect();
        placed.sort_by_key(|(window_id, _)| *window_id);
        placed
    }

    #[test]
    fn tree_mode_gives_the_first_window_the_whole_work_area() {
        let mut state = tree_state();

        observe(&mut state, vec![window_at(1, 1, Rect::new(0, 0, 400, 300))]);

        assert_eq!(arrangement(&state), vec![(1, Rect::new(0, 0, 1920, 1080))]);
        assert_eq!(state.trees[&DisplayId(1)].len(), 1);
    }

    #[test]
    fn a_new_window_splits_the_focused_leaf_in_half_on_its_longer_axis() {
        let mut state = tree_state();
        observe(&mut state, vec![window_at(1, 1, Rect::new(0, 0, 400, 300))]);

        observe(
            &mut state,
            vec![
                window_at(1, 1, Rect::new(0, 0, 1920, 1080)),
                window_at(2, 1, Rect::new(0, 0, 400, 300)),
            ],
        );

        assert_eq!(
            arrangement(&state),
            vec![
                (1, Rect::new(0, 0, 960, 1080)),
                (2, Rect::new(960, 0, 960, 1080)),
            ],
            "a 1920x1080 leaf is wider than tall, so it divides side by side"
        );

        // A third window, with the right-hand one focused, takes half of
        // that half -- and the left-hand window does not move.
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(2),
                display_id: DisplayId(1),
                bounds: Rect::new(960, 0, 960, 1080),
            },
        );
        observe(
            &mut state,
            vec![
                window_at(1, 1, Rect::new(0, 0, 960, 1080)),
                window_at(2, 1, Rect::new(960, 0, 960, 1080)),
                window_at(3, 1, Rect::new(0, 0, 400, 300)),
            ],
        );

        assert_eq!(
            arrangement(&state),
            vec![
                (1, Rect::new(0, 0, 960, 1080)),
                (2, Rect::new(960, 0, 960, 540)),
                (3, Rect::new(960, 540, 960, 540)),
            ],
            "insertion follows focus and reshapes nothing else"
        );
    }

    #[test]
    fn a_closed_window_gives_its_space_back_to_its_sibling() {
        let mut state = tree_state();
        observe(&mut state, vec![window_at(1, 1, Rect::new(0, 0, 400, 300))]);
        observe(
            &mut state,
            vec![
                window_at(1, 1, Rect::new(0, 0, 1920, 1080)),
                window_at(2, 1, Rect::new(0, 0, 400, 300)),
            ],
        );

        observe(
            &mut state,
            vec![window_at(1, 1, Rect::new(0, 0, 960, 1080))],
        );

        assert_eq!(arrangement(&state), vec![(1, Rect::new(0, 0, 1920, 1080))]);
        assert_eq!(state.trees[&DisplayId(1)].len(), 1);
    }

    #[test]
    fn the_arrangement_does_not_depend_on_the_order_windows_were_observed() {
        // Three windows arriving one at a time, versus the same three
        // arriving together, must settle into the same tree.
        let mut incremental = tree_state();
        observe(
            &mut incremental,
            vec![window_at(1, 1, Rect::new(0, 0, 400, 300))],
        );
        observe(
            &mut incremental,
            vec![
                window_at(1, 1, Rect::new(0, 0, 1920, 1080)),
                window_at(2, 1, Rect::new(0, 0, 400, 300)),
            ],
        );
        observe(
            &mut incremental,
            vec![
                window_at(1, 1, Rect::new(0, 0, 960, 1080)),
                window_at(2, 1, Rect::new(960, 0, 960, 1080)),
                window_at(3, 1, Rect::new(0, 0, 400, 300)),
            ],
        );

        let mut at_once = tree_state();
        observe(
            &mut at_once,
            vec![
                window_at(1, 1, Rect::new(0, 0, 400, 300)),
                window_at(2, 1, Rect::new(500, 0, 400, 300)),
                window_at(3, 1, Rect::new(1000, 0, 400, 300)),
            ],
        );

        assert_eq!(arrangement(&incremental), arrangement(&at_once));
    }

    #[test]
    fn a_tree_command_and_its_reflow_form_one_undo_transaction() {
        let mut state = tree_state();
        observe(
            &mut state,
            vec![
                window_at(41, 1, Rect::new(0, 0, 400, 300)),
                window_at(42, 1, Rect::new(500, 0, 400, 300)),
            ],
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(41),
                display_id: DisplayId(1),
                bounds: Rect::new(0, 0, 960, 1080),
            },
        );
        state.persistence_intents.clear();
        state.effects.clear();

        apply(
            &mut state,
            Event::DirectionalSwapRequested {
                direction: CardinalDirection::Right,
            },
        );

        let drafts = recorded_drafts(&state);
        assert_eq!(
            drafts.len(),
            1,
            "the command and the BSP reflow it caused are one transaction"
        );
        assert_eq!(
            drafts[0].members.len(),
            2,
            "exactly the two swapped windows are reversible together"
        );
    }

    // ---- Directional swap in tree mode --------------------------------

    /// H[ 1, V[ 2, dormant, 3 ] ] with the root divider resized, so the
    /// tree carries every kind of state a swap must leave alone.
    fn rich_tree_state() -> EngineState {
        let mut state = tree_state();
        observe(
            &mut state,
            vec![known_window_at(
                1,
                "alpha.exe",
                1,
                Rect::new(0, 0, 400, 300),
            )],
        );
        observe(
            &mut state,
            vec![
                known_window_at(1, "alpha.exe", 1, Rect::new(0, 0, 1920, 1080)),
                known_window_at(2, "beta.exe", 1, Rect::new(0, 0, 400, 300)),
            ],
        );
        focus(&mut state, 2, Rect::new(960, 0, 960, 1080));
        observe(
            &mut state,
            vec![
                known_window_at(1, "alpha.exe", 1, Rect::new(0, 0, 960, 1080)),
                known_window_at(2, "beta.exe", 1, Rect::new(960, 0, 960, 1080)),
                known_window_at(4, "delta.exe", 1, Rect::new(0, 0, 400, 300)),
            ],
        );
        focus(&mut state, 4, Rect::new(960, 540, 960, 540));
        observe(
            &mut state,
            vec![
                known_window_at(1, "alpha.exe", 1, Rect::new(0, 0, 960, 1080)),
                known_window_at(2, "beta.exe", 1, Rect::new(960, 0, 960, 540)),
                known_window_at(4, "delta.exe", 1, Rect::new(960, 540, 960, 540)),
                known_window_at(3, "gamma.exe", 1, Rect::new(0, 0, 400, 300)),
            ],
        );
        // delta closes: its slot between beta and gamma goes dormant.
        observe(
            &mut state,
            vec![
                known_window_at(1, "alpha.exe", 1, Rect::new(0, 0, 960, 1080)),
                known_window_at(2, "beta.exe", 1, Rect::new(960, 0, 960, 360)),
                known_window_at(3, "gamma.exe", 1, Rect::new(960, 720, 960, 360)),
            ],
        );
        assert_eq!(state.trees[&DisplayId(1)].dormant_positions().len(), 1);
        focus(&mut state, 2, Rect::new(960, 0, 960, 540));
        resize(&mut state, CardinalDirection::Left);
        assert_eq!(
            arrangement(&state),
            vec![
                (1, Rect::new(0, 0, 864, 1080)),
                (2, Rect::new(864, 0, 1056, 540)),
                (3, Rect::new(864, 540, 1056, 540)),
            ]
        );
        state.effects.clear();
        state.persistence_intents.clear();
        state
    }

    #[test]
    fn swap_selects_exactly_the_neighbor_directional_focus_selects() {
        let mut state = rich_tree_state();
        for id in [1, 2, 3] {
            let bounds = state.windows[&WindowId(id)].bounds;
            focus(&mut state, id, bounds);
            for direction in [
                CardinalDirection::Left,
                CardinalDirection::Right,
                CardinalDirection::Up,
                CardinalDirection::Down,
            ] {
                let focus_target = directional_neighbor(&state, direction);
                let swap_target = plan_directional_swap(&state, direction)
                    .ok()
                    .map(|plan| plan.neighbor_id);
                assert_eq!(
                    focus_target, swap_target,
                    "from {id} going {direction:?}: focus and swap must name one neighbor"
                );
            }
        }
    }

    #[test]
    fn swap_exchanges_only_two_bindings_and_leaves_every_other_slot_alone() {
        let mut state = rich_tree_state();
        let tree_before = state.trees[&DisplayId(1)].clone();
        // Right from window 1 is window 2, the top of the right column.
        focus(&mut state, 1, Rect::new(0, 0, 864, 1080));

        apply(
            &mut state,
            Event::DirectionalSwapRequested {
                direction: CardinalDirection::Right,
            },
        );

        let tree_after = &state.trees[&DisplayId(1)];
        // Mapping the two windows back reproduces the prior tree exactly:
        // containers, axes, weights, dormant slot, and insertion numbers.
        let mut unswapped = tree_after.clone();
        assert!(unswapped.swap_leaves(&WindowId(2), &WindowId(1)));
        assert_eq!(unswapped, tree_before);
        assert_eq!(
            tree_after.dormant_positions().len(),
            1,
            "the dormant slot in the right column is untouched"
        );
        assert_eq!(
            placements(&state)
                .iter()
                .map(|(id, _, _)| id.0)
                .collect::<std::collections::BTreeSet<_>>(),
            [1, 2].into_iter().collect(),
            "only the two swapped windows are placed; window 3 does not move"
        );
        assert_eq!(
            state.windows[&WindowId(3)].bounds,
            Rect::new(864, 540, 1056, 540)
        );
    }

    #[test]
    fn focus_stays_on_the_same_window_which_now_sits_where_its_neighbor_was() {
        let mut state = rich_tree_state();
        focus(&mut state, 1, Rect::new(0, 0, 864, 1080));
        let neighbor_was = state.windows[&WindowId(2)].bounds;

        apply(
            &mut state,
            Event::DirectionalSwapRequested {
                direction: CardinalDirection::Right,
            },
        );

        assert_eq!(state.focused_window, Some(WindowId(1)));
        assert_eq!(state.windows[&WindowId(1)].bounds, neighbor_was);
        assert!(
            !state
                .effects
                .iter()
                .any(|effect| matches!(effect, EngineEffect::FocusWindow { .. })),
            "no focus effect is needed: the same window keeps focus"
        );
        let drafts = recorded_drafts(&state);
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].command, "swap-right");
        assert_eq!(drafts[0].members.len(), 2);
    }

    #[test]
    fn swap_refuses_typed_at_boundaries_and_for_ineligible_endpoints() {
        let mut state = rich_tree_state();

        // Boundary: nothing lies left of window 1, and it does not wrap.
        focus(&mut state, 1, Rect::new(0, 0, 864, 1080));
        assert_eq!(
            plan_directional_swap(&state, CardinalDirection::Left),
            Err(DirectionalSwapRefusal::NoNeighbor {
                command: "swap-left".to_owned()
            })
        );
        let tree_before = state.trees[&DisplayId(1)].clone();
        apply(
            &mut state,
            Event::DirectionalSwapRequested {
                direction: CardinalDirection::Left,
            },
        );
        assert_eq!(state.trees[&DisplayId(1)], tree_before);
        assert!(recorded_drafts(&state).is_empty());

        // A floating window is not an endpoint, from either side.
        focus(&mut state, 3, Rect::new(864, 540, 1056, 540));
        apply(&mut state, Event::ToggleFloatingRequested);
        assert_eq!(
            plan_directional_swap(&state, CardinalDirection::Up),
            Err(DirectionalSwapRefusal::NotArranged {
                window_id: WindowId(3)
            })
        );
        focus(&mut state, 2, Rect::new(864, 0, 1056, 1080));
        assert!(matches!(
            plan_directional_swap(&state, CardinalDirection::Down),
            Err(DirectionalSwapRefusal::NoNeighbor { .. })
        ));

        state.paused = true;
        assert_eq!(
            plan_directional_swap(&state, CardinalDirection::Left),
            Err(DirectionalSwapRefusal::Paused)
        );
        state.focused_window = None;
        state.paused = false;
        assert_eq!(
            plan_directional_swap(&state, CardinalDirection::Left),
            Err(DirectionalSwapRefusal::NoFocusedWindow)
        );
    }

    #[test]
    fn swap_never_crosses_a_display_where_display_transfer_would() {
        let mut state = tree_state();
        state.displays = vec![display(1, "DISPLAY1", 0), display(2, "DISPLAY2", 1920)];
        observe(
            &mut state,
            vec![
                known_window_at(1, "alpha.exe", 1, Rect::new(0, 0, 400, 300)),
                known_window_at(2, "beta.exe", 2, Rect::new(1920, 0, 400, 300)),
            ],
        );
        focus(&mut state, 1, Rect::new(0, 0, 1920, 1080));

        assert_eq!(
            plan_directional_swap(&state, CardinalDirection::Right),
            Err(DirectionalSwapRefusal::NoNeighbor {
                command: "swap-right".to_owned()
            }),
            "the window on the next display is not a neighbor"
        );
        // The explicit command for that is a throw, which does move it.
        apply(
            &mut state,
            Event::WindowThrowToDisplayRequested {
                window_id: WindowId(1),
                direction: DisplayDirection::Next,
            },
        );
        assert_eq!(state.windows[&WindowId(1)].display_id, DisplayId(2));
    }

    // ---- Dormant positions --------------------------------------------

    /// [`app_window_at`] with the class and executable path a real window
    /// carries, which is what lets the matcher recognise it again: a
    /// signal absent on both sides scores nothing.
    fn known_window_at(id: isize, application: &str, display_id: isize, bounds: Rect) -> Window {
        let mut window = app_window_at(id, application, display_id, bounds);
        window.native_class = Some(format!("{application}-class"));
        window.executable_path = Some(std::path::PathBuf::from(format!("C:/apps/{application}")));
        window
    }

    /// Two distinguishable applications side by side, alpha on the left.
    fn two_apps_state() -> EngineState {
        let mut state = tree_state();
        observe(
            &mut state,
            vec![
                known_window_at(1, "alpha.exe", 1, Rect::new(0, 0, 400, 300)),
                known_window_at(2, "beta.exe", 1, Rect::new(500, 0, 400, 300)),
            ],
        );
        assert_eq!(
            arrangement(&state),
            vec![
                (1, Rect::new(0, 0, 960, 1080)),
                (2, Rect::new(960, 0, 960, 1080)),
            ]
        );
        state.effects.clear();
        state.persistence_intents.clear();
        state
    }

    fn dormant_positions(state: &EngineState) -> Vec<(u64, String, i64)> {
        state.trees[&DisplayId(1)]
            .dormant_positions()
            .into_iter()
            .map(|(position, dormant)| {
                (
                    position,
                    dormant.evidence.application_id.0.clone(),
                    dormant.since_unix,
                )
            })
            .collect()
    }

    #[test]
    fn a_closed_window_leaves_a_dormant_slot_that_reserves_no_space() {
        let mut state = two_apps_state();

        // beta closes.
        observe(
            &mut state,
            vec![known_window_at(
                1,
                "alpha.exe",
                1,
                Rect::new(0, 0, 960, 1080),
            )],
        );

        assert_eq!(
            arrangement(&state),
            vec![(1, Rect::new(0, 0, 1920, 1080))],
            "alpha uses the whole display; the slot holds no space open"
        );
        let dormant = dormant_positions(&state);
        assert_eq!(dormant.len(), 1);
        assert_eq!(dormant[0].0, 1, "the slot keeps its insertion number");
        assert_eq!(
            dormant[0].1, "beta.exe",
            "and privacy-safe evidence of who held it"
        );
        assert!(
            recorded_drafts(&state).is_empty(),
            "a passive close is not an undo transaction"
        );
        assert!(
            state
                .persistence_intents
                .iter()
                .any(|intent| matches!(intent, PersistenceIntent::SaveContainerTree { .. })),
            "the dormant slot is durable"
        );
    }

    #[test]
    fn a_returning_window_reclaims_its_slot_on_a_confident_match() {
        let mut state = two_apps_state();
        let tree_before = state.trees[&DisplayId(1)].clone();
        observe(
            &mut state,
            vec![known_window_at(
                1,
                "alpha.exe",
                1,
                Rect::new(0, 0, 960, 1080),
            )],
        );
        assert_eq!(dormant_positions(&state).len(), 1);

        // beta reopens with a new native handle, and alpha now has focus
        // -- so plain insertion would have split alpha, not restored the
        // right-hand slot.
        focus(&mut state, 1, Rect::new(0, 0, 1920, 1080));
        observe(
            &mut state,
            vec![
                known_window_at(1, "alpha.exe", 1, Rect::new(0, 0, 1920, 1080)),
                known_window_at(77, "beta.exe", 1, Rect::new(300, 300, 400, 300)),
            ],
        );

        assert_eq!(
            arrangement(&state),
            vec![
                (1, Rect::new(0, 0, 960, 1080)),
                (77, Rect::new(960, 0, 960, 1080)),
            ],
            "beta is back on the right, where its slot was"
        );
        assert!(dormant_positions(&state).is_empty());
        assert_eq!(
            state.trees[&DisplayId(1)].insertion_of(&WindowId(77)),
            tree_before.insertion_of(&WindowId(2)),
            "the reclaimed slot is the original slot, metadata and all"
        );
    }

    #[test]
    fn an_ambiguous_return_inserts_fresh_and_leaves_the_slot_alone() {
        let mut state = two_apps_state();
        observe(
            &mut state,
            vec![known_window_at(
                1,
                "alpha.exe",
                1,
                Rect::new(0, 0, 960, 1080),
            )],
        );

        // Two identical beta windows appear at once: neither can be told
        // from the other, so neither takes the slot.
        observe(
            &mut state,
            vec![
                known_window_at(1, "alpha.exe", 1, Rect::new(0, 0, 1920, 1080)),
                known_window_at(77, "beta.exe", 1, Rect::new(300, 300, 400, 300)),
                known_window_at(78, "beta.exe", 1, Rect::new(300, 300, 400, 300)),
            ],
        );

        assert_eq!(
            dormant_positions(&state).len(),
            1,
            "an ambiguous match must not mutate the tree"
        );
        assert_eq!(
            state.trees[&DisplayId(1)].len(),
            3,
            "both were inserted fresh"
        );
    }

    #[test]
    fn dormant_slots_expire_after_seven_days_on_the_next_reflow() {
        let mut state = two_apps_state();
        observe(
            &mut state,
            vec![known_window_at(
                1,
                "alpha.exe",
                1,
                Rect::new(0, 0, 960, 1080),
            )],
        );
        // Age the slot past retention.
        let mut aged = state.trees[&DisplayId(1)].clone();
        let evidence = aged.dormant_positions()[0].1.evidence.clone();
        aged.reclaim(1, WindowId(2));
        aged.make_dormant(
            &WindowId(2),
            mosaix_domain::DormantPosition {
                evidence,
                since_unix: now_unix() - mosaix_domain::DORMANT_RETENTION_SECONDS - 1,
            },
        );
        state.trees.insert(DisplayId(1), aged);

        // Any observation that changes the inventory provokes a reflow;
        // here alpha reports slightly different bounds.
        observe(
            &mut state,
            vec![known_window_at(
                1,
                "alpha.exe",
                1,
                Rect::new(2, 2, 1916, 1076),
            )],
        );

        assert!(
            dormant_positions(&state).is_empty(),
            "the aged slot is pruned"
        );
        assert!(
            matches!(
                state.trees[&DisplayId(1)].root(),
                Some(mosaix_domain::Node::Leaf(_))
            ),
            "and the container that located it is gone"
        );
    }

    #[test]
    fn removing_a_dormant_position_is_explicit_typed_and_undoable() {
        let mut state = two_apps_state();
        observe(
            &mut state,
            vec![known_window_at(
                1,
                "alpha.exe",
                1,
                Rect::new(0, 0, 960, 1080),
            )],
        );
        let tree_before = state.trees[&DisplayId(1)].clone();
        state.persistence_intents.clear();

        assert_eq!(
            plan_remove_position(&state, DisplayId(1), 5),
            Err(RemovePositionRefusal::UnknownPosition {
                display_id: DisplayId(1),
                position: 5
            })
        );
        assert_eq!(
            plan_remove_position(&state, DisplayId(1), 0),
            Err(RemovePositionRefusal::UnknownPosition {
                display_id: DisplayId(1),
                position: 0
            }),
            "a live slot is not removable this way"
        );
        assert_eq!(
            plan_remove_position(&state, DisplayId(9), 1),
            Err(RemovePositionRefusal::DisplayUnavailable {
                display_id: DisplayId(9)
            })
        );
        let applied = plan_remove_position(&state, DisplayId(1), 1).expect("the slot exists");
        assert_eq!(applied.application, "beta.exe");

        apply(
            &mut state,
            Event::TreePositionRemoveRequested {
                display_id: DisplayId(1),
                position: 1,
            },
        );

        assert!(dormant_positions(&state).is_empty());
        let drafts = recorded_drafts(&state);
        assert_eq!(
            drafts.len(),
            1,
            "a structural change with no placements is still a transaction"
        );
        assert_eq!(drafts[0].command, "remove-position");
        assert_eq!(drafts[0].prior_trees.len(), 1);

        let stored = stored(drafts[0], 1);
        apply(&mut state, Event::UndoHistoryLoaded(Some(Box::new(stored))));
        apply(&mut state, Event::UndoRequested);

        assert!(matches!(
            state.last_undo_result,
            Some(UndoResult::Applied(_))
        ));
        assert_eq!(
            dormant_positions(&state),
            dormant_positions(&EngineState {
                trees: [(DisplayId(1), tree_before)].into_iter().collect(),
                ..EngineState::default()
            }),
            "undo puts the dormant slot back"
        );
    }

    #[test]
    fn a_stored_arrangement_whose_window_is_missing_keeps_its_slot_dormant() {
        // Restart: the stored tree has alpha and beta, but only alpha is
        // open. Beta's slot waits; when beta opens later it goes home.
        let mut first = two_apps_state();
        let stored = first
            .persistence_intents
            .iter()
            .rev()
            .find_map(|intent| match intent {
                PersistenceIntent::SaveContainerTree { tree, .. } => Some((**tree).clone()),
                _ => None,
            })
            .or_else(|| {
                // The fixture cleared intents; rebuild the durable form.
                Some(durable_tree(&first, &first.trees[&DisplayId(1)]))
            })
            .expect("a durable tree");
        first.trees.clear();

        let mut state = tree_state();
        observe(
            &mut state,
            vec![known_window_at(
                501,
                "alpha.exe",
                1,
                Rect::new(0, 0, 400, 300),
            )],
        );
        apply(
            &mut state,
            Event::ContainerTreesLoaded([("DISPLAY1".to_owned(), stored)].into_iter().collect()),
        );
        observe(
            &mut state,
            vec![known_window_at(
                501,
                "alpha.exe",
                1,
                Rect::new(0, 0, 1920, 1080),
            )],
        );
        assert_eq!(
            arrangement(&state),
            vec![(501, Rect::new(0, 0, 1920, 1080))]
        );
        assert_eq!(dormant_positions(&state).len(), 1, "beta is remembered");

        observe(
            &mut state,
            vec![
                known_window_at(501, "alpha.exe", 1, Rect::new(0, 0, 1920, 1080)),
                known_window_at(502, "beta.exe", 1, Rect::new(10, 10, 400, 300)),
            ],
        );
        assert_eq!(
            arrangement(&state),
            vec![
                (501, Rect::new(0, 0, 960, 1080)),
                (502, Rect::new(960, 0, 960, 1080)),
            ],
            "beta reclaims the right-hand slot across the restart"
        );
        assert!(dormant_positions(&state).is_empty());
    }

    // ---- Tree resize --------------------------------------------------

    /// H[ 1, V[ 2, 3 ] ] on a 1920x1080 display: window 1 takes the left
    /// half, windows 2 and 3 stack on the right.
    fn nested_tree_state() -> EngineState {
        let mut state = tree_state();
        observe(&mut state, vec![window_at(1, 1, Rect::new(0, 0, 400, 300))]);
        observe(
            &mut state,
            vec![
                window_at(1, 1, Rect::new(0, 0, 1920, 1080)),
                window_at(2, 1, Rect::new(0, 0, 400, 300)),
            ],
        );
        focus(&mut state, 2, Rect::new(960, 0, 960, 1080));
        observe(
            &mut state,
            vec![
                window_at(1, 1, Rect::new(0, 0, 960, 1080)),
                window_at(2, 1, Rect::new(960, 0, 960, 1080)),
                window_at(3, 1, Rect::new(0, 0, 400, 300)),
            ],
        );
        assert_eq!(
            arrangement(&state),
            vec![
                (1, Rect::new(0, 0, 960, 1080)),
                (2, Rect::new(960, 0, 960, 540)),
                (3, Rect::new(960, 540, 960, 540)),
            ]
        );
        state.effects.clear();
        state.persistence_intents.clear();
        state
    }

    fn focus(state: &mut EngineState, id: isize, bounds: Rect) {
        apply(
            state,
            Event::WindowFocused {
                window_id: WindowId(id),
                display_id: DisplayId(1),
                bounds,
            },
        );
    }

    fn resize(state: &mut EngineState, direction: CardinalDirection) {
        apply(state, Event::TreeResizeRequested { direction });
    }

    #[test]
    fn resizing_moves_the_nearest_divider_by_five_points_and_reflows() {
        let mut state = nested_tree_state();
        focus(&mut state, 3, Rect::new(960, 540, 960, 540));

        // Up from window 3 moves the divider between 2 and 3: 3 grows to
        // 55% of the right column's 1080px, 2 shrinks to 45%.
        resize(&mut state, CardinalDirection::Up);

        assert_eq!(
            arrangement(&state),
            vec![
                (1, Rect::new(0, 0, 960, 1080)),
                (2, Rect::new(960, 0, 960, 486)),
                (3, Rect::new(960, 486, 960, 594)),
            ],
            "the focused window grew upward and its sibling gave the space; nothing else moved"
        );
    }

    #[test]
    fn resizing_climbs_to_the_outer_divider_when_the_parent_cannot_face_that_way() {
        let mut state = nested_tree_state();
        focus(&mut state, 3, Rect::new(960, 540, 960, 540));

        // Left from window 3: its parent is vertical, so the horizontal
        // root divider moves and the whole right column grows.
        resize(&mut state, CardinalDirection::Left);

        assert_eq!(
            arrangement(&state),
            vec![
                (1, Rect::new(0, 0, 864, 1080)),
                (2, Rect::new(864, 0, 1056, 540)),
                (3, Rect::new(864, 540, 1056, 540)),
            ]
        );
    }

    #[test]
    fn resizing_at_the_arrangement_edge_is_a_typed_no_op() {
        let mut state = nested_tree_state();
        focus(&mut state, 1, Rect::new(0, 0, 960, 1080));
        let before = state.trees[&DisplayId(1)].clone();

        assert_eq!(
            plan_tree_resize(&state, CardinalDirection::Left),
            Err(TreeResizeRefusal::NoDivider {
                command: "resize-left".to_owned()
            })
        );
        assert!(matches!(
            plan_tree_resize(&state, CardinalDirection::Up),
            Err(TreeResizeRefusal::NoDivider { .. })
        ));
        resize(&mut state, CardinalDirection::Left);

        assert_eq!(
            state.trees[&DisplayId(1)],
            before,
            "no unrelated axis was touched"
        );
        assert!(placements(&state).is_empty());
        assert!(
            recorded_drafts(&state).is_empty(),
            "a refused resize records no transaction"
        );
    }

    #[test]
    fn resizing_is_refused_for_windows_the_tree_does_not_arrange() {
        let mut state = nested_tree_state();
        assert_eq!(
            plan_tree_resize(&EngineState::default(), CardinalDirection::Left),
            Err(TreeResizeRefusal::NotTreeMode)
        );
        state.focused_window = None;
        assert_eq!(
            plan_tree_resize(&state, CardinalDirection::Left),
            Err(TreeResizeRefusal::NoFocusedWindow)
        );

        // A session-floating window has no leaf.
        focus(&mut state, 2, Rect::new(960, 0, 960, 540));
        apply(&mut state, Event::ToggleFloatingRequested);
        assert_eq!(
            plan_tree_resize(&state, CardinalDirection::Left),
            Err(TreeResizeRefusal::NotArranged {
                window_id: WindowId(2)
            })
        );

        state.paused = true;
        assert_eq!(
            plan_tree_resize(&state, CardinalDirection::Left),
            Err(TreeResizeRefusal::Paused)
        );
    }

    #[test]
    fn resizing_clamps_to_the_largest_step_that_respects_minimum_sizes() {
        // Two windows side by side on 1920px. Window 1 needs 940px; it
        // holds 960, so the divider can move 20px toward it: 1% of 1920
        // is 19.2px, 2% is 38.4px. Only the one-point step is legal.
        let mut state = tree_state();
        observe(
            &mut state,
            vec![
                window_needing(1, 1, Rect::new(0, 0, 400, 300), (940, 100)),
                window_at(2, 1, Rect::new(500, 0, 400, 300)),
            ],
        );
        focus(&mut state, 2, Rect::new(960, 0, 960, 1080));
        state.persistence_intents.clear();

        let plan = plan_tree_resize(&state, CardinalDirection::Left).expect("one point fits");
        assert_eq!(plan.applied.percentage_points, 1);
        resize(&mut state, CardinalDirection::Left);
        assert_eq!(state.windows[&WindowId(1)].bounds.width, 941);
        assert!(
            state.constraint_overflow.is_empty(),
            "the clamp never overflows a window"
        );

        // Now window 1 is at 941px: not even one more point is legal.
        assert_eq!(
            plan_tree_resize(&state, CardinalDirection::Left),
            Err(TreeResizeRefusal::MinimumSizeReached {
                command: "resize-left".to_owned()
            })
        );
        let before = state.trees[&DisplayId(1)].clone();
        resize(&mut state, CardinalDirection::Left);
        assert_eq!(state.trees[&DisplayId(1)], before);
    }

    #[test]
    fn resizing_never_pays_for_itself_with_gaps() {
        // With 20px gaps, window 1 at 960px raw is 946px placed and needs
        // 940. A one-point move (19px) would put it below 940 -- and the
        //      planner could rescue that by shrinking the gaps, which a
        //      resize must not do.
        let mut state = tree_state();
        state.resolved_config.gaps = mosaix_domain::Gaps::new(20, 12);
        observe(
            &mut state,
            vec![
                window_needing(1, 1, Rect::new(0, 0, 400, 300), (940, 100)),
                window_at(2, 1, Rect::new(500, 0, 400, 300)),
            ],
        );
        focus(&mut state, 2, Rect::new(960, 0, 960, 1080));

        assert!(matches!(
            plan_tree_resize(&state, CardinalDirection::Left),
            Err(TreeResizeRefusal::MinimumSizeReached { .. })
        ));
    }

    #[test]
    fn a_resize_and_its_placements_are_one_undo_transaction_that_restores_the_tree() {
        let mut state = nested_tree_state();
        focus(&mut state, 3, Rect::new(960, 540, 960, 540));
        let tree_before = state.trees[&DisplayId(1)].clone();
        let arrangement_before = arrangement(&state);

        resize(&mut state, CardinalDirection::Up);

        let drafts = recorded_drafts(&state);
        assert_eq!(drafts.len(), 1, "one command, one transaction");
        assert_eq!(drafts[0].command, "resize-up");
        assert_eq!(drafts[0].members.len(), 2, "both moved windows are members");
        assert_eq!(
            drafts[0].prior_trees.len(),
            1,
            "and the tree as it was rides along"
        );
        assert_eq!(drafts[0].prior_trees[0].display_fingerprint, "DISPLAY1");

        // Publish it back as stored history and undo it.
        let stored = stored(drafts[0], 1);
        apply(&mut state, Event::UndoHistoryLoaded(Some(Box::new(stored))));
        state.effects.clear();
        apply(&mut state, Event::UndoRequested);

        assert!(matches!(
            state.last_undo_result,
            Some(UndoResult::Applied(_))
        ));
        assert_eq!(
            arrangement(&state),
            arrangement_before,
            "the windows are back"
        );
        assert_eq!(
            state.trees[&DisplayId(1)],
            tree_before,
            "and so are the weights, so the next reflow does not redo the resize"
        );

        // A later passive reflow moves nothing.
        state.effects.clear();
        observe(
            &mut state,
            vec![
                window_at(1, 1, Rect::new(0, 0, 960, 1080)),
                window_at(2, 1, Rect::new(960, 0, 960, 540)),
                window_at(3, 1, Rect::new(960, 540, 960, 540)),
            ],
        );
        assert!(placements(&state).is_empty());
    }

    #[test]
    fn undoing_a_swap_restores_the_tree_as_well_as_the_windows() {
        let mut state = nested_tree_state();
        focus(&mut state, 1, Rect::new(0, 0, 960, 1080));
        let tree_before = state.trees[&DisplayId(1)].clone();

        apply(
            &mut state,
            Event::DirectionalSwapRequested {
                direction: CardinalDirection::Right,
            },
        );
        assert_ne!(state.trees[&DisplayId(1)], tree_before);
        let drafts = recorded_drafts(&state);
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].prior_trees.len(), 1);
        let stored = stored(drafts[0], 1);
        apply(&mut state, Event::UndoHistoryLoaded(Some(Box::new(stored))));

        apply(&mut state, Event::UndoRequested);

        assert_eq!(state.trees[&DisplayId(1)], tree_before);
    }

    #[test]
    fn resize_survives_deep_unbalanced_odd_and_negative_origin_arrangements() {
        // A five-window tree on an oddly sized display left of the primary,
        // resized repeatedly in every direction: every plan must keep
        // tiling exactly, and every resize must be reversible by the
        // opposite resize on the sibling.
        // Two displays at different scales: an odd, negative-origin one at
        // 125% on the left and the primary at 100%.
        let mut state = tree_state();
        state.displays[0].full_bounds = Rect::new(-1367, -13, 1367, 769);
        state.displays[0].work_area = Rect::new(-1367, -13, 1367, 741);
        state.displays[0].scale_factor = 1.25;
        state.displays.push(display(2, "DISPLAY2", 0));
        let mut windows = Vec::new();
        for id in 1..=5 {
            windows.push(window_at(
                id,
                1,
                Rect::new(-1300 + id as i32 * 10, 0, 300, 200),
            ));
            observe(&mut state, windows.clone());
            let bounds = state.windows[&WindowId(id)].bounds;
            focus(&mut state, id, bounds);
        }
        for id in 6..=8 {
            windows.push(window_at(id, 2, Rect::new(id as i32 * 10, 0, 300, 200)));
            observe(&mut state, windows.clone());
            let bounds = state.windows[&WindowId(id)].bounds;
            focus(&mut state, id, bounds);
        }
        let work_area_of_display =
            |state: &EngineState, id: isize| state.displays[usize::from(id != 1)].work_area;
        for (id, direction) in [
            (5, CardinalDirection::Left),
            (5, CardinalDirection::Up),
            (3, CardinalDirection::Down),
            (2, CardinalDirection::Right),
            (4, CardinalDirection::Left),
            (7, CardinalDirection::Left),
            (8, CardinalDirection::Up),
            (6, CardinalDirection::Right),
        ] {
            let bounds = state.windows[&WindowId(id)].bounds;
            focus(&mut state, id, bounds);
            resize(&mut state, direction);
            for display_id in [1, 2] {
                let work_area = work_area_of_display(&state, display_id);
                let placed: Vec<Rect> = state
                    .windows
                    .values()
                    .filter(|placement| placement.display_id == DisplayId(display_id))
                    .map(|placement| placement.bounds)
                    .collect();
                let covered: i64 = placed
                    .iter()
                    .map(|rect| (rect.width as i64) * (rect.height as i64))
                    .sum();
                assert_eq!(
                    covered,
                    (work_area.width as i64) * (work_area.height as i64),
                    "after {direction:?} on {id}: display {display_id} must still be tiled exactly"
                );
                for rect in &placed {
                    assert!(work_area.contains(rect), "{rect:?} escaped {work_area:?}");
                }
            }
        }
    }

    #[test]
    fn an_overflowed_window_returns_when_the_display_grows() {
        let mut state = narrow_tree_state(1000);
        // The topology change re-selects config, so the set has to keep
        // tree mode on for the new fingerprint too.
        state.config_set = ResolvedConfigSet {
            base: state.resolved_config.clone(),
            profiles: Vec::new(),
        };
        observe(
            &mut state,
            vec![
                window_needing(1, 1, Rect::new(0, 0, 400, 300), (600, 100)),
                window_needing(2, 1, Rect::new(500, 0, 400, 300), (600, 100)),
            ],
        );
        assert_eq!(
            state.constraint_overflow.get(&DisplayId(1)),
            Some(&vec![WindowId(2)])
        );
        let tree_before = state.trees[&DisplayId(1)].clone();

        let mut wider = display(1, "DISPLAY1", 0);
        wider.full_bounds = Rect::new(0, 0, 1400, 600);
        wider.work_area = Rect::new(0, 0, 1400, 600);
        apply(&mut state, Event::DisplayTopologyChanged(vec![wider]));

        assert!(state.constraint_overflow.is_empty());
        assert_eq!(state.trees[&DisplayId(1)], tree_before, "the leaf was kept");
        assert_eq!(
            state.windows[&WindowId(2)].bounds,
            Rect::new(700, 0, 700, 600)
        );
    }

    #[test]
    fn a_floated_or_transferred_window_leaves_the_tree_without_a_dormant_slot() {
        let mut state = two_apps_state();
        state.displays.push(display(2, "DISPLAY2", 1920));

        focus(&mut state, 2, Rect::new(960, 0, 960, 1080));
        apply(&mut state, Event::ToggleFloatingRequested);
        assert!(
            dormant_positions(&state).is_empty(),
            "a window the user floated is visible; a slot kept for it would be a ghost"
        );
        apply(&mut state, Event::ToggleFloatingRequested);

        apply(
            &mut state,
            Event::WindowThrowToDisplayRequested {
                window_id: WindowId(2),
                direction: DisplayDirection::Next,
            },
        );
        assert!(
            dormant_positions(&state).is_empty(),
            "display transfer is explicit; the source keeps no slot"
        );
    }

    #[test]
    fn a_stored_dormant_slot_past_retention_is_pruned_on_load() {
        let mut first = two_apps_state();
        let durable = durable_tree(&first, &first.trees[&DisplayId(1)]);
        first.trees.clear();
        let aged = durable.convert_leaves(&mut |leaf| match &leaf.occupant {
            mosaix_domain::Occupant::Live(evidence) if evidence.application_id.0 == "beta.exe" => {
                mosaix_domain::LeafFate::Dormant(mosaix_domain::DormantPosition {
                    evidence: evidence.clone(),
                    since_unix: now_unix() - mosaix_domain::DORMANT_RETENTION_SECONDS - 1,
                })
            }
            mosaix_domain::Occupant::Live(evidence) => {
                mosaix_domain::LeafFate::Live(evidence.clone())
            }
            mosaix_domain::Occupant::Dormant(position) => {
                mosaix_domain::LeafFate::Dormant(position.clone())
            }
        });

        let mut state = tree_state();
        observe(
            &mut state,
            vec![known_window_at(
                501,
                "alpha.exe",
                1,
                Rect::new(0, 0, 400, 300),
            )],
        );
        apply(
            &mut state,
            Event::ContainerTreesLoaded([("DISPLAY1".to_owned(), aged)].into_iter().collect()),
        );
        observe(
            &mut state,
            vec![known_window_at(
                501,
                "alpha.exe",
                1,
                Rect::new(0, 0, 1920, 1080),
            )],
        );

        assert!(
            dormant_positions(&state).is_empty(),
            "retention counts from when it went dormant"
        );
        assert_eq!(state.trees[&DisplayId(1)].len(), 1);
    }

    #[test]
    fn undo_refuses_when_a_leaf_of_the_prior_tree_is_ambiguous() {
        // alpha | beta, swapped. Then gamma opens twice, identically. The
        // transaction's members (alpha, beta) still resolve, but the prior
        // tree is edited to carry a gamma leaf whose evidence fits either
        // gamma window equally -- restoring it would guess.
        let mut state = two_apps_state();
        focus(&mut state, 1, Rect::new(0, 0, 960, 1080));
        apply(
            &mut state,
            Event::DirectionalSwapRequested {
                direction: CardinalDirection::Right,
            },
        );
        let mut draft = recorded_drafts(&state)[0].clone();
        observe(
            &mut state,
            vec![
                known_window_at(1, "alpha.exe", 1, Rect::new(960, 0, 960, 1080)),
                known_window_at(2, "beta.exe", 1, Rect::new(0, 0, 960, 1080)),
                known_window_at(8, "gamma.exe", 1, Rect::new(10, 10, 300, 200)),
                known_window_at(9, "gamma.exe", 1, Rect::new(10, 10, 300, 200)),
            ],
        );
        let gamma = mosaix_domain::WindowEvidence {
            application_id: mosaix_domain::ApplicationId("gamma.exe".to_owned()),
            executable_path: Some("C:/apps/gamma.exe".to_owned()),
            native_class: Some("gamma.exe-class".to_owned()),
            role: mosaix_domain::WindowRole::Normal,
            launch_order: 5,
            last_placement: Rect::new(5000, 5000, 10, 10),
            display_fingerprint: "DISPLAY1".to_owned(),
        };
        let mut with_gamma = draft.prior_trees[0].tree.clone();
        let first_leaf = with_gamma.windows()[0].clone();
        with_gamma.split_leaf(&first_leaf, SplitAxis::Vertical, gamma);
        draft.prior_trees[0].tree = with_gamma;
        let stored = stored(&draft, 1);
        apply(&mut state, Event::UndoHistoryLoaded(Some(Box::new(stored))));
        state.effects.clear();
        let tree_before = state.trees[&DisplayId(1)].clone();

        let planned = plan_undo(&state);
        let UndoResult::Refused(UndoRefusal::TargetsUnresolved { targets, .. }) = &planned else {
            panic!("an ambiguous leaf must refuse, got {planned:?}");
        };
        assert!(targets.iter().any(|target| {
            target.application == "gamma.exe"
                && matches!(target.outcome, MatchOutcome::Ambiguous { .. })
        }));
        apply(&mut state, Event::UndoRequested);
        assert!(placements(&state).is_empty(), "nothing moved");
        assert_eq!(state.trees[&DisplayId(1)], tree_before, "nothing reshaped");
        assert!(
            state.newest_undo.is_some(),
            "the transaction is kept for retry"
        );
    }

    // ---- Constraint overflow ------------------------------------------

    /// [`window_at`] with a known minimum size.
    fn window_needing(id: isize, display_id: isize, bounds: Rect, minimum: (i32, i32)) -> Window {
        let mut window = window_at(id, display_id, bounds);
        window.minimum_size = Some(mosaix_domain::Size::new(minimum.0, minimum.1));
        window
    }

    /// A tree-mode display too narrow for three windows of the given
    /// minimum width side by side.
    fn narrow_tree_state(width: i32) -> EngineState {
        let mut state = tree_state();
        state.displays[0].full_bounds = Rect::new(0, 0, width, 600);
        state.displays[0].work_area = Rect::new(0, 0, width, 600);
        state
    }

    #[test]
    fn a_window_the_tree_cannot_fit_overflows_and_keeps_its_leaf() {
        // 1000x600; each window needs 400x400. Two fit side by side at
        // 500x600; a third would stack under the first at 500x300, which
        // is too short. The newest inserted gives way; the others stay.
        let mut state = narrow_tree_state(1000);
        observe(
            &mut state,
            vec![
                window_needing(1, 1, Rect::new(0, 0, 400, 300), (400, 400)),
                window_needing(2, 1, Rect::new(500, 0, 400, 300), (400, 400)),
            ],
        );
        observe(
            &mut state,
            vec![
                window_needing(1, 1, Rect::new(0, 0, 500, 600), (400, 400)),
                window_needing(2, 1, Rect::new(500, 0, 500, 600), (400, 400)),
                window_needing(3, 1, Rect::new(10, 10, 400, 300), (400, 400)),
            ],
        );

        assert_eq!(
            state.constraint_overflow.get(&DisplayId(1)),
            Some(&vec![WindowId(3)]),
            "the newest window is the one that overflowed"
        );
        assert!(
            state.trees[&DisplayId(1)].contains(&WindowId(3)),
            "an overflowed window keeps its tree leaf"
        );
        assert_eq!(
            state.windows[&WindowId(3)].bounds,
            Rect::new(10, 10, 400, 300),
            "an overflowed window is left where it is, not squeezed"
        );
        assert_eq!(
            state.inventory[&WindowId(3)].eligibility,
            EligibilityReason::Eligible,
            "overflow is a planner outcome, not an eligibility change"
        );
        assert!(
            !state.session_floating.contains(&WindowId(3)),
            "overflow is distinct from a session-floating choice"
        );
        // The two established windows share the display between them.
        assert_eq!(
            state.windows[&WindowId(1)].bounds,
            Rect::new(0, 0, 500, 600)
        );
        assert_eq!(
            state.windows[&WindowId(2)].bounds,
            Rect::new(500, 0, 500, 600)
        );
    }

    #[test]
    fn an_overflowed_window_returns_to_its_leaf_when_the_tree_can_hold_it() {
        // 1000px wide; each window needs 600px, so only one fits at a
        // time and the newer overflows.
        let mut state = narrow_tree_state(1000);
        observe(
            &mut state,
            vec![
                window_needing(1, 1, Rect::new(0, 0, 400, 300), (600, 100)),
                window_needing(2, 1, Rect::new(500, 0, 400, 300), (600, 100)),
            ],
        );
        assert_eq!(
            state.constraint_overflow.get(&DisplayId(1)),
            Some(&vec![WindowId(2)]),
            "the fixture must overflow the newer window to mean anything"
        );
        state.effects.clear();
        state.persistence_intents.clear();

        // The first window closes: the tree can now hold the second.
        observe(
            &mut state,
            vec![window_needing(
                2,
                1,
                Rect::new(500, 0, 400, 300),
                (600, 100),
            )],
        );

        assert!(
            state.constraint_overflow.is_empty(),
            "nothing overflows once the constraints can be met"
        );
        assert_eq!(
            placements(&state),
            vec![(WindowId(2), DisplayId(1), Rect::new(0, 0, 1000, 600))],
            "the returning window is placed by the reflow, in the leaf it kept"
        );
        assert!(
            recorded_drafts(&state).is_empty(),
            "overflow and re-entry are passive; neither is an undo transaction"
        );
    }

    #[test]
    fn overflow_windows_are_not_directional_endpoints_in_tree_mode() {
        let mut state = narrow_tree_state(1000);
        observe(
            &mut state,
            vec![
                window_needing(1, 1, Rect::new(0, 0, 400, 300), (400, 400)),
                window_needing(2, 1, Rect::new(500, 0, 400, 300), (400, 400)),
                window_needing(3, 1, Rect::new(600, 10, 400, 300), (400, 400)),
            ],
        );
        assert_eq!(
            state.constraint_overflow.get(&DisplayId(1)),
            Some(&vec![WindowId(3)])
        );

        // Focus the overflowed window: it can neither be moved nor be
        // found from a neighbour.
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(3),
                display_id: DisplayId(1),
                bounds: Rect::new(600, 10, 400, 300),
            },
        );
        let tree_before = state.trees[&DisplayId(1)].clone();
        state.effects.clear();
        apply(
            &mut state,
            Event::DirectionalSwapRequested {
                direction: CardinalDirection::Left,
            },
        );
        assert_eq!(state.trees[&DisplayId(1)], tree_before);
        assert!(placements(&state).is_empty());

        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(2),
                display_id: DisplayId(1),
                bounds: Rect::new(500, 0, 500, 600),
            },
        );
        state.effects.clear();
        apply(
            &mut state,
            Event::DirectionalFocusRequested {
                direction: CardinalDirection::Right,
            },
        );
        assert!(
            state.effects.is_empty(),
            "the overflowed window to the right is not a focus target"
        );
    }

    #[test]
    fn overflow_reduces_gaps_before_it_removes_a_window() {
        // Two 490px-minimum windows on a 1000px display with 20px gaps:
        // undecorated they fit exactly.
        let mut state = narrow_tree_state(1000);
        state.resolved_config.gaps = mosaix_domain::Gaps::new(20, 12);
        observe(
            &mut state,
            vec![
                window_needing(1, 1, Rect::new(0, 0, 400, 300), (490, 100)),
                window_needing(2, 1, Rect::new(500, 0, 400, 300), (490, 100)),
            ],
        );

        assert!(state.constraint_overflow.is_empty());
        assert!(state.windows[&WindowId(1)].bounds.width >= 490);
        assert!(state.windows[&WindowId(2)].bounds.width >= 490);
    }

    #[test]
    fn balanced_mode_keeps_no_tree_at_all() {
        let mut state = tiling_state_with_saved_layout("unused", &[]);
        state.displays = vec![display(1, "DISPLAY1", 0)];

        observe(
            &mut state,
            vec![
                window_at(1, 1, Rect::new(0, 0, 400, 300)),
                window_at(2, 1, Rect::new(500, 0, 400, 300)),
            ],
        );

        assert!(
            state.trees.is_empty(),
            "the balanced grid is stateless; it must not accumulate structure"
        );
    }

    #[test]
    fn a_disconnected_display_takes_its_tree_with_it() {
        let mut state = tree_state();
        state.displays = vec![display(1, "DISPLAY1", 0), display(2, "DISPLAY2", 1920)];
        observe(
            &mut state,
            vec![
                window_at(1, 1, Rect::new(0, 0, 400, 300)),
                window_at(2, 2, Rect::new(1920, 0, 400, 300)),
            ],
        );
        assert_eq!(state.trees.len(), 2);

        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "DISPLAY1", 0)]),
        );

        assert_eq!(
            state.trees.keys().copied().collect::<Vec<_>>(),
            vec![DisplayId(1)],
            "a tree for a display nobody can see is structure with nowhere to go"
        );
    }

    #[test]
    fn a_storage_failure_leaves_the_committed_arrangement_intact() {
        // Durability is a promise about the database, not about the
        // desktop. Losing the first must not disturb the second, and must
        // not provoke compensating movement either.
        let mut state = state_with_saved_layout("half", &[(0.0, 0.0, 0.5, 1.0)]);
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![window_at(1, 1, Rect::new(0, 0, 600, 600))],
            },
        );
        apply(
            &mut state,
            Event::FocusDisplayRequested {
                display_id: DisplayId(1),
            },
        );
        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "half".to_owned(),
            },
        );
        assert!(
            !placements(&state).is_empty(),
            "the layout must have been applied for this test to mean anything"
        );
        let windows_before = state.windows.clone();
        let inventory_before = state.inventory.clone();
        state.effects.clear();

        apply(
            &mut state,
            Event::PersistenceHealthChanged(PersistenceHealth::Degraded {
                last_durable_revision: 3,
                reason: mosaix_persistence::PersistenceFailure::WriteFailed,
            }),
        );

        assert_eq!(
            state.windows, windows_before,
            "a storage failure must not move a window"
        );
        assert_eq!(
            state.inventory, inventory_before,
            "a storage failure must not forget a window"
        );
        assert!(
            state.effects.is_empty(),
            "a storage failure must emit no compensating effect"
        );
        assert_eq!(
            state.persistence_health,
            PersistenceHealth::Degraded {
                last_durable_revision: 3,
                reason: mosaix_persistence::PersistenceFailure::WriteFailed,
            },
            "the failure must still be reported honestly"
        );
    }

    #[test]
    fn saved_layout_uses_an_explicit_focused_display_without_a_focused_window() {
        let mut state = state_with_saved_layout("half", &[(0.0, 0.0, 0.5, 1.0)]);
        state.displays.push(display(2, "secondary", 1920));
        apply(
            &mut state,
            Event::WindowsObserved {
                windows: vec![window_at(2, 2, Rect::new(1920, 0, 600, 600))],
            },
        );
        state.focused_window = None;
        apply(
            &mut state,
            Event::FocusDisplayRequested {
                display_id: DisplayId(2),
            },
        );
        state.effects.clear();

        apply(
            &mut state,
            Event::SavedLayoutApplyRequested {
                name: "half".to_owned(),
            },
        );

        assert_eq!(
            placements(&state),
            vec![(WindowId(2), DisplayId(2), Rect::new(1920, 0, 960, 1080))]
        );
    }

    #[test]
    fn saved_layout_without_any_display_target_is_rejected_with_a_typed_reason() {
        let state = EngineState {
            resolved_config: ResolvedConfig {
                layouts: [("half".to_owned(), saved_layout(&[(0.0, 0.0, 0.5, 1.0)]))]
                    .into_iter()
                    .collect(),
                ..ResolvedConfig::default()
            },
            ..EngineState::default()
        };

        assert_eq!(
            plan_saved_layout(&state, "half"),
            Err(SavedLayoutRejection::NoFocusedDisplay)
        );
    }

    // ---- Logical workspaces -------------------------------------------

    fn ws(name: &str) -> WorkspaceName {
        WorkspaceName::new(name).unwrap()
    }

    /// Tree mode over `display_ids`, with configuration declaring
    /// `workspaces`. Every display that can be given a workspace has one
    /// before any window is observed, exactly as `spawn_engine` does.
    fn workspace_state(display_ids: &[isize], workspaces: &[&str]) -> EngineState {
        let mut state = tree_state();
        state.displays = display_ids
            .iter()
            .map(|id| display(*id, &format!("DISPLAY{id}"), (*id as i32 - 1) * 1920))
            .collect();
        state.focused_display = Some(DisplayId(display_ids[0]));
        state.resolved_config.workspaces = workspaces.iter().map(|name| ws(name)).collect();
        // A topology change re-selects from the config set, so it has to
        // agree with the resolved config or tiling would switch off.
        state.config_set.base = state.resolved_config.clone();
        sync_workspaces_from_config(&mut state);
        fill_empty_displays(&mut state);
        state
    }

    fn create(state: &mut EngineState, name: &str) -> WorkspaceCommandResult {
        apply(
            state,
            Event::WorkspaceCreateRequested {
                name: name.to_owned(),
            },
        );
        state.last_workspace_result.clone().unwrap()
    }

    fn focus_workspace(state: &mut EngineState, name: &str) -> WorkspaceCommandResult {
        apply(
            state,
            Event::WorkspaceFocusRequested {
                name: name.to_owned(),
            },
        );
        state.last_workspace_result.clone().unwrap()
    }

    /// A one-display state with experimental switching authorised: a
    /// matched profile requests it, its mapping displays `displayed`, and
    /// the adapter has verified a parking site.
    fn switching_state(workspaces: &[&str], displayed: &str) -> EngineState {
        let mut state = workspace_state(&[1], workspaces);
        let set = switching_set(
            &state.displays,
            workspaces,
            true,
            &[("DISPLAY1", displayed)],
        );
        apply(&mut state, Event::ConfigChanged(Box::new(set)));
        apply(
            &mut state,
            Event::ParkingCapabilityReported(ParkingCapability::Verified),
        );
        assert_eq!(
            state.workspace_switching_status(),
            WorkspaceSwitchingStatus::Experimental
        );
        state
    }

    /// Answers every durable acknowledgement and native move an in-flight
    /// switch is waiting on, the way an adapter would, and returns the
    /// new effect cursor.
    ///
    /// `fail` decides which moves the adapter could not carry out, so a
    /// test can fail exactly one park or restore and watch compensation
    /// rather than mocking a platform.
    fn settle_switch_from(
        state: &mut EngineState,
        mut cursor: usize,
        fail: &mut dyn FnMut(WindowId, ParkingStage) -> Option<String>,
    ) -> usize {
        let mut next_entry = 900;
        for _ in 0..64 {
            if let Some(pending) = state.pending_parking.first().cloned() {
                next_entry += 1;
                apply(
                    state,
                    Event::RecoveryEntryDurable {
                        token: pending.token,
                        entry_id: RecoveryEntryId(next_entry),
                    },
                );
                continue;
            }
            if cursor >= state.effects.len() {
                return cursor;
            }
            let effect = state.effects[cursor];
            cursor += 1;
            match effect {
                EngineEffect::ParkWindow {
                    window_id,
                    entry_id,
                } => match fail(window_id, ParkingStage::Park) {
                    None => apply(
                        state,
                        Event::WindowParked {
                            window_id,
                            entry_id,
                        },
                    ),
                    Some(reason) => apply(
                        state,
                        Event::WindowParkFailed {
                            window_id,
                            entry_id,
                            reason,
                        },
                    ),
                },
                EngineEffect::RestoreWindow {
                    window_id,
                    entry_id,
                } => match fail(window_id, ParkingStage::Restore) {
                    None => apply(state, Event::WindowRestored { window_id }),
                    Some(reason) => apply(
                        state,
                        Event::WindowRestoreFailed {
                            window_id,
                            entry_id,
                            reason,
                        },
                    ),
                },
                _ => {}
            }
        }
        panic!("the switch did not settle");
    }

    /// Focuses `name` and carries the switch it starts to completion with
    /// an adapter that never fails.
    fn switch_to(state: &mut EngineState, name: &str) -> WorkspaceCommandResult {
        let cursor = state.effects.len();
        focus_workspace(state, name);
        settle_switch_from(state, cursor, &mut |_, _| None);
        state.last_workspace_result.clone().unwrap()
    }

    fn saved_workspaces(state: &EngineState) -> Vec<&PersistedWorkspace> {
        state
            .persistence_intents
            .iter()
            .filter_map(|intent| match intent {
                PersistenceIntent::SaveWorkspace(workspace) => Some(workspace.as_ref()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn configuration_declares_the_pool_and_fills_displays_primary_first() {
        let state = workspace_state(&[2, 1], &["dev", "chat", "media"]);

        assert_eq!(
            state.workspaces.names(),
            vec![ws("chat"), ws("dev"), ws("media")]
        );
        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev")), (DisplayId(2), ws("chat"))],
            "the primary display is filled first, in the order configuration declares"
        );
        assert!(!state.workspaces.is_displayed(&ws("media")));
        for name in state.workspaces.names() {
            assert_eq!(
                state.workspaces.get(&name).unwrap().origin,
                WorkspaceOrigin::Configuration
            );
        }
    }

    #[test]
    fn a_display_the_pool_cannot_fill_arranges_its_windows_as_before() {
        let mut state = workspace_state(&[1, 2], &["main"]);
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "b.exe", 2, Rect::new(1920, 0, 400, 300)),
            ],
        );

        assert_eq!(
            state.workspaces.workspace_of(WindowId(1)),
            Some(&ws("main"))
        );
        assert_eq!(
            state.workspaces.workspace_of(WindowId(2)),
            None,
            "the engine never invents a workspace for the second display"
        );
        assert_eq!(
            arrangement(&state),
            vec![
                (1, Rect::new(0, 0, 1920, 1080)),
                (2, Rect::new(1920, 0, 1920, 1080))
            ],
            "the unfilled display still tiles what is on it"
        );
    }

    #[test]
    fn every_new_managed_window_joins_the_workspace_displayed_where_it_appears() {
        let mut state = workspace_state(&[1, 2], &["dev", "chat"]);
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "b.exe", 2, Rect::new(1920, 0, 400, 300)),
            ],
        );

        assert_eq!(state.workspaces.workspace_of(WindowId(1)), Some(&ws("dev")));
        assert_eq!(
            state.workspaces.workspace_of(WindowId(2)),
            Some(&ws("chat"))
        );
    }

    #[test]
    fn floating_windows_belong_to_a_workspace_and_excluded_windows_to_none() {
        let mut state = workspace_state(&[1], &["dev"]);
        let mut popup = app_window_at(2, "b.exe", 1, Rect::new(0, 0, 400, 300));
        popup.role = mosaix_domain::WindowRole::Popup;
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                popup,
            ],
        );
        apply(&mut state, Event::ToggleFloatingRequested);
        focus(&mut state, 1, Rect::new(0, 0, 400, 300));
        apply(&mut state, Event::ToggleFloatingRequested);

        assert_eq!(
            state.inventory[&WindowId(1)].eligibility,
            EligibilityReason::SessionFloating
        );
        assert_eq!(state.workspaces.workspace_of(WindowId(1)), Some(&ws("dev")));
        assert!(
            !state.inventory.contains_key(&WindowId(2)),
            "the built-in rules exclude popups"
        );
        assert_eq!(state.workspaces.workspace_of(WindowId(2)), None);
    }

    #[test]
    fn create_is_explicit_and_refuses_duplicates_and_invalid_names() {
        let mut state = workspace_state(&[1], &["dev"]);

        assert_eq!(
            create(&mut state, "  scratch "),
            WorkspaceCommandResult::Created(WorkspaceCreateApplied {
                name: ws("scratch")
            })
        );
        assert_eq!(
            state.workspaces.get(&ws("scratch")).unwrap().origin,
            WorkspaceOrigin::Command
        );
        assert!(!state.workspaces.is_displayed(&ws("scratch")));
        assert_eq!(
            create(&mut state, "DEV"),
            WorkspaceCommandResult::Refused(WorkspaceRefusal::AlreadyExists { name: ws("dev") })
        );
        assert_eq!(
            create(&mut state, ""),
            WorkspaceCommandResult::Refused(WorkspaceRefusal::InvalidName {
                reason: mosaix_domain::WorkspaceNameError::Empty
            })
        );
        assert_eq!(state.workspaces.len(), 2);
    }

    #[test]
    fn unknown_focus_move_and_delete_targets_are_typed_refusals_that_create_nothing() {
        let mut state = workspace_state(&[1], &["dev"]);

        assert_eq!(
            focus_workspace(&mut state, "typo"),
            WorkspaceCommandResult::Refused(WorkspaceRefusal::UnknownWorkspace {
                name: "typo".to_owned()
            })
        );
        apply(
            &mut state,
            Event::WorkspaceMoveRequested {
                name: "typo".to_owned(),
                display_id: DisplayId(1),
            },
        );
        assert_eq!(
            state.last_workspace_result,
            Some(WorkspaceCommandResult::Refused(
                WorkspaceRefusal::UnknownWorkspace {
                    name: "typo".to_owned()
                }
            ))
        );
        apply(
            &mut state,
            Event::WorkspaceDeleteRequested {
                name: "typo".to_owned(),
            },
        );
        assert_eq!(
            state.last_workspace_result,
            Some(WorkspaceCommandResult::Refused(
                WorkspaceRefusal::UnknownWorkspace {
                    name: "typo".to_owned()
                }
            ))
        );
        assert_eq!(state.workspaces.names(), vec![ws("dev")]);
        assert!(saved_workspaces(&state)
            .iter()
            .all(|w| w.name != ws("typo")));
    }

    #[test]
    fn a_rule_target_assigns_membership_and_an_unknown_target_is_recorded_not_created() {
        let mut state = workspace_state(&[1], &["dev", "chat"]);
        let rule = |id: &str, application: &str, workspace: &str| Rule {
            id: id.to_owned(),
            priority: 10,
            enabled: true,
            matcher: mosaix_rules::WindowMatcher {
                application_id: Some(application.to_owned()),
                application_regex: None,
                title_regex: None,
                native_class: None,
                class_regex: None,
                exe_path_regex: None,
                exe_path: None,
                role: None,
            },
            actions: mosaix_rules::RuleActions {
                manage: ManageAction::Tile,
                workspace: Some(workspace.to_owned()),
            },
        };
        apply(
            &mut state,
            Event::RulesChanged {
                rules: vec![
                    rule("chat-app", "slack.exe", "chat"),
                    rule("typo", "code.exe", "dv"),
                ],
            },
        );

        observe(
            &mut state,
            vec![
                app_window_at(1, "slack.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "code.exe", 1, Rect::new(0, 0, 400, 300)),
            ],
        );

        assert_eq!(
            state.workspaces.workspace_of(WindowId(1)),
            Some(&ws("chat")),
            "the rule sent the window to the hidden workspace"
        );
        assert_eq!(
            state.workspaces.workspace_of(WindowId(2)),
            Some(&ws("dev")),
            "an unknown target leaves the window in the workspace displayed where it appeared"
        );
        assert_eq!(
            state.rule_workspace_refusals,
            vec![RuleWorkspaceRefusal {
                window_id: WindowId(2),
                rule_id: Some("typo".to_owned()),
                workspace: "dv".to_owned(),
            }]
        );
        assert_eq!(state.workspaces.names(), vec![ws("chat"), ws("dev")]);
        assert_eq!(
            arrangement(&state),
            vec![
                (1, Rect::new(0, 0, 400, 300)),
                (2, Rect::new(0, 0, 1920, 1080))
            ],
            "a hidden workspace's window is not arranged; the displayed one's fills the display"
        );
    }

    #[test]
    fn focusing_a_hidden_workspace_displays_it_on_the_focused_display_and_arranges_its_windows() {
        let mut state = switching_state(&["dev"], "dev");
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "b.exe", 1, Rect::new(500, 0, 400, 300)),
            ],
        );
        create(&mut state, "chat");

        assert_eq!(
            switch_to(&mut state, "chat"),
            WorkspaceCommandResult::Focused(WorkspaceFocusApplied::Displayed {
                name: ws("chat"),
                display_id: DisplayId(1),
                replaced: Some(ws("dev")),
            })
        );
        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("chat"))]
        );
        assert!(
            state
                .workspaces
                .get(&ws("dev"))
                .unwrap()
                .stashed_tree
                .as_ref()
                .unwrap()
                .contains(&WindowId(1)),
            "the hidden workspace keeps its tree"
        );
        assert!(state
            .trees
            .get(&DisplayId(1))
            .is_none_or(|tree| tree.is_empty()));

        // A new window joins the displayed workspace and gets the whole
        // display; dev's windows stay where they were, unarranged.
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 960, 1080)),
                app_window_at(2, "b.exe", 1, Rect::new(960, 0, 960, 1080)),
                app_window_at(3, "c.exe", 1, Rect::new(10, 10, 400, 300)),
            ],
        );
        assert_eq!(
            state.workspaces.workspace_of(WindowId(3)),
            Some(&ws("chat"))
        );
        assert_eq!(
            arrangement(&state),
            vec![
                (1, Rect::new(0, 0, 960, 1080)),
                (2, Rect::new(960, 0, 960, 1080)),
                (3, Rect::new(0, 0, 1920, 1080)),
            ]
        );

        // Returning to dev brings its tree back exactly.
        assert_eq!(
            switch_to(&mut state, "dev"),
            WorkspaceCommandResult::Focused(WorkspaceFocusApplied::Displayed {
                name: ws("dev"),
                display_id: DisplayId(1),
                replaced: Some(ws("chat")),
            })
        );
        assert_eq!(
            state.trees[&DisplayId(1)].windows(),
            vec![&WindowId(1), &WindowId(2)]
        );
        assert!(state
            .workspaces
            .get(&ws("chat"))
            .unwrap()
            .stashed_tree
            .as_ref()
            .unwrap()
            .contains(&WindowId(3)));
    }

    #[test]
    fn focusing_a_workspace_displayed_elsewhere_focuses_its_last_window_and_does_not_move_it() {
        let mut state = workspace_state(&[1, 2], &["dev", "chat"]);
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 2, Rect::new(1920, 0, 400, 300)),
                app_window_at(2, "b.exe", 2, Rect::new(2400, 0, 400, 300)),
            ],
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(2),
                display_id: DisplayId(2),
                bounds: Rect::new(2400, 0, 400, 300),
            },
        );
        apply(
            &mut state,
            Event::FocusDisplayRequested {
                display_id: DisplayId(1),
            },
        );
        let effects_before = state.effects.len();

        assert_eq!(
            focus_workspace(&mut state, "chat"),
            WorkspaceCommandResult::Focused(WorkspaceFocusApplied::FocusedExisting {
                name: ws("chat"),
                display_id: DisplayId(2),
                focused_window: Some(WindowId(2)),
            })
        );
        assert_eq!(state.workspaces.display_of(&ws("chat")), Some(DisplayId(2)));
        assert_eq!(state.focused_display, Some(DisplayId(2)));
        assert_eq!(
            &state.effects[effects_before..],
            &[EngineEffect::FocusWindow {
                window_id: WindowId(2)
            }]
        );
    }

    #[test]
    fn focusing_a_hidden_workspace_needs_a_focused_display_and_refuses_while_paused() {
        let mut state = workspace_state(&[1], &["dev", "chat"]);
        state.focused_display = None;
        assert_eq!(
            focus_workspace(&mut state, "chat"),
            WorkspaceCommandResult::Refused(WorkspaceRefusal::NoFocusedDisplay)
        );

        state.focused_display = Some(DisplayId(1));
        state.paused = true;
        assert_eq!(
            focus_workspace(&mut state, "chat"),
            WorkspaceCommandResult::Refused(WorkspaceRefusal::Paused)
        );
        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev"))]
        );
    }

    #[test]
    fn moving_a_displayed_workspace_swaps_it_with_the_target_and_keeps_its_tree() {
        let mut state = workspace_state(&[1, 2], &["dev", "chat"]);
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "b.exe", 1, Rect::new(500, 0, 400, 300)),
                app_window_at(3, "c.exe", 2, Rect::new(1920, 0, 400, 300)),
            ],
        );
        focus(&mut state, 1, Rect::new(0, 0, 960, 1080));
        resize(&mut state, CardinalDirection::Right);
        let shape_before = state.trees[&DisplayId(1)].clone();
        assert_eq!(
            arrangement(&state)[0],
            (1, Rect::new(0, 0, 1056, 1080)),
            "the resize took"
        );

        apply(
            &mut state,
            Event::WorkspaceMoveRequested {
                name: "dev".to_owned(),
                display_id: DisplayId(2),
            },
        );

        assert_eq!(
            state.last_workspace_result,
            Some(WorkspaceCommandResult::Moved(WorkspaceMoveApplied {
                name: ws("dev"),
                from_display_id: DisplayId(1),
                to_display_id: DisplayId(2),
                swapped_with: Some(ws("chat")),
            }))
        );
        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("chat")), (DisplayId(2), ws("dev"))]
        );
        assert_eq!(
            state.trees[&DisplayId(2)],
            shape_before,
            "the tree travelled with the workspace, weights and all"
        );
        assert_eq!(
            arrangement(&state),
            vec![
                (1, Rect::new(1920, 0, 1056, 1080)),
                (2, Rect::new(2976, 0, 864, 1080)),
                (3, Rect::new(0, 0, 1920, 1080)),
            ]
        );
        assert_eq!(state.workspaces.workspace_of(WindowId(1)), Some(&ws("dev")));
        assert_eq!(
            state.workspaces.workspace_of(WindowId(3)),
            Some(&ws("chat"))
        );
    }

    #[test]
    fn a_move_refuses_a_hidden_workspace_an_unknown_display_and_the_same_display() {
        let mut state = workspace_state(&[1], &["dev", "chat"]);
        let attempt = |state: &mut EngineState, name: &str, display: isize| {
            apply(
                state,
                Event::WorkspaceMoveRequested {
                    name: name.to_owned(),
                    display_id: DisplayId(display),
                },
            );
            state.last_workspace_result.clone().unwrap()
        };

        assert_eq!(
            attempt(&mut state, "chat", 1),
            WorkspaceCommandResult::Refused(WorkspaceRefusal::NotDisplayed { name: ws("chat") })
        );
        assert_eq!(
            attempt(&mut state, "dev", 9),
            WorkspaceCommandResult::Refused(WorkspaceRefusal::UnknownDisplay {
                display_id: DisplayId(9)
            })
        );
        assert_eq!(
            attempt(&mut state, "dev", 1),
            WorkspaceCommandResult::Refused(WorkspaceRefusal::AlreadyDisplayedThere {
                name: ws("dev"),
                display_id: DisplayId(1)
            })
        );
    }

    #[test]
    fn delete_succeeds_only_for_a_hidden_empty_command_workspace() {
        let mut state = switching_state(&["dev"], "dev");
        observe(
            &mut state,
            vec![app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300))],
        );
        create(&mut state, "scratch");
        let attempt = |state: &mut EngineState, name: &str| {
            apply(
                state,
                Event::WorkspaceDeleteRequested {
                    name: name.to_owned(),
                },
            );
            state.last_workspace_result.clone().unwrap()
        };

        assert_eq!(
            attempt(&mut state, "dev"),
            WorkspaceCommandResult::Refused(WorkspaceRefusal::Displayed {
                name: ws("dev"),
                display_id: DisplayId(1)
            })
        );
        // Display scratch, give it a window, hide it again: now it owns a
        // live member.
        switch_to(&mut state, "scratch");
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "b.exe", 1, Rect::new(0, 0, 400, 300)),
            ],
        );
        switch_to(&mut state, "dev");
        assert_eq!(
            attempt(&mut state, "scratch"),
            WorkspaceCommandResult::Refused(WorkspaceRefusal::NotEmpty {
                name: ws("scratch"),
                live_members: 1,
                dormant_positions: 0
            })
        );
        // The member closes while scratch is hidden: it goes dormant, and
        // deletion still refuses, because the position is still owned.
        observe(
            &mut state,
            vec![app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300))],
        );
        assert_eq!(
            attempt(&mut state, "scratch"),
            WorkspaceCommandResult::Refused(WorkspaceRefusal::NotEmpty {
                name: ws("scratch"),
                live_members: 0,
                dormant_positions: 1
            })
        );
        // Removing the dormant position by displaying and pruning is a
        // tree command; here the stash is emptied directly to show the
        // last refusal was the only thing in the way.
        state
            .workspaces
            .get_mut(&ws("scratch"))
            .unwrap()
            .stashed_tree = None;
        assert_eq!(
            attempt(&mut state, "scratch"),
            WorkspaceCommandResult::Deleted(WorkspaceDeleteApplied {
                name: ws("scratch")
            })
        );
        assert!(state
            .persistence_intents
            .iter()
            .any(|intent| *intent == PersistenceIntent::DeleteWorkspace(ws("scratch"))));
    }

    #[test]
    fn a_vanished_display_hides_its_workspace_without_displacing_survivors() {
        let mut state = workspace_state(&[1, 2], &["dev", "chat"]);
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "b.exe", 2, Rect::new(1920, 0, 400, 300)),
            ],
        );

        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "DISPLAY1", 0)]),
        );

        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev"))]
        );
        let chat = state.workspaces.get(&ws("chat")).unwrap();
        assert!(chat.stashed_tree.as_ref().unwrap().contains(&WindowId(2)));
        assert_eq!(
            state.workspaces.workspace_of(WindowId(2)),
            Some(&ws("chat")),
            "the migrated window keeps its membership"
        );
        assert_eq!(
            state.trees[&DisplayId(1)].windows(),
            vec![&WindowId(1)],
            "the survivor's workspace is untouched; the hidden member is not arranged into it"
        );

        // Reconnecting reveals nothing: chat has a live member.
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![
                display(1, "DISPLAY1", 0),
                display(2, "DISPLAY2", 1920),
            ]),
        );
        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev"))]
        );
        assert_eq!(state.workspaces.displayed_on(DisplayId(2)), None);

        // An explicit focus is what brings it back.
        apply(
            &mut state,
            Event::FocusDisplayRequested {
                display_id: DisplayId(2),
            },
        );
        focus_workspace(&mut state, "chat");
        assert_eq!(state.workspaces.display_of(&ws("chat")), Some(DisplayId(2)));
        assert_eq!(
            arrangement(&state)
                .iter()
                .find(|(id, _)| *id == 2)
                .unwrap()
                .1,
            Rect::new(1920, 0, 1920, 1080)
        );
    }

    #[test]
    fn last_focused_is_forgotten_when_the_window_closes_and_focus_falls_back_to_a_member() {
        let mut state = workspace_state(&[1, 2], &["dev", "chat"]);
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 2, Rect::new(1920, 0, 400, 300)),
                app_window_at(2, "b.exe", 2, Rect::new(2400, 0, 400, 300)),
            ],
        );
        apply(
            &mut state,
            Event::WindowFocused {
                window_id: WindowId(2),
                display_id: DisplayId(2),
                bounds: Rect::new(2400, 0, 400, 300),
            },
        );
        observe(
            &mut state,
            vec![app_window_at(1, "a.exe", 2, Rect::new(1920, 0, 400, 300))],
        );
        apply(
            &mut state,
            Event::FocusDisplayRequested {
                display_id: DisplayId(1),
            },
        );

        assert_eq!(
            state.workspaces.get(&ws("chat")).unwrap().last_focused,
            None
        );
        assert_eq!(
            plan_workspace_focus(&state, "chat"),
            Ok(WorkspaceFocusApplied::FocusedExisting {
                name: ws("chat"),
                display_id: DisplayId(2),
                focused_window: Some(WindowId(1)),
            })
        );
    }

    #[test]
    fn workspace_changes_are_written_as_intents_with_display_origin_and_tree() {
        let mut state = workspace_state(&[1], &["dev"]);
        observe(
            &mut state,
            vec![app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300))],
        );
        let dev = saved_workspaces(&state).last().copied().unwrap().clone();
        assert_eq!(dev.name, ws("dev"));
        assert_eq!(dev.origin, WorkspaceOrigin::Configuration);
        assert_eq!(dev.displayed_fingerprint.as_deref(), Some("DISPLAY1"));
        assert_eq!(dev.tree.as_ref().unwrap().windows().len(), 1);
        assert!(
            !state
                .persistence_intents
                .iter()
                .any(|intent| matches!(intent, PersistenceIntent::SaveContainerTree { .. })),
            "a display with a workspace stores its tree under the workspace"
        );

        create(&mut state, "chat");
        let chat = saved_workspaces(&state).last().copied().unwrap().clone();
        assert_eq!(
            chat,
            PersistedWorkspace {
                name: ws("chat"),
                origin: WorkspaceOrigin::Command,
                displayed_fingerprint: None,
                tree: None,
            }
        );

        let before = state.persistence_intents.len();
        observe(
            &mut state,
            vec![app_window_at(1, "a.exe", 1, Rect::new(0, 0, 1920, 1080))],
        );
        assert_eq!(
            state.persistence_intents.len(),
            before,
            "a reflow that changed nothing writes nothing"
        );
    }

    #[test]
    fn restart_restores_command_workspaces_their_displays_and_their_trees() {
        let mut state = workspace_state(&[1, 2], &["dev"]);
        let b = app_window_at(1, "b.exe", 2, Rect::new(1920, 0, 960, 1080));
        let a = app_window_at(2, "a.exe", 2, Rect::new(2880, 0, 960, 1080));
        let stored_tree = {
            let mut tree = PersistedTree::new();
            tree.insert_first(WindowEvidence::capture(&a, 0, "DISPLAY2"));
            tree.split_root(
                SplitAxis::Horizontal,
                WindowEvidence::capture(&b, 0, "DISPLAY2"),
            );
            tree
        };

        apply(
            &mut state,
            Event::WorkspacesLoaded(vec![
                PersistedWorkspace {
                    name: ws("chat"),
                    origin: WorkspaceOrigin::Command,
                    displayed_fingerprint: Some("DISPLAY2".to_owned()),
                    tree: Some(stored_tree),
                },
                PersistedWorkspace {
                    name: ws("dev"),
                    origin: WorkspaceOrigin::Command,
                    displayed_fingerprint: Some("DISPLAY1".to_owned()),
                    tree: None,
                },
                PersistedWorkspace {
                    name: ws("old"),
                    origin: WorkspaceOrigin::Configuration,
                    displayed_fingerprint: Some("DISPLAY9".to_owned()),
                    tree: None,
                },
            ]),
        );

        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev")), (DisplayId(2), ws("chat"))],
            "each workspace went back to the display it was on"
        );
        assert_eq!(
            state.workspaces.get(&ws("dev")).unwrap().origin,
            WorkspaceOrigin::Configuration,
            "configuration still declares dev, so configuration owns it"
        );
        assert_eq!(
            state.workspaces.get(&ws("old")).unwrap().origin,
            WorkspaceOrigin::Command,
            "a workspace configuration stopped declaring becomes deletable by command"
        );
        assert!(!state.workspaces.is_displayed(&ws("old")));

        observe(&mut state, vec![b, a]);
        assert_eq!(
            state.trees[&DisplayId(2)].windows(),
            vec![&WindowId(2), &WindowId(1)],
            "the stored tree placed a.exe before b.exe regardless of observation order"
        );
    }

    #[test]
    fn a_removed_declaration_keeps_the_workspace_and_a_new_one_creates_it() {
        let mut state = workspace_state(&[1], &["dev", "chat"]);
        let mut set = state.config_set.clone();
        set.base.workspaces = vec![ws("dev"), ws("media")];
        set.base.tiling_mode = TilingMode::Tree;
        set.base.automatic_tiling_enabled = true;

        apply(&mut state, Event::ConfigChanged(Box::new(set)));

        assert_eq!(
            state.workspaces.names(),
            vec![ws("chat"), ws("dev"), ws("media")]
        );
        assert_eq!(
            state.workspaces.get(&ws("chat")).unwrap().origin,
            WorkspaceOrigin::Command
        );
        assert_eq!(
            state.workspaces.get(&ws("media")).unwrap().origin,
            WorkspaceOrigin::Configuration
        );
    }

    // ---- Experimental switching mappings ------------------------------

    fn switching_set(
        displays: &[Display],
        workspaces: &[&str],
        experimental: bool,
        mapping: &[(&str, &str)],
    ) -> ResolvedConfigSet {
        let config = ResolvedConfig {
            workspaces: workspaces.iter().map(|name| ws(name)).collect(),
            workspace_switching: Some(mosaix_config::ResolvedWorkspaceSwitching {
                experimental,
                displayed: mapping
                    .iter()
                    .map(|(display, name)| ((*display).to_owned(), ws(name)))
                    .collect(),
            }),
            automatic_tiling_enabled: true,
            tiling_mode: TilingMode::Tree,
            ..ResolvedConfig::default()
        };
        ResolvedConfigSet {
            base: ResolvedConfig {
                workspaces: config.workspaces.clone(),
                ..ResolvedConfig::default()
            },
            profiles: vec![ResolvedProfile {
                fingerprint: topology_fingerprint(displays),
                config,
            }],
        }
    }

    // ---- Workspace switch transaction ---------------------------------

    /// Every window currently at the parking site, in window-id order:
    /// the map is keyed by handle, so its own iteration order says
    /// nothing.
    fn sorted_parked(state: &EngineState) -> Vec<WindowId> {
        let mut parked: Vec<WindowId> = state.parked_windows.keys().copied().collect();
        parked.sort_by_key(|window_id| window_id.0);
        parked
    }

    /// A switching-authorised state with `dev` displayed and two windows
    /// on it, plus a hidden `chat`.
    fn switch_fixture() -> EngineState {
        let mut state = switching_state(&["dev"], "dev");
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "b.exe", 1, Rect::new(500, 0, 400, 300)),
            ],
        );
        create(&mut state, "chat");
        state
    }

    #[test]
    fn a_switch_parks_the_outgoing_windows_before_the_assignment_changes() {
        let mut state = switch_fixture();
        let cursor = state.effects.len();

        let started = focus_workspace(&mut state, "chat");

        assert_eq!(
            started,
            WorkspaceCommandResult::Focused(WorkspaceFocusApplied::SwitchStarted {
                name: ws("chat"),
                display_id: DisplayId(1),
                replaced: Some(ws("dev")),
                parking: 2,
                restoring: 0,
            })
        );
        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev"))],
            "nothing is displayed differently until every window has moved"
        );
        assert_eq!(
            state.pending_parking.len(),
            2,
            "both windows are waiting on durable recovery data"
        );
        assert!(
            park_effects(&state).is_empty(),
            "no window leaves the screen before its way back is on disk"
        );

        settle_switch_from(&mut state, cursor, &mut |_, _| None);

        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("chat"))]
        );
        assert_eq!(sorted_parked(&state), vec![WindowId(1), WindowId(2)]);
        assert!(state.switch.is_none() && state.switch_degraded.is_none());
        assert_eq!(
            state.last_workspace_result,
            Some(WorkspaceCommandResult::Focused(
                WorkspaceFocusApplied::Displayed {
                    name: ws("chat"),
                    display_id: DisplayId(1),
                    replaced: Some(ws("dev")),
                }
            ))
        );
    }

    #[test]
    fn a_switch_back_restores_the_target_workspaces_parked_windows_and_its_focus() {
        let mut state = switch_fixture();
        switch_to(&mut state, "chat");
        // Dev's last-focused window is the one to come back to.
        state.workspaces.note_focus(WindowId(2));
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "b.exe", 1, Rect::new(500, 0, 400, 300)),
                app_window_at(3, "c.exe", 1, Rect::new(10, 10, 400, 300)),
            ],
        );
        let cursor = state.effects.len();

        switch_to(&mut state, "dev");

        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev"))]
        );
        assert_eq!(
            sorted_parked(&state),
            vec![WindowId(3)],
            "chat's window took dev's place at the parking site"
        );
        let effects: Vec<EngineEffect> = state.effects[cursor..].to_vec();
        let park_at = effects
            .iter()
            .position(|effect| matches!(effect, EngineEffect::ParkWindow { .. }))
            .expect("the outgoing window parks");
        let restore_at = effects
            .iter()
            .position(|effect| matches!(effect, EngineEffect::RestoreWindow { .. }))
            .expect("the target's window comes back");
        assert!(
            park_at < restore_at,
            "the outgoing workspace leaves the screen before the target arrives on it"
        );
        assert!(
            effects.contains(&EngineEffect::FocusWindow {
                window_id: WindowId(2)
            }),
            "the target workspace's last-focused window gets the attention back"
        );
    }

    #[test]
    fn a_switch_that_moves_no_window_needs_no_parking_site() {
        // An empty outgoing workspace and an empty target: nothing leaves
        // the screen, so nothing about parking applies and the switch is
        // pure bookkeeping.
        let mut state = workspace_state(&[1], &["dev"]);
        create(&mut state, "chat");

        assert_eq!(
            focus_workspace(&mut state, "chat"),
            WorkspaceCommandResult::Focused(WorkspaceFocusApplied::Displayed {
                name: ws("chat"),
                display_id: DisplayId(1),
                replaced: Some(ws("dev")),
            })
        );
        assert_eq!(state.parking_capability, ParkingCapability::Unverified);
        assert!(state.pending_parking.is_empty());
    }

    #[test]
    fn a_switch_that_would_move_a_window_is_refused_without_authorised_switching() {
        let mut state = workspace_state(&[1], &["dev"]);
        observe(
            &mut state,
            vec![app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300))],
        );
        create(&mut state, "chat");

        let refused = focus_workspace(&mut state, "chat");

        assert_eq!(
            refused,
            WorkspaceCommandResult::Refused(WorkspaceRefusal::SwitchingNotAuthorised {
                status: "disabled".to_owned(),
                reason: Some(
                    "no matched topology profile requests it, and base config cannot".to_owned()
                ),
            })
        );
        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev"))],
            "a refusal moves nothing and changes no assignment"
        );
        assert!(state.pending_parking.is_empty());
    }

    #[test]
    fn switch_preflight_refuses_a_full_screen_member_and_moves_nothing() {
        let mut state = switching_state(&["dev"], "dev");
        let mut full_screen = app_window_at(1, "a.exe", 1, Rect::new(0, 0, 1920, 1080));
        full_screen.lifecycle = WindowLifecycle::Fullscreen;
        observe(&mut state, vec![full_screen]);
        create(&mut state, "chat");

        assert_eq!(
            focus_workspace(&mut state, "chat"),
            WorkspaceCommandResult::Refused(WorkspaceRefusal::FullscreenMember {
                name: ws("dev"),
                window_id: WindowId(1),
            })
        );
        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev"))]
        );
        assert!(state.pending_parking.is_empty());
    }

    #[test]
    fn a_minimized_member_stays_minimized_and_is_never_parked_for_a_switch() {
        let mut state = switching_state(&["dev"], "dev");
        let mut minimized = app_window_at(2, "b.exe", 1, Rect::new(500, 0, 400, 300));
        minimized.lifecycle = WindowLifecycle::Minimized;
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                minimized,
            ],
        );
        create(&mut state, "chat");

        let plan = plan_workspace_switch(&state, "chat").expect("the switch is authorised");

        assert_eq!(
            plan.park,
            vec![WindowId(1)],
            "a minimized window occupies no screen, so it is left as it is"
        );

        switch_to(&mut state, "chat");

        assert!(!state.parked_windows.contains_key(&WindowId(2)));
    }

    #[test]
    fn a_switch_is_refused_while_the_state_database_is_degraded() {
        let mut state = switch_fixture();
        state.persistence_health = PersistenceHealth::Degraded {
            last_durable_revision: 3,
            reason: mosaix_persistence::PersistenceFailure::WriteFailed,
        };

        // Durability is what authorises parking at all, so the status
        // itself falls back to `requested` before the switch is reached.
        assert_eq!(
            focus_workspace(&mut state, "chat"),
            WorkspaceCommandResult::Refused(WorkspaceRefusal::SwitchingNotAuthorised {
                status: "requested".to_owned(),
                reason: Some("persistence_degraded".to_owned()),
            })
        );
        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev"))]
        );
    }

    #[test]
    fn a_switch_refuses_a_window_already_waiting_on_an_explicit_park() {
        // Two recovery entries for one window would leave the ledger with
        // an open row nobody restores, which the next session probes.
        let mut state = switch_fixture();
        apply(
            &mut state,
            Event::ParkingAuthorizationRequested {
                window_id: WindowId(2),
            },
        );
        assert_eq!(state.pending_parking.len(), 1);

        let refused = plan_workspace_switch(&state, "chat");

        assert_eq!(
            refused,
            Err(WorkspaceRefusal::MemberNotParkable {
                name: ws("dev"),
                window_id: WindowId(2),
                reason: ParkingRefusal::AlreadyPending {
                    window_id: WindowId(2)
                },
            })
        );
        assert_eq!(
            focus_workspace(&mut state, "chat"),
            WorkspaceCommandResult::Refused(WorkspaceRefusal::MemberNotParkable {
                name: ws("dev"),
                window_id: WindowId(2),
                reason: ParkingRefusal::AlreadyPending {
                    window_id: WindowId(2)
                },
            }),
            "the refusal reaches the command, so nothing moves"
        );
        assert_eq!(
            state.pending_parking.len(),
            1,
            "no second recovery entry was requested for the same window"
        );
    }

    #[test]
    fn a_second_switch_is_refused_while_one_is_in_flight() {
        let mut state = switch_fixture();
        create(&mut state, "media");
        focus_workspace(&mut state, "chat");
        assert!(state.switch.is_some());

        assert_eq!(
            focus_workspace(&mut state, "media"),
            WorkspaceCommandResult::Refused(WorkspaceRefusal::SwitchInFlight {
                display_id: DisplayId(1),
            })
        );
    }

    #[test]
    fn a_workspace_move_is_refused_while_a_switch_is_in_flight() {
        let mut state = switch_fixture();
        focus_workspace(&mut state, "chat");
        assert!(state.switch.is_some());

        let refused = plan_workspace_move(&state, "dev", DisplayId(1));

        assert_eq!(
            refused,
            Err(WorkspaceRefusal::SwitchInFlight {
                display_id: DisplayId(1)
            }),
            "exchanging what two monitors show under a switch would leave it compensating onto a display that changed underneath it"
        );
    }

    #[test]
    fn a_failed_park_compensates_and_leaves_the_original_assignment() {
        let mut state = switch_fixture();
        let cursor = state.effects.len();

        focus_workspace(&mut state, "chat");
        settle_switch_from(&mut state, cursor, &mut |window_id, stage| {
            (window_id == WindowId(2) && stage == ParkingStage::Park)
                .then(|| "the window refused to move".to_owned())
        });

        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev"))],
            "a failed switch preserves the original displayed assignment"
        );
        assert!(
            state.parked_windows.is_empty(),
            "the window that had parked came back"
        );
        assert!(state.switch.is_none());
        assert_eq!(state.switch_degraded, None);
        assert_eq!(
            state.last_workspace_result,
            Some(WorkspaceCommandResult::SwitchFailed(
                mosaix_domain::WorkspaceSwitchFailed {
                    name: ws("chat"),
                    display_id: DisplayId(1),
                    reason: "the window refused to move".to_owned(),
                    compensated: true,
                    stranded_windows: Vec::new(),
                }
            ))
        );
    }

    #[test]
    fn a_failed_restore_parks_again_what_it_restored_and_compensates() {
        // Get chat displayed with dev's windows parked, then switch back
        // and fail restoring the second of them.
        let mut state = switch_fixture();
        switch_to(&mut state, "chat");
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "b.exe", 1, Rect::new(500, 0, 400, 300)),
                app_window_at(3, "c.exe", 1, Rect::new(10, 10, 400, 300)),
            ],
        );
        let cursor = state.effects.len();

        focus_workspace(&mut state, "dev");
        settle_switch_from(&mut state, cursor, &mut |window_id, stage| {
            (window_id == WindowId(2) && stage == ParkingStage::Restore)
                .then(|| "the window would not come back".to_owned())
        });

        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("chat"))],
            "chat is still displayed; the switch changed nothing"
        );
        assert_eq!(
            sorted_parked(&state),
            vec![WindowId(1), WindowId(2)],
            "both of dev's windows are back at the parking site"
        );
        assert!(
            !state.parked_windows.contains_key(&WindowId(3)),
            "chat's window came back off the parking site"
        );
        assert_eq!(state.switch_degraded, None);
    }

    #[test]
    fn a_failed_compensation_enters_workspace_switch_degraded_and_blocks_switching() {
        let mut state = switch_fixture();
        create(&mut state, "media");
        let cursor = state.effects.len();

        // Window 2 refuses to park, which cancels the switch -- and then
        // window 1 refuses to come back, which compensation cannot fix.
        focus_workspace(&mut state, "chat");
        settle_switch_from(
            &mut state,
            cursor,
            &mut |window_id, stage| match (window_id, stage) {
                (WindowId(2), ParkingStage::Park) => Some("the window refused to move".to_owned()),
                (WindowId(1), ParkingStage::Restore) => {
                    Some("the window would not come back".to_owned())
                }
                _ => None,
            },
        );

        let degraded = state
            .switch_degraded
            .clone()
            .expect("compensation could not put every window back");
        assert_eq!(degraded.display_id, DisplayId(1));
        assert_eq!(degraded.target, ws("chat"));
        assert_eq!(degraded.outgoing, Some(ws("dev")));
        assert_eq!(degraded.stranded_windows, vec![WindowId(1)]);
        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev"))],
            "the displayed assignment is still the one the switch started from"
        );

        assert_eq!(
            focus_workspace(&mut state, "media"),
            WorkspaceCommandResult::Refused(WorkspaceRefusal::SwitchDegraded {
                stranded_windows: 1
            }),
            "further switching is blocked until the stranded window is reconciled"
        );
    }

    #[test]
    fn restoring_the_stranded_windows_unblocks_switching() {
        let mut state = switch_fixture();
        create(&mut state, "media");
        let cursor = state.effects.len();
        focus_workspace(&mut state, "chat");
        settle_switch_from(
            &mut state,
            cursor,
            &mut |window_id, stage| match (window_id, stage) {
                (WindowId(2), ParkingStage::Park) => Some("the window refused to move".to_owned()),
                (WindowId(1), ParkingStage::Restore) => {
                    Some("the window would not come back".to_owned())
                }
                _ => None,
            },
        );
        assert!(state.switch_degraded.is_some());

        assert_eq!(
            plan_workspace_switch_restore(&state),
            WorkspaceSwitchRestoreResult::Requested {
                windows: vec![WindowId(1)]
            }
        );
        let cursor = state.effects.len();
        apply(&mut state, Event::WorkspaceSwitchRestoreRequested);
        settle_switch_from(&mut state, cursor, &mut |_, _| None);

        assert_eq!(state.switch_degraded, None);
        assert!(state.parked_windows.is_empty());
        assert!(
            plan_workspace_switch(&state, "media").is_ok(),
            "switching is available again"
        );
    }

    /// Strands a window on the *visible* side of the boundary: its
    /// re-park failed during compensation, so it is sitting on a monitor
    /// that shows another workspace.
    ///
    /// Returns the state with chat displayed, window 1 stranded and
    /// visible while belonging to hidden dev, window 2 parked, and
    /// window 3 back on screen as chat's member.
    fn stranded_visible_window() -> EngineState {
        let mut state = switch_fixture();
        switch_to(&mut state, "chat");
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "b.exe", 1, Rect::new(500, 0, 400, 300)),
                app_window_at(3, "c.exe", 1, Rect::new(10, 10, 400, 300)),
            ],
        );
        let cursor = state.effects.len();
        focus_workspace(&mut state, "dev");
        settle_switch_from(&mut state, cursor, &mut |window_id, stage| {
            match (window_id, stage) {
                // Dev's second window will not come back, which cancels
                // the switch...
                (WindowId(2), ParkingStage::Restore) => {
                    Some("the window would not come back".to_owned())
                }
                // ...and the first one will not go back to the parking
                // site, which is what strands it in plain sight.
                (WindowId(1), ParkingStage::Park) => {
                    Some("the window would not park again".to_owned())
                }
                _ => None,
            }
        });
        state
    }

    #[test]
    fn a_window_stranded_in_plain_sight_is_parked_by_the_reconcile_not_declared_fine() {
        let mut state = stranded_visible_window();

        let degraded = state
            .switch_degraded
            .clone()
            .expect("compensation could not put window 1 back");
        assert_eq!(degraded.stranded_windows, vec![WindowId(1)]);
        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("chat"))],
            "the switch was cancelled, so chat is still displayed"
        );
        assert!(
            !state.parked_windows.contains_key(&WindowId(1)),
            "the stranded window is visible on a monitor showing chat, \
             while belonging to hidden dev"
        );

        // The bug this guards: filtering the stranded set down to what is
        // still parked emptied it, so the reconcile answered "reconciled",
        // unblocked switching, and left window 1 mixed in with chat.
        assert_eq!(
            plan_workspace_switch_restore(&state),
            WorkspaceSwitchRestoreResult::Requested {
                windows: vec![WindowId(1)]
            },
            "a stranded window owes a move in whichever direction its \
             workspace requires, not only a restore"
        );

        let cursor = state.effects.len();
        apply(&mut state, Event::WorkspaceSwitchRestoreRequested);

        assert!(
            state.switch_degraded.is_some(),
            "the condition stands until the window has actually landed"
        );
        assert_eq!(
            state.pending_parking.len(),
            1,
            "it leaves the screen the only way any window does: ledger first"
        );

        settle_switch_from(&mut state, cursor, &mut |_, _| None);

        assert_eq!(state.switch_degraded, None);
        assert_eq!(sorted_parked(&state), vec![WindowId(1), WindowId(2)]);
        assert!(
            plan_workspace_switch(&state, "dev").is_ok(),
            "switching is available again only now"
        );
    }

    #[test]
    fn a_failed_reconcile_leaves_the_window_stranded_and_switching_blocked() {
        let mut state = stranded_visible_window();
        // The parking site is gone, so the reconcile cannot put window 1
        // where it belongs and must not pretend otherwise.
        apply(
            &mut state,
            Event::ParkingCapabilityReported(ParkingCapability::Refused {
                reason: "every edge is covered by a display".to_owned(),
            }),
        );

        apply(&mut state, Event::WorkspaceSwitchRestoreRequested);

        assert_eq!(
            state
                .switch_degraded
                .as_ref()
                .map(|degraded| degraded.stranded_windows.clone()),
            Some(vec![WindowId(1)]),
            "an explicit failure, not a cleared condition"
        );
        assert!(state.pending_parking.is_empty());
        assert_eq!(
            state.last_parking_refusal,
            Some(ParkingRefusal::ParkingRefused {
                reason: "every edge is covered by a display".to_owned()
            })
        );
    }

    #[test]
    fn a_committed_switch_records_the_prior_assignment_and_placements_as_one_transaction() {
        let mut state = switch_fixture();

        switch_to(&mut state, "chat");

        let drafts = recorded_drafts(&state);
        assert_eq!(drafts.len(), 1, "one command, one undo transaction");
        let draft = drafts[0];
        assert_eq!(draft.command, "workspace-focus chat");
        assert_eq!(
            draft.prior_assignments,
            vec![UndoAssignment {
                display_fingerprint: "DISPLAY1".to_owned(),
                workspace: Some("dev".to_owned()),
            }]
        );
        assert_eq!(
            draft
                .members
                .iter()
                .map(|member| member.prior_placement)
                .collect::<Vec<_>>(),
            vec![Rect::new(0, 0, 960, 1080), Rect::new(960, 0, 960, 1080)],
            "each parked window's placement before the switch is what undo restores"
        );
    }

    #[test]
    fn undoing_a_switch_switches_back_as_a_fresh_guarded_transaction() {
        let mut state = switch_fixture();
        switch_to(&mut state, "chat");
        let draft = recorded_drafts(&state).last().copied().cloned().unwrap();
        state.newest_undo = Some(stored(&draft, 7));
        let cursor = state.effects.len();

        apply(&mut state, Event::UndoRequested);
        settle_switch_from(&mut state, cursor, &mut |_, _| None);

        assert!(matches!(
            state.last_undo_result,
            Some(UndoResult::Applied(_))
        ));
        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev"))],
            "the display is back on the workspace it showed before the command"
        );
        assert!(
            state.parked_windows.is_empty(),
            "dev's windows came off the parking site"
        );
        assert_eq!(
            recorded_drafts(&state).len(),
            1,
            "undo is not itself undoable, so the switch back records nothing"
        );
    }

    #[test]
    fn undo_is_refused_when_the_switch_back_would_be() {
        let mut state = switch_fixture();
        switch_to(&mut state, "chat");
        let draft = recorded_drafts(&state).last().copied().cloned().unwrap();
        state.newest_undo = Some(stored(&draft, 7));
        // The parking site is gone, so dev's parked windows have no
        // verified way back and the switch that would bring them is
        // refused before undo moves anything.
        apply(
            &mut state,
            Event::ParkingCapabilityReported(ParkingCapability::Refused {
                reason: "every edge is covered by a display".to_owned(),
            }),
        );

        apply(&mut state, Event::UndoRequested);

        let Some(UndoResult::Refused(UndoRefusal::WorkspaceSwitchRefused { reason, .. })) =
            &state.last_undo_result
        else {
            panic!(
                "undo refused for the switch's own reason: {:?}",
                state.last_undo_result
            );
        };
        assert!(
            matches!(
                reason,
                WorkspaceRefusal::SwitchingNotAuthorised { status, .. } if status == "unavailable"
            ),
            "the switch's own typed refusal survives into undo: {reason:?}"
        );
        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("chat"))],
            "a refused undo changes nothing"
        );
    }

    // ---- Hidden-workspace lifecycle and recovery ----------------------

    /// A two-display state with experimental switching authorised, `dev`
    /// on display 1 and `chat` on display 2.
    fn two_display_switching_state() -> EngineState {
        let mut state = workspace_state(&[1, 2], &["dev", "chat"]);
        let set = switching_set(
            &state.displays,
            &["dev", "chat"],
            true,
            &[("DISPLAY1", "dev"), ("DISPLAY2", "chat")],
        );
        apply(&mut state, Event::ConfigChanged(Box::new(set)));
        apply(
            &mut state,
            Event::ParkingCapabilityReported(ParkingCapability::Verified),
        );
        state
    }

    /// The recovery drafts the reducer has asked to be made durable, in
    /// the order it asked.
    fn recorded_recovery(state: &EngineState) -> Vec<&RecoveryDraft> {
        state
            .persistence_intents
            .iter()
            .filter_map(|intent| match intent {
                PersistenceIntent::RecordRecovery { draft, .. } => Some(&**draft),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn disconnecting_a_monitor_parks_the_windows_of_the_workspace_it_took_with_it() {
        let mut state = two_display_switching_state();
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "b.exe", 2, Rect::new(1920, 0, 400, 300)),
            ],
        );
        assert_eq!(
            state.workspaces.workspace_of(WindowId(2)),
            Some(&ws("chat"))
        );
        let cursor = state.effects.len();

        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "DISPLAY1", 0)]),
        );

        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev"))],
            "the surviving display keeps the workspace it showed"
        );
        assert!(
            !state.workspaces.is_displayed(&ws("chat")),
            "the vanished display's workspace is hidden, not moved onto a survivor"
        );
        let drafts = recorded_recovery(&state);
        assert_eq!(drafts.len(), 1, "one window has to leave the screen");
        assert_eq!(drafts[0].native_handle, 2);
        assert_eq!(
            drafts[0].original_display_fingerprint, "DISPLAY1",
            "the window is recorded against the survivor it migrated to"
        );

        settle_switch_from(&mut state, cursor, &mut |_, _| None);

        assert_eq!(sorted_parked(&state), vec![WindowId(2)]);
        assert_eq!(
            state.workspaces.workspace_of(WindowId(2)),
            Some(&ws("chat")),
            "membership is unchanged; only where the window sits is"
        );
    }

    #[test]
    fn reconnecting_a_monitor_reveals_a_hidden_workspace_only_when_the_profile_maps_it() {
        let mut state = two_display_switching_state();
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "b.exe", 2, Rect::new(1920, 0, 400, 300)),
            ],
        );
        let cursor = state.effects.len();
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "DISPLAY1", 0)]),
        );
        settle_switch_from(&mut state, cursor, &mut |_, _| None);
        assert_eq!(sorted_parked(&state), vec![WindowId(2)]);

        // The profile's mapping names both displays, so reconnecting the
        // second one is exactly the case where a valid topology profile
        // selects a workspace -- and chat goes back to display 2.
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![
                display(1, "DISPLAY1", 0),
                display(2, "DISPLAY2", 1920),
            ]),
        );

        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev")), (DisplayId(2), ws("chat"))],
            "the profile's mapping is what selects a workspace on reconnect"
        );
    }

    #[test]
    fn reconnecting_a_monitor_leaves_a_hidden_workspace_hidden_when_nothing_selects_it() {
        // No profile mapping, so nothing selects a workspace for the
        // display that comes back: a hidden workspace with windows stays
        // hidden rather than being revealed by the reconnect itself.
        let mut state = workspace_state(&[1, 2], &["dev", "chat"]);
        apply(
            &mut state,
            Event::ParkingCapabilityReported(ParkingCapability::Verified),
        );
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "b.exe", 2, Rect::new(1920, 0, 400, 300)),
            ],
        );
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "DISPLAY1", 0)]),
        );
        assert!(!state.workspaces.is_displayed(&ws("chat")));

        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![
                display(1, "DISPLAY1", 0),
                display(2, "DISPLAY2", 1920),
            ]),
        );

        assert!(
            !state.workspaces.is_displayed(&ws("chat")),
            "a reconnect reveals nothing on its own"
        );
        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev"))]
        );
    }

    #[test]
    fn a_rule_sending_a_new_window_to_a_hidden_workspace_parks_it_without_switching_or_focus() {
        let mut state = switching_state(&["dev"], "dev");
        create(&mut state, "chat");
        state.rules = vec![Rule {
            id: "chat".to_owned(),
            priority: 10,
            enabled: true,
            matcher: mosaix_rules::WindowMatcher {
                application_id: Some("slack.exe".to_owned()),
                application_regex: None,
                title_regex: None,
                native_class: None,
                class_regex: None,
                exe_path_regex: None,
                exe_path: None,
                role: None,
            },
            actions: mosaix_rules::RuleActions {
                manage: ManageAction::Tile,
                workspace: Some("chat".to_owned()),
            },
        }];
        let cursor = state.effects.len();

        observe(
            &mut state,
            vec![app_window_at(1, "slack.exe", 1, Rect::new(0, 0, 400, 300))],
        );

        assert_eq!(
            state.workspaces.workspace_of(WindowId(1)),
            Some(&ws("chat"))
        );
        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev"))],
            "a rule target never switches the displayed workspace"
        );
        let drafts = recorded_recovery(&state);
        assert_eq!(
            drafts.len(),
            1,
            "the way back is recorded before the window leaves the screen"
        );
        assert_eq!(state.pending_parking.len(), 1);
        assert!(
            park_effects(&state).is_empty(),
            "nothing parks until the entry is durable"
        );

        settle_switch_from(&mut state, cursor, &mut |_, _| None);

        assert_eq!(sorted_parked(&state), vec![WindowId(1)]);
        assert!(
            !state.effects[cursor..]
                .iter()
                .any(|effect| matches!(effect, EngineEffect::FocusWindow { .. })),
            "a window parked into a hidden workspace never takes the focus"
        );
    }

    #[test]
    fn a_minimized_member_stays_unparked_while_hidden_and_parks_when_its_application_restores_it() {
        let mut state = switching_state(&["dev"], "dev");
        create(&mut state, "chat");
        let mut minimized = app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300));
        minimized.lifecycle = WindowLifecycle::Minimized;
        observe(&mut state, vec![minimized.clone()]);
        // Put the window in chat, which is hidden.
        let chat = ws("chat");
        state.workspaces.assign(WindowId(1), &chat);
        let cursor = state.effects.len();
        observe(&mut state, vec![minimized]);

        assert!(
            recorded_recovery(&state).is_empty(),
            "a minimized window occupies no screen and is left as it is"
        );

        // The application restores it: it is on screen now, in a hidden
        // workspace, so it has to leave -- with its way back written
        // first, like every other park.
        observe(
            &mut state,
            vec![app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300))],
        );

        let drafts = recorded_recovery(&state);
        assert_eq!(drafts.len(), 1);
        assert_eq!(
            drafts[0].show_state,
            mosaix_domain::recovery::ShowState::Normal
        );
        assert!(
            state.parked_windows.is_empty(),
            "nothing has moved while the entry is still pending"
        );

        settle_switch_from(&mut state, cursor, &mut |_, _| None);

        assert_eq!(sorted_parked(&state), vec![WindowId(1)]);
    }

    #[test]
    fn a_maximized_window_records_its_show_state_and_normal_bounds_before_parking() {
        let mut state = switching_state(&["dev"], "dev");
        observe(
            &mut state,
            vec![app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300))],
        );
        create(&mut state, "chat");
        // The reflow placed it; maximizing it does not change the
        // placement the reducer intends, which is what it goes back to.
        let normal = state.windows[&WindowId(1)].bounds;
        let mut maximized = app_window_at(1, "a.exe", 1, Rect::new(-8, -8, 1936, 1096));
        maximized.lifecycle = WindowLifecycle::Maximized;
        observe(&mut state, vec![maximized]);

        focus_workspace(&mut state, "chat");

        let drafts = recorded_recovery(&state);
        assert_eq!(drafts.len(), 1);
        assert_eq!(
            drafts[0].show_state,
            mosaix_domain::recovery::ShowState::Maximized,
            "the show state is on disk before the window is taken out of it"
        );
        assert_eq!(
            drafts[0].normal_bounds, normal,
            "the size it returns to is recorded, not its maximized extent"
        );
        assert_eq!(
            drafts[0].visible_bounds,
            Rect::new(-8, -8, 1936, 1096),
            "where it was on screen is recorded too"
        );
    }

    #[test]
    fn a_full_screen_member_of_a_hidden_workspace_is_never_forced_out_of_full_screen() {
        let mut state = switching_state(&["dev"], "dev");
        create(&mut state, "chat");
        let mut full_screen = app_window_at(1, "a.exe", 1, Rect::new(0, 0, 1920, 1080));
        full_screen.lifecycle = WindowLifecycle::Fullscreen;
        observe(&mut state, vec![full_screen.clone()]);
        let chat = ws("chat");
        state.workspaces.assign(WindowId(1), &chat);

        observe(&mut state, vec![full_screen]);

        assert!(
            recorded_recovery(&state).is_empty(),
            "nothing forces a full-screen window out of full-screen"
        );
        assert!(state.parked_windows.is_empty());
    }

    #[test]
    fn stored_assignments_are_reapplied_by_parking_through_the_ledger() {
        // What startup does: the pool comes back, and every window whose
        // workspace the stored assignment leaves hidden goes to the
        // parking site through the same durable-first path a switch uses.
        let mut state = switching_state(&["dev"], "dev");
        create(&mut state, "chat");
        observe(
            &mut state,
            vec![app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300))],
        );
        let chat = ws("chat");
        state.workspaces.assign(WindowId(1), &chat);
        let cursor = state.effects.len();

        apply(
            &mut state,
            Event::WorkspacesLoaded(vec![
                PersistedWorkspace {
                    name: ws("chat"),
                    origin: WorkspaceOrigin::Command,
                    displayed_fingerprint: None,
                    tree: None,
                },
                PersistedWorkspace {
                    name: ws("dev"),
                    origin: WorkspaceOrigin::Configuration,
                    displayed_fingerprint: Some("DISPLAY1".to_owned()),
                    tree: None,
                },
            ]),
        );

        assert_eq!(
            recorded_recovery(&state).len(),
            1,
            "no window leaves the screen without its way back on disk"
        );
        settle_switch_from(&mut state, cursor, &mut |_, _| None);
        assert_eq!(sorted_parked(&state), vec![WindowId(1)]);
    }

    #[test]
    fn a_half_applied_stored_assignment_is_never_silent() {
        // Reapplying a stored assignment moves one window at a time. If
        // one will not move, the workspace is split across the boundary,
        // and that has to be said rather than left to be discovered.
        let mut state = switching_state(&["dev"], "dev");
        create(&mut state, "chat");
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "b.exe", 1, Rect::new(500, 0, 400, 300)),
            ],
        );
        let chat = ws("chat");
        state.workspaces.assign(WindowId(1), &chat);
        state.workspaces.assign(WindowId(2), &chat);
        let cursor = state.effects.len();

        apply(
            &mut state,
            Event::WorkspacesLoaded(vec![
                PersistedWorkspace {
                    name: ws("chat"),
                    origin: WorkspaceOrigin::Command,
                    displayed_fingerprint: None,
                    tree: None,
                },
                PersistedWorkspace {
                    name: ws("dev"),
                    origin: WorkspaceOrigin::Configuration,
                    displayed_fingerprint: Some("DISPLAY1".to_owned()),
                    tree: None,
                },
            ]),
        );
        settle_switch_from(&mut state, cursor, &mut |window_id, stage| {
            (window_id == WindowId(2) && stage == ParkingStage::Park)
                .then(|| "the window refused to move".to_owned())
        });

        assert_eq!(sorted_parked(&state), vec![WindowId(1)]);
        let degraded = state
            .switch_degraded
            .clone()
            .expect("a window of a hidden workspace is still on screen");
        assert_eq!(degraded.stranded_windows, vec![WindowId(2)]);
        assert_eq!(degraded.target, ws("chat"));
        assert_eq!(
            plan_workspace_switch(&state, "chat"),
            Err(WorkspaceRefusal::SwitchDegraded {
                stranded_windows: 1
            }),
            "switching is blocked while a workspace is split across the boundary"
        );

        // And the same explicit path clears it.
        let cursor = state.effects.len();
        apply(&mut state, Event::WorkspaceSwitchRestoreRequested);
        settle_switch_from(&mut state, cursor, &mut |_, _| None);

        assert_eq!(state.switch_degraded, None);
        assert_eq!(sorted_parked(&state), vec![WindowId(1), WindowId(2)]);
    }

    #[test]
    fn a_window_of_a_hidden_workspace_stays_visible_when_no_parking_site_is_verified() {
        // Explicit failure over silent misbehaviour: without a site the
        // engine parks nothing and publishes why, rather than reaching
        // for another way to hide the window.
        let mut state = switching_state(&["dev"], "dev");
        apply(
            &mut state,
            Event::ParkingCapabilityReported(ParkingCapability::Refused {
                reason: "every edge is covered by a display".to_owned(),
            }),
        );
        create(&mut state, "chat");
        observe(
            &mut state,
            vec![app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300))],
        );
        let chat = ws("chat");
        state.workspaces.assign(WindowId(1), &chat);

        observe(
            &mut state,
            vec![app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300))],
        );

        assert!(recorded_recovery(&state).is_empty());
        assert!(state.parked_windows.is_empty());
        assert_eq!(
            state.last_parking_refusal,
            Some(ParkingRefusal::ParkingRefused {
                reason: "every edge is covered by a display".to_owned()
            })
        );
    }

    #[test]
    fn the_reconciler_stands_aside_while_a_switch_is_in_flight() {
        let mut state = switch_fixture();

        focus_workspace(&mut state, "chat");
        let pending_before = state.pending_parking.len();
        // An observation mid-switch must not start a second set of moves
        // over the same windows.
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "b.exe", 1, Rect::new(500, 0, 400, 300)),
            ],
        );

        assert_eq!(state.pending_parking.len(), pending_before);
    }

    #[test]
    fn base_config_cannot_request_switching_so_it_stays_disabled() {
        let state = workspace_state(&[1], &["dev"]);
        assert_eq!(
            state.workspace_switching_status(),
            WorkspaceSwitchingStatus::Disabled
        );
    }

    #[test]
    fn a_valid_profile_mapping_applies_to_every_display_at_once_and_is_requested() {
        let mut state = workspace_state(&[1, 2], &["dev", "chat", "media"]);
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "b.exe", 2, Rect::new(1920, 0, 400, 300)),
            ],
        );
        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev")), (DisplayId(2), ws("chat"))]
        );
        let set = switching_set(
            &state.displays,
            &["dev", "chat", "media"],
            true,
            &[("DISPLAY1", "media"), ("DISPLAY2", "dev")],
        );

        apply(&mut state, Event::ConfigChanged(Box::new(set)));

        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("media")), (DisplayId(2), ws("dev"))],
            "the whole mapping applied as one transition"
        );
        assert!(
            state
                .workspaces
                .get(&ws("chat"))
                .unwrap()
                .stashed_tree
                .as_ref()
                .unwrap()
                .contains(&WindowId(2)),
            "the displaced workspace kept its tree"
        );
        assert_eq!(
            state.trees[&DisplayId(2)].windows(),
            vec![&WindowId(1)],
            "dev carried its tree to its new display"
        );
        assert_eq!(
            state.workspace_switching_status(),
            WorkspaceSwitchingStatus::Requested {
                pending: SwitchingPending::ParkingCapabilityUnverified
            },
            "the mapping is in effect; switching itself waits for a verified parking site"
        );
    }

    #[test]
    fn an_inert_mapping_changes_nothing_and_reports_disabled() {
        let mut state = workspace_state(&[1, 2], &["dev", "chat"]);
        let set = switching_set(
            &state.displays,
            &["dev", "chat"],
            false,
            &[("DISPLAY1", "chat"), ("DISPLAY2", "dev")],
        );

        apply(&mut state, Event::ConfigChanged(Box::new(set)));

        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("dev")), (DisplayId(2), ws("chat"))]
        );
        assert_eq!(
            state.workspace_switching_status(),
            WorkspaceSwitchingStatus::Disabled
        );
    }

    #[test]
    fn a_mapping_that_cannot_be_completed_changes_nothing_and_is_unavailable() {
        let mut state = workspace_state(&[1, 2], &["dev", "chat"]);
        let before = state.workspaces.displayed();

        // Validation would refuse these; the reducer still must not apply
        // half of one if they ever arrive.
        let unknown = switching_set(
            &state.displays,
            &["dev", "chat"],
            true,
            &[("DISPLAY1", "chat"), ("DISPLAY2", "media")],
        );
        apply(&mut state, Event::ConfigChanged(Box::new(unknown)));
        assert_eq!(state.workspaces.displayed(), before);
        assert_eq!(
            state.workspace_switching_status(),
            WorkspaceSwitchingStatus::Unavailable {
                reason: WorkspaceSwitchingUnavailable::UnknownWorkspace {
                    name: "media".to_owned()
                }
            }
        );
        assert_eq!(
            state.workspaces.names(),
            vec![ws("chat"), ws("dev")],
            "no name was invented"
        );

        let incomplete = switching_set(
            &state.displays,
            &["dev", "chat"],
            true,
            &[("DISPLAY1", "chat")],
        );
        apply(&mut state, Event::ConfigChanged(Box::new(incomplete)));
        assert_eq!(state.workspaces.displayed(), before);
        assert_eq!(
            state.workspace_switching_status(),
            WorkspaceSwitchingStatus::Unavailable {
                reason: WorkspaceSwitchingUnavailable::MappingIncomplete {
                    display_fingerprint: "DISPLAY2".to_owned()
                }
            }
        );

        let stray = switching_set(
            &state.displays,
            &["dev", "chat"],
            true,
            &[
                ("DISPLAY1", "chat"),
                ("DISPLAY2", "dev"),
                ("DISPLAY9", "dev"),
            ],
        );
        apply(&mut state, Event::ConfigChanged(Box::new(stray)));
        assert_eq!(state.workspaces.displayed(), before);
        assert_eq!(
            state.workspace_switching_status(),
            WorkspaceSwitchingStatus::Unavailable {
                reason: WorkspaceSwitchingUnavailable::DisplayNotConnected {
                    display_fingerprint: "DISPLAY9".to_owned()
                }
            }
        );
    }

    #[test]
    fn a_mapping_already_in_effect_leaves_the_trees_alone_on_reload() {
        let mut state = workspace_state(&[1], &["dev"]);
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "b.exe", 1, Rect::new(500, 0, 400, 300)),
            ],
        );
        let set = switching_set(&state.displays, &["dev"], true, &[("DISPLAY1", "dev")]);
        apply(&mut state, Event::ConfigChanged(Box::new(set.clone())));
        let tree = state.trees[&DisplayId(1)].clone();
        let mut again = set;
        again.base.gaps = mosaix_domain::Gaps::new(4, 4);

        apply(&mut state, Event::ConfigChanged(Box::new(again)));

        assert_eq!(state.trees[&DisplayId(1)], tree);
        assert!(state
            .workspaces
            .get(&ws("dev"))
            .unwrap()
            .stashed_tree
            .is_none());
    }

    #[test]
    fn switching_status_follows_persistence_health_and_parking_capability() {
        let mut state = workspace_state(&[1], &["dev"]);
        let set = switching_set(&state.displays, &["dev"], true, &[("DISPLAY1", "dev")]);
        apply(&mut state, Event::ConfigChanged(Box::new(set)));

        apply(
            &mut state,
            Event::PersistenceHealthChanged(PersistenceHealth::Degraded {
                last_durable_revision: 1,
                reason: mosaix_persistence::PersistenceFailure::WriteFailed,
            }),
        );
        assert_eq!(
            state.workspace_switching_status(),
            WorkspaceSwitchingStatus::Requested {
                pending: SwitchingPending::PersistenceDegraded
            }
        );

        apply(
            &mut state,
            Event::PersistenceHealthChanged(PersistenceHealth::Healthy {
                last_durable_revision: 2,
            }),
        );
        state.parking_capability = ParkingCapability::Verified;
        assert_eq!(
            state.workspace_switching_status(),
            WorkspaceSwitchingStatus::Experimental
        );

        state.parking_capability = ParkingCapability::Refused {
            reason: "no off-screen edge".to_owned(),
        };
        assert_eq!(
            state.workspace_switching_status(),
            WorkspaceSwitchingStatus::Unavailable {
                reason: WorkspaceSwitchingUnavailable::ParkingRefused {
                    reason: "no off-screen edge".to_owned()
                }
            }
        );
    }

    #[test]
    fn a_profile_mapping_outranks_a_remembered_assignment_at_restart() {
        let mut state = workspace_state(&[1], &["dev", "chat"]);
        let set = switching_set(
            &state.displays,
            &["dev", "chat"],
            true,
            &[("DISPLAY1", "chat")],
        );
        apply(&mut state, Event::ConfigChanged(Box::new(set)));
        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("chat"))]
        );

        apply(
            &mut state,
            Event::WorkspacesLoaded(vec![PersistedWorkspace {
                name: ws("dev"),
                origin: WorkspaceOrigin::Configuration,
                displayed_fingerprint: Some("DISPLAY1".to_owned()),
                tree: None,
            }]),
        );

        assert_eq!(
            state.workspaces.displayed(),
            vec![(DisplayId(1), ws("chat"))]
        );
    }

    // ---- Recovery ledger authorisation --------------------------------

    fn parkable_state() -> EngineState {
        let mut state = workspace_state(&[1], &["dev"]);
        state.session_id = "session-1".to_owned();
        state.parking_capability = ParkingCapability::Verified;
        observe(
            &mut state,
            vec![app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300))],
        );
        state
    }

    fn recovery_intents(state: &EngineState) -> Vec<&PersistenceIntent> {
        state
            .persistence_intents
            .iter()
            .filter(|intent| {
                matches!(
                    intent,
                    PersistenceIntent::RecordRecovery { .. }
                        | PersistenceIntent::MarkParked(_)
                        | PersistenceIntent::MarkRestored(_)
                )
            })
            .collect()
    }

    fn park_effects(state: &EngineState) -> Vec<(WindowId, RecoveryEntryId)> {
        state
            .effects
            .iter()
            .filter_map(|effect| match effect {
                EngineEffect::ParkWindow {
                    window_id,
                    entry_id,
                } => Some((*window_id, *entry_id)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn parking_is_authorised_only_after_the_ledger_acknowledges_the_entry() {
        let mut state = parkable_state();

        apply(
            &mut state,
            Event::ParkingAuthorizationRequested {
                window_id: WindowId(1),
            },
        );

        assert_eq!(state.last_parking_refusal, None);
        assert_eq!(
            state.pending_parking,
            vec![PendingParking {
                token: 1,
                window_id: WindowId(1),
                transaction: None,
            }]
        );
        let intents = recovery_intents(&state);
        assert_eq!(intents.len(), 1);
        let PersistenceIntent::RecordRecovery { token, draft } = intents[0] else {
            panic!("expected a recovery intent");
        };
        assert_eq!(*token, 1);
        assert_eq!(draft.session_id, "session-1");
        assert_eq!(draft.native_handle, 1);
        assert_eq!(draft.original_display_fingerprint, "DISPLAY1");
        assert_eq!(draft.visible_bounds, Rect::new(0, 0, 1920, 1080));
        assert!(
            park_effects(&state).is_empty(),
            "nothing may move before the entry is durable"
        );

        apply(
            &mut state,
            Event::RecoveryEntryDurable {
                token: 1,
                entry_id: RecoveryEntryId(9),
            },
        );

        assert!(state.pending_parking.is_empty());
        assert_eq!(
            park_effects(&state),
            vec![(WindowId(1), RecoveryEntryId(9))]
        );
    }

    #[test]
    fn a_refused_ledger_write_parks_nothing() {
        let mut state = parkable_state();
        apply(
            &mut state,
            Event::ParkingAuthorizationRequested {
                window_id: WindowId(1),
            },
        );

        apply(&mut state, Event::RecoveryEntryRefused { token: 1 });

        assert!(state.pending_parking.is_empty());
        assert!(park_effects(&state).is_empty());
    }

    #[test]
    fn a_degraded_state_database_refuses_new_parking_but_tiling_continues() {
        let mut state = parkable_state();
        apply(
            &mut state,
            Event::PersistenceHealthChanged(PersistenceHealth::Degraded {
                last_durable_revision: 1,
                reason: mosaix_persistence::PersistenceFailure::WriteFailed,
            }),
        );

        apply(
            &mut state,
            Event::ParkingAuthorizationRequested {
                window_id: WindowId(1),
            },
        );
        assert_eq!(
            state.last_parking_refusal,
            Some(ParkingRefusal::PersistenceDegraded)
        );
        assert!(recovery_intents(&state).is_empty());

        // Ordinary in-memory tiling is untouched: a second window still
        // gets arranged.
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 1920, 1080)),
                app_window_at(2, "b.exe", 1, Rect::new(0, 0, 400, 300)),
            ],
        );
        assert_eq!(
            arrangement(&state),
            vec![
                (1, Rect::new(0, 0, 960, 1080)),
                (2, Rect::new(960, 0, 960, 1080))
            ]
        );
    }

    #[test]
    fn parking_refuses_without_a_verified_site_a_fullscreen_or_minimized_window_or_an_unmanaged_one(
    ) {
        let mut state = parkable_state();
        state.parking_capability = ParkingCapability::Unverified;
        assert_eq!(
            plan_parking_authorization(&state, WindowId(1)),
            Err(ParkingRefusal::ParkingCapabilityUnverified)
        );
        state.parking_capability = ParkingCapability::Refused {
            reason: "no edge".to_owned(),
        };
        assert_eq!(
            plan_parking_authorization(&state, WindowId(1)),
            Err(ParkingRefusal::ParkingRefused {
                reason: "no edge".to_owned()
            })
        );
        state.parking_capability = ParkingCapability::Verified;
        assert_eq!(
            plan_parking_authorization(&state, WindowId(99)),
            Err(ParkingRefusal::NotManaged {
                window_id: WindowId(99)
            })
        );
        let mut fullscreen = app_window_at(1, "a.exe", 1, Rect::new(0, 0, 1920, 1080));
        fullscreen.lifecycle = WindowLifecycle::Fullscreen;
        observe(&mut state, vec![fullscreen]);
        assert_eq!(
            plan_parking_authorization(&state, WindowId(1)),
            Err(ParkingRefusal::Fullscreen {
                window_id: WindowId(1)
            })
        );
        let mut minimized = app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300));
        minimized.lifecycle = WindowLifecycle::Minimized;
        observe(&mut state, vec![minimized]);
        assert_eq!(
            plan_parking_authorization(&state, WindowId(1)),
            Err(ParkingRefusal::Minimized {
                window_id: WindowId(1)
            }),
            "a minimized window occupies no screen and is left as it is"
        );
    }

    #[test]
    fn a_parked_window_is_marked_in_the_ledger_and_restoration_consumes_the_entry() {
        let mut state = parkable_state();
        apply(
            &mut state,
            Event::ParkingAuthorizationRequested {
                window_id: WindowId(1),
            },
        );
        apply(
            &mut state,
            Event::RecoveryEntryDurable {
                token: 1,
                entry_id: RecoveryEntryId(9),
            },
        );

        apply(
            &mut state,
            Event::WindowParked {
                window_id: WindowId(1),
                entry_id: RecoveryEntryId(9),
            },
        );
        assert_eq!(
            state.parked_windows.get(&WindowId(1)),
            Some(&RecoveryEntryId(9))
        );
        assert_eq!(
            plan_parking_authorization(&state, WindowId(1)).map(|_| ()),
            Ok(()),
            "a parked window may be re-recorded; the ledger keeps every entry"
        );

        apply(
            &mut state,
            Event::WindowRestored {
                window_id: WindowId(1),
            },
        );
        assert!(state.parked_windows.is_empty());
        assert!(recovery_intents(&state)
            .iter()
            .any(|intent| matches!(intent, PersistenceIntent::MarkRestored(RecoveryEntryId(9)))));
    }

    #[test]
    fn an_acknowledgement_for_a_window_that_left_management_parks_nothing() {
        let mut state = parkable_state();
        apply(
            &mut state,
            Event::ParkingAuthorizationRequested {
                window_id: WindowId(1),
            },
        );
        observe(&mut state, vec![]);

        apply(
            &mut state,
            Event::RecoveryEntryDurable {
                token: 1,
                entry_id: RecoveryEntryId(9),
            },
        );

        assert!(park_effects(&state).is_empty());
    }

    fn restore_effects(state: &EngineState) -> Vec<(WindowId, RecoveryEntryId)> {
        state
            .effects
            .iter()
            .filter_map(|effect| match effect {
                EngineEffect::RestoreWindow {
                    window_id,
                    entry_id,
                } => Some((*window_id, *entry_id)),
                _ => None,
            })
            .collect()
    }

    fn park(state: &mut EngineState, window_id: isize, entry: i64) {
        apply(
            state,
            Event::ParkingAuthorizationRequested {
                window_id: WindowId(window_id),
            },
        );
        let token = state
            .pending_parking
            .iter()
            .find(|pending| pending.window_id == WindowId(window_id))
            .expect("the request was accepted")
            .token;
        apply(
            state,
            Event::RecoveryEntryDurable {
                token,
                entry_id: RecoveryEntryId(entry),
            },
        );
        apply(
            state,
            Event::WindowParked {
                window_id: WindowId(window_id),
                entry_id: RecoveryEntryId(entry),
            },
        );
    }

    #[test]
    fn a_failed_park_is_recorded_and_leaves_the_window_unparked() {
        let mut state = parkable_state();
        apply(
            &mut state,
            Event::ParkingAuthorizationRequested {
                window_id: WindowId(1),
            },
        );
        apply(
            &mut state,
            Event::RecoveryEntryDurable {
                token: 1,
                entry_id: RecoveryEntryId(9),
            },
        );

        apply(
            &mut state,
            Event::WindowParkFailed {
                window_id: WindowId(1),
                entry_id: RecoveryEntryId(9),
                reason: "SetWindowPos failed".to_owned(),
            },
        );

        assert!(state.parked_windows.is_empty());
        assert_eq!(
            state.last_parking_failure,
            Some(ParkingFailure {
                window_id: WindowId(1),
                entry_id: RecoveryEntryId(9),
                stage: ParkingStage::Park,
                reason: "SetWindowPos failed".to_owned(),
            })
        );
        assert!(
            !recovery_intents(&state)
                .iter()
                .any(|intent| matches!(intent, PersistenceIntent::MarkParked(_))),
            "an entry whose park failed is never marked parked"
        );
    }

    #[test]
    fn restoring_parked_windows_asks_for_each_and_a_failed_restore_keeps_the_entry_open() {
        let mut state = parkable_state();
        observe(
            &mut state,
            vec![
                app_window_at(1, "a.exe", 1, Rect::new(0, 0, 400, 300)),
                app_window_at(2, "b.exe", 1, Rect::new(0, 0, 400, 300)),
            ],
        );
        park(&mut state, 1, 9);
        park(&mut state, 2, 10);

        apply(&mut state, Event::RestoreParkedWindowsRequested);

        assert_eq!(
            restore_effects(&state),
            vec![
                (WindowId(1), RecoveryEntryId(9)),
                (WindowId(2), RecoveryEntryId(10))
            ]
        );

        apply(
            &mut state,
            Event::WindowRestored {
                window_id: WindowId(1),
            },
        );
        apply(
            &mut state,
            Event::WindowRestoreFailed {
                window_id: WindowId(2),
                entry_id: RecoveryEntryId(10),
                reason: "SetWindowPlacement failed".to_owned(),
            },
        );

        assert_eq!(
            state.parked_windows.get(&WindowId(2)),
            Some(&RecoveryEntryId(10)),
            "a window whose restore failed stays parked, so recovery still finds it"
        );
        assert!(!state.parked_windows.contains_key(&WindowId(1)));
        assert_eq!(
            state
                .last_parking_failure
                .as_ref()
                .map(|failure| failure.stage),
            Some(ParkingStage::Restore)
        );
        assert!(recovery_intents(&state)
            .iter()
            .any(|intent| matches!(intent, PersistenceIntent::MarkRestored(RecoveryEntryId(9)))));
        assert!(!recovery_intents(&state)
            .iter()
            .any(|intent| matches!(intent, PersistenceIntent::MarkRestored(RecoveryEntryId(10)))));
    }

    #[test]
    fn a_parked_window_that_closes_releases_its_entry() {
        let mut state = parkable_state();
        park(&mut state, 1, 9);

        observe(&mut state, vec![]);

        assert!(state.parked_windows.is_empty());
        assert!(recovery_intents(&state)
            .iter()
            .any(|intent| matches!(intent, PersistenceIntent::MarkRestored(RecoveryEntryId(9)))));
    }

    #[test]
    fn startup_recovery_outcomes_are_published() {
        let mut state = parkable_state();
        let outcome = RecoveryOutcome {
            entry_id: RecoveryEntryId(3),
            native_handle: 77,
            application_id: mosaix_domain::ApplicationId("code.exe".to_owned()),
            verdict: mosaix_domain::HandleVerdict::Stale,
            restored: false,
            failure: None,
        };

        apply(&mut state, Event::RecoveryReported(vec![outcome.clone()]));

        assert_eq!(state.recovery_outcomes, vec![outcome]);
    }
}
