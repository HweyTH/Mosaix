//! The logical workspace pool end to end, across a real state database and
//! a real restart of the engine.
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

use mosaix_config::{
    ResolvedConfig, ResolvedConfigSet, ResolvedProfile, ResolvedWorkspaceSwitching, TilingMode,
};
use mosaix_domain::recovery::RecoveryEntryId;
use mosaix_domain::workspace::ParkingCapability;
use mosaix_domain::{
    topology_fingerprint, ApplicationId, Display, DisplayId, Rect, Rotation, Window,
    WindowCapabilities, WindowId, WindowLifecycle, WindowRole, WorkspaceName, WorkspaceOrigin,
};
use mosaix_engine::{
    spawn_engine, CardinalDirection, EngineEffect, EngineState, Event, EventSender,
    PersistenceIntent, StateReader,
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

/// A profile that turns on tree-mode automatic tiling for this topology,
/// declares one workspace, `dev`, and requests experimental switching for
/// it -- which is the only way a switch may move a window.
fn tree_config() -> ResolvedConfigSet {
    let config = ResolvedConfig {
        workspaces: vec![WorkspaceName::new("dev").unwrap()],
        workspace_switching: Some(ResolvedWorkspaceSwitching {
            experimental: true,
            displayed: [("DISPLAY1".to_owned(), WorkspaceName::new("dev").unwrap())]
                .into_iter()
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

/// As much of the agent's two answering loops as a switch needs: the
/// persistence worker acknowledging each recovery entry durable, and the
/// placement executor reporting each park and restore as landed.
///
/// Without these the engine is right to sit still: a window may not leave
/// visible geometry until its way back is on disk.
struct Adapter {
    intents: usize,
    effects: usize,
    next_entry: i64,
}

impl Adapter {
    fn new() -> Self {
        Self {
            intents: 0,
            effects: 0,
            next_entry: 0,
        }
    }

    fn pump(&mut self, events: &EventSender, state: &EngineState) {
        for intent in state.persistence_intents.iter().skip(self.intents) {
            if let PersistenceIntent::RecordRecovery { token, .. } = intent {
                self.next_entry += 1;
                let _ = events.send(Event::RecoveryEntryDurable {
                    token: *token,
                    entry_id: RecoveryEntryId(self.next_entry),
                });
            }
        }
        self.intents = state.persistence_intents.len();
        for effect in state.effects.iter().skip(self.effects) {
            match effect {
                EngineEffect::ParkWindow {
                    window_id,
                    entry_id,
                } => {
                    let _ = events.send(Event::WindowParked {
                        window_id: *window_id,
                        entry_id: *entry_id,
                    });
                }
                EngineEffect::RestoreWindow { window_id, .. } => {
                    let _ = events.send(Event::WindowRestored {
                        window_id: *window_id,
                    });
                }
                _ => {}
            }
        }
        self.effects = state.effects.len();
    }
}

fn settle<T>(
    events: &EventSender,
    adapter: &mut Adapter,
    reader: &StateReader,
    what: &str,
    condition: impl Fn(&EngineState) -> Option<T>,
) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let snapshot = reader.snapshot();
        adapter.pump(events, &snapshot);
        if let Some(value) = condition(&snapshot) {
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

    // ---- session one: shape dev, switch away, then back -------------
    {
        let engine = spawn_engine(vec![display()], tree_config());
        let events = engine.events();
        let reader = engine.state_reader();
        let mut adapter = Adapter::new();
        let _ = events.send(Event::ParkingCapabilityReported(
            ParkingCapability::Verified,
        ));

        let _ = events.send(Event::WindowsObserved {
            windows: vec![
                window(11, "alpha.exe", "AlphaClass"),
                window(12, "beta.exe", "BetaClass"),
            ],
        });
        settle(
            &events,
            &mut adapter,
            &reader,
            "the first arrangement",
            |state| (order_by_application(state).len() == 2).then_some(()),
        );
        let _ = events.send(Event::WindowFocused {
            window_id: WindowId(11),
            display_id: DisplayId(1),
            bounds: Rect::new(0, 0, 960, 1080),
        });
        let _ = events.send(Event::DirectionalSwapRequested {
            direction: CardinalDirection::Right,
        });
        settle(&events, &mut adapter, &reader, "the swap", |state| {
            (order_by_application(state) == vec!["beta.exe", "alpha.exe"]).then_some(())
        });

        // Switching to chat is a transaction: alpha and beta leave the
        // screen before chat is displayed, and only then does the
        // assignment change.
        let _ = events.send(Event::WorkspaceCreateRequested {
            name: "chat".to_owned(),
        });
        let _ = events.send(Event::WorkspaceFocusRequested {
            name: "chat".to_owned(),
        });
        settle(
            &events,
            &mut adapter,
            &reader,
            "chat to be displayed",
            |state| (state.workspaces.display_of(&ws("chat")) == Some(DisplayId(1))).then_some(()),
        );
        let parked = reader.snapshot();
        assert_eq!(
            parked.parked_windows.len(),
            2,
            "the outgoing workspace's windows left the screen through the ledger"
        );
        assert!(
            parked.switch.is_none() && parked.switch_degraded.is_none(),
            "the transaction closed cleanly"
        );

        let _ = events.send(Event::WindowsObserved {
            windows: vec![
                window(11, "alpha.exe", "AlphaClass"),
                window(12, "beta.exe", "BetaClass"),
                window(13, "gamma.exe", "GammaClass"),
            ],
        });
        settle(
            &events,
            &mut adapter,
            &reader,
            "gamma to join chat",
            |state| {
                (state.workspaces.workspace_of(WindowId(13)) == Some(&ws("chat"))).then_some(())
            },
        );

        // Back to dev: gamma parks, alpha and beta come off the parking
        // site, and dev is displayed again with the shape it had.
        let _ = events.send(Event::WorkspaceFocusRequested {
            name: "dev".to_owned(),
        });
        settle(
            &events,
            &mut adapter,
            &reader,
            "dev to be displayed again",
            |state| (state.workspaces.display_of(&ws("dev")) == Some(DisplayId(1))).then_some(()),
        );
        let back = reader.snapshot();
        let mut parked: Vec<WindowId> = back.parked_windows.keys().copied().collect();
        parked.sort_by_key(|window_id| window_id.0);
        assert_eq!(
            parked,
            vec![WindowId(13)],
            "only the newly hidden workspace's window is parked"
        );

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

    // ---- session two: same applications, different native handles -----
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
            ("chat", WorkspaceOrigin::Command, None, Some(1)),
            (
                "dev",
                WorkspaceOrigin::Configuration,
                Some("DISPLAY1"),
                Some(2)
            ),
        ],
        "the pool, each workspace's display, and each tree were stored"
    );

    let engine = spawn_engine(vec![display()], tree_config());
    let events = engine.events();
    let reader = engine.state_reader();
    let mut adapter = Adapter::new();
    let _ = events.send(Event::ParkingCapabilityReported(
        ParkingCapability::Verified,
    ));
    let _ = events.send(Event::WorkspacesLoaded(restored));
    settle(
        &events,
        &mut adapter,
        &reader,
        "the pool to be restored",
        |state| {
            (state.workspaces.display_of(&ws("dev")) == Some(DisplayId(1))
                && state.workspaces.contains(&ws("chat"))
                && !state.workspaces.is_displayed(&ws("chat")))
            .then_some(())
        },
    );

    // Dev's stored tree reclaims alpha and beta by evidence, in the
    // swapped order the user left them in -- none of which depends on a
    // native handle, and all of the handles are new.
    let _ = events.send(Event::WindowsObserved {
        windows: vec![
            window(901, "alpha.exe", "AlphaClass"),
            window(902, "beta.exe", "BetaClass"),
        ],
    });
    settle(
        &events,
        &mut adapter,
        &reader,
        "dev's stored shape",
        |state| (order_by_application(state) == vec!["beta.exe", "alpha.exe"]).then_some(()),
    );
    let state = reader.snapshot();
    assert!(
        state.trees[&DisplayId(1)].dormant_positions().is_empty(),
        "both stored positions found their window again"
    );
    for id in [901, 902] {
        assert_eq!(
            state.workspaces.workspace_of(WindowId(id)),
            Some(&ws("dev")),
            "a window is a member of the workspace displayed where it appeared"
        );
    }

    // Switching to chat in the new session parks dev's windows and brings
    // chat's stored tree back; gamma is gone, so its position is dormant.
    let _ = events.send(Event::WorkspaceFocusRequested {
        name: "chat".to_owned(),
    });
    settle(
        &events,
        &mut adapter,
        &reader,
        "chat to be displayed again",
        |state| (state.workspaces.display_of(&ws("chat")) == Some(DisplayId(1))).then_some(()),
    );
    let state = reader.snapshot();
    assert_eq!(
        state.parked_windows.len(),
        2,
        "dev's two windows left the screen for the switch"
    );
    assert_eq!(
        state.trees[&DisplayId(1)]
            .dormant_positions()
            .into_iter()
            .map(|(_, dormant)| dormant.evidence.application_id.0.clone())
            .collect::<Vec<_>>(),
        vec!["gamma.exe"],
        "chat's stored tree came back, keeping the position of a window that is gone"
    );
    engine.stop();
}
