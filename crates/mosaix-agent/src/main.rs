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
//! failure (a topology read, the topology watcher) is logged and degrades
//! the agent instead -- it's meant to run all day.

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

    let engine = mosaix_engine::spawn_engine(initial_displays);

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
                                tracing::warn!("reducer stopped; display topology forwarder exiting");
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
    engine.stop();
    tracing::info!("mosaix-agent stopped");
}

#[cfg(not(windows))]
fn main() {
    eprintln!("mosaix-agent currently only supports Windows (no macOS platform adapter yet).");
    std::process::exit(1);
}
