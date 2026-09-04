//! The container tree end to end, across a real state database and a real
//! restart of the engine (issue #51).
//!
//! The reducer's own tests cover insertion, removal, and swapping. What
//! only this level can show is that a *user-shaped* arrangement comes
//! back: the test deliberately produces a tree that building the same
//! windows from scratch would not produce, so a restored arrangement is
//! distinguishable from a freshly computed one.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use mosaix_config::{ResolvedConfig, ResolvedConfigSet, ResolvedProfile, TilingMode};
use mosaix_domain::{
    topology_fingerprint, ApplicationId, Display, Rect, Rotation, Window, WindowCapabilities,
    WindowId, WindowLifecycle, WindowRole,
};
use mosaix_engine::{
    spawn_engine, CardinalDirection, EngineState, Event, PersistenceIntent, StateReader,
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
            "mosaix-tree-{label}-{}-{unique}",
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

fn display() -> Display {
    Display {
        id: mosaix_domain::DisplayId(1),
        stable_fingerprint: "DISPLAY1".to_owned(),
        full_bounds: Rect::new(0, 0, 1920, 1080),
        work_area: Rect::new(0, 0, 1920, 1080),
        scale_factor: 1.0,
        rotation: Rotation::Landscape,
        is_primary: true,
    }
}

/// A profile that turns on tree-mode automatic tiling for this topology.
fn tree_config() -> ResolvedConfigSet {
    ResolvedConfigSet {
        base: ResolvedConfig::default(),
        profiles: vec![ResolvedProfile {
            fingerprint: topology_fingerprint(&[display()]),
            config: ResolvedConfig {
                automatic_tiling_enabled: true,
                tiling_mode: TilingMode::Tree,
                ..ResolvedConfig::default()
            },
        }],
    }
}

fn window(id: isize, application: &str, class: &str) -> Window {
    Window {
        id: WindowId(id),
        process_id: 1000 + id as u32,
        application_id: ApplicationId(application.to_owned()),
        executable_path: Some(PathBuf::from(format!("C:/apps/{application}"))),
        title: "Quarterly salary review.xlsx".to_owned(),
        native_class: Some(class.to_owned()),
        role: WindowRole::Normal,
        bounds: Rect::new(0, 0, 400, 300),
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

fn settle<T>(reader: &StateReader, what: &str, condition: impl Fn(&EngineState) -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(value) = condition(&reader.snapshot()) {
            return value;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

/// The left-to-right order of the windows currently arranged, by
/// application, which is the part that survives a change of handles.
fn order_by_application(state: &EngineState) -> Vec<String> {
    let mut placed: Vec<(i32, String)> = state
        .trees
        .values()
        .flat_map(|tree| tree.windows())
        .filter_map(|window_id| {
            let managed = state.inventory.get(window_id)?;
            let placement = state.windows.get(window_id)?;
            Some((placement.bounds.x, managed.window.application_id.0.clone()))
        })
        .collect();
    placed.sort_by_key(|(x, _)| *x);
    placed
        .into_iter()
        .map(|(_, application)| application)
        .collect()
}

#[test]
fn a_user_shaped_arrangement_comes_back_after_a_restart() {
    let temporary = TempDatabase::new("shape");

    // --- session one: build an arrangement, then reshape it ---
    let saved = {
        let engine = spawn_engine(vec![display()], tree_config());
        let events = engine.events();
        let reader = engine.state_reader();

        let _ = events.send(Event::WindowsObserved {
            windows: vec![
                window(11, "alpha.exe", "AlphaClass"),
                window(12, "beta.exe", "BetaClass"),
            ],
        });
        settle(&reader, "the first arrangement", |state| {
            (order_by_application(state).len() == 2).then_some(())
        });
        assert_eq!(
            order_by_application(&reader.snapshot()),
            vec!["alpha.exe", "beta.exe"],
            "insertion order puts alpha on the left"
        );

        // Swapping is what makes the stored arrangement different from the
        // one these two windows would produce on their own.
        let _ = events.send(Event::WindowFocused {
            window_id: WindowId(11),
            display_id: mosaix_domain::DisplayId(1),
            bounds: Rect::new(0, 0, 960, 1080),
        });
        let _ = events.send(Event::DirectionalSwapRequested {
            direction: CardinalDirection::Right,
        });
        settle(&reader, "the swap", |state| {
            (order_by_application(state) == vec!["beta.exe", "alpha.exe"]).then_some(())
        });

        let saved = settle(&reader, "the arrangement to be stored", |state| {
            state
                .persistence_intents
                .iter()
                .rev()
                .find_map(|intent| match intent {
                    PersistenceIntent::SaveContainerTree {
                        display_fingerprint,
                        tree,
                    } => Some((display_fingerprint.clone(), (**tree).clone())),
                    _ => None,
                })
        });
        engine.stop();
        saved
    };

    let (fingerprint, tree) = saved;
    assert_eq!(fingerprint, "DISPLAY1");
    {
        let mut store = Persistence::open(&temporary.path()).expect("database opens");
        store.save_tree(&fingerprint, &tree).expect("it stores");
    }

    // --- session two: same applications, different native handles ---
    let store = Persistence::open(&temporary.path()).expect("database reopens");
    let restored = store.load_trees().expect("arrangements are readable");
    assert_eq!(restored.len(), 1);

    let engine = spawn_engine(vec![display()], tree_config());
    let events = engine.events();
    let reader = engine.state_reader();

    let _ = events.send(Event::WindowsObserved {
        windows: vec![
            window(901, "alpha.exe", "AlphaClass"),
            window(902, "beta.exe", "BetaClass"),
        ],
    });
    settle(&reader, "the fresh arrangement", |state| {
        (order_by_application(state).len() == 2).then_some(())
    });
    assert_eq!(
        order_by_application(&reader.snapshot()),
        vec!["alpha.exe", "beta.exe"],
        "without the stored arrangement, insertion order applies again"
    );

    let _ = events.send(Event::ContainerTreesLoaded(restored));

    settle(&reader, "the stored arrangement to be adopted", |state| {
        (order_by_application(state) == vec!["beta.exe", "alpha.exe"]).then_some(())
    });
    engine.stop();
}

/// Each arranged window's width, by application, left to right.
fn widths_by_application(state: &EngineState) -> Vec<(String, i32)> {
    let mut placed: Vec<(i32, String, i32)> = state
        .trees
        .values()
        .flat_map(|tree| tree.windows())
        .filter_map(|window_id| {
            let managed = state.inventory.get(window_id)?;
            let placement = state.windows.get(window_id)?;
            Some((
                placement.bounds.x,
                managed.window.application_id.0.clone(),
                placement.bounds.width,
            ))
        })
        .collect();
    placed.sort_by_key(|(x, _, _)| *x);
    placed
        .into_iter()
        .map(|(_, application, width)| (application, width))
        .collect()
}

#[test]
fn resized_divider_weights_come_back_after_a_restart() {
    let temporary = TempDatabase::new("resize");

    // --- session one: two windows, then grow the right one twice ---
    let saved = {
        let engine = spawn_engine(vec![display()], tree_config());
        let events = engine.events();
        let reader = engine.state_reader();
        let _ = events.send(Event::WindowsObserved {
            windows: vec![
                window(11, "alpha.exe", "AlphaClass"),
                window(12, "beta.exe", "BetaClass"),
            ],
        });
        settle(&reader, "the first arrangement", |state| {
            (order_by_application(state).len() == 2).then_some(())
        });
        let _ = events.send(Event::WindowFocused {
            window_id: WindowId(12),
            display_id: mosaix_domain::DisplayId(1),
            bounds: Rect::new(960, 0, 960, 1080),
        });
        for _ in 0..2 {
            let _ = events.send(Event::TreeResizeRequested {
                direction: CardinalDirection::Left,
            });
        }
        settle(&reader, "both resizes", |state| {
            (widths_by_application(state)
                == vec![("alpha.exe".to_owned(), 768), ("beta.exe".to_owned(), 1152)])
            .then_some(())
        });
        let transactions = reader
            .snapshot()
            .persistence_intents
            .iter()
            .filter(|intent| matches!(intent, PersistenceIntent::RecordUndoTransaction(_)))
            .count();
        assert_eq!(transactions, 2, "each resize is its own undo transaction");

        let saved = settle(&reader, "the arrangement to be stored", |state| {
            state
                .persistence_intents
                .iter()
                .rev()
                .find_map(|intent| match intent {
                    PersistenceIntent::SaveContainerTree {
                        display_fingerprint,
                        tree,
                    } => Some((display_fingerprint.clone(), (**tree).clone())),
                    _ => None,
                })
        });
        engine.stop();
        saved
    };
    {
        let mut store = Persistence::open(&temporary.path()).expect("database opens");
        store.save_tree(&saved.0, &saved.1).expect("it stores");
    }

    // --- session two: the same applications, new handles ---
    let restored = Persistence::open(&temporary.path())
        .expect("database reopens")
        .load_trees()
        .expect("arrangements are readable");
    let engine = spawn_engine(vec![display()], tree_config());
    let events = engine.events();
    let reader = engine.state_reader();
    let _ = events.send(Event::WindowsObserved {
        windows: vec![
            window(901, "alpha.exe", "AlphaClass"),
            window(902, "beta.exe", "BetaClass"),
        ],
    });
    settle(&reader, "the fresh arrangement", |state| {
        (widths_by_application(state).len() == 2).then_some(())
    });
    let _ = events.send(Event::ContainerTreesLoaded(restored));

    settle(&reader, "the resized weights to be adopted", |state| {
        (widths_by_application(state)
            == vec![("alpha.exe".to_owned(), 768), ("beta.exe".to_owned(), 1152)])
        .then_some(())
    });
    engine.stop();
}

#[test]
fn a_window_missing_at_restart_keeps_a_dormant_slot_it_reclaims_when_it_reopens() {
    let temporary = TempDatabase::new("dormant");
    let saved = {
        let engine = spawn_engine(vec![display()], tree_config());
        let events = engine.events();
        let reader = engine.state_reader();
        let _ = events.send(Event::WindowsObserved {
            windows: vec![
                window(11, "alpha.exe", "AlphaClass"),
                window(12, "beta.exe", "BetaClass"),
            ],
        });
        // Swap so the stored shape is beta | alpha, which fresh insertion
        // of alpha alone followed by beta would not reproduce.
        settle(&reader, "the first arrangement", |state| {
            (order_by_application(state).len() == 2).then_some(())
        });
        let _ = events.send(Event::WindowFocused {
            window_id: WindowId(11),
            display_id: mosaix_domain::DisplayId(1),
            bounds: Rect::new(0, 0, 960, 1080),
        });
        let _ = events.send(Event::DirectionalSwapRequested {
            direction: CardinalDirection::Right,
        });
        settle(&reader, "the swap", |state| {
            (order_by_application(state) == vec!["beta.exe", "alpha.exe"]).then_some(())
        });
        let saved = settle(&reader, "the arrangement to be stored", |state| {
            state
                .persistence_intents
                .iter()
                .rev()
                .find_map(|intent| match intent {
                    PersistenceIntent::SaveContainerTree {
                        display_fingerprint,
                        tree,
                    } => Some((display_fingerprint.clone(), (**tree).clone())),
                    _ => None,
                })
        });
        engine.stop();
        saved
    };
    {
        let mut store = Persistence::open(&temporary.path()).expect("database opens");
        store.save_tree(&saved.0, &saved.1).expect("it stores");
    }

    // --- session two: only alpha is open at first ---
    let restored = Persistence::open(&temporary.path())
        .expect("database reopens")
        .load_trees()
        .expect("arrangements are readable");
    let engine = spawn_engine(vec![display()], tree_config());
    let events = engine.events();
    let reader = engine.state_reader();
    let _ = events.send(Event::WindowsObserved {
        windows: vec![window(901, "alpha.exe", "AlphaClass")],
    });
    settle(&reader, "alpha alone", |state| {
        (order_by_application(state).len() == 1).then_some(())
    });
    let _ = events.send(Event::ContainerTreesLoaded(restored));
    settle(&reader, "beta's slot to be kept dormant", |state| {
        let tree = state.trees.get(&mosaix_domain::DisplayId(1))?;
        (tree.dormant_positions().len() == 1).then_some(())
    });
    let alpha_alone = reader.snapshot().windows[&WindowId(901)].bounds;
    assert_eq!(
        alpha_alone,
        Rect::new(0, 0, 1920, 1080),
        "a dormant slot reserves no space"
    );

    // beta opens: it goes back to the left, where the stored shape had it.
    let _ = events.send(Event::WindowsObserved {
        windows: vec![
            window(901, "alpha.exe", "AlphaClass"),
            window(902, "beta.exe", "BetaClass"),
        ],
    });
    settle(&reader, "beta to reclaim its slot", |state| {
        (order_by_application(state) == vec!["beta.exe", "alpha.exe"]).then_some(())
    });
    assert!(reader.snapshot().trees[&mosaix_domain::DisplayId(1)]
        .dormant_positions()
        .is_empty());
    engine.stop();
}

#[test]
fn an_arrangement_whose_windows_are_gone_restores_nothing_rather_than_guessing() {
    let temporary = TempDatabase::new("absent");
    let stored = {
        let engine = spawn_engine(vec![display()], tree_config());
        let events = engine.events();
        let reader = engine.state_reader();
        let _ = events.send(Event::WindowsObserved {
            windows: vec![
                window(11, "alpha.exe", "AlphaClass"),
                window(12, "beta.exe", "BetaClass"),
            ],
        });
        let stored = settle(&reader, "an arrangement to store", |state| {
            state
                .persistence_intents
                .iter()
                .rev()
                .find_map(|intent| match intent {
                    PersistenceIntent::SaveContainerTree { tree, .. } => Some((**tree).clone()),
                    _ => None,
                })
        });
        engine.stop();
        stored
    };
    {
        let mut store = Persistence::open(&temporary.path()).expect("database opens");
        store.save_tree("DISPLAY1", &stored).unwrap();
    }
    let store = Persistence::open(&temporary.path()).expect("database reopens");
    let restored = store.load_trees().unwrap();

    // Completely different applications this session.
    let engine = spawn_engine(vec![display()], tree_config());
    let events = engine.events();
    let reader = engine.state_reader();
    let _ = events.send(Event::WindowsObserved {
        windows: vec![window(901, "gamma.exe", "GammaClass")],
    });
    settle(&reader, "the fresh arrangement", |state| {
        (order_by_application(state).len() == 1).then_some(())
    });

    let _ = events.send(Event::ContainerTreesLoaded(restored));
    std::thread::sleep(Duration::from_millis(100));

    let state = reader.snapshot();
    assert_eq!(
        order_by_application(&state),
        vec!["gamma.exe"],
        "an arrangement whose windows cannot be identified must not displace the live one"
    );
    engine.stop();
}
