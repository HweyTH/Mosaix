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

/// Starts `RegisterHotKey` registration for `bindings` and a forwarder
/// thread translating each firing into `Event::ZoneSnapRequested`,
/// mirroring the shape of every other OS-event forwarder in this file.
/// Per-binding registration failures are logged and otherwise ignored (ADR
/// 0002's partial-success posture, preserved through every re-registration,
/// not just the first); only a failure to start the registration thread
/// itself is reported to the caller.
#[cfg(windows)]
fn start_hotkeys_and_forward(
    bindings: Vec<mosaix_platform_windows::HotkeyBinding>,
    events: mosaix_engine::EventSender,
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
            let direction = hotkeys::direction_for_command(command);
            if events
                .send(mosaix_engine::Event::ZoneSnapRequested { direction })
                .is_err()
            {
                tracing::warn!("reducer stopped; hotkey forwarder exiting");
                break;
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

    let event_hooks_and_forwarder = match mosaix_platform_windows::start_event_hooks() {
        Ok((hooks, raw_events)) => {
            let events = engine.events();
            let forwarder = std::thread::spawn(move || {
                // `RawEvent::Focused` and `RawEvent::LocationChanged` are
                // forwarded here. The other variants (WindowCreated/
                // WindowDestroyed/MoveResizeStart/MoveResizeEnd) aren't
                // consumed by the engine yet.
                for event in raw_events {
                    match event {
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
                        _ => {}
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
    let last_registered_hotkeys = engine.state_reader().snapshot().resolved_config.hotkeys;
    let initial_hotkey_registration = match start_hotkeys_and_forward(
        hotkeys::bindings_from_resolved(&last_registered_hotkeys),
        engine.events(),
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
        std::thread::spawn(move || {
            let mut previous_hotkeys = last_registered_hotkeys;
            let mut current_registration = initial_hotkey_registration;
            loop {
                match hotkey_rebind_stop_rx.recv_timeout(HOTKEY_REBIND_POLL_INTERVAL) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }

                let current_hotkeys = state_reader.snapshot().resolved_config.hotkeys;
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

    // Polls committed engine state for placements that haven't reached the
    // real window yet, and applies them via `SetWindowPos` (architecture
    // doc section 6's Diff -> Executor stage). Nothing else in this binary
    // ever calls `move_resize_window`, and the reducer deliberately never
    // touches the OS itself -- without this, a snap hotkey updates
    // `EngineState` but the window on screen never moves.
    const PLACEMENT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
    let (executor_stop_tx, executor_stop_rx) = std::sync::mpsc::channel::<()>();
    let executor_forwarder = {
        let state_reader = engine.state_reader();
        std::thread::spawn(move || {
            let mut previous = std::collections::HashMap::new();
            loop {
                let current = state_reader.snapshot().windows;
                for (window_id, _display_id, bounds) in
                    mosaix_engine::diff_placements(&previous, &current)
                {
                    if let Err(err) =
                        mosaix_platform_windows::move_resize_window_by_id(window_id, bounds)
                    {
                        tracing::warn!(?window_id, %err, "failed to apply computed placement to the real window");
                    }
                }
                previous = current;

                match executor_stop_rx.recv_timeout(PLACEMENT_POLL_INTERVAL) {
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        })
    };

    let shutdown = mosaix_platform_windows::register_shutdown_signal()
        .expect("failed to register shutdown signal handler at startup");
    tracing::info!(
        "mosaix-agent ready; waiting for a shutdown signal (Ctrl+C, console close, logoff, or system shutdown)"
    );
    let _ = shutdown.recv();
    tracing::info!("shutdown signal received; stopping");

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
    let _ = executor_stop_tx.send(());
    let _ = executor_forwarder.join();
    engine.stop();
    tracing::info!("mosaix-agent stopped");
}

#[cfg(not(windows))]
fn main() {
    eprintln!("mosaix-agent currently only supports Windows (no macOS platform adapter yet).");
    std::process::exit(1);
}
