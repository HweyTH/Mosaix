//! Authoritative state reducer, reconciliation, placement diff, and transaction planning.
//!
//! Holds the event queue and reducer described in the architecture doc's
//! "Concurrency model" (section 14.1): "Native adapter threads translate
//! callbacks into normalized events. A bounded multi-producer queue feeds
//! one reducer task. The reducer is the only writer of domain state."
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
//!   agent enumerates all open windows at launch and bulk-registers them so
//!   the engine starts with an accurate picture of what's already on screen.
//!
//! - **Sleep/wake recovery** ([`Event::WakeReconciliation`]): After the
//!   system resumes from sleep the display topology may have changed.  The
//!   agent re-enumerates displays and windows and sends this event, which
//!   migrates any window whose previous display is gone to the nearest
//!   surviving one.
//!
//! - **Display hotplug** ([`Event::DisplayTopologyChanged`]): When a monitor
//!   is unplugged, windows tracked on the vanished display are migrated to
//!   the nearest surviving display rather than being left off-screen.
//!
//! - **Per-window circuit breaker** ([`Event::PlacementRejected`],
//!   [`CIRCUIT_BREAKER_THRESHOLD`]): If a window repeatedly rejects
//!   `SetWindowPos` (e.g. because it enforces a minimum size), the engine
//!   marks it temporarily unmanaged after
//!   [`CIRCUIT_BREAKER_THRESHOLD`] consecutive rejections.  A deliberate
//!   zone-snap command from the user resets the breaker.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use mosaix_config::{Command, ResolvedConfig, ResolvedConfigSet, TilingMode};
use mosaix_domain::identity::{match_window_with_order, WindowEvidence};
use mosaix_domain::tree::{ContainerTree, PersistedTree};
use mosaix_domain::undo::{
    now_unix, UndoApplied, UndoMember, UndoRefusal, UndoRestoredWindow, UndoResult,
    UndoTargetOutcome, UndoTransaction, UndoTransactionDraft, UndoTransactionId,
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
/// generously relative to expected event rates -- architecture doc section
/// 18 budgets "ordinary OS event to stable layout plan" at under 100ms at
/// p95, so a deep queue is not needed to absorb bursts.
pub const DEFAULT_QUEUE_CAPACITY: usize = 256;

/// Number of consecutive placement rejections before a window's circuit
/// breaker opens and the engine stops trying to manage it.  Chosen to
/// tolerate a transient mis-report while reacting quickly enough that the
/// user never sees a sustained battle between Mosaix and a stubborn app.
/// The breaker resets automatically on any explicit zone-snap command.
pub const CIRCUIT_BREAKER_THRESHOLD: u8 = 3;

/// A platform-neutral operation the engine has committed and an adapter must
/// perform, in reducer order.  Effects contain no native handles or Win32
/// structures so deterministic engine tests can observe intent directly.
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
}

/// A durable write the reducer has committed to but does not perform.
///
/// The reducer owns no SQL connection (ADR 0025), so it records what must
/// become durable and the persistence worker drains these in order, the
/// same way the placement executor drains [`EngineEffect`]. Kept separate
/// from effects because an effect touches the desktop and an intent
/// touches the disk -- and because a storage failure must never look like
/// a placement failure.
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
/// reversed as a unit (ADR 0024).
///
/// A window is captured the first time the command moves it, at the
/// position it held beforehand. Moving it again within the same command --
/// as an automatic reflow does -- does not overwrite that, because undo
/// restores where the window was before the *command*, not before its last
/// internal step.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UndoScope {
    command: String,
    members: Vec<UndoMember>,
    claimed: HashSet<WindowId>,
}

/// State the reducer owns and is the only writer of.
///
/// Display topology and per-window placement exist as real domain state
/// today; a full window registry, workspaces, and rules will extend this
/// as those domain types land (architecture doc section 7).
#[derive(Debug, Clone, Default)]
pub struct EngineState {
    /// Bumped on every committed mutation (architecture doc section 13:
    /// "Monotonic state revision on every committed mutation").
    pub revision: u64,
    /// Whether committed state is currently durable independently of the
    /// reducer's in-memory authority.
    pub persistence_health: PersistenceHealth,
    /// Durable writes the reducer has committed to, drained in order by
    /// the persistence worker. Appended to, never rewritten.
    pub persistence_intents: Vec<PersistenceIntent>,
    /// The only transaction undo will consider (ADR 0024). Published by
    /// the agent from the state database, so the reducer never reads SQL
    /// to decide what undo would do.
    pub newest_undo: Option<UndoTransaction>,
    /// What the last undo request concluded, kept so IPC and the CLI can
    /// report a refusal that happened between requests.
    pub last_undo_result: Option<UndoResult>,
    /// Transient: the explicit command currently being applied, collecting
    /// everything it moves. Lives only for the duration of one [`apply`]
    /// call, and is `None` between events.
    undo_scope: Option<UndoScope>,
    /// Each display's container tree, when tree mode is the resolved
    /// arrangement (ADR 0023). Empty under the balanced grid, which keeps
    /// no structure between reflows. A display that leaves the topology
    /// takes its tree with it, the way its arrangement name does.
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
    /// The windows each display's tree could not fit at their minimum
    /// size, newest insertion first (CONTEXT.md "Constraint-overflow
    /// window"). They keep their leaves and stay managed; they are simply
    /// not placed until the tree can satisfy them again. Distinct from
    /// session-floating, which is the user's choice and outlives a reflow.
    pub constraint_overflow: HashMap<DisplayId, Vec<WindowId>>,
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
    /// foreground-change notification (architecture doc section 8.2).
    pub focused_window: Option<WindowId>,
    /// Last display targeted by focus or an explicit display command.
    pub focused_display: Option<DisplayId>,
    /// The currently active hotkeys/gaps/behavior settings (CONTEXT.md
    /// "Resolved config") -- whichever of `config_set`'s `base` or one of
    /// its `profiles` currently matches `displays`' topology. Updated by
    /// [`Event::ConfigChanged`] (ADR 0005) and, per ADR 0004/this ticket, by
    /// [`Event::DisplayTopologyChanged`] re-selecting against the same
    /// `config_set` whenever the topology itself changes. Defaults to
    /// `ResolvedConfig::default()` (no hotkeys bound) before the first
    /// config load completes -- the same "nothing observed yet" role
    /// `displays: Vec::new()` plays for topology.
    pub resolved_config: ResolvedConfig,
    /// The full base-config-plus-profiles set most recently delivered by
    /// [`Event::ConfigChanged`] (ADR 0004, 0005) -- kept around so
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
    /// topology changes or the agent restarts (ADR 0012).
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
    /// stay unregistered (ADR 0021).
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
    /// shortcut (ADR 0021).
    pub unregistered_bindings: Vec<Command>,
}

impl EngineState {
    /// The number of windows whose circuit breaker is currently open
    /// (Feature 31).  Useful for diagnostics via `mosaix state --json`.
    pub fn circuit_breaker_count(&self) -> usize {
        self.windows.values().filter(|p| p.circuit_open()).count()
    }
}

/// A tracked window's current bounds and display, plus the display and
/// bounds it had immediately before its most recent placement, if any --
/// the "remembered pre-snap size" [`mosaix_layout`]'s zone planner docs say
/// belongs with whatever tracks window state, not the stateless planner
/// itself (architecture doc section 20, "restore" command).
///
/// Both the display and the bounds are remembered together, not bounds
/// alone: a placement can move a window to a different display (a throw),
/// so bounds computed relative to the source display would be wrong if
/// reapplied under the target display's `display_id`.
///
/// Phase 1's restore is a single remembered step, not a full undo stack
/// (that's Phase 2's "undo" -- architecture doc section 20), so
/// `previous_placement` holds at most one prior placement, and restoring
/// clears it rather than pushing the restored state back onto a stack.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowPlacement {
    pub display_id: DisplayId,
    pub bounds: Rect,
    /// Where the OS last reported this window, as opposed to [`bounds`], the
    /// placement Mosaix last *intended* (architecture doc section 8.3's
    /// expected-vs-actual distinction).
    ///
    /// The two agree whenever Mosaix owns the window's position, and
    /// diverge the moment anything else moves it -- an app repositioning
    /// its own window, a native OS snap, a session-floating window dragged
    /// by the user. `bounds` deliberately keeps holding the intent, because
    /// comparing the two is exactly how [`Event::WindowBoundsObserved`]
    /// detects an external move and resets cycle state (ADR 0001); readers
    /// that want to know where the window actually *is* -- drawing on or
    /// around it, say -- want this field instead.
    ///
    /// [`bounds`]: Self::bounds
    pub observed_bounds: Rect,
    pub previous_placement: Option<(DisplayId, Rect)>,
    /// The horizontal zone command and step that produced this placement,
    /// if it came from [`Event::ZoneSnapRequested`] with a left/right
    /// direction (CONTEXT.md "Cycle step"). `None` for windows never
    /// horizontally zone-snapped, and left untouched by placements that
    /// don't participate in cycling (top/bottom zone-snaps, restores,
    /// throws, plain [`Event::WindowPlaced`]) -- only a repeated
    /// same-direction [`Event::ZoneSnapRequested`] advances it, and only a
    /// future bounds-observed event mismatching the expected placement
    /// transaction resets it (ADR 0001; that reset lands with ticket 05).
    pub cycle_step: Option<(HorizontalDirection, CycleStep)>,
    /// Consecutive `SetWindowPos` rejection count for the circuit breaker
    /// (see [`CIRCUIT_BREAKER_THRESHOLD`]).  Each [`Event::PlacementRejected`]
    /// increments this; reaching the threshold causes the engine to stop
    /// issuing placements for this window.  Any explicit user zone-snap
    /// command resets it to zero, giving the user a deliberate escape hatch.
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

/// The direction a zone-snap hotkey requests (CONTEXT.md "Zone"). Only
/// [`ZoneSnapDirection::Left`]/[`ZoneSnapDirection::Right`] participate in
/// zone cycling -- `ZoneSnapDirection::horizontal` is how
/// [`Event::ZoneSnapRequested`]'s handler tells them apart from top/bottom.
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
    /// change; carries a fresh enumeration, not a diff (architecture doc
    /// section 8.2: "Platform adapters may emit incomplete events. The
    /// reducer therefore treats them as hints"). The reducer itself
    /// decides, via [`topology_fingerprint`], whether anything actually
    /// changed.
    DisplayTopologyChanged(Vec<Display>),

    /// Ordered persistence acknowledgement or failure from the worker.
    PersistenceHealthChanged(PersistenceHealth),

    /// The newest stored undo transaction, or `None` for empty history.
    /// Published by the agent at startup and after every durable write, so
    /// the reducer's view of history matches the database's.
    UndoHistoryLoaded(Option<Box<UndoTransaction>>),

    /// Reverse the newest transaction, or refuse and keep it (ADR 0024).
    UndoRequested,

    /// The container trees the state database holds, keyed by display
    /// fingerprint. Published once at startup.
    ContainerTreesLoaded(HashMap<String, PersistedTree>),

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
    /// display and bounds it had immediately before (architecture doc
    /// section 20, "restore"). A no-op if the window isn't tracked, or has
    /// no remembered prior placement (e.g. it was only ever placed once,
    /// or was already restored).
    WindowRestoreRequested {
        window_id: WindowId,
    },

    /// Move a window to the adjacent display in `direction`, preserving
    /// its position/size as a fraction of the display's work area
    /// (architecture doc section 20, "next-display" command). A no-op if
    /// the window isn't tracked, its current display is no longer in the
    /// topology, or there's no adjacent display to move to (e.g. only one
    /// display is connected).
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

    /// A zone-snap hotkey fired for `direction` (architecture doc section
    /// 20's directional snap commands; CONTEXT.md "Zone cycle"). Carries no
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
    /// Compared against the window's own last placement transaction (ADR
    /// 0001, ARCHITECTURE.md section 8.3): a match confirms the
    /// observation is just an echo of Mosaix's own last placement and
    /// leaves cycle-step state untouched; a mismatch means something else
    /// moved or resized the window (a manual drag, another app, a native
    /// OS snap), which invalidates (resets to step 1) the window's
    /// cycle-step state. Either way this never alters the window's tracked
    /// placement bounds -- reconciling tracked state from raw observation
    /// is a separate, out-of-scope concern (`.scratch/cycle-sizes-and-global-hotkeys/issues/05-placement-transaction-correlation.md`).
    /// A no-op for an untracked window.
    WindowBoundsObserved {
        window_id: WindowId,
        display_id: DisplayId,
        bounds: Rect,
    },

    /// `mosaix-config`'s directory watcher validated a new candidate
    /// config directory successfully (ADR 0005, 0007, 0008); carries the
    /// full base-config-plus-profiles set, not a diff. Sent for the
    /// initial startup load as well as every subsequent hot-edit -- an
    /// edit that fails validation never produces this event at all, so
    /// [`EngineState::config_set`] (and the [`EngineState::resolved_config`]
    /// re-selected from it) simply keeps its last-known-good value (ADR
    /// 0007). The active profile is re-selected against [`EngineState`]'s
    /// current topology exactly the way [`Event::DisplayTopologyChanged`]
    /// re-selects it against the current config set (ADR 0004).
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

    InteractivePlacementStarted {
        window_id: WindowId,
    },

    InteractivePlacementEnded {
        window_id: WindowId,
        committed_manual_placement: bool,
    },

    /// Bulk-register all windows that were already open when the agent
    /// started (feature 28 — startup reconciliation). Each entry is
    /// `(window_id, display_id, bounds)` as observed by the platform
    /// adapter's initial enumeration.  Windows already tracked (e.g. by
    /// an earlier [`Event::WindowFocused`]) are silently skipped; windows
    /// not yet tracked are registered with no `previous_placement`.  The
    /// event is sent once, right after the engine is spawned, before any
    /// OS event hooks are active.
    StartupReconciliation {
        windows: Vec<(WindowId, DisplayId, Rect)>,
    },

    /// Re-synchronise display topology and tracked windows after the
    /// system wakes from sleep (feature 29 — sleep/wake recovery).
    ///
    /// The agent re-enumerates both displays and windows after a
    /// configurable settling delay and sends this event.  The handler:
    ///   1. Applies the new display topology (same fingerprint-based guard
    ///      as [`Event::DisplayTopologyChanged`]).
    ///   2. Migrates orphaned windows (whose previous `display_id` no
    ///      longer exists) to the nearest surviving display, preserving
    ///      their normalized position via [`throw_preserving_ratio`].
    ///   3. Bulk-registers any newly observed windows that the engine
    ///      doesn't know about yet.
    WakeReconciliation {
        displays: Vec<Display>,
        windows: Option<Vec<Window>>,
    },

    /// The platform reported that the `SetWindowPos` call for `window_id`
    /// was rejected — the window's actual bounds after a settling period
    /// differ too much from the target (feature 31 — circuit breaker).
    ///
    /// Increments the window's `rejection_count`.  When the count reaches
    /// [`CIRCUIT_BREAKER_THRESHOLD`], subsequent automatic placements for
    /// that window are suppressed and a warning is logged.  The count is
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

    /// Apply the saved layout called `name` (CONTEXT.md "Saved layout") to
    /// the display of the focused managed window (ADR 0020), filling its
    /// cells from that display's managed windows in visual window order.
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

    /// The settings application opened its hotkey editor, so every
    /// binding must stay unregistered until it closes (ADR 0021).
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
    /// rather than by a message that can be lost (ADR 0021).
    HotkeyCaptureEnded,

    /// The outcome of a registration pass: the bindings the platform
    /// refused, in the spelling configuration files use.
    ///
    /// Sent by the agent after each pass, so a binding that did not come
    /// back from capture can be named to the user rather than discovered
    /// as a dead shortcut (ADR 0021).
    HotkeyRegistrationReported {
        unregistered: Vec<Command>,
    },
}

/// Why applying a saved layout changes nothing. Every variant names
/// something the user can act on: a layout command never silently does
/// nothing, and never falls back to another display (ADR 0020).
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
/// rather than left to notice the omission (spec #29 user story 15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedLayoutPlan {
    /// The one display every placement lands on: the focused managed
    /// window's (ADR 0020). A field rather than a column repeated down
    /// `placements`, because a plan touching two displays is not a thing
    /// this type can represent.
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
/// [`resolve_saved_layout`] stays gap-unaware (ADR 0006).
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

/// `display_id`'s managed windows in visual window order (CONTEXT.md
/// "Visual window order"): whatever order the engine already recorded for
/// that display, then any managed window that order doesn't mention,
/// seeded top-to-bottom then left-to-right with the native window id
/// breaking final ties -- the same seeding [`reconcile_balanced_grids`]
/// performs.
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

        Event::UndoRequested => {
            if state.paused {
                tracing::debug!("undo requested while paused; ignoring");
                return;
            }
            let result = plan_undo(state);
            if let UndoResult::Applied(applied) = &result {
                // No scope is opened here: undo is not itself undoable.
                // Persistent redo is deferred to issue #44, and recording
                // one would make a second undo reverse the first.
                for restored in &applied.restored {
                    place_window(
                        state,
                        restored.window_id,
                        restored.display_id,
                        restored.placement,
                        None,
                    );
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
            // Feature 30 — display hotplug: migrate windows whose previous
            // display is no longer present in the new topology to the nearest
            // surviving display.  We do this *before* committing `displays` so
            // we can still read the old topology to compute the migration.
            state.interactive_placement = None;
            state.deferred_reflow_displays.clear();
            migrate_orphaned_windows(state, &displays);
            migrate_focused_display(state, &displays);
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
            }
            reconcile_arrangements(state);
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

            // Feature 31 — circuit breaker reset: an explicit zone-snap command
            // from the user always resets the rejection counter, giving a
            // deliberate escape hatch even for windows that previously rejected
            // automatic placements.
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
            if !state.resolved_config.automatic_tiling_enabled {
                state.automatic_tiling_suspended = false;
            }
            state.automatic_tiling_active =
                state.resolved_config.automatic_tiling_enabled && !state.automatic_tiling_suspended;
            state.config_set = *config_set;
            reconcile_arrangements(state);
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
            let Some(focused) = state.focused_window else {
                return;
            };
            let Some(neighbor) = directional_neighbor(state, direction) else {
                return;
            };
            let Some(display_id) = state
                .inventory
                .get(&focused)
                .map(|managed| managed.window.display_id)
            else {
                return;
            };
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
            // In tree mode the arrangement comes from the tree, not from
            // visual order, so the exchange has to happen there too. It is
            // an exchange of the two leaves' occupants and nothing else:
            // containers, axes, weights, and parentage all stay as they
            // were (ADR 0026). Focus is deliberately not moved, so the user
            // keeps controlling the window they just moved.
            if state.resolved_config.tiling_mode == TilingMode::Tree {
                if let Some(tree) = state.trees.get_mut(&display_id) {
                    tree.swap_leaves(&focused, &neighbor);
                }
            }
            open_undo_scope(state, swap_command(direction));
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

        // Feature 28 — startup reconciliation.
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

        // Feature 29 — sleep/wake recovery.
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
                state.displays = displays;
                state.resolved_config = select_resolved_config(&state.config_set, &state.displays);
                state.automatic_tiling_suspended = false;
                state.automatic_tiling_active = state.resolved_config.automatic_tiling_enabled;
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

        // Feature 31 — per-window circuit breaker.
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
                    // One command, one transaction, however many windows it
                    // moves -- including the reflow below (ADR 0024).
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
                    // the result in one pass (ADR 0011).
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
            // that can tell the user (ADR 0021).
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
/// `display_id`: eligible for tiling and, in tree mode, actually arranged
/// (CONTEXT.md "Directional focus", "Directional swap").
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
    for window in observed {
        let action = evaluator.evaluate(&window).actions.manage;
        if action == ManageAction::Exclude {
            continue;
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
    state.inventory = next;
    inventory_changed || placements_changed
}

/// Updates visual order and emits one final Balanced-grid plan per affected
/// display. It is intentionally a no-op in manual mode, leaving current
/// bounds untouched on profile deactivation (ADR 0015).
/// Reflows every display's tiled windows using whichever arrangement the
/// resolved configuration selects.
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
    for (display_id, order) in &mut state.visual_window_order {
        order.retain(|id| {
            state.inventory.get(id).is_some_and(|managed| {
                (managed.action == ManageAction::Tile || state.session_tiled.contains(id))
                    && managed.window.display_id == *display_id
            })
        });
    }
    let mut candidates: Vec<_> = state
        .inventory
        .values()
        .filter(|managed| {
            managed.action == ManageAction::Tile || state.session_tiled.contains(&managed.window.id)
        })
        .map(|managed| {
            (
                managed.window.display_id,
                managed.window.id,
                managed.window.bounds,
            )
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
                        managed.window.display_id == display.id
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
        if let Some(stored) = fingerprint
            .as_ref()
            .and_then(|fingerprint| state.pending_trees.remove(fingerprint))
        {
            tree = restore_tree(state, &stored, &active);
        }

        // Windows that are no longer tiled here give their space back.
        // Dormant leaves, which would hold that space open for a later
        // match, are deferred: see issue #9.
        let departed: Vec<WindowId> = tree
            .windows()
            .into_iter()
            .filter(|window_id| !active.contains(window_id))
            .copied()
            .collect();
        for window_id in departed {
            tree.remove(&window_id);
        }

        // New windows arrive in visual order, so the arrangement a set of
        // windows produces does not depend on which order they were
        // observed in.
        for window_id in &active {
            if tree.contains(window_id) {
                continue;
            }
            if tree.insert_first(*window_id) {
                continue;
            }
            let placements = plan_tree_raw(&tree, work_area);
            let Some(insertion) = choose_insertion(&placements, focused) else {
                continue;
            };
            tree.split_leaf(&insertion.target, insertion.axis, *window_id);
        }

        // Only a structural change is worth a write. A reflow that moved
        // windows without reshaping the tree -- a display resizing, say --
        // has nothing new to store.
        if state.saved_trees.get(&display_id) != Some(&tree) {
            if let Some(fingerprint) = fingerprint {
                let durable = durable_tree(state, &tree);
                state
                    .persistence_intents
                    .push(PersistenceIntent::SaveContainerTree {
                        display_fingerprint: fingerprint,
                        tree: Box::new(durable),
                    });
                state.saved_trees.insert(display_id, tree.clone());
            }
        }

        // A window the display cannot fit at its minimum size is left
        // where it is rather than squeezed: it keeps its leaf and its
        // management, and comes back the moment the tree can hold it
        // (spec user stories 45 and 46). Nothing here floats it by choice,
        // so the overflow set is recomputed from scratch every reflow.
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
}

/// Turns a stored arrangement back into a live one.
///
/// Each leaf's evidence is matched against the windows currently tiled on
/// that display. A leaf that cannot be identified confidently is dropped
/// rather than guessed at, and no live window is claimed by two leaves --
/// the same refusal-first rule undo follows, applied to structure.
fn restore_tree(state: &EngineState, stored: &PersistedTree, active: &[WindowId]) -> ContainerTree {
    let candidates: Vec<&Window> = active
        .iter()
        .filter_map(|window_id| state.inventory.get(window_id))
        .map(|managed| &managed.window)
        .collect();
    let mut claimed: HashSet<WindowId> = HashSet::new();
    stored.filter_map_windows(&mut |evidence| {
        match_window_with_order(
            evidence,
            candidates.iter().copied(),
            |window| display_fingerprint_of(state, window.display_id),
            |window| Some(launch_order_of(state, window.id)),
        )
        .confident_window()
        .filter(|window_id| claimed.insert(*window_id))
    })
}

/// The durable form of a live tree: the same structure with each window
/// replaced by evidence that can find it again. A window the inventory
/// cannot describe is dropped, because a leaf nothing could ever match
/// would only hold space open forever.
fn durable_tree(state: &EngineState, tree: &ContainerTree) -> PersistedTree {
    tree.filter_map_windows(&mut |window_id| {
        let managed = state.inventory.get(window_id)?;
        let fingerprint = display_fingerprint_of(state, managed.window.display_id)?;
        Some(WindowEvidence::capture(
            &managed.window,
            launch_order_of(state, *window_id),
            &fingerprint,
        ))
    })
}

/// The resolved config that should be active for `displays`' current
/// topology: whichever profile in `config_set.profiles` has a `fingerprint`
/// matching [`topology_fingerprint`] of `displays`, or `config_set.base` if
/// none does (ADR 0004 -- profiles are opt-in overrides, never
/// auto-created, so "no match" is an ordinary outcome, not an error).
/// Shared by [`Event::ConfigChanged`] and [`Event::DisplayTopologyChanged`]
/// so a topology already seen before always re-selects the same profile it
/// matched last time.
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
/// `previous` (architecture doc section 6's "Placement diff" stage,
/// scoped down to what a poll-driven executor needs: it doesn't do
/// transaction planning, just "what changed since I last looked").
///
/// A window present in `previous` but missing from `current` needs no
/// call -- there's nothing sensible to move it to, and the engine never
/// removes tracked windows today anyway.
///
/// Windows whose circuit breaker is open (Feature 31) are excluded from
/// the diff so the executor never issues a `SetWindowPos` call for them --
/// they will re-appear in the diff automatically once the user resets the
/// breaker via a zone-snap command.
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
/// is what makes undo all-or-nothing (ADR 0024): there is no state in which
/// some members have moved and a later one turns out to be unresolvable.
/// The same function answers the IPC preflight and drives the reducer, so a
/// caller is never told something different from what happens.
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

    UndoResult::Applied(UndoApplied {
        transaction_id: transaction.id,
        command: transaction.command.clone(),
        restored,
    })
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
/// automatic reflow an explicit command provokes (spec user story 10).
fn open_undo_scope(state: &mut EngineState, command: &str) {
    state.undo_scope = Some(UndoScope {
        command: command.to_owned(),
        members: Vec::new(),
        claimed: HashSet::new(),
    });
}

/// Closes the open scope, recording a transaction if it moved anything.
///
/// A command that moved nothing -- refused, suppressed by a circuit
/// breaker, or a no-op -- records nothing, so undo never offers to reverse
/// something the user never saw happen.
fn close_undo_scope(state: &mut EngineState) {
    let Some(scope) = state.undo_scope.take() else {
        return;
    };
    if scope.members.is_empty() {
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
/// circuit breaker is open (Feature 31). The caller is free to ignore this
/// return value -- it's informational only; the event has already been
/// handled (by doing nothing).
fn place_window(
    state: &mut EngineState,
    window_id: WindowId,
    display_id: DisplayId,
    bounds: Rect,
    cycle_step: Option<(HorizontalDirection, CycleStep)>,
) -> bool {
    let existing = state.windows.get(&window_id);
    // Feature 31 — circuit breaker: if the window is in open-circuit state
    // (repeated rejections), suppress this placement without modifying state.
    if existing.is_some_and(|p| p.circuit_open()) {
        tracing::debug!(
            ?window_id,
            "placement suppressed: circuit breaker is open for this window"
        );
        return false;
    }
    let previous_placement = existing.map(|placement| (placement.display_id, placement.bounds));
    let cycle_step = cycle_step.or_else(|| existing.and_then(|placement| placement.cycle_step));
    // Preserve rejection_count across placements so the breaker state survives
    // non-user-initiated placements (e.g. throw-to-display).  Only an explicit
    // zone-snap command resets it (handled in Event::ZoneSnapRequested above).
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

/// Migrate windows whose current `display_id` is absent from `new_displays`
/// to the nearest surviving display, preserving their normalized position
/// via [`throw_preserving_ratio`].  Called *before* `state.displays` is
/// updated so the old topology is still available to compute ratios from.
///
/// This is the shared implementation for both [`Event::DisplayTopologyChanged`]
/// (hotplug, Feature 30) and [`Event::WakeReconciliation`] (sleep/wake, Feature 29).
fn migrate_orphaned_windows(state: &mut EngineState, new_displays: &[Display]) {
    if new_displays.is_empty() {
        // No surviving displays -- nothing sensible to migrate to.  Leave
        // windows untouched; they'll be reconciled when a display comes back.
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

/// The queue actually carries this, not `Event` directly, so [`stop`]
/// can terminate the reducer with an explicit poison pill rather than by
/// waiting for every sender to be dropped -- callers are expected to hand
/// out cloned [`EventSender`]s to multiple producer threads (architecture
/// doc's "bounded multi-producer queue"), so those threads' lifetimes, not
/// the last sender's, would otherwise decide when `stop` can return.
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
    /// that backpressure is intentional (architecture doc's "Bounded
    /// event queue"). Fails, returning the event back, once the reducer
    /// has stopped.
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
        constraint_overflow: HashMap::new(),
        displays: initial_displays,
        windows: HashMap::new(),
        inventory: HashMap::new(),
        observed_windows: HashMap::new(),
        rules: Vec::new(),
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
        // Regression test: when a monitor unplugs, the window is *migrated*
        // to the surviving display by `DisplayTopologyChanged`.  A subsequent
        // throw then operates on that surviving display -- but with only one
        // display remaining there is no adjacent display to throw to, so the
        // throw is still a no-op.  The key assertion is that the window ends
        // up on the surviving display, not stranded on the vanished one.
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
        // When a monitor unplugs, `migrate_orphaned_windows` moves any window
        // that was on it to the nearest surviving display.  A subsequent
        // zone-snap must therefore operate on the window's *new* display, not
        // fail because the original display is gone.
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
             external move is detected (ADR 0001)"
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

    // ── Feature 28: Startup reconciliation ────────────────────────────────

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

    // ── Feature 29 / 30: Display migration (hotplug and wake) ─────────────

    #[test]
    fn topology_change_migrates_window_to_nearest_surviving_display() {
        // The window starts on MON-A (left monitor).  When MON-A unplugs,
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

    // ── Feature 31: Per-window circuit breaker ────────────────────────────

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

    /// [`state_with_saved_layout`] plus gaps, for the ADR 0006
    /// post-processing step.
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
        // can produce (ADR 0006).
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
        // changes nothing (ADR 0020).

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

    // ---- Persistent undo (issue #48) ----------------------------------

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
                | PersistenceIntent::SaveContainerTree { .. } => None,
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
            "redo is deferred to issue #44; undo must not become undoable"
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

    // ---- Command-level atomic undo (issue #49) ------------------------

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

    // ---- Undo coverage and retention (issue #50) ----------------------

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

        // The distinct meaning the ticket asks to preserve: one remembered
        // in-session placement, consumed when it is used.
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

    // ---- Container tree (issue #51) -----------------------------------

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
        assert!(
            drafts[0].members.len() >= 2,
            "both swapped windows are reversible together, found {:?}",
            drafts[0].members.len()
        );
    }

    // ---- Constraint overflow (issue #54) ------------------------------

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
        // ADR 0025: durability is a promise about the database, not about
        // the desktop. Losing the first must not disturb the second, and
        // must not provoke compensating movement either.
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
}
