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
    /// Exchange the focused window with its neighbor that way. Reports
    /// the typed outcome; a swap with no neighbor that way is refused
    /// rather than wrapping or crossing a display.
    SwapLeft {
        #[arg(long)]
        json: bool,
    },
    SwapRight {
        #[arg(long)]
        json: bool,
    },
    SwapUp {
        #[arg(long)]
        json: bool,
    },
    SwapDown {
        #[arg(long)]
        json: bool,
    },
    /// Move the nearest container-tree divider facing that way by five
    /// percentage points, growing the focused window's side.
    ///
    /// Stops short of pushing any window below its minimum size, and does
    /// nothing at the edge of the arrangement. Either way the outcome is
    /// reported, not silently swallowed.
    Resize {
        #[command(subcommand)]
        direction: ResizeDirection,
        #[arg(long)]
        json: bool,
    },
    /// Forget a dormant tree position: a slot kept for a window that has
    /// closed, listed by `mosaix arrangement`.
    ///
    /// Undoable. A slot for an open window cannot be removed this way;
    /// close the window instead.
    RemovePosition {
        /// The display whose tree holds the slot.
        #[arg(long)]
        display: isize,
        /// The position number `mosaix arrangement` reports.
        position: u64,
        #[arg(long)]
        json: bool,
    },
    /// Work with saved layouts.
    Layout {
        #[command(subcommand)]
        action: LayoutAction,
    },
    /// Work with logical workspaces: named groups of managed windows,
    /// each displayed on at most one display at a time.
    Workspace {
        #[command(subcommand)]
        action: WorkspaceAction,
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
    /// Report the automatic-tiling arrangement and, in tree mode, each
    /// display's container tree.
    Arrangement {
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
enum WorkspaceAction {
    /// List every workspace, where it is displayed, and its members.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Report experimental workspace switching for the current topology:
    /// disabled, requested, unavailable, or experimental, with the
    /// reason and the mapping the matched profile declares.
    Switching {
        #[arg(long)]
        json: bool,
    },
    /// Create a hidden, empty workspace. Refuses a name that already
    /// exists under any casing.
    Create {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Delete a workspace. Refuses one that is displayed, still owns a
    /// window or a dormant position, or is declared in configuration.
    Delete {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Display a hidden workspace on the focused display, or focus the
    /// last-focused window of one already displayed elsewhere. Never
    /// moves a displayed workspace; see `move` for that.
    Focus {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Move a displayed workspace to another display, exchanging it with
    /// whatever that display shows.
    Move {
        name: String,
        /// The display to move it to, by the id `mosaix state` reports.
        #[arg(long)]
        display: isize,
        #[arg(long)]
        json: bool,
    },
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

#[derive(Debug, Subcommand, Clone, Copy)]
enum ResizeDirection {
    Left,
    Right,
    Up,
    Down,
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
    // Arrangement reads fields the agent already publishes and renders
    // them, rather than asking for a report the agent does not have.
    if let Command::Arrangement { json } = cli.command {
        report_arrangement(json);
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
    // So does a tree resize, and removing a dormant position.
    if let Command::Resize { direction, json } = cli.command {
        run_resize(direction, json);
        return;
    }
    // And every workspace command.
    if let Command::Workspace { action } = cli.command {
        run_workspace(action);
        return;
    }
    if let Some((request, json)) = match cli.command {
        Command::SwapLeft { json } => Some((IpcRequest::SwapLeft, json)),
        Command::SwapRight { json } => Some((IpcRequest::SwapRight, json)),
        Command::SwapUp { json } => Some((IpcRequest::SwapUp, json)),
        Command::SwapDown { json } => Some((IpcRequest::SwapDown, json)),
        _ => None,
    } {
        run_swap(request, json);
        return;
    }
    if let Command::RemovePosition {
        display,
        position,
        json,
    } = cli.command
    {
        run_remove_position(display, position, json);
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
        Command::Layout {
            action: LayoutAction::Apply { name },
        } => IpcRequest::ApplyLayout { name },
        Command::State { .. } => IpcRequest::GetState,
        Command::Ping => IpcRequest::Ping,
        Command::Persistence { .. }
        | Command::Undo { .. }
        | Command::Arrangement { .. }
        | Command::Resize { .. }
        | Command::Workspace { .. }
        | Command::RemovePosition { .. }
        | Command::SwapLeft { .. }
        | Command::SwapRight { .. }
        | Command::SwapUp { .. }
        | Command::SwapDown { .. } => {
            unreachable!("handled above")
        }
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

/// Renders the arrangement section of published state.
///
/// A pure function of the snapshot so the wording can be tested without an
/// agent to talk to, which is the only part of this command with any
/// decisions in it.
fn format_arrangement(state: &serde_json::Value) -> String {
    let mode = state["tiling_mode"].as_str().unwrap_or("unknown");
    let active = state["automatic_tiling_active"].as_bool().unwrap_or(false);
    let suspended = state["automatic_tiling_suspended"]
        .as_bool()
        .unwrap_or(false);

    // Suspension is reported ahead of activity because a suspended
    // arrangement is still the configured one -- saying "off" would read as
    // "you are not using tree mode".
    let status = if suspended {
        "suspended"
    } else if active {
        "active"
    } else {
        "off"
    };
    let mut rendered = format!("arrangement: {mode} ({status})");

    let trees = state["container_trees"].as_array();
    match trees {
        Some(trees) if !trees.is_empty() => {
            for tree in trees {
                let display = tree["display_id"].as_i64().unwrap_or(0);
                let windows: Vec<String> = tree["windows"]
                    .as_array()
                    .map(|windows| {
                        windows
                            .iter()
                            .filter_map(|window| window.as_i64())
                            .map(|window| window.to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                rendered.push_str(&format!("\n  display {display}: {}", windows.join(" ")));
                let overflow: Vec<String> = tree["constraint_overflow"]
                    .as_array()
                    .map(|windows| {
                        windows
                            .iter()
                            .filter_map(|window| window.as_i64())
                            .map(|window| window.to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                if !overflow.is_empty() {
                    rendered.push_str(&format!(
                        "\n    in constraint overflow (cannot fit at minimum size): {}",
                        overflow.join(" ")
                    ));
                }
                if let Some(dormant) = tree["dormant_positions"].as_array() {
                    for slot in dormant {
                        rendered.push_str(&format!(
                            "\n    dormant position {} kept for {} (expires {})",
                            slot["position"].as_u64().unwrap_or(0),
                            slot["application"].as_str().unwrap_or("?"),
                            slot["expires_unix"].as_i64().unwrap_or(0),
                        ));
                    }
                }
            }
        }
        _ if mode == "tree" => {
            rendered.push_str("\n  no display is arranging windows yet");
        }
        _ => {}
    }
    rendered
}

#[cfg(windows)]
fn report_arrangement(json: bool) {
    let Some(state) = published_persistence() else {
        eprintln!("mosaix: no agent is running");
        std::process::exit(1);
    };

    if json {
        let value = serde_json::json!({
            "tiling_mode": state["tiling_mode"],
            "automatic_tiling_active": state["automatic_tiling_active"],
            "automatic_tiling_suspended": state["automatic_tiling_suspended"],
            "container_trees": state["container_trees"],
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).expect("JSON value serializes")
        );
        return;
    }
    println!("{}", format_arrangement(&state));
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

/// Renders a tree-resize outcome for a person.
///
/// A pure function of the result so the wording can be tested without an
/// agent. The exit status is the caller's concern: a refusal is a normal
/// answer to print, but still a failure to act on.
fn format_resize(result: &mosaix_domain::TreeResizeResult) -> String {
    use mosaix_domain::TreeResizeResult;
    match result {
        TreeResizeResult::Applied(applied) => {
            let grew: Vec<String> = applied.grew.iter().map(|id| id.0.to_string()).collect();
            let shrank: Vec<String> = applied.shrank.iter().map(|id| id.0.to_string()).collect();
            let mut rendered = format!(
                "{}: moved the divider {} percentage point{}",
                applied.command,
                applied.percentage_points,
                if applied.percentage_points == 1 {
                    ""
                } else {
                    "s"
                }
            );
            if applied.percentage_points < mosaix_domain::TREE_RESIZE_STEP_PERCENT {
                rendered.push_str(" (clamped at a minimum size)");
            }
            rendered.push_str(&format!(
                "\n  grew: {}\n  shrank: {}",
                grew.join(" "),
                shrank.join(" ")
            ));
            rendered
        }
        TreeResizeResult::Refused(refusal) => format!("mosaix: {refusal}"),
    }
}

/// Renders a directional-swap outcome for a person.
fn format_swap(result: &mosaix_domain::DirectionalSwapResult) -> String {
    use mosaix_domain::DirectionalSwapResult;
    match result {
        DirectionalSwapResult::Applied(applied) => format!(
            "{}: swapped window {} with window {}",
            applied.command, applied.window_id.0, applied.neighbor_id.0
        ),
        DirectionalSwapResult::Refused(refusal) => format!("mosaix: {refusal}"),
    }
}

#[cfg(windows)]
fn run_swap(request: mosaix_ipc::IpcRequest, json: bool) {
    use mosaix_ipc::{send_request, IpcResponse};

    let data = match send_request(request) {
        Ok(IpcResponse::Ok { data: Some(data) }) => data,
        Ok(IpcResponse::Ok { data: None }) => {
            eprintln!("mosaix: the agent answered without a swap result");
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
    let result: mosaix_domain::DirectionalSwapResult = match serde_json::from_value(data.clone()) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("mosaix: could not read the agent's swap result: {error}");
            std::process::exit(2);
        }
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&data).expect("JSON value serializes")
        );
    } else if result.is_applied() {
        println!("{}", format_swap(&result));
    } else {
        eprintln!("{}", format_swap(&result));
    }
    if !result.is_applied() {
        std::process::exit(1);
    }
}

/// Renders a workspace command outcome for a person.
fn format_workspace_result(result: &mosaix_domain::WorkspaceCommandResult) -> String {
    use mosaix_domain::{WorkspaceCommandResult, WorkspaceFocusApplied};
    match result {
        WorkspaceCommandResult::Created(applied) => {
            format!("created workspace {} (hidden; focus it to display it)", applied.name)
        }
        WorkspaceCommandResult::Deleted(applied) => format!("deleted workspace {}", applied.name),
        WorkspaceCommandResult::Focused(WorkspaceFocusApplied::Displayed {
            name,
            display_id,
            replaced,
        }) => match replaced {
            Some(replaced) => format!(
                "workspace {name} is now displayed on display {} (replacing {replaced}, now hidden)",
                display_id.0
            ),
            None => format!("workspace {name} is now displayed on display {}", display_id.0),
        },
        WorkspaceCommandResult::Focused(WorkspaceFocusApplied::FocusedExisting {
            name,
            display_id,
            focused_window,
        }) => match focused_window {
            Some(window) => format!(
                "workspace {name} is already displayed on display {}; focused window {}",
                display_id.0, window.0
            ),
            None => format!(
                "workspace {name} is already displayed on display {}; it has no window to focus",
                display_id.0
            ),
        },
        WorkspaceCommandResult::Moved(applied) => match &applied.swapped_with {
            Some(swapped) => format!(
                "moved workspace {} from display {} to display {}; {swapped} now occupies display {}",
                applied.name, applied.from_display_id.0, applied.to_display_id.0, applied.from_display_id.0
            ),
            None => format!(
                "moved workspace {} from display {} to display {}",
                applied.name, applied.from_display_id.0, applied.to_display_id.0
            ),
        },
        WorkspaceCommandResult::Refused(refusal) => {
            format!("mosaix: {refusal} ({})", refusal.code())
        }
    }
}

/// Renders the switching section of published state.
fn format_switching(state: &serde_json::Value) -> String {
    let switching = &state["workspace_switching"];
    let status = switching["status"].as_str().unwrap_or("unknown");
    let mut lines = vec![match (status, switching["reason"].as_str()) {
        ("disabled", _) => "workspace switching: disabled (no matched profile requests it; base config cannot)".to_owned(),
        ("requested", Some(reason)) => format!(
            "workspace switching: requested by the matched profile; not yet active ({reason})"
        ),
        ("unavailable", Some(reason)) => format!(
            "workspace switching: unavailable; the previous displayed assignment stands ({reason})"
        ),
        ("experimental", _) => {
            "workspace switching: experimental (parking via public APIs; not native virtual desktops)".to_owned()
        }
        (status, reason) => format!("workspace switching: {status} ({})", reason.unwrap_or("no reason")),
    }];
    if let Some(file) = switching["profile_file"].as_str() {
        lines.push(format!("  requested by: {file}"));
    }
    if let Some(displayed) = switching["displayed"].as_object() {
        for (display, name) in displayed {
            lines.push(format!("  {display} -> {}", name.as_str().unwrap_or("?")));
        }
    }
    if let Some(capability) = switching["parking_capability"].as_str() {
        lines.push(format!("  parking site: {capability}"));
    }
    lines.join("\n")
}

/// Renders the workspace section of published state.
fn format_workspaces(state: &serde_json::Value) -> String {
    let Some(workspaces) = state["workspaces"].as_array() else {
        return "workspaces: none".to_owned();
    };
    if workspaces.is_empty() {
        return "workspaces: none".to_owned();
    }
    let mut lines = vec!["workspaces:".to_owned()];
    for workspace in workspaces {
        let name = workspace["name"].as_str().unwrap_or("?");
        let origin = workspace["origin"].as_str().unwrap_or("?");
        let members: Vec<String> = workspace["members"]
            .as_array()
            .map(|members| {
                members
                    .iter()
                    .filter_map(|id| id.as_i64())
                    .map(|id| id.to_string())
                    .collect()
            })
            .unwrap_or_default();
        let dormant = workspace["dormant_positions"].as_u64().unwrap_or(0);
        let place = match workspace["displayed_on"].as_i64() {
            Some(display) => format!("display {display}"),
            None => "hidden".to_owned(),
        };
        let mut line = format!("  {name}: {place}, from {origin}");
        if members.is_empty() {
            line.push_str(", no windows");
        } else {
            line.push_str(&format!(", windows {}", members.join(" ")));
        }
        if dormant > 0 {
            line.push_str(&format!(", {dormant} dormant position(s)"));
        }
        lines.push(line);
    }
    if let Some(refusals) = state["rule_workspace_refusals"].as_array() {
        for refusal in refusals {
            lines.push(format!(
                "  rule {} names unknown workspace {:?} for window {}",
                refusal["rule_id"].as_str().unwrap_or("?"),
                refusal["workspace"].as_str().unwrap_or("?"),
                refusal["window_id"].as_i64().unwrap_or(0)
            ));
        }
    }
    lines.join("\n")
}

#[cfg(windows)]
fn run_workspace(action: WorkspaceAction) {
    use mosaix_ipc::{send_request, IpcRequest, IpcResponse};

    let (request, json) = match action {
        WorkspaceAction::Switching { json } => {
            let Some(state) = published_persistence() else {
                eprintln!("mosaix: no agent is running");
                std::process::exit(1);
            };
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&state["workspace_switching"])
                        .expect("JSON value serializes")
                );
            } else {
                println!("{}", format_switching(&state));
            }
            return;
        }
        WorkspaceAction::List { json } => {
            let Some(state) = published_persistence() else {
                eprintln!("mosaix: no agent is running");
                std::process::exit(1);
            };
            if json {
                let value = serde_json::json!({
                    "workspaces": state["workspaces"],
                    "rule_workspace_refusals": state["rule_workspace_refusals"],
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).expect("JSON value serializes")
                );
            } else {
                println!("{}", format_workspaces(&state));
            }
            return;
        }
        WorkspaceAction::Create { name, json } => (IpcRequest::CreateWorkspace { name }, json),
        WorkspaceAction::Delete { name, json } => (IpcRequest::DeleteWorkspace { name }, json),
        WorkspaceAction::Focus { name, json } => (IpcRequest::FocusWorkspace { name }, json),
        WorkspaceAction::Move {
            name,
            display,
            json,
        } => (
            IpcRequest::MoveWorkspace {
                name,
                display_id: display,
            },
            json,
        ),
    };
    let data = match send_request(request) {
        Ok(IpcResponse::Ok { data: Some(data) }) => data,
        Ok(IpcResponse::Ok { data: None }) => {
            eprintln!("mosaix: the agent answered without a workspace result");
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
    let result: mosaix_domain::WorkspaceCommandResult = match serde_json::from_value(data.clone()) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("mosaix: could not read the agent's workspace result: {error}");
            std::process::exit(2);
        }
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&data).expect("JSON value serializes")
        );
    } else if result.is_applied() {
        println!("{}", format_workspace_result(&result));
    } else {
        eprintln!("{}", format_workspace_result(&result));
    }
    if !result.is_applied() {
        std::process::exit(1);
    }
}

/// Renders a remove-position outcome for a person.
fn format_remove_position(result: &mosaix_domain::RemovePositionResult) -> String {
    use mosaix_domain::RemovePositionResult;
    match result {
        RemovePositionResult::Applied(applied) => format!(
            "removed dormant position {} on display {} (was kept for {})",
            applied.position, applied.display_id.0, applied.application
        ),
        RemovePositionResult::Refused(refusal) => format!("mosaix: {refusal}"),
    }
}

#[cfg(windows)]
fn run_remove_position(display: isize, position: u64, json: bool) {
    use mosaix_ipc::{send_request, IpcRequest, IpcResponse};

    let data = match send_request(IpcRequest::RemoveTreePosition {
        display_id: display,
        position,
    }) {
        Ok(IpcResponse::Ok { data: Some(data) }) => data,
        Ok(IpcResponse::Ok { data: None }) => {
            eprintln!("mosaix: the agent answered without a result");
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
    let result: mosaix_domain::RemovePositionResult = match serde_json::from_value(data.clone()) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("mosaix: could not read the agent's result: {error}");
            std::process::exit(2);
        }
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&data).expect("JSON value serializes")
        );
    } else if result.is_applied() {
        println!("{}", format_remove_position(&result));
    } else {
        eprintln!("{}", format_remove_position(&result));
    }
    if !result.is_applied() {
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn run_resize(direction: ResizeDirection, json: bool) {
    use mosaix_ipc::{send_request, IpcRequest, IpcResponse};

    let request = match direction {
        ResizeDirection::Left => IpcRequest::ResizeLeft,
        ResizeDirection::Right => IpcRequest::ResizeRight,
        ResizeDirection::Up => IpcRequest::ResizeUp,
        ResizeDirection::Down => IpcRequest::ResizeDown,
    };
    let data = match send_request(request) {
        Ok(IpcResponse::Ok { data: Some(data) }) => data,
        Ok(IpcResponse::Ok { data: None }) => {
            eprintln!("mosaix: the agent answered without a resize result");
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
    let result: mosaix_domain::TreeResizeResult = match serde_json::from_value(data.clone()) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("mosaix: could not read the agent's resize result: {error}");
            std::process::exit(2);
        }
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&data).expect("JSON value serializes")
        );
    } else if result.is_applied() {
        println!("{}", format_resize(&result));
    } else {
        eprintln!("{}", format_resize(&result));
    }
    if !result.is_applied() {
        std::process::exit(1);
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
                    eprintln!("  targets {ordinals:?} all matched window {}", window_id.0);
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
fn match_candidates(outcome: &mosaix_domain::MatchOutcome) -> &[mosaix_domain::ScoredCandidate] {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_balanced_agent_reports_no_trees() {
        let state = serde_json::json!({
            "tiling_mode": "balanced",
            "automatic_tiling_active": true,
            "automatic_tiling_suspended": false,
            "container_trees": [],
        });

        assert_eq!(format_arrangement(&state), "arrangement: balanced (active)");
    }

    #[test]
    fn tree_mode_lists_each_displays_windows_in_visual_order() {
        let state = serde_json::json!({
            "tiling_mode": "tree",
            "automatic_tiling_active": true,
            "automatic_tiling_suspended": false,
            "container_trees": [
                { "display_id": 1, "windows": [11, 12, 13] },
                { "display_id": 2, "windows": [21] },
            ],
        });

        assert_eq!(
            format_arrangement(&state),
            "arrangement: tree (active)\n  display 1: 11 12 13\n  display 2: 21"
        );
    }

    #[test]
    fn tree_mode_names_the_windows_it_could_not_fit() {
        let state = serde_json::json!({
            "tiling_mode": "tree",
            "automatic_tiling_active": true,
            "automatic_tiling_suspended": false,
            "container_trees": [
                { "display_id": 1, "windows": [11, 12, 13], "constraint_overflow": [13] },
            ],
        });

        assert_eq!(
            format_arrangement(&state),
            "arrangement: tree (active)\n  display 1: 11 12 13\n    in constraint overflow (cannot fit at minimum size): 13"
        );
    }

    #[test]
    fn tree_mode_lists_dormant_positions_by_number_and_application() {
        let state = serde_json::json!({
            "tiling_mode": "tree",
            "automatic_tiling_active": true,
            "automatic_tiling_suspended": false,
            "container_trees": [
                {
                    "display_id": 1,
                    "windows": [11],
                    "constraint_overflow": [],
                    "dormant_positions": [
                        { "position": 1, "application": "Code.exe", "expires_unix": 1756604800 },
                    ],
                },
            ],
        });

        assert_eq!(
            format_arrangement(&state),
            "arrangement: tree (active)\n  display 1: 11\n    dormant position 1 kept for Code.exe (expires 1756604800)"
        );
    }

    #[test]
    fn a_swap_report_names_both_windows_or_the_reason_nothing_moved() {
        use mosaix_domain::{
            DirectionalSwapApplied, DirectionalSwapRefusal, DirectionalSwapResult, DisplayId,
            WindowId,
        };

        assert_eq!(
            format_swap(&DirectionalSwapResult::Applied(DirectionalSwapApplied {
                command: "swap-down".to_owned(),
                display_id: DisplayId(1),
                window_id: WindowId(2),
                neighbor_id: WindowId(3),
            })),
            "swap-down: swapped window 2 with window 3"
        );
        assert_eq!(
            format_swap(&DirectionalSwapResult::Refused(
                DirectionalSwapRefusal::NoNeighbor {
                    command: "swap-left".to_owned()
                }
            )),
            "mosaix: swap-left: no arranged window lies that way on this display"
        );
    }

    #[test]
    fn a_remove_position_report_names_the_slot_and_who_it_was_for() {
        use mosaix_domain::{
            DisplayId, RemovePositionApplied, RemovePositionRefusal, RemovePositionResult,
        };

        assert_eq!(
            format_remove_position(&RemovePositionResult::Applied(RemovePositionApplied {
                display_id: DisplayId(1),
                position: 4,
                application: "Code.exe".to_owned(),
            })),
            "removed dormant position 4 on display 1 (was kept for Code.exe)"
        );
        assert_eq!(
            format_remove_position(&RemovePositionResult::Refused(
                RemovePositionRefusal::UnknownPosition {
                    display_id: DisplayId(1),
                    position: 4,
                }
            )),
            "mosaix: display 1 has no dormant position 4"
        );
    }

    #[test]
    fn tree_mode_with_nothing_arranged_says_so_rather_than_printing_nothing() {
        let state = serde_json::json!({
            "tiling_mode": "tree",
            "automatic_tiling_active": true,
            "automatic_tiling_suspended": false,
            "container_trees": [],
        });

        assert_eq!(
            format_arrangement(&state),
            "arrangement: tree (active)\n  no display is arranging windows yet"
        );
    }

    #[test]
    fn a_suspended_arrangement_is_not_reported_as_off() {
        let state = serde_json::json!({
            "tiling_mode": "tree",
            "automatic_tiling_active": false,
            "automatic_tiling_suspended": true,
            "container_trees": [],
        });

        assert!(
            format_arrangement(&state).starts_with("arrangement: tree (suspended)"),
            "a suspended arrangement is still the configured one"
        );
    }

    #[test]
    fn a_resize_report_names_the_step_and_both_sides_of_the_divider() {
        use mosaix_domain::{DisplayId, TreeResizeApplied, TreeResizeResult, WindowId};

        let full = TreeResizeResult::Applied(TreeResizeApplied {
            command: "resize-left".to_owned(),
            display_id: DisplayId(1),
            window_id: WindowId(3),
            percentage_points: 5,
            grew: vec![WindowId(2), WindowId(3)],
            shrank: vec![WindowId(1)],
            weights_before: vec![0.5, 0.5],
            weights_after: vec![0.45, 0.55],
        });
        assert_eq!(
            format_resize(&full),
            "resize-left: moved the divider 5 percentage points\n  grew: 2 3\n  shrank: 1"
        );

        let clamped = TreeResizeResult::Applied(TreeResizeApplied {
            percentage_points: 1,
            ..match full {
                TreeResizeResult::Applied(applied) => applied,
                TreeResizeResult::Refused(_) => unreachable!(),
            }
        });
        assert!(format_resize(&clamped).contains("1 percentage point (clamped at a minimum size)"));
    }

    #[test]
    fn a_refused_resize_is_rendered_as_its_own_reason() {
        use mosaix_domain::{TreeResizeRefusal, TreeResizeResult};

        let refused = TreeResizeResult::Refused(TreeResizeRefusal::NoDivider {
            command: "resize-up".to_owned(),
        });
        assert_eq!(
            format_resize(&refused),
            "mosaix: resize-up: the window is at the edge of the arrangement on that side"
        );
    }

    #[test]
    fn a_snapshot_missing_its_fields_renders_without_panicking() {
        assert_eq!(
            format_arrangement(&serde_json::json!({})),
            "arrangement: unknown (off)"
        );
    }

    #[test]
    fn workspace_list_names_each_workspace_its_place_and_members() {
        let state = serde_json::json!({
            "workspaces": [
                { "name": "chat", "origin": "command", "displayed_on": null,
                  "members": [], "last_focused_window": null, "dormant_positions": 2 },
                { "name": "dev", "origin": "configuration", "displayed_on": 1,
                  "members": [11, 12], "last_focused_window": 12, "dormant_positions": 0 },
            ],
            "rule_workspace_refusals": [
                { "window_id": 13, "rule_id": "typo", "workspace": "dv" }
            ],
        });

        assert_eq!(
            format_workspaces(&state),
            "workspaces:\n  chat: hidden, from command, no windows, 2 dormant position(s)\n  dev: display 1, from configuration, windows 11 12\n  rule typo names unknown workspace \"dv\" for window 13"
        );
    }

    #[test]
    fn a_refused_workspace_command_renders_the_reason_and_its_code() {
        let result = mosaix_domain::WorkspaceCommandResult::Refused(
            mosaix_domain::WorkspaceRefusal::UnknownWorkspace {
                name: "typo".to_owned(),
            },
        );

        let rendered = format_workspace_result(&result);

        assert!(rendered.starts_with("mosaix: no workspace named \"typo\""));
        assert!(rendered.ends_with("(unknown_workspace)"));
    }

    #[test]
    fn a_workspace_focus_that_displayed_it_says_what_it_replaced() {
        let result = mosaix_domain::WorkspaceCommandResult::Focused(
            mosaix_domain::WorkspaceFocusApplied::Displayed {
                name: mosaix_domain::WorkspaceName::new("chat").unwrap(),
                display_id: mosaix_domain::DisplayId(1),
                replaced: Some(mosaix_domain::WorkspaceName::new("dev").unwrap()),
            },
        );

        assert_eq!(
            format_workspace_result(&result),
            "workspace chat is now displayed on display 1 (replacing dev, now hidden)"
        );
    }

    #[test]
    fn switching_status_names_the_reason_the_profile_and_the_mapping() {
        let state = serde_json::json!({
            "workspace_switching": {
                "status": "requested",
                "reason": "parking_capability_unverified",
                "profile_file": "office.toml",
                "displayed": { "MON-A": "dev", "MON-B": "chat" },
                "parking_capability": "unverified",
            }
        });

        assert_eq!(
            format_switching(&state),
            "workspace switching: requested by the matched profile; not yet active (parking_capability_unverified)\n  requested by: office.toml\n  MON-A -> dev\n  MON-B -> chat\n  parking site: unverified"
        );
    }

    #[test]
    fn switching_status_disabled_says_why_base_config_cannot_enable_it() {
        let state = serde_json::json!({
            "workspace_switching": {
                "status": "disabled",
                "reason": null,
                "profile_file": null,
                "displayed": {},
                "parking_capability": "unverified",
            }
        });

        assert!(format_switching(&state).starts_with("workspace switching: disabled"));
    }
}
