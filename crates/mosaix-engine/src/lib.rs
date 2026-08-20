//! Authoritative state reducer, reconciliation, placement diff, and transaction planning.
//!
//! Currently holds the event queue and reducer described in the
//! architecture doc's "Concurrency model" (section 14.1): "Native adapter
//! threads translate callbacks into normalized events. A bounded
//! multi-producer queue feeds one reducer task. The reducer is the only
//! writer of domain state."
//!
//! [`spawn_engine`] starts one dedicated thread that owns [`EngineState`]
//! outright -- no lock guards the mutation itself, because nothing else
//! ever touches it. After each event, the thread publishes a clone of the
//! new state into an [`EngineHandle`]-shared `Mutex` purely so other
//! threads can read a consistent snapshot; that `Mutex` is not a second
//! mutation path.
//!
//! Reconciliation, placement diffing, and transaction planning belong here
//! too per the crate's description, but nothing upstream (window registry,
//! commands) exists yet for them to operate on -- they land once those
//! domain types do.

use std::collections::HashMap;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use mosaix_domain::{topology_fingerprint, Display, DisplayId, Rect, WindowId};
use mosaix_layout::{cycle_display, throw_preserving_ratio, DisplayDirection};

/// Default bound on the event queue before a sender blocks. Chosen
/// generously relative to expected event rates -- architecture doc section
/// 18 budgets "ordinary OS event to stable layout plan" at under 100ms at
/// p95, so a deep queue is not needed to absorb bursts.
pub const DEFAULT_QUEUE_CAPACITY: usize = 256;

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
    /// [`Event::WindowPlaced`] and [`Event::WindowThrowToDisplayRequested`].
    pub windows: HashMap<WindowId, WindowPlacement>,
    /// The window that currently has OS foreground focus, `None` until the
    /// first [`Event::WindowFocused`] is observed. Sourced from the OS's
    /// foreground-change notification (architecture doc section 8.2).
    pub focused_window: Option<WindowId>,
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
    WindowRestoreRequested { window_id: WindowId },

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

    /// The OS reported `window_id` as having gained foreground focus.
    /// Sourced from the platform adapter's foreground-change notification;
    /// updates [`EngineState::focused_window`].
    WindowFocused { window_id: WindowId },
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
            state.displays = displays;
            state.revision += 1;
        }

        Event::WindowPlaced { window_id, display_id, bounds } => {
            place_window(state, window_id, display_id, bounds);
        }

        Event::WindowRestoreRequested { window_id } => {
            let Some(placement) = state.windows.get_mut(&window_id) else {
                tracing::debug!(?window_id, "restore requested for an untracked window; ignoring");
                return;
            };
            let Some((previous_display_id, previous_bounds)) = placement.previous_placement.take() else {
                tracing::debug!(?window_id, "restore requested but no prior placement is remembered; ignoring");
                return;
            };
            placement.display_id = previous_display_id;
            placement.bounds = previous_bounds;
            state.revision += 1;
        }

        Event::WindowThrowToDisplayRequested { window_id, direction } => {
            let Some(placement) = state.windows.get(&window_id) else {
                tracing::debug!(?window_id, "throw-to-display requested for an untracked window; ignoring");
                return;
            };
            let (from_display_id, bounds) = (placement.display_id, placement.bounds);

            let Some(to_display_id) = cycle_display(&state.displays, from_display_id, direction) else {
                tracing::debug!(?window_id, "no adjacent display to throw the window to; ignoring");
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
                tracing::debug!(?window_id, ?to_display_id, "target display vanished mid-throw; ignoring");
                return;
            };

            let new_bounds = throw_preserving_ratio(bounds, from_work_area, to_work_area);
            place_window(state, window_id, to_display_id, new_bounds);
        }

        Event::WindowFocused { window_id } => {
            state.focused_window = Some(window_id);
            state.revision += 1;
        }
    }
}

/// The work area of the display with `id`, if it's still in `displays`.
fn work_area_of(displays: &[Display], id: DisplayId) -> Option<Rect> {
    displays.iter().find(|display| display.id == id).map(|display| display.work_area)
}

/// Records `bounds` as `window_id`'s current placement on `display_id`,
/// stashing wherever it was before (if it was already tracked) as the one
/// step [`Event::WindowRestoreRequested`] can undo, and bumps the
/// revision.
fn place_window(state: &mut EngineState, window_id: WindowId, display_id: DisplayId, bounds: Rect) {
    let previous_placement = state
        .windows
        .get(&window_id)
        .map(|placement| (placement.display_id, placement.bounds));
    state.windows.insert(
        window_id,
        WindowPlacement {
            display_id,
            bounds,
            previous_placement,
        },
    );
    state.revision += 1;
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
        self.inner.send(Message::Event(event)).map_err(|err| match err.0 {
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
        self.state.lock().expect("engine state mutex poisoned").clone()
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

/// Spawns the reducer thread with `initial_displays` as the starting
/// state, using [`DEFAULT_QUEUE_CAPACITY`].
pub fn spawn_engine(initial_displays: Vec<Display>) -> EngineHandle {
    spawn_engine_with_capacity(initial_displays, DEFAULT_QUEUE_CAPACITY)
}

/// Like [`spawn_engine`], with an explicit event-queue bound.
pub fn spawn_engine_with_capacity(initial_displays: Vec<Display>, capacity: usize) -> EngineHandle {
    let (tx, rx) = sync_channel::<Message>(capacity);
    let initial_state = EngineState {
        revision: 0,
        displays: initial_displays,
        windows: HashMap::new(),
        focused_window: None,
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

    fn wait_for(
        mut condition: impl FnMut() -> bool,
        timeout: Duration,
    ) -> bool {
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
        apply(&mut state, Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]));

        assert_eq!(state.revision, 1);
        assert_eq!(state.displays.len(), 1);
    }

    #[test]
    fn apply_ignores_a_repeated_topology_hint() {
        let mut state = EngineState::default();
        apply(&mut state, Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]));
        apply(&mut state, Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]));

        assert_eq!(state.revision, 1, "an identical re-enumeration must not bump the revision");
    }

    #[test]
    fn apply_bumps_revision_again_when_topology_actually_changes() {
        let mut state = EngineState::default();
        apply(&mut state, Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]));
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
            Event::WindowPlaced { window_id: WindowId(1), display_id: DisplayId(1), bounds },
        );

        let placement = state.windows.get(&WindowId(1)).expect("window should be tracked");
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
            Event::WindowPlaced { window_id: WindowId(1), display_id: DisplayId(1), bounds: first },
        );
        apply(
            &mut state,
            Event::WindowPlaced { window_id: WindowId(1), display_id: DisplayId(1), bounds: second },
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
            Event::WindowPlaced { window_id: WindowId(1), display_id: DisplayId(1), bounds: first },
        );
        apply(
            &mut state,
            Event::WindowPlaced { window_id: WindowId(1), display_id: DisplayId(1), bounds: second },
        );
        apply(&mut state, Event::WindowRestoreRequested { window_id: WindowId(1) });

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(placement.bounds, first, "restore should return to the bounds before the last placement");
        assert_eq!(placement.display_id, DisplayId(1));
        assert_eq!(placement.previous_placement, None, "restore is a single step, not a stack");
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
            Event::WindowPlaced { window_id: WindowId(1), display_id: DisplayId(1), bounds: original_bounds },
        );
        apply(
            &mut state,
            Event::WindowThrowToDisplayRequested { window_id: WindowId(1), direction: DisplayDirection::Next },
        );
        apply(&mut state, Event::WindowRestoreRequested { window_id: WindowId(1) });

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(placement.display_id, DisplayId(1), "restore must move the window back to its source display");
        assert_eq!(placement.bounds, original_bounds);
    }

    #[test]
    fn apply_restore_twice_only_undoes_one_step() {
        let mut state = EngineState::default();
        let first = Rect::new(0, 0, 960, 1080);
        let second = Rect::new(0, 0, 1920, 1080);
        apply(
            &mut state,
            Event::WindowPlaced { window_id: WindowId(1), display_id: DisplayId(1), bounds: first },
        );
        apply(
            &mut state,
            Event::WindowPlaced { window_id: WindowId(1), display_id: DisplayId(1), bounds: second },
        );
        apply(&mut state, Event::WindowRestoreRequested { window_id: WindowId(1) });
        let revision_after_first_restore = state.revision;
        apply(&mut state, Event::WindowRestoreRequested { window_id: WindowId(1) });

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(placement.bounds, first, "a second restore with nothing remembered must be a no-op");
        assert_eq!(state.revision, revision_after_first_restore, "a no-op restore must not bump the revision");
    }

    #[test]
    fn apply_restore_is_a_noop_for_an_untracked_window() {
        let mut state = EngineState::default();
        apply(&mut state, Event::WindowRestoreRequested { window_id: WindowId(1) });

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
            Event::WindowPlaced { window_id: WindowId(1), display_id: DisplayId(1), bounds: left_half },
        );
        apply(
            &mut state,
            Event::WindowThrowToDisplayRequested { window_id: WindowId(1), direction: DisplayDirection::Next },
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
            Event::WindowThrowToDisplayRequested { window_id: WindowId(1), direction: DisplayDirection::Prev },
        );

        assert_eq!(state.windows.get(&WindowId(1)).unwrap().display_id, DisplayId(2));
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
            Event::WindowThrowToDisplayRequested { window_id: WindowId(1), direction: DisplayDirection::Next },
        );

        assert_eq!(state.revision, 1, "only the topology change should have bumped the revision");
        assert!(state.windows.is_empty());
    }

    #[test]
    fn apply_throw_is_a_noop_with_only_one_display() {
        let mut state = EngineState::default();
        apply(&mut state, Event::DisplayTopologyChanged(vec![display(1, "MON-A", 0)]));
        let bounds = Rect::new(0, 0, 960, 1080);
        apply(
            &mut state,
            Event::WindowPlaced { window_id: WindowId(1), display_id: DisplayId(1), bounds },
        );
        let revision_before_throw = state.revision;
        apply(
            &mut state,
            Event::WindowThrowToDisplayRequested { window_id: WindowId(1), direction: DisplayDirection::Next },
        );

        let placement = state.windows.get(&WindowId(1)).unwrap();
        assert_eq!(placement.bounds, bounds, "with no other display, the window must not move");
        assert_eq!(placement.display_id, DisplayId(1));
        assert_eq!(state.revision, revision_before_throw);
    }

    #[test]
    fn apply_throw_is_a_noop_when_the_windows_display_left_the_topology() {
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
        apply(&mut state, Event::DisplayTopologyChanged(vec![display(2, "MON-B", 1920)]));
        let revision_before_throw = state.revision;
        apply(
            &mut state,
            Event::WindowThrowToDisplayRequested { window_id: WindowId(1), direction: DisplayDirection::Next },
        );

        assert_eq!(
            state.windows.get(&WindowId(1)).unwrap().display_id,
            DisplayId(1),
            "the window's placement should be left untouched"
        );
        assert_eq!(state.revision, revision_before_throw);
    }

    #[test]
    fn apply_focused_sets_the_focused_window_and_bumps_revision() {
        let mut state = EngineState::default();
        assert_eq!(state.focused_window, None);

        apply(&mut state, Event::WindowFocused { window_id: WindowId(1) });

        assert_eq!(state.focused_window, Some(WindowId(1)));
        assert_eq!(state.revision, 1);
    }

    #[test]
    fn apply_focused_again_with_a_different_window_replaces_it() {
        let mut state = EngineState::default();
        apply(&mut state, Event::WindowFocused { window_id: WindowId(1) });
        apply(&mut state, Event::WindowFocused { window_id: WindowId(2) });

        assert_eq!(state.focused_window, Some(WindowId(2)));
        assert_eq!(state.revision, 2);
    }

    #[test]
    fn spawned_engine_starts_with_initial_state_and_applies_events() {
        let handle = spawn_engine(vec![display(1, "MON-A", 0)]);
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
        let handle = spawn_engine(Vec::new());
        let events = handle.events();
        handle.stop();

        // The reducer thread is gone; the queue is closed, so further
        // sends must fail rather than hang.
        assert!(events.send(Event::DisplayTopologyChanged(Vec::new())).is_err());
    }
}
