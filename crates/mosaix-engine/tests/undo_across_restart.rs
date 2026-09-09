//! Persistent undo end to end, across a real state database and a real
//! restart of the engine.
//!
//! The reducer's own tests cover each refusal in isolation. What only this
//! level can show is the round trip: a command recorded through the
//! persistence-intent seam, written to SQLite, read back by a process that
//! never saw the command happen, and undone against windows whose native
//! handles are different from the ones the command moved.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use mosaix_domain::{
    ApplicationId, Display, Rect, Rotation, Window, WindowCapabilities, WindowId, WindowLifecycle,
    WindowRole,
};
use mosaix_engine::{
    spawn_engine, EngineEffect, Event, PersistenceIntent, StateReader, ZoneSnapDirection,
};
use mosaix_persistence::Persistence;

struct TempDatabase {
    directory: PathBuf,
}

impl TempDatabase {
    fn new(label: &str) -> Self {
        static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "mosaix-undo-{label}-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("temporary directory is creatable");
        Self { directory }
    }

    fn path(&self) -> PathBuf {
        self.directory.join("state.db")
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn display(fingerprint: &str, x: i32) -> Display {
    Display {
        id: mosaix_domain::DisplayId(1),
        stable_fingerprint: fingerprint.to_owned(),
        full_bounds: Rect::new(x, 0, 1920, 1080),
        work_area: Rect::new(x, 0, 1920, 1080),
        scale_factor: 1.0,
        rotation: Rotation::Landscape,
        is_primary: true,
    }
}

fn window(id: isize, bounds: Rect) -> Window {
    Window {
        id: WindowId(id),
        process_id: 4242,
        application_id: ApplicationId("Code.exe".to_owned()),
        executable_path: Some(PathBuf::from("C:/apps/Code.exe")),
        title: "Quarterly salary review.xlsx".to_owned(),
        native_class: Some("Chrome_WidgetWin_1".to_owned()),
        role: WindowRole::Normal,
        bounds,
        display_id: mosaix_domain::DisplayId(1),
        capabilities: WindowCapabilities {
            can_move: true,
            can_resize: true,
            can_minimize: true,
            can_maximize: true,
        },
        elevated: false,
        lifecycle: WindowLifecycle::Active,
        minimum_size: None,
    }
}

/// Waits for `condition` to hold of a snapshot, since the reducer runs on
/// its own thread. Fails the test rather than hanging.
fn settle<T>(
    reader: &StateReader,
    what: &str,
    condition: impl Fn(&mosaix_engine::EngineState) -> Option<T>,
) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(value) = condition(&reader.snapshot()) {
            return value;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

/// Runs one explicit snap and returns the transaction it asked to store.
fn record_a_snap(fingerprint: &str) -> mosaix_domain::UndoTransactionDraft {
    let engine = spawn_engine(vec![display(fingerprint, 0)], Default::default());
    let events = engine.events();
    let reader = engine.state_reader();

    let _ = events.send(Event::WindowsObserved {
        windows: vec![window(700, Rect::new(10, 10, 500, 500))],
    });
    let _ = events.send(Event::WindowFocused {
        window_id: WindowId(700),
        display_id: mosaix_domain::DisplayId(1),
        bounds: Rect::new(10, 10, 500, 500),
    });
    let _ = events.send(Event::ZoneSnapRequested {
        direction: ZoneSnapDirection::Left,
    });

    let draft = settle(&reader, "the snap to be recorded", |state| {
        state
            .persistence_intents
            .iter()
            .find_map(|intent| match intent {
                PersistenceIntent::RecordUndoTransaction(draft) => Some(draft.clone()),
                _ => None,
            })
    });
    engine.stop();
    draft
}

#[test]
fn a_snap_survives_a_restart_and_undoes_against_a_new_native_handle() {
    let temporary = TempDatabase::new("round-trip");
    let draft = record_a_snap("DISPLAY1");

    // The agent's worker would do this; doing it directly is what makes
    // the restart below a real one rather than a shared in-memory handoff.
    let stored_id = {
        let mut store = Persistence::open(&temporary.path()).expect("database opens");
        store
            .record_transaction(&draft)
            .expect("the transaction records")
    };

    // ---- restart: a fresh database handle and a fresh engine ----------
    let store = Persistence::open(&temporary.path()).expect("database reopens");
    let transaction = store
        .newest_transaction()
        .expect("history is readable")
        .expect("the transaction outlived the restart");
    assert_eq!(transaction.id, stored_id);

    let engine = spawn_engine(vec![display("DISPLAY1", 0)], Default::default());
    let events = engine.events();
    let reader = engine.state_reader();

    // The window is back, at the position the snap left it, but the OS has
    // given it a different handle. Only the durable evidence can connect
    // the two.
    let _ = events.send(Event::WindowsObserved {
        windows: vec![window(915, Rect::new(0, 0, 960, 1080))],
    });
    let _ = events.send(Event::UndoHistoryLoaded(Some(Box::new(transaction))));
    settle(&reader, "history to be published", |state| {
        state.newest_undo.as_ref().map(|_| ())
    });

    let _ = events.send(Event::UndoRequested);

    let placement = settle(&reader, "the window to be restored", |state| {
        state.effects.iter().rev().find_map(|effect| match effect {
            EngineEffect::PlaceWindow {
                window_id, bounds, ..
            } if *window_id == WindowId(915) && *bounds == Rect::new(10, 10, 500, 500) => {
                Some(*bounds)
            }
            _ => None,
        })
    });
    assert_eq!(placement, Rect::new(10, 10, 500, 500));

    settle(&reader, "the transaction to be consumed", |state| {
        state
            .persistence_intents
            .contains(&PersistenceIntent::ConsumeUndoTransaction(stored_id))
            .then_some(())
    });
    engine.stop();
}

#[test]
fn a_restart_onto_different_displays_refuses_and_keeps_the_transaction() {
    let temporary = TempDatabase::new("topology");
    let draft = record_a_snap("DISPLAY1");
    let stored_id = {
        let mut store = Persistence::open(&temporary.path()).expect("database opens");
        store.record_transaction(&draft).expect("records")
    };

    let store = Persistence::open(&temporary.path()).expect("database reopens");
    let transaction = store.newest_transaction().unwrap().unwrap();

    // Same window, same everything -- but plugged into a different monitor.
    let engine = spawn_engine(vec![display("DISPLAY2", 0)], Default::default());
    let events = engine.events();
    let reader = engine.state_reader();
    let _ = events.send(Event::WindowsObserved {
        windows: vec![window(915, Rect::new(0, 0, 960, 1080))],
    });
    let _ = events.send(Event::UndoHistoryLoaded(Some(Box::new(transaction))));
    settle(&reader, "history to be published", |state| {
        state.newest_undo.as_ref().map(|_| ())
    });
    let baseline_effects = reader.snapshot().effects.len();

    let _ = events.send(Event::UndoRequested);

    let refusal = settle(&reader, "undo to refuse", |state| {
        state.last_undo_result.clone()
    });
    let mosaix_domain::UndoResult::Refused(refusal) = refusal else {
        panic!("expected a refusal on a different topology");
    };
    assert_eq!(refusal.code(), "topology_changed");

    let after = reader.snapshot();
    assert_eq!(
        after.effects.len(),
        baseline_effects,
        "a refusal must place nothing"
    );
    assert!(
        after.newest_undo.is_some(),
        "the refused transaction is kept for retry"
    );

    // And it is still on disk: refusing never consumes.
    assert_eq!(
        store.newest_transaction().unwrap().map(|held| held.id),
        Some(stored_id)
    );
    engine.stop();
}
