//! Mosaix background window management agent.
//!
//! Runs at login, owns the authoritative window-manager state via the
//! reducer in `mosaix-engine`, and stays useful whether or not the
//! settings UI is open (architecture doc section 5.1).
//!
//! Two startup preconditions are treated as fatal (`.expect`, no degraded
//! mode): DPI awareness, because every downstream geometry call is wrong
//! without it, and the shutdown-signal handler, because running without
//! any way to shut down gracefully means a real logoff/system-shutdown
//! force-kills the agent mid-mutation -- worse than not starting at all,
//! and this call fails in practice only for genuinely exceptional OS
//! reasons, not from how this binary calls it. Every other subsystem
//! failure (a topology read, the topology watcher, config directory
//! creation/read/validation) is logged and degrades the agent instead --
//! it's meant to run all day. An unusable config directory falls back to
//! `mosaix_config::fallback_config()` rather than refusing to start (ADR
//! 0007).

#[cfg(windows)]
mod hotkeys;
#[cfg(windows)]
mod overlay;
#[cfg(windows)]
mod recovery;

/// Starts `RegisterHotKey` registration for `bindings` and a forwarder
/// thread translating each firing into `Event::ZoneSnapRequested`,
/// mirroring the shape of every other OS-event forwarder in this file.
/// Registration stays partial-success (ADR 0002), preserved through every
/// re-registration and not just the first; only a failure to start the
/// registration thread itself is reported to the caller.
///
/// Which bindings the platform refused travels back into engine state as
/// [`mosaix_engine::Event::HotkeyRegistrationReported`], so a combination
/// another application took while hotkey capture held registration
/// suspended is named to the user rather than left as a shortcut that
/// silently stopped working (ADR 0021).
///
/// When `overlay_tx` is present, each successful enqueue also signals the
/// snap-preview controller (Feature 34) with the pre-send revision so it
/// can flash the committed placement.
#[cfg(windows)]
fn start_hotkeys_and_forward(
    bindings: Vec<mosaix_platform_windows::HotkeyBinding>,
    mut registry: hotkeys::HotkeyRegistry,
    events: mosaix_engine::EventSender,
    state_reader: mosaix_engine::StateReader,
    overlay_tx: Option<std::sync::mpsc::Sender<overlay::OverlayRequest>>,
) -> mosaix_platform_windows::Result<(
    mosaix_platform_windows::HotkeyRegistrations,
    std::thread::JoinHandle<()>,
)> {
    let (registrations, hotkey_events) = mosaix_platform_windows::start_hotkeys(bindings)?;
    let mut unregistered = Vec::new();
    for result in &registrations.results {
        if let Err(err) = &result.outcome {
            // A binding the OS refused holds no registry entry, so its id
            // resolves to nothing rather than to a command that never
            // actually got a hotkey.
            let command = registry.forget(result.id);
            tracing::error!(
                hotkey_id = result.id,
                ?command,
                %err,
                "failed to register hotkey; that binding will not work, the rest still will"
            );
            unregistered.extend(command);
        }
    }
    // Sent on every pass, including the one that reports nothing: an
    // empty report is what clears a previous pass's failures once the
    // combination comes back.
    let _ = events.send(mosaix_engine::Event::HotkeyRegistrationReported { unregistered });
    let forwarder = std::thread::spawn(move || {
        for fired in hotkey_events {
            let Some(command) = registry.command_for(fired.id) else {
                tracing::warn!(
                    hotkey_id = fired.id,
                    "hotkey fired for an unknown id; ignoring"
                );
                continue;
            };
            // One snapshot for both reads, so the revision the overlay
            // flashes from and the pause state `toggle-pause` inverts
            // describe the same instant.
            let snapshot = state_reader.snapshot();
            let pre_revision = snapshot.revision;
            let event = hotkeys::event_for_command(&command, snapshot.paused);
            if events.send(event).is_err() {
                tracing::warn!("reducer stopped; hotkey forwarder exiting");
                break;
            }
            if hotkeys::is_zone_snap(&command) {
                if let Some(tx) = &overlay_tx {
                    let _ = tx.send(overlay::OverlayRequest::FlashAfterSnap {
                        revision: pre_revision,
                    });
                }
            }
        }
    });
    Ok((registrations, forwarder))
}

#[cfg(windows)]
fn retry_empty_topology(mut displays: Vec<mosaix_domain::Display>) -> Vec<mosaix_domain::Display> {
    const RETRIES: usize = 3;
    const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(250);
    for attempt in 1..=RETRIES {
        if !displays.is_empty() {
            break;
        }
        tracing::warn!(attempt, "empty topology observation; retrying enumeration");
        std::thread::sleep(RETRY_DELAY);
        displays = mosaix_platform_windows::enumerate_displays().unwrap_or_default();
    }
    displays
}

#[cfg(windows)]
fn main() {
    // Must happen before any window/monitor query.
    mosaix_platform_windows::enable_per_monitor_dpi_awareness()
        .expect("failed to set DPI awareness at startup");

    tracing_subscriber::fmt::init();
    tracing::info!("mosaix-agent starting");

    let initial_displays = match mosaix_platform_windows::enumerate_displays() {
        Ok(displays) => displays,
        Err(err) => {
            tracing::error!(%err, "failed to enumerate displays at startup; starting with an empty topology");
            Vec::new()
        }
    };

    // `%APPDATA%\Mosaix\config` -- resolved here rather than in
    // `mosaix-config`, since that crate is meant to stay platform-neutral
    // and this environment variable is Windows-specific.
    let config_dir = std::env::var_os("APPDATA").map(|appdata| {
        std::path::PathBuf::from(appdata)
            .join("Mosaix")
            .join("config")
    });

    // First-run default generation and the initial synchronous load both
    // happen before the agent is considered started (ticket 03); an
    // unreadable/uncreatable directory, or one that fails validation with
    // no last-known-good yet to fall back to, degrades to the in-memory
    // fallback constant rather than refusing to start (ADR 0007) -- the
    // same posture every other subsystem in this file already takes.
    // `Some(dir)` only once `dir` is confirmed to exist with a readable
    // `config.toml` -- the one thing `watch` below needs and never
    // establishes itself.
    let fallback_config_set = || mosaix_config::ResolvedConfigSet {
        base: mosaix_config::fallback_config(),
        profiles: Vec::new(),
    };

    let mut watchable_config_dir = None;
    let initial_config_set = match &config_dir {
        Some(dir) => match mosaix_config::ensure_default_config(dir) {
            Ok(()) => {
                watchable_config_dir = Some(dir.clone());
                match mosaix_config::load(dir) {
                    Ok(Ok(set)) => set,
                    Ok(Err(errors)) => {
                        for error in &errors {
                            tracing::error!(
                                %error,
                                "config directory failed validation at startup; falling back to built-in defaults"
                            );
                        }
                        fallback_config_set()
                    }
                    Err(err) => {
                        tracing::error!(
                            %err,
                            "failed to read config directory at startup; falling back to built-in defaults"
                        );
                        fallback_config_set()
                    }
                }
            }
            Err(err) => {
                tracing::error!(
                    %err,
                    "failed to create or initialize the config directory at startup; falling back to built-in defaults"
                );
                fallback_config_set()
            }
        },
        None => {
            tracing::error!(
                "could not determine a config directory (%APPDATA% not set); falling back to built-in defaults"
            );
            fallback_config_set()
        }
    };

    // A recoverable parking site is validated for the initial topology
    // before anything can ask to park (ADR 0023, ADR 0029), and again on
    // every topology change below. The site itself stays with the agent;
    // the engine only learns whether one is verified.
    let parking_site: recovery::SharedParkingSite = Default::default();
    let initial_parking = recovery::report_parking_capability(&initial_displays, &parking_site);

    let engine = mosaix_engine::spawn_engine(initial_displays, initial_config_set);
    let _ = engine
        .events()
        .send(mosaix_engine::Event::ParkingCapabilityReported(
            initial_parking,
        ));

    // Recovery first (ADR 0023). Before the state database is opened,
    // before any window is observed, and before any stored identity is
    // matched, every window a previous session parked and never put back
    // is restored -- if its handle still verifiably names that window.
    // A stale or reused handle is reported and left alone. Doing this
    // before the worker starts is what makes "recovery precedes
    // reconciliation" a fact of ordering rather than a hope.
    let session_id = engine.state_reader().snapshot().session_id;
    let ledger_path = mosaix_persistence::default_ledger_path();
    if let Some(path) = &ledger_path {
        match mosaix_persistence::RecoveryLedger::open(path) {
            Ok(mut ledger) => {
                let outcomes = recovery::recover_with_platform(&mut ledger);
                match &outcomes {
                    Ok(outcomes) => {
                        for outcome in outcomes {
                            tracing::info!(
                                entry = outcome.entry_id.0,
                                handle = outcome.native_handle,
                                verdict = outcome.verdict.code(),
                                restored = outcome.restored,
                                failure = outcome.failure.as_deref().unwrap_or(""),
                                "startup recovery"
                            );
                        }
                        let _ = engine
                            .events()
                            .send(mosaix_engine::Event::RecoveryReported(outcomes.clone()));
                    }
                    Err(error) => {
                        tracing::error!(%error, "startup recovery could not read the ledger");
                    }
                }
                if let Err(error) = ledger.prune(&session_id) {
                    tracing::warn!(%error, "recovery ledger could not be pruned");
                }
            }
            Err(error) => {
                tracing::error!(%error, "recovery ledger could not be opened; no window will be parked this session");
            }
        }
    }

    // The worker owns the per-user bundled-SQLite connection. A failure is
    // deliberately reflected into reducer state instead of aborting live
    // window management.
    let persistence_path = mosaix_persistence::default_database_path();
    let persistence_start_failure = std::sync::Arc::new(std::sync::Mutex::new(None));
    let persistence_worker = persistence_path.as_ref().and_then(|path| {
        mosaix_persistence::PersistenceWorker::start_with_ledger(path, ledger_path.as_deref())
            .map_err(|error| {
                tracing::error!(%error, "persistence worker could not start");
                *persistence_start_failure
                    .lock()
                    .expect("persistence failure mutex poisoned") = Some(error.failure());
                error
            })
            .ok()
    });
    if persistence_worker.is_none() {
        let _ = engine
            .events()
            .send(mosaix_engine::Event::PersistenceHealthChanged(
                mosaix_persistence::PersistenceHealth::Degraded {
                    last_durable_revision: 0,
                    reason: persistence_start_failure
                        .lock()
                        .expect("persistence failure mutex poisoned")
                        .take()
                        .unwrap_or(mosaix_persistence::PersistenceFailure::OpenFailed),
                },
            ));
    }
    let (persistence_stop_tx, persistence_bridge) = if let Some(worker) = persistence_worker {
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let state_reader = engine.state_reader();
        let events = engine.events();
        let bridge = std::thread::spawn(move || {
            let mut submitted_revision = None;
            // Intents accumulate in reducer state and are consumed by
            // index, the same way the placement executor consumes effects.
            let mut next_intent = 0usize;
            // Recording prunes as it writes, so this only has to catch the
            // agent that is left running without issuing any command.
            const PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);
            let mut last_prune = std::time::Instant::now();
            loop {
                if stop_rx.try_recv().is_ok() {
                    break;
                }
                let snapshot = state_reader.snapshot();

                // Undo writes go first. A revision commit that overtook the
                // transaction it describes would claim durability for a
                // command whose record had not been stored yet.
                let mut submission_failed = false;
                for intent in snapshot.persistence_intents.iter().skip(next_intent) {
                    let request = match intent {
                        mosaix_engine::PersistenceIntent::RecordUndoTransaction(draft) => {
                            mosaix_persistence::PersistenceRequest::RecordUndoTransaction(
                                draft.clone(),
                            )
                        }
                        mosaix_engine::PersistenceIntent::ConsumeUndoTransaction(id) => {
                            mosaix_persistence::PersistenceRequest::ConsumeUndoTransaction(*id)
                        }
                        mosaix_engine::PersistenceIntent::SaveContainerTree {
                            display_fingerprint,
                            tree,
                        } => mosaix_persistence::PersistenceRequest::SaveContainerTree {
                            display_fingerprint: display_fingerprint.clone(),
                            tree: tree.clone(),
                        },
                        mosaix_engine::PersistenceIntent::SaveWorkspace(workspace) => {
                            mosaix_persistence::PersistenceRequest::SaveWorkspace(workspace.clone())
                        }
                        mosaix_engine::PersistenceIntent::DeleteWorkspace(name) => {
                            mosaix_persistence::PersistenceRequest::DeleteWorkspace(name.clone())
                        }
                        mosaix_engine::PersistenceIntent::RecordRecovery { token, draft } => {
                            // The reducer has no platform access, so the
                            // process instance and the true show state are
                            // read here, from the live window, before the
                            // entry is written.
                            let mut draft = (**draft).clone();
                            recovery::enrich_draft(&mut draft);
                            mosaix_persistence::PersistenceRequest::RecordRecovery {
                                token: *token,
                                draft: Box::new(draft),
                            }
                        }
                        mosaix_engine::PersistenceIntent::MarkParked(id) => {
                            mosaix_persistence::PersistenceRequest::MarkParked(*id)
                        }
                        mosaix_engine::PersistenceIntent::MarkRestored(id) => {
                            mosaix_persistence::PersistenceRequest::MarkRestored(*id)
                        }
                    };
                    if worker.submit(request).is_err() {
                        submission_failed = true;
                        break;
                    }
                }
                if !submission_failed {
                    next_intent = snapshot.persistence_intents.len();
                }

                if !submission_failed && last_prune.elapsed() >= PRUNE_INTERVAL {
                    if worker
                        .submit(mosaix_persistence::PersistenceRequest::PruneHistory)
                        .is_err()
                    {
                        submission_failed = true;
                    } else {
                        last_prune = std::time::Instant::now();
                    }
                }

                let revision = snapshot.revision;
                if !submission_failed && submitted_revision != Some(revision) {
                    if worker.commit(revision).is_err() {
                        submission_failed = true;
                    } else {
                        submitted_revision = Some(revision);
                    }
                }

                if submission_failed {
                    let _ = events.send(mosaix_engine::Event::PersistenceHealthChanged(
                        mosaix_persistence::PersistenceHealth::Degraded {
                            last_durable_revision: submitted_revision.unwrap_or(0),
                            reason: mosaix_persistence::PersistenceFailure::WriteFailed,
                        },
                    ));
                    break;
                }

                // Draining without blocking keeps this loop responsive to
                // the stop signal even while the worker is busy.
                while let Some(update) = worker.try_next_update() {
                    let _ = events.send(mosaix_engine::Event::PersistenceHealthChanged(
                        update.health,
                    ));
                    let _ = events.send(mosaix_engine::Event::UndoHistoryLoaded(
                        update.newest_undo.map(Box::new),
                    ));
                    // Only the worker's first update carries these, so the
                    // reducer's arrangements are never overwritten by the
                    // database once the reducer owns them.
                    if let Some(trees) = update.restored_trees {
                        let _ = events.send(mosaix_engine::Event::ContainerTreesLoaded(trees));
                    }
                    if let Some(workspaces) = update.restored_workspaces {
                        let _ = events.send(mosaix_engine::Event::WorkspacesLoaded(workspaces));
                    }
                    for (token, entry) in update.recovery_acknowledged {
                        let _ = events.send(match entry {
                            Some(entry_id) => {
                                mosaix_engine::Event::RecoveryEntryDurable { token, entry_id }
                            }
                            None => mosaix_engine::Event::RecoveryEntryRefused { token },
                        });
                    }
                }

                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            worker.stop();
        });
        (Some(stop_tx), Some(bridge))
    } else {
        (None, None)
    };

    // Feature 28 — startup reconciliation.
    //
    // Before the OS-event hooks are active, enumerate every existing window
    // and register it with the engine so that already-open apps are tracked
    // from the start.  Any windows that a very-early-arriving
    // `Event::WindowFocused` already registered are silently skipped by the
    // engine's `or_insert` logic.
    {
        let windows = match mosaix_platform_windows::enumerate_windows() {
            Ok(w) => w,
            Err(err) => {
                tracing::error!(%err, "failed to enumerate windows for startup reconciliation; skipping");
                Vec::new()
            }
        };
        tracing::info!(
            window_count = windows.len(),
            "sending normalized startup window observations"
        );
        if engine
            .events()
            .send(mosaix_engine::Event::WindowsObserved { windows })
            .is_err()
        {
            tracing::error!("reducer stopped before startup reconciliation could be sent");
        }

        // The engine otherwise only learns about focus from a
        // foreground-*change* notification, so a freshly started agent has
        // no focus anchor until the user next switches windows -- leaving
        // directional focus/swap silent no-ops and the Focus border hidden
        // while automatic tiling is already active.  Seed it from whatever
        // owns the foreground right now, as an ordinary observation.
        match mosaix_platform_windows::foreground_window_handle() {
            Some(handle) => {
                let window_id = mosaix_platform_windows::window_id_from_handle(handle);
                match mosaix_platform_windows::observed_window_state(handle) {
                    Some((display_id, bounds)) => {
                        tracing::info!(?window_id, "seeding the initial focused window");
                        if engine
                            .events()
                            .send(mosaix_engine::Event::WindowFocused {
                                window_id,
                                display_id,
                                bounds,
                            })
                            .is_err()
                        {
                            tracing::error!(
                                "reducer stopped before the initial focus could be sent"
                            );
                        }
                    }
                    None => tracing::debug!(
                        ?window_id,
                        "could not read bounds/display for the foreground window; not seeding focus"
                    ),
                }
            }
            None => tracing::debug!("no window owns the foreground at startup; not seeding focus"),
        }
    }

    // Configuration writes requested over IPC go through the same
    // directory the watcher is reading. An agent that never found a
    // directory refuses them with that reason rather than reporting a
    // save it did not make.
    let config_store: std::sync::Arc<dyn mosaix_ipc::ConfigStore> = match &watchable_config_dir {
        Some(dir) => std::sync::Arc::new(DirectoryConfigStore { dir: dir.clone() }),
        None => std::sync::Arc::new(mosaix_ipc::UnavailableConfigStore {
            reason: "Mosaix could not open its configuration directory, so it cannot save changes"
                .to_owned(),
        }),
    };

    let ipc_server = match mosaix_ipc::IpcServer::start(
        engine.events(),
        engine.state_reader(),
        config_store,
        std::sync::Arc::new(PlatformHotkeyProbe),
    ) {
        Ok(server) => Some(server),
        Err(err) => {
            tracing::error!(%err, "failed to start IPC server; the mosaix CLI will be unavailable");
            None
        }
    };

    // Watches `config_dir` for hot-edits and forwards each successfully
    // validated reload into the reducer as `Event::ConfigChanged`,
    // structurally identical to the display-topology forwarder below. A
    // rejected reload is already logged inside `mosaix_config::watch`'s own
    // debounce loop, so there's nothing left for this forwarder to do with
    // it -- `EngineState`'s resolved config simply stays at its
    // last-known-good value.
    let config_watcher_and_forwarder = match watchable_config_dir.map(mosaix_config::watch) {
        Some(Ok((watcher, config_events))) => {
            let events = engine.events();
            let forwarder = std::thread::spawn(move || {
                for event in config_events {
                    if let mosaix_config::ConfigEvent::Changed(set) = event {
                        if events
                            .send(mosaix_engine::Event::ConfigChanged(Box::new(set)))
                            .is_err()
                        {
                            tracing::warn!("reducer stopped; config forwarder exiting");
                            break;
                        }
                    }
                }
            });
            Some((watcher, forwarder))
        }
        Some(Err(err)) => {
            tracing::error!(%err, "failed to start config directory watcher; the agent will not observe config file edits");
            None
        }
        None => None,
    };

    // Display topology watcher — forwards `WM_DISPLAYCHANGE` events (hotplug,
    // resolution change) and `WM_POWERBROADCAST` wake events from the
    // platform layer into the engine.
    let watcher_and_forwarder = match mosaix_platform_windows::watch_display_topology() {
        Ok((watcher, topology_events)) => {
            let events = engine.events();
            let parking_site = parking_site.clone();
            let forwarder = std::thread::spawn(move || {
                for event in topology_events {
                    match event {
                        mosaix_platform_windows::TopologyEvent::Changed(displays) => {
                            let displays = retry_empty_topology(displays);
                            // The site is only as good as its topology: a
                            // display connected beyond the parking edge would
                            // make parked windows visible, so it is re-validated
                            // before the engine hears about the change.
                            let capability =
                                recovery::report_parking_capability(&displays, &parking_site);
                            if events
                                .send(mosaix_engine::Event::DisplayTopologyChanged(displays))
                                .is_err()
                            {
                                tracing::warn!(
                                    "reducer stopped; display topology forwarder exiting"
                                );
                                break;
                            }
                            let _ = events
                                .send(mosaix_engine::Event::ParkingCapabilityReported(capability));
                        }
                        // Feature 29 — sleep/wake recovery.
                        //
                        // After wake the platform layer re-enumerates displays
                        // and sends `WakeFromSleep`.  We also re-enumerate
                        // windows here because the platform layer's hidden
                        // window only sees display events, not window events.
                        mosaix_platform_windows::TopologyEvent::WakeFromSleep(displays) => {
                            let displays = retry_empty_topology(displays);
                            let windows = match mosaix_platform_windows::enumerate_windows() {
                                Ok(w) => Some(w),
                                Err(err) => {
                                    tracing::error!(%err, "failed to enumerate windows after wake; retaining the last window inventory");
                                    None
                                }
                            };
                            tracing::info!(
                                display_count = displays.len(),
                                window_count = windows.as_ref().map_or(0, Vec::len),
                                "forwarding wake reconciliation to engine"
                            );
                            let capability =
                                recovery::report_parking_capability(&displays, &parking_site);
                            if events
                                .send(mosaix_engine::Event::WakeReconciliation {
                                    displays,
                                    windows,
                                })
                                .is_err()
                            {
                                tracing::warn!(
                                    "reducer stopped; wake reconciliation forwarder exiting"
                                );
                                break;
                            }
                            let _ = events
                                .send(mosaix_engine::Event::ParkingCapabilityReported(capability));
                        }
                    }
                }
            });
            Some((watcher, forwarder))
        }
        Err(err) => {
            tracing::error!(
                %err,
                "failed to start display topology watcher; the agent will not observe display changes"
            );
            None
        }
    };

    // Feature 34 — snap preview overlay. Started before the hotkey and
    // event-hook forwarders so both can feed it. Failure degrades to no
    // overlay rather than blocking the rest of the agent.
    let (overlay_tx, overlay_controller) = match mosaix_platform_windows::start_preview_overlay() {
        Ok(preview) => {
            let (tx, join_handle) =
                overlay::start_overlay_controller(engine.state_reader(), engine.events(), preview);
            (Some(tx), Some(join_handle))
        }
        Err(err) => {
            tracing::error!(
                %err,
                "failed to start snap preview overlay; drag/hotkey previews will be unavailable"
            );
            (None, None)
        }
    };

    let focus_border_and_controller = match mosaix_platform_windows::start_focus_border() {
        Ok(border) => {
            let state_reader = engine.state_reader();
            let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
            let controller = std::thread::spawn(move || {
                const POLL: std::time::Duration = std::time::Duration::from_millis(50);
                let mut last_visible: Option<(
                    mosaix_domain::Rect,
                    mosaix_platform_windows::FocusBorderStyle,
                )> = None;
                loop {
                    match stop_rx.recv_timeout(POLL) {
                        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    let state = state_reader.snapshot();
                    let desired = if state.automatic_tiling_active
                        && !state.paused
                        && state.resolved_config.focus_border.enabled
                    {
                        state.focused_window.and_then(|window_id| {
                            let managed = state.inventory.get(&window_id)?;
                            // The inventory keeps minimized, hidden, and
                            // cloaked windows -- they are temporarily
                            // ineligible, not unmanaged -- so membership
                            // alone would leave a border on bare desktop
                            // after a minimize. Maximized and full-screen
                            // are ineligible for a cell but plainly visible.
                            if !matches!(
                                managed.window.lifecycle,
                                mosaix_domain::WindowLifecycle::Active
                                    | mosaix_domain::WindowLifecycle::Maximized
                                    | mosaix_domain::WindowLifecycle::Fullscreen
                            ) {
                                return None;
                            }
                            // The engine defers placement for a window being
                            // dragged (ADR 0016), so anything drawn now would
                            // trail it under the cursor until the drop.
                            if state
                                .interactive_placement
                                .is_some_and(|session| session.window_id == window_id)
                            {
                                return None;
                            }
                            // `observed_bounds`, not `bounds`: the latter is
                            // the placement Mosaix last *intended*, kept stale
                            // on purpose so the engine can detect an external
                            // move by comparing the two (ADR 0001). Drawing
                            // from it leaves the border behind any window
                            // something else repositioned.
                            let bounds = state
                                .windows
                                .get(&window_id)
                                .map(|placement| placement.observed_bounds)
                                .unwrap_or(managed.window.bounds);
                            let config = state.resolved_config.focus_border;
                            let scale = state
                                .displays
                                .iter()
                                .find(|display| display.id == managed.window.display_id)
                                .map_or(1.0, |display| display.scale_factor);
                            Some((
                                bounds,
                                mosaix_platform_windows::FocusBorderStyle {
                                    red: config.color.red,
                                    green: config.color.green,
                                    blue: config.color.blue,
                                    alpha: config.color.alpha,
                                    thickness: (f64::from(config.thickness) * scale)
                                        .round()
                                        .clamp(1.0, f64::from(u16::MAX))
                                        as u16,
                                },
                            ))
                        })
                    } else {
                        None
                    };
                    if desired == last_visible {
                        continue;
                    }
                    if let Some((bounds, style)) = desired {
                        border.show(bounds, style);
                    } else {
                        border.hide();
                    }
                    last_visible = desired;
                }
                border.stop();
            });
            Some((stop_tx, controller))
        }
        Err(err) => {
            tracing::error!(%err, "failed to start focus border; automatic tiling will continue without focus decoration");
            None
        }
    };

    let event_hooks_and_forwarder = match mosaix_platform_windows::start_event_hooks() {
        Ok((hooks, raw_events)) => {
            let events = engine.events();
            let overlay_tx = overlay_tx.clone();
            let (inventory_refresh_tx, inventory_refresh_rx) = std::sync::mpsc::sync_channel(1);
            let inventory_events = events.clone();
            let inventory_refresher = std::thread::spawn(move || {
                const SETTLE: std::time::Duration = std::time::Duration::from_millis(50);
                loop {
                    if inventory_refresh_rx.recv().is_err() {
                        break;
                    }
                    while inventory_refresh_rx.recv_timeout(SETTLE).is_ok() {}
                    let windows = match mosaix_platform_windows::enumerate_windows() {
                        Ok(windows) => windows,
                        Err(err) => {
                            tracing::debug!(%err, "could not refresh normalized window inventory");
                            continue;
                        }
                    };
                    if inventory_events
                        .send(mosaix_engine::Event::WindowsObserved { windows })
                        .is_err()
                    {
                        break;
                    }
                }
            });
            let forwarder = std::thread::spawn(move || {
                // Focus and location changes feed the engine; move/resize
                // start/end feed the snap-preview drag controller (Feature 34).
                for event in raw_events {
                    match event {
                        mosaix_platform_windows::RawEvent::WindowCreated(_)
                        | mosaix_platform_windows::RawEvent::WindowDestroyed(_) => {
                            // Coalesce noisy lifecycle bursts into one complete
                            // authoritative observation and therefore one final
                            // grid plan.
                            let _ = inventory_refresh_tx.try_send(());
                        }
                        mosaix_platform_windows::RawEvent::Focused(handle) => {
                            let window_id = mosaix_platform_windows::window_id_from_handle(handle);
                            let Some((display_id, bounds)) =
                                mosaix_platform_windows::observed_window_state(handle)
                            else {
                                tracing::debug!(
                                    ?window_id,
                                    "could not read bounds/display for a newly-focused window; skipping"
                                );
                                continue;
                            };
                            if events
                                .send(mosaix_engine::Event::WindowFocused {
                                    window_id,
                                    display_id,
                                    bounds,
                                })
                                .is_err()
                            {
                                tracing::warn!("reducer stopped; focus forwarder exiting");
                                break;
                            }
                        }
                        mosaix_platform_windows::RawEvent::LocationChanged(handle) => {
                            let window_id = mosaix_platform_windows::window_id_from_handle(handle);
                            let Some((display_id, bounds)) =
                                mosaix_platform_windows::observed_window_state(handle)
                            else {
                                tracing::debug!(
                                    ?window_id,
                                    "could not read bounds/display for a location-changed window; skipping"
                                );
                                continue;
                            };
                            if events
                                .send(mosaix_engine::Event::WindowBoundsObserved {
                                    window_id,
                                    display_id,
                                    bounds,
                                })
                                .is_err()
                            {
                                tracing::warn!(
                                    "reducer stopped; bounds-observed forwarder exiting"
                                );
                                break;
                            }
                            let _ = inventory_refresh_tx.try_send(());
                        }
                        mosaix_platform_windows::RawEvent::MoveResizeStart(handle) => {
                            let window_id = mosaix_platform_windows::window_id_from_handle(handle);
                            if let Some(tx) = &overlay_tx {
                                let _ = tx.send(overlay::OverlayRequest::DragStarted { window_id });
                            } else {
                                let _ = events.send(
                                    mosaix_engine::Event::InteractivePlacementStarted { window_id },
                                );
                            }
                        }
                        mosaix_platform_windows::RawEvent::MoveResizeEnd(handle) => {
                            let window_id = mosaix_platform_windows::window_id_from_handle(handle);
                            if let Some(tx) = &overlay_tx {
                                let _ = tx.send(overlay::OverlayRequest::DragEnded { window_id });
                            } else {
                                let _ =
                                    events.send(mosaix_engine::Event::InteractivePlacementEnded {
                                        window_id,
                                        committed_manual_placement: false,
                                    });
                            }
                        }
                    }
                }
            });
            Some((hooks, forwarder, inventory_refresher))
        }
        Err(err) => {
            tracing::error!(%err, "failed to start OS event hooks; the agent will not observe focus changes");
            None
        }
    };

    // Initial registration reads whatever the startup path above produced
    // (generated default, loaded file, or the in-memory fallback, ADR
    // 0007) -- there is no separate hardcoded table anymore (ADR 0003,
    // superseded by ADR 0005). The hotkey-rebind poller further down keeps
    // this in sync with `EngineState`'s resolved config for the rest of the
    // agent's lifetime.
    let last_registered_hotkeys =
        hotkeys::runtime_hotkeys(&engine.state_reader().snapshot().resolved_config);
    let (initial_bindings, initial_registry) =
        hotkeys::bindings_from_resolved(&last_registered_hotkeys);
    let initial_hotkey_registration = match start_hotkeys_and_forward(
        initial_bindings,
        initial_registry,
        engine.events(),
        engine.state_reader(),
        overlay_tx.clone(),
    ) {
        Ok(pair) => Some(pair),
        Err(err) => {
            tracing::error!(%err, "failed to start hotkey registration thread; snap hotkeys will not work");
            None
        }
    };

    // Diffs EngineState's resolved hotkey bindings against whatever was
    // last registered with the OS, and re-registers (stopping the old
    // registration first) whenever `mosaix_config::diff_bindings` reports a
    // change -- structurally identical to the placement-executor poller
    // just below, but over hotkey bindings instead of window placements
    // (ADR 0005). A hot-edited `config.toml` (ticket 03) and a
    // topology-triggered profile switch (ticket 04) both flow through the
    // same `EngineState::resolved_config`, so this one poller covers both.
    //
    // It is also the one place that reads
    // `EngineState::hotkey_capture_suspended`: while the settings
    // application's hotkey editor is open, this registers nothing, so a
    // combination the user is about to press reaches the editor instead
    // of firing a command. Keeping that here rather than in a new engine
    // effect is what keeps hotkey ownership in one place (ADR 0021).
    const HOTKEY_REBIND_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);
    let (hotkey_rebind_stop_tx, hotkey_rebind_stop_rx) = std::sync::mpsc::channel::<()>();
    let hotkey_rebind_forwarder = {
        let state_reader = engine.state_reader();
        let events = engine.events();
        let overlay_tx = overlay_tx.clone();
        std::thread::spawn(move || {
            let mut previous_hotkeys = last_registered_hotkeys;
            let mut current_registration = initial_hotkey_registration;
            // What the current registration was made under, so lifting or
            // entering suspension is itself a reason to act -- the
            // bindings need not have changed for the answer to.
            let mut previously_suspended = false;
            loop {
                match hotkey_rebind_stop_rx.recv_timeout(HOTKEY_REBIND_POLL_INTERVAL) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }

                // One snapshot for both reads, so a capture that starts
                // between them cannot leave this pass registering
                // bindings the editor is about to need.
                let state = state_reader.snapshot();
                let suspended = state.hotkey_capture_suspended;
                let current_hotkeys = hotkeys::runtime_hotkeys(&state.resolved_config);
                let bindings_changed =
                    !mosaix_config::diff_bindings(&previous_hotkeys, &current_hotkeys).is_empty();
                if !bindings_changed && suspended == previously_suspended {
                    continue;
                }

                if let Some((registrations, forwarder)) = current_registration.take() {
                    registrations.stop();
                    let _ = forwarder.join();
                }
                current_registration = if suspended {
                    // A config reload arriving mid-capture updates what
                    // will be registered when the editor closes, and
                    // registers nothing now.
                    tracing::info!("hotkey capture is active; leaving every binding unregistered");
                    None
                } else {
                    tracing::info!("registering hotkeys for the current resolved bindings");
                    // A fresh registry every time: an id the previous
                    // registration owned cannot survive into this one.
                    let (bindings, registry) = hotkeys::bindings_from_resolved(&current_hotkeys);
                    match start_hotkeys_and_forward(
                        bindings,
                        registry,
                        events.clone(),
                        state_reader.clone(),
                        overlay_tx.clone(),
                    ) {
                        Ok(pair) => Some(pair),
                        Err(err) => {
                            tracing::error!(%err, "failed to re-register hotkeys after a binding change; hotkeys are unregistered until the next change");
                            None
                        }
                    }
                };
                previous_hotkeys = current_hotkeys;
                previously_suspended = suspended;
            }

            if let Some((registrations, forwarder)) = current_registration {
                registrations.stop();
                let _ = forwarder.join();
            }
        })
    };

    // Applies the engine's ordered, platform-neutral effect stream via
    // `SetWindowPos`. The reducer never touches the OS; this is the sole
    // Windows execution boundary and reports rejections back as normalized
    // engine events.
    //
    // Feature 31 — after each `SetWindowPos` call, the executor waits
    // briefly and re-reads the window's actual bounds.  If they differ
    // significantly from the target, a `PlacementRejected` event is sent
    // back to the engine, which increments the per-window circuit breaker.
    const PLACEMENT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
    /// How long to wait after a `SetWindowPos` before re-reading the
    /// window's actual bounds to detect rejection (Feature 31).
    const REJECTION_SETTLE_MILLIS: u64 = 500;
    /// Absolute pixel tolerance for rejection detection — if the observed
    /// bounds differ from the target by more than this in *any* axis, the
    /// placement is considered rejected.
    const REJECTION_TOLERANCE_PX: i32 = 2;
    let (executor_stop_tx, executor_stop_rx) = std::sync::mpsc::channel::<()>();
    let executor_forwarder = {
        let state_reader = engine.state_reader();
        let rejection_events = engine.events();
        let parking_site = parking_site.clone();
        let executor_ledger_path = ledger_path.clone();
        std::thread::spawn(move || {
            let mut next_effect = 0usize;
            loop {
                let snapshot = state_reader.snapshot();
                if snapshot.paused {
                    match executor_stop_rx.recv_timeout(PLACEMENT_POLL_INTERVAL) {
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                for effect in snapshot.effects.iter().skip(next_effect) {
                    let mosaix_engine::EngineEffect::PlaceWindow {
                        window_id, bounds, ..
                    } = *effect
                    else {
                        match *effect {
                            mosaix_engine::EngineEffect::FocusWindow { window_id } => {
                                if let Err(err) =
                                    mosaix_platform_windows::focus_window_by_id(window_id)
                                {
                                    tracing::warn!(?window_id, %err, "failed to focus directional neighbor");
                                }
                            }
                            mosaix_engine::EngineEffect::ReconcileWindows => {
                                match mosaix_platform_windows::enumerate_windows() {
                                    Ok(windows) => {
                                        let _ = rejection_events.send(
                                            mosaix_engine::Event::RearrangeReconciliationComplete {
                                                windows,
                                            },
                                        );
                                    }
                                    Err(err) => {
                                        tracing::warn!(%err, "rearrange could not enumerate windows");
                                    }
                                }
                            }
                            mosaix_engine::EngineEffect::ParkWindow {
                                window_id,
                                entry_id,
                            } => {
                                // Recovery data for this window is durable, so
                                // parking is authorised (ADR 0023). The window is
                                // moved beyond the validated edge without
                                // activation; the reducer marks the entry parked
                                // only when the move verifiably landed.
                                let _ = rejection_events.send(
                                    match recovery::park(window_id, &parking_site) {
                                        Ok(parked_as) => {
                                            tracing::info!(
                                                ?window_id,
                                                entry = entry_id.0,
                                                ?parked_as,
                                                "window parked"
                                            );
                                            mosaix_engine::Event::WindowParked {
                                                window_id,
                                                entry_id,
                                            }
                                        }
                                        Err(reason) => mosaix_engine::Event::WindowParkFailed {
                                            window_id,
                                            entry_id,
                                            reason,
                                        },
                                    },
                                );
                            }
                            mosaix_engine::EngineEffect::RestoreWindow {
                                window_id,
                                entry_id,
                            } => {
                                let _ = rejection_events.send(
                                    match recovery::restore_parked_window(
                                        executor_ledger_path.as_deref(),
                                        window_id,
                                        entry_id,
                                    ) {
                                        Ok(()) => {
                                            tracing::info!(
                                                ?window_id,
                                                entry = entry_id.0,
                                                "parked window restored"
                                            );
                                            mosaix_engine::Event::WindowRestored { window_id }
                                        }
                                        Err(reason) => mosaix_engine::Event::WindowRestoreFailed {
                                            window_id,
                                            entry_id,
                                            reason,
                                        },
                                    },
                                );
                            }
                            mosaix_engine::EngineEffect::PlaceWindow { .. } => unreachable!(),
                        }
                        continue;
                    };
                    if let Err(err) =
                        mosaix_platform_windows::move_resize_window_by_id(window_id, bounds)
                    {
                        tracing::warn!(?window_id, %err, "failed to apply computed placement to the real window");

                        // Any failed platform call is a placement rejection;
                        // elevation is only useful extra diagnostics.
                        let handle = mosaix_platform_windows::window_handle_from_id(window_id);
                        if mosaix_platform_windows::is_window_elevated(handle) {
                            tracing::warn!(
                                ?window_id,
                                "window is elevated (Administrator); emitting PlacementRejected"
                            );
                        }
                        let _ = rejection_events
                            .send(mosaix_engine::Event::PlacementRejected { window_id });
                        continue;
                    }

                    // Feature 31 — rejection detection.
                    //
                    // Wait briefly for the window to settle, then re-read its
                    // actual bounds.  If they deviate too far from the
                    // intended placement, the window is rejecting our resize
                    // (e.g. min-size constraint), so we notify the engine.
                    std::thread::sleep(std::time::Duration::from_millis(REJECTION_SETTLE_MILLIS));
                    let handle = mosaix_platform_windows::window_handle_from_id(window_id);
                    if let Some((_actual_display, actual_bounds)) =
                        mosaix_platform_windows::observed_window_state(handle)
                    {
                        let dx = (actual_bounds.x - bounds.x).abs();
                        let dy = (actual_bounds.y - bounds.y).abs();
                        let dw = (actual_bounds.width - bounds.width).abs();
                        let dh = (actual_bounds.height - bounds.height).abs();
                        if dx > REJECTION_TOLERANCE_PX
                            || dy > REJECTION_TOLERANCE_PX
                            || dw > REJECTION_TOLERANCE_PX
                            || dh > REJECTION_TOLERANCE_PX
                        {
                            tracing::debug!(
                                ?window_id,
                                ?bounds,
                                ?actual_bounds,
                                "placement rejected: actual bounds differ from target"
                            );
                            let _ = rejection_events
                                .send(mosaix_engine::Event::PlacementRejected { window_id });
                        } else {
                            let _ = rejection_events
                                .send(mosaix_engine::Event::PlacementAccepted { window_id });
                        }
                    } else {
                        let _ = rejection_events
                            .send(mosaix_engine::Event::PlacementRejected { window_id });
                    }
                }
                next_effect = snapshot.effects.len();

                match executor_stop_rx.recv_timeout(PLACEMENT_POLL_INTERVAL) {
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })
    };

    // Feature 33 — system tray icon. Pause/Resume mirrors the IPC handler;
    // Settings opens the config folder; Quit joins the merged shutdown path.
    // The `TrayHandle` stays on the main path so console quit can stop it
    // and unblock the forwarder (which only sees tray-channel disconnect).
    let (quit_tx, quit_rx) = std::sync::mpsc::channel::<&'static str>();
    let tray_and_forwarder = match mosaix_platform_windows::start_tray() {
        Ok((tray, tray_events)) => {
            let events = engine.events();
            let state_reader = engine.state_reader();
            let quit_tx = quit_tx.clone();
            let config_dir_for_tray = config_dir.clone();
            let tray_for_status = engine.state_reader();
            let (tray_stop_tx, tray_stop_rx) = std::sync::mpsc::channel::<()>();
            let forwarder = std::thread::spawn(move || {
                const TRAY_STATUS_POLL: std::time::Duration = std::time::Duration::from_millis(250);
                let status_for = |state: &mosaix_engine::EngineState| {
                    if state.paused {
                        mosaix_platform_windows::TrayStatus::Paused
                    } else if state.automatic_tiling_suspended {
                        mosaix_platform_windows::TrayStatus::Suspended
                    } else if state.automatic_tiling_active && state.circuit_breaker_count() > 0 {
                        mosaix_platform_windows::TrayStatus::Degraded
                    } else if state.automatic_tiling_active {
                        mosaix_platform_windows::TrayStatus::Active
                    } else {
                        mosaix_platform_windows::TrayStatus::Manual
                    }
                };
                let mut last_status = status_for(&state_reader.snapshot());
                tray.set_status(last_status);
                loop {
                    // Prefer an explicit stop from main (console quit path).
                    match tray_stop_rx.try_recv() {
                        Ok(()) | Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                        Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    }
                    match tray_events.recv_timeout(TRAY_STATUS_POLL) {
                        Ok(mosaix_platform_windows::TrayEvent::TogglePause) => {
                            let paused = state_reader.snapshot().paused;
                            let event = if paused {
                                mosaix_engine::Event::ResumeRequested
                            } else {
                                mosaix_engine::Event::PauseRequested
                            };
                            if events.send(event).is_err() {
                                tracing::warn!("reducer stopped; tray forwarder exiting");
                                break;
                            }
                        }
                        Ok(mosaix_platform_windows::TrayEvent::OpenConfig) => {
                            match &config_dir_for_tray {
                                Some(dir) => {
                                    if let Err(err) =
                                        std::process::Command::new("explorer").arg(dir).spawn()
                                    {
                                        tracing::error!(
                                            %err,
                                            path = %dir.display(),
                                            "failed to open config folder from tray"
                                        );
                                    }
                                }
                                None => {
                                    tracing::warn!(
                                        "tray Open config folder: no config directory available"
                                    );
                                }
                            }
                        }
                        Ok(mosaix_platform_windows::TrayEvent::Quit) => {
                            let _ = quit_tx.send("tray");
                            break;
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            let status = status_for(&tray_for_status.snapshot());
                            if status != last_status {
                                tray.set_status(status);
                                last_status = status;
                            }
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                // Dropping `tray` removes the icon and joins its thread.
            });
            Some((tray_stop_tx, forwarder))
        }
        Err(err) => {
            tracing::error!(
                %err,
                "failed to start system tray icon; pause/quit via tray will be unavailable"
            );
            None
        }
    };

    let shutdown = mosaix_platform_windows::register_shutdown_signal()
        .expect("failed to register shutdown signal handler at startup");
    // Console shutdown and tray Quit both feed one channel so main has a
    // single place to wait (Feature 33).
    {
        let quit_tx = quit_tx.clone();
        std::thread::spawn(move || {
            let _ = shutdown.recv();
            let _ = quit_tx.send("console");
        });
    }
    // Drop our clone so the channel closes once every forwarder exits.
    drop(quit_tx);

    tracing::info!(
        "mosaix-agent ready; waiting for a shutdown signal (Ctrl+C, console close, logoff, system shutdown, or tray Quit)"
    );
    let source = quit_rx.recv().unwrap_or("unknown");
    tracing::info!(source, "shutdown signal received; stopping");

    if let Some((tray_stop_tx, forwarder)) = tray_and_forwarder {
        let _ = tray_stop_tx.send(());
        let _ = forwarder.join();
    }
    if let Some((stop_tx, controller)) = focus_border_and_controller {
        let _ = stop_tx.send(());
        let _ = controller.join();
    }
    if let Some((watcher, forwarder)) = watcher_and_forwarder {
        watcher.stop();
        let _ = forwarder.join();
    }
    if let Some((watcher, forwarder)) = config_watcher_and_forwarder {
        watcher.stop();
        let _ = forwarder.join();
    }
    if let Some((hooks, forwarder, inventory_refresher)) = event_hooks_and_forwarder {
        hooks.stop();
        let _ = forwarder.join();
        let _ = inventory_refresher.join();
    }
    let _ = hotkey_rebind_stop_tx.send(());
    let _ = hotkey_rebind_forwarder.join();
    if let Some(stop_tx) = persistence_stop_tx {
        let _ = stop_tx.send(());
    }
    if let Some(bridge) = persistence_bridge {
        let _ = bridge.join();
    }

    // The overlay controller stops only when every `OverlayRequest` sender is
    // gone, so this must come after the three forwarders that hold clones --
    // the event-hook forwarder, the hotkey forwarder, and the rebind poller
    // (which owns the current hotkey registration and so the hotkey forwarder
    // with it). Dropping main's sender any earlier leaves those clones alive,
    // the controller blocked in `recv`, and this join hanging forever, which
    // is a shutdown that never completes rather than a graceful one.
    drop(overlay_tx);
    if let Some(join_handle) = overlay_controller {
        let _ = join_handle.join();
    }

    let _ = executor_stop_tx.send(());
    let _ = executor_forwarder.join();
    if let Some(server) = ipc_server {
        server.stop();
    }
    engine.stop();

    // A clean exit puts back everything this session parked, through the
    // same verified path startup recovery uses, so a stop-and-uninstall
    // never leaves a window off screen (ADR 0023). The worker has stopped
    // by now, so the ledger is reopened here without contention.
    if let Some(path) = &ledger_path {
        match mosaix_persistence::RecoveryLedger::open(path) {
            Ok(mut ledger) => match recovery::recover_with_platform(&mut ledger) {
                Ok(outcomes) => {
                    let restored = outcomes.iter().filter(|outcome| outcome.restored).count();
                    if !outcomes.is_empty() {
                        tracing::info!(
                            restored,
                            total = outcomes.len(),
                            "clean exit restored parked windows"
                        );
                    }
                }
                Err(error) => tracing::error!(%error, "clean exit could not read the ledger"),
            },
            Err(error) => tracing::error!(%error, "clean exit could not open the ledger"),
        }
    }
    tracing::info!("mosaix-agent stopped");
}

#[cfg(not(windows))]
fn main() {
    eprintln!("mosaix-agent currently only supports Windows (no macOS platform adapter yet).");
    std::process::exit(1);
}

/// The agent's configuration directory, as the IPC handler sees it.
///
/// The whole implementation is `mosaix_config`'s two edit functions; what
/// this adds is the directory the agent resolved at startup and the
/// translation of a config error into the sentence the person who asked
/// for the change reads.
#[derive(Debug)]
struct DirectoryConfigStore {
    dir: std::path::PathBuf,
}

impl mosaix_ipc::ConfigStore for DirectoryConfigStore {
    fn edit_layouts(
        &self,
        fingerprint: &str,
        edit: mosaix_config::LayoutEdit,
    ) -> Result<mosaix_config::LayoutWrite, mosaix_ipc::ConfigError> {
        mosaix_config::edit_layouts(&self.dir, fingerprint, edit)
            .map_err(|error| mosaix_ipc::ConfigError(error.to_string()))
    }

    fn edit_bindings(
        &self,
        fingerprint: &str,
        edit: mosaix_config::BindingEdit,
    ) -> Result<mosaix_config::BindingWrite, mosaix_ipc::ConfigError> {
        mosaix_config::edit_bindings(&self.dir, fingerprint, edit)
            .map_err(|error| mosaix_ipc::ConfigError(error.to_string()))
    }
}

/// The real `RegisterHotKey` probe, behind the handler's platform-neutral
/// trait.
///
/// Translating a `KeyCombo` into modifier flags and a virtual-key code is
/// the same translation registration already goes through, so a
/// combination that probes as available is one that can actually be
/// registered -- and a key name with no virtual-key code behind it is
/// reported as unsupported rather than as taken.
#[derive(Debug)]
struct PlatformHotkeyProbe;

impl mosaix_ipc::HotkeyProbe for PlatformHotkeyProbe {
    fn probe(&self, combo: &mosaix_config::KeyCombo) -> mosaix_ipc::ProbeOutcome {
        let Some((modifiers, vk)) = hotkeys::binding_parts(combo) else {
            return mosaix_ipc::ProbeOutcome::Unsupported {
                reason: format!("Mosaix has no key named {:?}", combo.key),
            };
        };
        match mosaix_platform_windows::probe_hotkey(modifiers, vk) {
            mosaix_platform_windows::HotkeyAvailability::Available => {
                mosaix_ipc::ProbeOutcome::Available
            }
            mosaix_platform_windows::HotkeyAvailability::Taken => mosaix_ipc::ProbeOutcome::Taken,
        }
    }
}
