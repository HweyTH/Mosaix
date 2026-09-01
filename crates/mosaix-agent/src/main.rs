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

/// Starts `RegisterHotKey` registration for `bindings` and a forwarder
/// thread translating each firing into `Event::ZoneSnapRequested`,
/// mirroring the shape of every other OS-event forwarder in this file.
/// Per-binding registration failures are logged and otherwise ignored (ADR
/// 0002's partial-success posture, preserved through every re-registration,
/// not just the first); only a failure to start the registration thread
/// itself is reported to the caller.
///
/// When `overlay_tx` is present, each successful enqueue also signals the
/// snap-preview controller (Feature 34) with the pre-send revision so it
/// can flash the committed placement.
#[cfg(windows)]
fn start_hotkeys_and_forward(
    bindings: Vec<mosaix_platform_windows::HotkeyBinding>,
    events: mosaix_engine::EventSender,
    state_reader: mosaix_engine::StateReader,
    overlay_tx: Option<std::sync::mpsc::Sender<overlay::OverlayRequest>>,
) -> mosaix_platform_windows::Result<(
    mosaix_platform_windows::HotkeyRegistrations,
    std::thread::JoinHandle<()>,
)> {
    let (registrations, hotkey_events) = mosaix_platform_windows::start_hotkeys(bindings)?;
    for result in &registrations.results {
        if let Err(err) = &result.outcome {
            let command = hotkeys::command_for_hotkey_id(result.id);
            tracing::error!(
                hotkey_id = result.id,
                ?command,
                %err,
                "failed to register hotkey; that binding will not work, the rest still will"
            );
        }
    }
    let forwarder = std::thread::spawn(move || {
        for fired in hotkey_events {
            let Some(command) = hotkeys::command_for_hotkey_id(fired.id) else {
                tracing::warn!(
                    hotkey_id = fired.id,
                    "hotkey fired for an unknown id; ignoring"
                );
                continue;
            };
            let pre_revision = state_reader.snapshot().revision;
            let event = match command {
                mosaix_config::Command::Rearrange => mosaix_engine::Event::RearrangeRequested,
                mosaix_config::Command::ToggleAutomaticTiling => {
                    mosaix_engine::Event::ToggleAutomaticTilingRequested
                }
                mosaix_config::Command::ToggleFloating => {
                    mosaix_engine::Event::ToggleFloatingRequested
                }
                mosaix_config::Command::FocusLeft => {
                    mosaix_engine::Event::DirectionalFocusRequested {
                        direction: mosaix_engine::CardinalDirection::Left,
                    }
                }
                mosaix_config::Command::FocusRight => {
                    mosaix_engine::Event::DirectionalFocusRequested {
                        direction: mosaix_engine::CardinalDirection::Right,
                    }
                }
                mosaix_config::Command::FocusUp => {
                    mosaix_engine::Event::DirectionalFocusRequested {
                        direction: mosaix_engine::CardinalDirection::Up,
                    }
                }
                mosaix_config::Command::FocusDown => {
                    mosaix_engine::Event::DirectionalFocusRequested {
                        direction: mosaix_engine::CardinalDirection::Down,
                    }
                }
                mosaix_config::Command::SwapLeft => {
                    mosaix_engine::Event::DirectionalSwapRequested {
                        direction: mosaix_engine::CardinalDirection::Left,
                    }
                }
                mosaix_config::Command::SwapRight => {
                    mosaix_engine::Event::DirectionalSwapRequested {
                        direction: mosaix_engine::CardinalDirection::Right,
                    }
                }
                mosaix_config::Command::SwapUp => mosaix_engine::Event::DirectionalSwapRequested {
                    direction: mosaix_engine::CardinalDirection::Up,
                },
                mosaix_config::Command::SwapDown => {
                    mosaix_engine::Event::DirectionalSwapRequested {
                        direction: mosaix_engine::CardinalDirection::Down,
                    }
                }
                mosaix_config::Command::TogglePause => {
                    if state_reader.snapshot().paused {
                        mosaix_engine::Event::ResumeRequested
                    } else {
                        mosaix_engine::Event::PauseRequested
                    }
                }
                command => mosaix_engine::Event::ZoneSnapRequested {
                    direction: hotkeys::direction_for_command(command),
                },
            };
            if events.send(event).is_err() {
                tracing::warn!("reducer stopped; hotkey forwarder exiting");
                break;
            }
            if !matches!(
                command,
                mosaix_config::Command::Rearrange
                    | mosaix_config::Command::ToggleAutomaticTiling
                    | mosaix_config::Command::ToggleFloating
                    | mosaix_config::Command::FocusLeft
                    | mosaix_config::Command::FocusRight
                    | mosaix_config::Command::FocusUp
                    | mosaix_config::Command::FocusDown
                    | mosaix_config::Command::SwapLeft
                    | mosaix_config::Command::SwapRight
                    | mosaix_config::Command::SwapUp
                    | mosaix_config::Command::SwapDown
                    | mosaix_config::Command::TogglePause
            ) {
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

    let engine = mosaix_engine::spawn_engine(initial_displays, initial_config_set);

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
    }

    let ipc_server = match mosaix_ipc::IpcServer::start(engine.events(), engine.state_reader()) {
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
                            .send(mosaix_engine::Event::ConfigChanged(set))
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
            let forwarder = std::thread::spawn(move || {
                for event in topology_events {
                    match event {
                        mosaix_platform_windows::TopologyEvent::Changed(displays) => {
                            if events
                                .send(mosaix_engine::Event::DisplayTopologyChanged(displays))
                                .is_err()
                            {
                                tracing::warn!(
                                    "reducer stopped; display topology forwarder exiting"
                                );
                                break;
                            }
                        }
                        // Feature 29 — sleep/wake recovery.
                        //
                        // After wake the platform layer re-enumerates displays
                        // and sends `WakeFromSleep`.  We also re-enumerate
                        // windows here because the platform layer's hidden
                        // window only sees display events, not window events.
                        mosaix_platform_windows::TopologyEvent::WakeFromSleep(displays) => {
                            let windows = match mosaix_platform_windows::enumerate_windows() {
                                Ok(w) => w,
                                Err(err) => {
                                    tracing::error!(%err, "failed to enumerate windows after wake; sending display-only reconciliation");
                                    Vec::new()
                                }
                            };
                            let placements: Vec<_> = windows
                                .iter()
                                .filter_map(|window| {
                                    let handle =
                                        mosaix_platform_windows::window_handle_from_id(window.id);
                                    let (display_id, bounds) =
                                        mosaix_platform_windows::observed_window_state(handle)?;
                                    Some((window.id, display_id, bounds))
                                })
                                .collect();
                            tracing::info!(
                                display_count = displays.len(),
                                window_count = placements.len(),
                                "forwarding wake reconciliation to engine"
                            );
                            if events
                                .send(mosaix_engine::Event::WakeReconciliation {
                                    displays,
                                    windows: placements,
                                })
                                .is_err()
                            {
                                tracing::warn!(
                                    "reducer stopped; wake reconciliation forwarder exiting"
                                );
                                break;
                            }
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

    let event_hooks_and_forwarder = match mosaix_platform_windows::start_event_hooks() {
        Ok((hooks, raw_events)) => {
            let events = engine.events();
            let overlay_tx = overlay_tx.clone();
            let forwarder = std::thread::spawn(move || {
                // Focus and location changes feed the engine; move/resize
                // start/end feed the snap-preview drag controller (Feature 34).
                for event in raw_events {
                    match event {
                        mosaix_platform_windows::RawEvent::WindowCreated(_)
                        | mosaix_platform_windows::RawEvent::WindowDestroyed(_) => {
                            // Hooks are hints; re-enumerate a complete
                            // normalized batch so creation and destruction
                            // update the engine-owned inventory together.
                            let windows = match mosaix_platform_windows::enumerate_windows() {
                                Ok(windows) => windows,
                                Err(err) => {
                                    tracing::debug!(%err, "could not refresh inventory after lifecycle event");
                                    continue;
                                }
                            };
                            if events
                                .send(mosaix_engine::Event::WindowsObserved { windows })
                                .is_err()
                            {
                                tracing::warn!("reducer stopped; lifecycle forwarder exiting");
                                break;
                            }
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
            Some((hooks, forwarder))
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
    let initial_hotkey_registration = match start_hotkeys_and_forward(
        hotkeys::bindings_from_resolved(&last_registered_hotkeys),
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
    const HOTKEY_REBIND_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);
    let (hotkey_rebind_stop_tx, hotkey_rebind_stop_rx) = std::sync::mpsc::channel::<()>();
    let hotkey_rebind_forwarder = {
        let state_reader = engine.state_reader();
        let events = engine.events();
        let overlay_tx = overlay_tx.clone();
        std::thread::spawn(move || {
            let mut previous_hotkeys = last_registered_hotkeys;
            let mut current_registration = initial_hotkey_registration;
            loop {
                match hotkey_rebind_stop_rx.recv_timeout(HOTKEY_REBIND_POLL_INTERVAL) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }

                let current_hotkeys =
                    hotkeys::runtime_hotkeys(&state_reader.snapshot().resolved_config);
                if mosaix_config::diff_bindings(&previous_hotkeys, &current_hotkeys).is_empty() {
                    continue;
                }
                tracing::info!("resolved hotkey bindings changed; re-registering hotkeys");

                if let Some((registrations, forwarder)) = current_registration.take() {
                    registrations.stop();
                    let _ = forwarder.join();
                }
                current_registration = match start_hotkeys_and_forward(
                    hotkeys::bindings_from_resolved(&current_hotkeys),
                    events.clone(),
                    state_reader.clone(),
                    overlay_tx.clone(),
                ) {
                    Ok(pair) => Some(pair),
                    Err(err) => {
                        tracing::error!(%err, "failed to re-register hotkeys after a binding change; hotkeys are unregistered until the next change");
                        None
                    }
                };
                previous_hotkeys = current_hotkeys;
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
                        if let mosaix_engine::EngineEffect::FocusWindow { window_id } = *effect {
                            if let Err(err) = mosaix_platform_windows::focus_window_by_id(window_id)
                            {
                                tracing::warn!(?window_id, %err, "failed to focus directional neighbor");
                            }
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
                        }
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
            // Status updates need the handle; share via a channel of bools
            // that the forwarder pushes and a tiny status thread applies —
            // simpler: keep handle in the forwarder and use a stop channel
            // from main so console quit unblocks without hanging.
            let (tray_stop_tx, tray_stop_rx) = std::sync::mpsc::channel::<()>();
            let forwarder = std::thread::spawn(move || {
                const TRAY_STATUS_POLL: std::time::Duration = std::time::Duration::from_millis(250);
                let mut last_paused = state_reader.snapshot().paused;
                tray.set_paused(last_paused);
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
                            let paused = tray_for_status.snapshot().paused;
                            if paused != last_paused {
                                tray.set_paused(paused);
                                last_paused = paused;
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
    if let Some((watcher, forwarder)) = watcher_and_forwarder {
        watcher.stop();
        let _ = forwarder.join();
    }
    if let Some((watcher, forwarder)) = config_watcher_and_forwarder {
        watcher.stop();
        let _ = forwarder.join();
    }
    if let Some((hooks, forwarder)) = event_hooks_and_forwarder {
        hooks.stop();
        let _ = forwarder.join();
    }
    let _ = hotkey_rebind_stop_tx.send(());
    let _ = hotkey_rebind_forwarder.join();

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
    tracing::info!("mosaix-agent stopped");
}

#[cfg(not(windows))]
fn main() {
    eprintln!("mosaix-agent currently only supports Windows (no macOS platform adapter yet).");
    std::process::exit(1);
}
