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
        Command::Persistence { .. } => unreachable!("handled above"),
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
                        "window management continues from memory; \
                         run `mosaix persistence reset` to start a fresh database"
                    );
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
