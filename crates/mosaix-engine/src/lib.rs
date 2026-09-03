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

use std::collections::HashMap;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use mosaix_config::{ResolvedConfig, ResolvedConfigSet};
use mosaix_domain::{topology_fingerprint, Display, DisplayId, Rect, WindowId};
use mosaix_layout::{
    apply_gaps, cycle_display, resolve_layout_cells, resolve_zone_cycle, snap_to_half,
    throw_preserving_ratio, CycleStep, DisplayDirection, HalfZone, HorizontalDirection,
};

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

/// State the reducer owns and is the only writer of.
///
/// Display topology and per-window placement exist as real domain state
/// today; a full window registry, workspaces, and rules will extend this
/// as those domain types land (architecture doc section 7).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineState {
    /// Bumped on every committed mutation (architecture doc section 13:
    /// "Monotonic state revision on every committed mutation").
    pub revision: u64,
    pub displays: Vec<Display>,
    /// Where each tracked window currently sits, keyed by window. Entries
    /// are created (and their `previous_placement` remembered) by
    /// [`Event::WindowPlaced`] and [`Event::WindowThrowToDisplayRequested`],
    /// or, for a window seen focused before either of those ever fires,
    /// by [`Event::WindowFocused`] itself (with no `previous_placement`).
    pub windows: HashMap<WindowId, WindowPlacement>,
    /// The window that currently has OS foreground focus, `None` until the
    /// first [`Event::WindowFocused`] is observed. Sourced from the OS's
    /// foreground-change notification (architecture doc section 8.2).
    pub focused_window: Option<WindowId>,
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
    LayoutApplyRequested {
        name: String,
    },
    LayoutApplyDraftRequested {
        cells: Vec<(f64, f64, f64, f64)>,
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
    ConfigChanged(ResolvedConfigSet),

    /// Pause all window management. Placement-related events become no-ops
    /// until [`Event::ResumeRequested`] fires. Idempotent — pausing when
    /// already paused is a no-op.
    PauseRequested,

    /// Resume window management after a pause. Idempotent — resuming when
    /// not paused is a no-op.
    ResumeRequested,

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
        windows: Vec<(WindowId, DisplayId, Rect)>,
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
}

/// Applies one event to `state`. Must never panic -- a single bad event
/// must not take down a reducer thread meant to run all day. Every `Event`
/// variant is handled explicitly so this stays true as the enum grows.
fn apply(state: &mut EngineState, event: Event) {
    match event {
        Event::DisplayTopologyChanged(displays) => {
            if topology_fingerprint(&displays) == topology_fingerprint(&state.displays) {
                tracing::debug!("display topology event was not a real change; ignoring");
                return;
            }
            tracing::info!(display_count = displays.len(), "display topology changed");
            // Feature 30 — display hotplug: migrate windows whose previous
            // display is no longer present in the new topology to the nearest
            // surviving display.  We do this *before* committing `displays` so
            // we can still read the old topology to compute the migration.
            migrate_orphaned_windows(state, &displays);
            state.displays = displays;
            state.resolved_config = select_resolved_config(&state.config_set, &state.displays);
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
            place_window(state, window_id, display_id, bounds, None);
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
            placement.display_id = previous_display_id;
            placement.bounds = previous_bounds;
            state.revision += 1;
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
            place_window(state, window_id, to_display_id, new_bounds, None);
        }

        Event::WindowFocused {
            window_id,
            display_id,
            bounds,
        } => {
            state.windows.entry(window_id).or_insert(WindowPlacement {
                display_id,
                bounds,
                previous_placement: None,
                cycle_step: None,
                rejection_count: 0,
            });
            state.focused_window = Some(window_id);
            state.revision += 1;
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
        }

        Event::LayoutApplyRequested { name } => {
            let Some(layout) = state
                .resolved_config
                .layouts
                .iter()
                .find(|layout| layout.name.eq_ignore_ascii_case(&name))
            else {
                return;
            };
            apply_layout_cells(
                state,
                layout
                    .cells
                    .iter()
                    .map(|cell| (cell.x, cell.y, cell.width, cell.height))
                    .collect(),
            );
        }

        Event::LayoutApplyDraftRequested { cells } => apply_layout_cells(state, cells),

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
            if placement.display_id == display_id && placement.bounds == bounds {
                tracing::debug!(
                    ?window_id,
                    "observed bounds match the last placement transaction; cycle state unaffected"
                );
                return;
            }
            if placement.cycle_step.take().is_some() {
                tracing::debug!(
                    ?window_id,
                    "observed bounds don't match the last placement transaction; cycle step reset"
                );
                state.revision += 1;
            }
        }

        Event::ConfigChanged(config_set) => {
            if config_set == state.config_set {
                tracing::debug!("config event was not a real change; ignoring");
                return;
            }
            tracing::info!("resolved config changed");
            state.resolved_config = select_resolved_config(&config_set, &state.displays);
            state.config_set = config_set;
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
                window_count = windows.len(),
                "wake reconciliation starting"
            );
            // Step 1: migrate orphaned windows using the old topology before
            // committing the new one (same as the hotplug path in
            // DisplayTopologyChanged).
            if topology_fingerprint(&displays) != topology_fingerprint(&state.displays) {
                migrate_orphaned_windows(state, &displays);
                state.displays = displays;
                state.resolved_config = select_resolved_config(&state.config_set, &state.displays);
            }
            // Step 2: bulk-register any newly observed windows.
            let mut registered = 0usize;
            for (window_id, display_id, bounds) in windows {
                state.windows.entry(window_id).or_insert_with(|| {
                    registered += 1;
                    WindowPlacement {
                        display_id,
                        bounds,
                        previous_placement: None,
                        cycle_step: None,
                        rejection_count: 0,
                    }
                });
            }
            tracing::info!(registered, "wake reconciliation complete");
            state.revision += 1;
        }

        // Feature 31 — per-window circuit breaker.
        Event::PlacementRejected { window_id } => {
            let Some(placement) = state.windows.get_mut(&window_id) else {
                tracing::debug!(
                    ?window_id,
                    "placement rejection for an untracked window; ignoring"
                );
                return;
            };
            placement.rejection_count = placement.rejection_count.saturating_add(1);
            if placement.rejection_count == CIRCUIT_BREAKER_THRESHOLD {
                tracing::warn!(
                    ?window_id,
                    threshold = CIRCUIT_BREAKER_THRESHOLD,
                    "circuit breaker opened: window repeatedly rejected placement; \
                     stopping automatic management until user resets with a zone-snap command"
                );
                state.revision += 1;
            } else {
                tracing::debug!(
                    ?window_id,
                    rejection_count = placement.rejection_count,
                    threshold = CIRCUIT_BREAKER_THRESHOLD,
                    "placement rejection recorded"
                );
            }
        }
    }
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
            previous_placement,
            cycle_step,
            rejection_count,
        },
    );
    state.revision += 1;
    true
}

fn apply_layout_cells(state: &mut EngineState, cells: Vec<(f64, f64, f64, f64)>) {
    if state.paused {
        return;
    }
    let Some(focused) = state.focused_window else {
        return;
    };
    let Some(display_id) = state
        .windows
        .get(&focused)
        .map(|placement| placement.display_id)
    else {
        return;
    };
    let Some(work_area) = work_area_of(&state.displays, display_id) else {
        return;
    };
    let mut windows: Vec<(WindowId, Rect)> = state
        .windows
        .iter()
        .filter(|(_, placement)| placement.display_id == display_id && !placement.circuit_open())
        .map(|(id, placement)| (*id, placement.bounds))
        .collect();
    windows.sort_by_key(|(id, bounds)| (bounds.y, bounds.x, id.0));
    for ((window_id, _), raw) in windows
        .into_iter()
        .zip(resolve_layout_cells(work_area, &cells))
    {
        let bounds = apply_gaps(raw, work_area, state.resolved_config.gaps);
        place_window(state, window_id, display_id, bounds, None);
    }
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
    for placement in state.windows.values_mut() {
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
        migrated += 1;
    }

    if migrated > 0 {
        tracing::info!(migrated, "window migration complete");
    }
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
    let initial_state = EngineState {
        revision: 0,
        displays: initial_displays,
        windows: HashMap::new(),
        focused_window: None,
        resolved_config: initial_resolved_config,
        config_set: initial_config_set,
        paused: false,
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
            Event::ConfigChanged(config_set_with_gaps(mosaix_domain::Gaps::new(10, 4))),
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
            Event::ConfigChanged(config_set_with_gaps(mosaix_domain::Gaps::new(10, 4))),
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
            Event::ConfigChanged(config_set_with_gaps(mosaix_domain::Gaps::new(10, 4))),
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
            Event::ConfigChanged(config_set_with_gaps(mosaix_domain::Gaps::new(20, 8))),
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
    fn apply_bounds_observed_mismatch_with_no_cycle_step_does_not_bump_the_revision() {
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
            state.revision, revision_before,
            "resetting an already-None cycle step is not a real change, so must not bump the revision"
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
            behavior: mosaix_config::BehaviorSection::default(),
            layouts: Vec::new(),
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
                hotkeys: std::collections::BTreeMap::new(),
                gaps,
                behavior: mosaix_config::BehaviorSection::default(),
                layouts: Vec::new(),
            },
            profiles: Vec::new(),
        }
    }

    #[test]
    fn apply_config_changed_updates_resolved_config_and_bumps_revision() {
        let mut state = EngineState::default();
        assert_eq!(state.resolved_config, ResolvedConfig::default());

        let new_config_set = config_set_with_left_binding("ctrl+alt+left");
        apply(&mut state, Event::ConfigChanged(new_config_set.clone()));

        assert_eq!(state.resolved_config, new_config_set.base);
        assert_eq!(state.config_set, new_config_set);
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn apply_config_changed_ignores_an_identical_resolved_config() {
        let mut state = EngineState::default();
        let config_set = config_set_with_left_binding("ctrl+alt+left");
        apply(&mut state, Event::ConfigChanged(config_set.clone()));
        let revision_after_first = state.revision;

        apply(&mut state, Event::ConfigChanged(config_set));

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
            Event::ConfigChanged(config_set_with_left_binding("ctrl+alt+left")),
        );
        let revision_after_first = state.revision;

        let second_config_set = config_set_with_left_binding("ctrl+shift+left");
        apply(&mut state, Event::ConfigChanged(second_config_set.clone()));

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
        apply(&mut state, Event::ConfigChanged(config_set));
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
        apply(&mut state, Event::ConfigChanged(config_set));

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
        apply(&mut state, Event::ConfigChanged(config_set));

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
        let mut state = EngineState::default();
        state.paused = true;
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
        let mut state = EngineState::default();
        state.paused = true;
        state.focused_window = Some(WindowId(1));
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
        let mut state = EngineState::default();
        state.paused = true;
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
        let mut state = EngineState::default();
        state.paused = true;
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
        let mut state = EngineState::default();
        state.paused = true;
        apply(
            &mut state,
            Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]),
        );
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn apply_focus_still_processes_while_paused() {
        let mut state = EngineState::default();
        state.paused = true;
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
    fn apply_config_still_processes_while_paused() {
        let mut state = EngineState::default();
        state.paused = true;
        let config_set = ResolvedConfigSet {
            base: ResolvedConfig::default(),
            profiles: vec![],
        };
        apply(&mut state, Event::ConfigChanged(config_set));
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
        let new_windows = vec![
            (WindowId(1), DisplayId(1), Rect::new(0, 0, 960, 1080)), // already tracked
            (WindowId(99), DisplayId(2), Rect::new(1920, 0, 960, 1080)), // new
        ];
        apply(
            &mut state,
            Event::WakeReconciliation {
                displays: new_displays,
                windows: new_windows,
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
    fn diff_placements_excludes_open_circuit_windows() {
        // Windows whose circuit is open must not appear in the diff so the
        // executor never tries to `SetWindowPos` them again.
        let mut previous: HashMap<WindowId, WindowPlacement> = HashMap::new();
        let open_circuit = WindowPlacement {
            display_id: DisplayId(1),
            bounds: Rect::new(0, 0, 100, 100),
            previous_placement: None,
            cycle_step: None,
            rejection_count: CIRCUIT_BREAKER_THRESHOLD,
        };
        let healthy = WindowPlacement {
            display_id: DisplayId(1),
            bounds: Rect::new(100, 100, 200, 200),
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
}
