//! Command-line client for a running Mosaix agent.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "mosaix", about = "Control a running Mosaix agent")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Snap {
        #[command(subcommand)]
        direction: SnapDirection,
    },
    Focus {
        #[command(subcommand)]
        direction: FocusDirection,
    },
    Pause,
    Resume,
    TogglePause,
    Rearrange,
    ToggleAutomaticTiling,
    ToggleFloating,
    /// Select a display for commands such as saved-layout apply.
    FocusDisplay {
        display_id: isize,
    },
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    SwapLeft,
    SwapRight,
    SwapUp,
    SwapDown,
    /// Work with saved layouts.
    Layout {
        #[command(subcommand)]
        action: LayoutAction,
    },
    /// Reverse the newest placement command.
    ///
    /// Refuses, and keeps the command available to retry, whenever a target
    /// window cannot be identified beyond doubt or your displays have
    /// changed. There is no way to force it. There is no redo: it is
    /// deferred to issue #44.
    Undo {
        /// Report what undo would do without doing it.
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    /// Inspect or recover the durable state database.
    Persistence {
        #[command(subcommand)]
        action: PersistenceAction,
    },
    State {
        #[arg(long)]
        json: bool,
    },
    Ping,
}

#[derive(Debug, Subcommand)]
enum LayoutAction {
    /// Apply a saved layout to the focused display. Fails with the agent's
    /// own reason if no display is targeted or no layout carries that name.
    Apply {
        /// The layout's name, as declared under `[layouts]` in config.
        name: String,
    },
}

#[derive(Debug, Subcommand)]
enum PersistenceAction {
    /// Report whether committed state is durable, and why not if it is not.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Move an unusable state database aside and start a fresh one.
    ///
    /// Nothing is deleted: the old file is preserved next to it. Refused
    /// while a running agent still holds a healthy database, since that
    /// database is not the one needing recovery.
    Reset,
}

#[derive(Debug, Subcommand)]
enum SnapDirection {
    #[command(name = "left-half")]
    Left,
    #[command(name = "right-half")]
    Right,
    #[command(name = "top-half")]
    Top,
    #[command(name = "bottom-half")]
    Bottom,
}

#[derive(Debug, Subcommand)]
enum FocusDirection {
    Left,
    Right,
}

#[cfg(windows)]
fn main() {
    use mosaix_ipc::{send_request, IpcRequest, IpcResponse};

    let cli = Cli::parse();
    // The two persistence actions do not map onto a single request: status
    // renders fields the agent already publishes, and reset deliberately
    // works on the file itself, because every failure that calls for it is
    // one where the agent never got the database open.
    if let Command::Persistence { action } = cli.command {
        run_persistence(action);
        return;
    }
    // Undo answers with a typed result either way, so a refusal has to be
    // rendered rather than printed as a bare error string.
    if let Command::Undo { dry_run, json } = cli.command {
        if dry_run {
            report_undo_availability(json);
        } else {
            run_undo(json);
        }
        return;
    }
    let state_json = matches!(&cli.command, Command::State { json: true });
    let request = match cli.command {
        Command::Snap {
            direction: SnapDirection::Left,
        } => IpcRequest::SnapLeft,
        Command::Snap {
            direction: SnapDirection::Right,
        } => IpcRequest::SnapRight,
        Command::Snap {
            direction: SnapDirection::Top,
        } => IpcRequest::SnapTop,
        Command::Snap {
            direction: SnapDirection::Bottom,
        } => IpcRequest::SnapBottom,
        Command::Focus {
            direction: FocusDirection::Left,
        } => IpcRequest::FocusLeft,
        Command::Focus {
            direction: FocusDirection::Right,
        } => IpcRequest::FocusRight,
        Command::Pause => IpcRequest::Pause,
        Command::Resume => IpcRequest::Resume,
        Command::TogglePause => IpcRequest::TogglePause,
        Command::Rearrange => IpcRequest::Rearrange,
        Command::ToggleAutomaticTiling => IpcRequest::ToggleAutomaticTiling,
        Command::ToggleFloating => IpcRequest::ToggleFloating,
        Command::FocusDisplay { display_id } => IpcRequest::FocusDisplay { display_id },
        Command::FocusLeft => IpcRequest::FocusLeft,
        Command::FocusRight => IpcRequest::FocusRight,
        Command::FocusUp => IpcRequest::FocusUp,
        Command::FocusDown => IpcRequest::FocusDown,
        Command::SwapLeft => IpcRequest::SwapLeft,
        Command::SwapRight => IpcRequest::SwapRight,
        Command::SwapUp => IpcRequest::SwapUp,
        Command::SwapDown => IpcRequest::SwapDown,
        Command::Layout {
            action: LayoutAction::Apply { name },
        } => IpcRequest::ApplyLayout { name },
        Command::State { .. } => IpcRequest::GetState,
        Command::Ping => IpcRequest::Ping,
        Command::Persistence { .. } | Command::Undo { .. } => unreachable!("handled above"),
    };
    match send_request(request) {
        Ok(IpcResponse::Ok { data: Some(data) }) if state_json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&data).expect("JSON value serializes")
            );
        }
        Ok(IpcResponse::Ok { data: Some(data) }) => println!("{data}"),
        Ok(IpcResponse::Ok { data: None }) => println!("Ok"),
        Ok(IpcResponse::Error { message }) => {
            eprintln!("mosaix: {message}");
            std::process::exit(1);
        }
        Ok(IpcResponse::VersionMismatch { server_version }) => {
            eprintln!("mosaix: protocol version mismatch (server: v{server_version})");
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("mosaix: {error}");
            std::process::exit(2);
        }
    }
}

/// Reports whether undo would work right now, and why not if it would not.
///
/// Reads the agent's published state rather than asking undo to preflight,
/// because the agent already computed the same verdict there -- and because
/// a dry run must not be able to move a window by accident.
#[cfg(windows)]
fn report_undo_availability(json: bool) {
    let Some(state) = published_persistence() else {
        eprintln!("mosaix: no agent is running, so there is nothing to undo");
        std::process::exit(1);
    };
    let available = state["undo_available"].as_bool().unwrap_or(false);
    let command = state["undo_command"].as_str();
    let blocked = state["undo_blocked_reason"].as_str();
    let transaction = state["undo_transaction_id"].as_i64();

    if json {
        let value = serde_json::json!({
            "undo_available": available,
            "undo_command": command,
            "undo_transaction_id": transaction,
            "undo_blocked_reason": blocked,
            "persistence_status": state["persistence_status"],
            "persistence_reason": state["persistence_reason"],
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).expect("JSON value serializes")
        );
        if !available {
            std::process::exit(1);
        }
        return;
    }

    match (available, command) {
        (true, Some(command)) => println!("undo would reverse {command}"),
        (true, None) => println!("undo is available"),
        (false, Some(command)) => {
            println!("undo cannot run");
            println!("  next in history: {command}");
            if let Some(blocked) = blocked {
                println!("  blocked by: {blocked}");
            }
            std::process::exit(1);
        }
        (false, None) => {
            println!("there is nothing to undo");
            std::process::exit(1);
        }
    }
}

#[cfg(windows)]
fn run_undo(json: bool) {
    use mosaix_domain::undo::{UndoRefusal, UndoResult};
    use mosaix_ipc::{send_request, IpcRequest, IpcResponse};

    let data = match send_request(IpcRequest::Undo) {
        Ok(IpcResponse::Ok { data: Some(data) }) => data,
        Ok(IpcResponse::Ok { data: None }) => {
            eprintln!("mosaix: the agent answered without an undo result");
            std::process::exit(2);
        }
        Ok(IpcResponse::Error { message }) => {
            eprintln!("mosaix: {message}");
            std::process::exit(1);
        }
        Ok(IpcResponse::VersionMismatch { server_version }) => {
            eprintln!("mosaix: protocol version mismatch (server: v{server_version})");
            std::process::exit(1);
        }
        Err(error) => {
            eprintln!("mosaix: {error}");
            std::process::exit(2);
        }
    };

    let result: UndoResult = match serde_json::from_value(data.clone()) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("mosaix: could not read the agent's undo result: {error}");
            std::process::exit(2);
        }
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&data).expect("JSON value serializes")
        );
        // A refusal is a normal answer to report, but it is still a
        // failure to act on, so scripts see it in the exit status too.
        if !result.is_applied() {
            std::process::exit(1);
        }
        return;
    }

    match result {
        UndoResult::Applied(applied) => {
            let count = applied.restored.len();
            let plural = if count == 1 { "window" } else { "windows" };
            println!("undid {} ({count} {plural})", applied.command);
            for restored in applied.restored {
                println!(
                    "  window {} back to {}x{} at {},{} on display {}",
                    restored.window_id.0,
                    restored.placement.width,
                    restored.placement.height,
                    restored.placement.x,
                    restored.placement.y,
                    restored.display_id.0,
                );
            }
        }
        UndoResult::Refused(refusal) => {
            eprintln!("mosaix: {refusal}");
            match &refusal {
                UndoRefusal::TopologyChanged {
                    recorded_fingerprint,
                    current_fingerprint,
                    ..
                } => {
                    eprintln!("  recorded on: {recorded_fingerprint}");
                    eprintln!("  now:         {current_fingerprint}");
                    eprintln!("  reconnect that arrangement and try again");
                }
                UndoRefusal::TargetsUnresolved { targets, .. } => {
                    for target in targets {
                        eprintln!(
                            "  target {} ({}): {}",
                            target.ordinal,
                            target.application,
                            target.outcome.code()
                        );
                        for candidate in match_candidates(&target.outcome) {
                            eprintln!(
                                "      candidate window {} scored {}",
                                candidate.window_id.0, candidate.score
                            );
                        }
                    }
                }
                UndoRefusal::TargetsCollide {
                    window_id,
                    ordinals,
                    ..
                } => {
                    eprintln!(
                        "  targets {ordinals:?} all matched window {}",
                        window_id.0
                    );
                }
                UndoRefusal::PersistenceDegraded { reason, .. } => {
                    eprintln!("  reason: {reason}");
                    eprintln!("  see `mosaix persistence status`");
                }
                UndoRefusal::NothingToUndo => {}
            }
            std::process::exit(1);
        }
    }
}

/// The scored candidates a refusal has to show, whichever shape the
/// outcome took.
#[cfg(windows)]
fn match_candidates(
    outcome: &mosaix_domain::MatchOutcome,
) -> &[mosaix_domain::ScoredCandidate] {
    use mosaix_domain::MatchOutcome;

    match outcome {
        MatchOutcome::Confident(_) => &[],
        MatchOutcome::Ambiguous { candidates } => candidates,
        MatchOutcome::NoMatch { considered } => considered,
    }
}

/// The agent's published view of durability, or `None` when no agent
/// answered. "No agent" and "an agent that cannot reach its database" are
/// different situations and only the first is safe to reset blindly.
#[cfg(windows)]
fn published_persistence() -> Option<serde_json::Value> {
    use mosaix_ipc::{send_request, IpcRequest, IpcResponse};

    match send_request(IpcRequest::GetState) {
        Ok(IpcResponse::Ok { data: Some(data) }) => Some(data),
        _ => None,
    }
}

#[cfg(windows)]
fn run_persistence(action: PersistenceAction) {
    match action {
        PersistenceAction::Status { json } => {
            let Some(state) = published_persistence() else {
                eprintln!("mosaix: no agent is running, so durability is not being tracked");
                std::process::exit(1);
            };
            let status = state["persistence_status"].as_str().unwrap_or("unknown");
            let revision = state["last_durable_revision"].as_u64().unwrap_or(0);
            let reason = state["persistence_reason"].as_str();
            if json {
                let value = serde_json::json!({
                    "persistence_status": status,
                    "last_durable_revision": revision,
                    "persistence_reason": reason,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).expect("JSON value serializes")
                );
                return;
            }
            match reason {
                None => println!("persistence: healthy (durable through revision {revision})"),
                Some(reason) => {
                    println!("persistence: degraded ({reason})");
                    println!("last durable revision: {revision}");
                    println!(
                        "window management continues from memory, but nothing new is \
                         being made durable and undo will refuse"
                    );
                    println!("run `mosaix persistence reset` to start a fresh database");
                }
            }
        }
        PersistenceAction::Reset => {
            if let Some(state) = published_persistence() {
                if state["persistence_status"].as_str() == Some("healthy") {
                    eprintln!(
                        "mosaix: the running agent's state database is healthy; \
                         stop the agent before resetting it"
                    );
                    std::process::exit(1);
                }
            }
            let Some(path) = mosaix_persistence::default_database_path() else {
                eprintln!("mosaix: this platform has no state database location");
                std::process::exit(2);
            };
            match mosaix_persistence::Persistence::reset(&path) {
                Ok(outcome) => {
                    match outcome.preserved {
                        Some(preserved) => {
                            println!("previous database preserved at {}", preserved.display());
                        }
                        None => println!("no previous database was present"),
                    }
                    println!("fresh state database created at {}", path.display());
                    println!("restart the agent to resume durable state");
                }
                Err(error) => {
                    eprintln!("mosaix: {error}");
                    std::process::exit(1);
                }
            }
        }
    }
}

#[cfg(not(windows))]
fn main() {
    let _ = Cli::parse();
    eprintln!("mosaix currently only supports Windows");
    std::process::exit(2);
}
