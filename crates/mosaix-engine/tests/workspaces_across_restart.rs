//! The logical workspace pool end to end, across a real state database and
//! a real restart of the engine (issue #56).
//!
//! The reducer's own tests cover the lifecycle. What only this level can
//! show is that the pool as a whole comes back: a command-created
//! workspace rejoins the pool, each workspace returns to the display it
//! was on, and a hidden workspace's user-shaped tree is still its tree
//! when it is next displayed -- with none of it depending on native
//! handles, which a restart replaces.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use mosaix_config::{ResolvedConfig, ResolvedConfigSet, ResolvedProfile, TilingMode};
use mosaix_domain::{
    topology_fingerprint, ApplicationId, Display, DisplayId, Rect, Rotation, Window,
    WindowCapabilities, WindowId, WindowLifecycle, WindowRole, WorkspaceName, WorkspaceOrigin,
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
            "mosaix-workspaces-{label}-{}-{unique}",
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
        id: DisplayId(1),
        stable_fingerprint: "DISPLAY1".to_owned(),
        full_bounds: Rect::new(0, 0, 1920, 1080),
        work_area: Rect::new(0, 0, 1920, 1080),
        scale_factor: 1.0,
        rotation: Rotation::Landscape,
        is_primary: true,
    }
}

/// A profile that turns on tree-mode automatic tiling for this topology
/// and declares one workspace, `dev`.
fn tree_config() -> ResolvedConfigSet {
    let config = ResolvedConfig {
        workspaces: vec![WorkspaceName::new("dev").unwrap()],
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
            fingerprint: topology_fingerprint(&[display()]),
            config,
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
        display_id: DisplayId(1),
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

/// The left-to-right order of the windows arranged on display 1, by
/// application.
fn order_by_application(state: &EngineState) -> Vec<String> {
    let mut placed: Vec<(i32, String)> = state
        .trees
        .get(&DisplayId(1))
        .map(|tree| tree.windows())
        .unwrap_or_default()
        .into_iter()
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

fn ws(name: &str) -> WorkspaceName {
    WorkspaceName::new(name).unwrap()
}

#[test]
fn the_pool_its_displays_and_a_hidden_workspaces_tree_come_back_after_a_restart() {
    let temporary = TempDatabase::new("pool");

    // --- session one: shape dev, then hide it behind a new workspace ---
    {
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
            window_id: WindowId(11),
            display_id: DisplayId(1),
            bounds: Rect::new(0, 0, 960, 1080),
        });
        let _ = events.send(Event::DirectionalSwapRequested {
            direction: CardinalDirection::Right,
        });
        settle(&reader, "the swap", |state| {
            (order_by_application(state) == vec!["beta.exe", "alpha.exe"]).then_some(())
        });

        let _ = events.send(Event::WorkspaceCreateRequested {
            name: "chat".to_owned(),
        });
        let _ = events.send(Event::WorkspaceFocusRequested {
            name: "chat".to_owned(),
        });
        settle(&reader, "chat to be displayed", |state| {
            (state.workspaces.display_of(&ws("chat")) == Some(DisplayId(1))).then_some(())
        });
        let _ = events.send(Event::WindowsObserved {
            windows: vec![
                window(11, "alpha.exe", "AlphaClass"),
                window(12, "beta.exe", "BetaClass"),
                window(13, "gamma.exe", "GammaClass"),
            ],
        });
        settle(&reader, "gamma to join chat", |state| {
            (state.workspaces.workspace_of(WindowId(13)) == Some(&ws("chat"))).then_some(())
        });

        // Every workspace write the reducer asked for, applied in order to
        // a real database -- what the agent's persistence bridge does.
        let snapshot = reader.snapshot();
        let mut store = Persistence::open(&temporary.path()).expect("database opens");
        for intent in &snapshot.persistence_intents {
            match intent {
                PersistenceIntent::SaveWorkspace(workspace) => {
                    store.save_workspace(workspace).expect("it stores");
                }
                PersistenceIntent::DeleteWorkspace(name) => {
                    store.delete_workspace(name).expect("it deletes");
                }
                _ => {}
            }
        }
        engine.stop();
    }

    // --- session two: same applications, different native handles ---
    let restored = {
        let store = Persistence::open(&temporary.path()).expect("database reopens");
        store.load_workspaces().expect("workspaces are readable")
    };
    assert_eq!(
        restored
            .iter()
            .map(|workspace| {
                (
                    workspace.name.as_str(),
                    workspace.origin,
                    workspace.displayed_fingerprint.as_deref(),
                    workspace.tree.as_ref().map(|tree| tree.windows().len()),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            ("chat", WorkspaceOrigin::Command, Some("DISPLAY1"), Some(1)),
            ("dev", WorkspaceOrigin::Configuration, None, Some(2)),
        ],
        "the pool, each workspace's display, and each tree were stored"
    );

    let engine = spawn_engine(vec![display()], tree_config());
    let events = engine.events();
    let reader = engine.state_reader();
    let _ = events.send(Event::WorkspacesLoaded(restored));
    settle(&reader, "the pool to be restored", |state| {
        (state.workspaces.display_of(&ws("chat")) == Some(DisplayId(1))
            && state.workspaces.contains(&ws("dev"))
            && !state.workspaces.is_displayed(&ws("dev")))
        .then_some(())
    });

    let _ = events.send(Event::WindowsObserved {
        windows: vec![
            window(901, "alpha.exe", "AlphaClass"),
            window(902, "beta.exe", "BetaClass"),
            window(903, "gamma.exe", "GammaClass"),
        ],
    });
    settle(&reader, "the new windows to be assigned", |state| {
        (state.inventory.len() == 3).then_some(())
    });
    let state = reader.snapshot();
    // Every new window joined the displayed workspace: membership is a
    // native-handle fact and does not survive a restart, so alpha and
    // beta are chat's now and are arranged with gamma, while dev's stored
    // tree keeps their positions dormant until they are matched back.
    for id in [901, 902, 903] {
        assert_eq!(
            state.workspaces.workspace_of(WindowId(id)),
            Some(&ws("chat"))
        );
    }
    assert_eq!(
        order_by_application(&state).len(),
        3,
        "chat is displayed, and every window is its member"
    );

    // Focusing dev brings its tree back, and the tree reclaims alpha and
    // beta from evidence -- in the swapped order the user left them in.
    let _ = events.send(Event::WorkspaceFocusRequested {
        name: "dev".to_owned(),
    });
    settle(&reader, "dev to be displayed", |state| {
        (state.workspaces.display_of(&ws("dev")) == Some(DisplayId(1))).then_some(())
    });
    // Alpha and beta are chat's members, so dev's tree finds no live
    // candidates yet: its two positions stay dormant, exactly as a tree
    // whose windows are absent does (issue #51). Reassigning membership
    // to a workspace is issue #60's switch transaction; here the
    // persisted shape is what is under test.
    let state = reader.snapshot();
    let dev_tree = &state.trees[&DisplayId(1)];
    assert_eq!(
        dev_tree.dormant_positions().len(),
        2,
        "the stored tree came back with both positions kept for their windows"
    );
    let stored_order: Vec<String> = dev_tree
        .dormant_positions()
        .into_iter()
        .map(|(_, dormant)| dormant.evidence.application_id.0.clone())
        .collect();
    assert_eq!(
        stored_order,
        vec!["beta.exe", "alpha.exe"],
        "the swap the user made is the shape that was stored"
    );
    engine.stop();
}
