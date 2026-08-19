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

use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use mosaix_domain::{topology_fingerprint, Display};

/// Default bound on the event queue before a sender blocks. Chosen
/// generously relative to expected event rates -- architecture doc section
/// 18 budgets "ordinary OS event to stable layout plan" at under 100ms at
/// p95, so a deep queue is not needed to absorb bursts.
pub const DEFAULT_QUEUE_CAPACITY: usize = 256;

/// State the reducer owns and is the only writer of.
///
/// Only display topology exists as real domain state today; window
/// registry, workspaces, and rules will extend this as those domain types
/// land (architecture doc section 7).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineState {
    /// Bumped on every committed mutation (architecture doc section 13:
    /// "Monotonic state revision on every committed mutation").
    pub revision: u64,
    pub displays: Vec<Display>,
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

    fn display(id: u64, fingerprint: &str, x: i32) -> Display {
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
