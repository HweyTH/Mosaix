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
    /// Snap the focused window to a half zone.
    ///
    /// Repeating the same horizontal snap cycles half, third, two-thirds.
    Snap {
        #[command(subcommand)]
        direction: SnapDirection,
    },
    /// Move focus to the nearest window that way, without moving anything.
    ///
    /// Does not wrap, and does not cross a display boundary when there is
    /// no neighbour that way.
    Focus {
        #[command(subcommand)]
        direction: FocusDirection,
    },
    /// Stop managing windows until resumed. Nothing is rearranged while
    /// paused, and nothing moves when it takes effect.
    Pause,
    /// Resume management after `pause`, rearranging to catch up.
    Resume,
    /// Pause if running, resume if paused.
    TogglePause,
    /// Recover the arrangement after windows have been moved by hand.
    ///
    /// Re-enumerates windows, resets open placement circuits once, and
    /// reflows. Changes no rule and no floating state.
    Rearrange,
    /// Suspend or resume automatic tiling for the current display
    /// topology. Manual snapping keeps working either way.
    ToggleAutomaticTiling,
    /// Float the focused window out of the tiling arrangement, or return
    /// it to the arrangement if it is already floating.
    ToggleFloating,
    /// Select a display for commands such as saved-layout apply.
    FocusDisplay {
        display_id: isize,
    },
    /// `focus left`, spelled as one word. Kept because bindings and
    /// scripts already use it.
    FocusLeft,
    /// `focus right`, spelled as one word.
    FocusRight,
    /// `focus up`, spelled as one word.
    FocusUp,
    /// `focus down`, spelled as one word.
    FocusDown,
    /// Exchange the focused window with its neighbor that way. Reports
    /// the typed outcome; a swap with no neighbor that way is refused
    /// rather than wrapping or crossing a display.
    SwapLeft {
        #[arg(long)]
        json: bool,
    },
    /// Exchange the focused window with its neighbour to the right.
    SwapRight {
        #[arg(long)]
        json: bool,
    },
    /// Exchange the focused window with its neighbour above.
    SwapUp {
        #[arg(long)]
        json: bool,
    },
    /// Exchange the focused window with its neighbour below.
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
    /// Refuses, and keeps the command available to retry, whenever a
    /// target window cannot be identified beyond doubt or your displays
    /// have changed. There is no way to force it, and there is no redo.
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
    /// Put back every window a Mosaix session parked and never restored,
    /// without the agent running.
    ///
    /// Reads the recovery ledger directly and touches only a handle that
    /// still verifiably names the window it recorded: same process
    /// instance, same window class. A stale, reused, or ambiguous handle
    /// is reported and left alone. Refuses while an agent is running,
    /// because the running agent owns the ledger; stop it first, or pass
    /// `--force` to proceed anyway.
    RestoreWindows {
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// Report everything a release decision rests on, in one place: tree
    /// mode, focused display, the workspace pool and where each workspace
    /// is displayed, dormant positions, constraint overflow, durability,
    /// undo availability, parking capability, and what recovery is
    /// waiting on a person (issue #63).
    ///
    /// `mosaix state --json` remains the complete machine-readable
    /// snapshot. This is the same facts, selected and ordered for someone
    /// deciding whether the experiment is behaving.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Return the focused window to where it was before Mosaix last
    /// placed it.
    ///
    /// One step back for one window, not the persistent undo history:
    /// `mosaix undo` reverses a whole transaction. Does nothing if the
    /// window has no remembered prior placement.
    RestorePlacement,
    /// Move the focused window to another display, keeping its position
    /// and size as a fraction of that display's work area.
    Throw {
        #[command(subcommand)]
        direction: ThrowDirection,
    },
    /// Report the agent's published state.
    State {
        #[arg(long)]
        json: bool,
    },
    /// Check that an agent is running and answering.
    Ping,
}

/// Displays are ordered left-to-right then top-to-bottom, and the ends
/// wrap, so `next` from the last display reaches the first.
#[derive(Debug, Subcommand)]
enum ThrowDirection {
    Next,
    Prev,
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
    /// Experimental: park one managed window through the public-API
    /// parking path. Recovery data is written to the ledger first, and the
    /// window leaves visible geometry only once that is durable. Refuses,
    /// and moves nothing, without a verified parking site.
    Park {
        /// The window, by the id `mosaix state` reports.
        window: isize,
        #[arg(long)]
        json: bool,
    },
    /// Put back every window this agent session parked, through the
    /// verified restore path. With no agent running, use
    /// `mosaix restore-windows` instead.
    Restore {
        #[arg(long)]
        json: bool,
    },
    /// Reconcile the windows a failed switch left unaccounted for. This
    /// is the only way out of the workspace-switch-degraded condition,
    /// and switching stays blocked until it succeeds.
    RestoreSwitch {
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
    Up,
    Down,
}

#[derive(Debug, Subcommand, Clone, Copy)]
enum ResizeDirection {
    Left,
    Right,
    Up,
    Down,
}

#[cfg(any(windows, target_os = "macos"))]
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
    if let Command::RestoreWindows { force, json } = cli.command {
        run_restore_windows(force, json);
        return;
    }
    // Arrangement reads fields the agent already publishes and renders
    // them, rather than asking for a report the agent does not have.
    if let Command::Arrangement { json } = cli.command {
        report_arrangement(json);
        return;
    }
    // So does status, which selects the same published fields a release
    // decision rests on.
    if let Command::Status { json } = cli.command {
        report_status(json);
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
        Command::Focus {
            direction: FocusDirection::Up,
        } => IpcRequest::FocusUp,
        Command::Focus {
            direction: FocusDirection::Down,
        } => IpcRequest::FocusDown,
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
        Command::RestorePlacement => IpcRequest::RestorePlacement,
        Command::Throw {
            direction: ThrowDirection::Next,
        } => IpcRequest::ThrowNext,
        Command::Throw {
            direction: ThrowDirection::Prev,
        } => IpcRequest::ThrowPrev,
        Command::State { .. } => IpcRequest::GetState,
        Command::Ping => IpcRequest::Ping,
        Command::Persistence { .. }
        | Command::RestoreWindows { .. }
        | Command::Undo { .. }
        | Command::Arrangement { .. }
        | Command::Resize { .. }
        | Command::Workspace { .. }
        | Command::RemovePosition { .. }
        | Command::SwapLeft { .. }
        | Command::SwapRight { .. }
        | Command::SwapUp { .. }
        | Command::SwapDown { .. }
        | Command::Status { .. } => {
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
    if let Some(condition) = format_conditions(state) {
        rendered.push_str(&format!(
            "
  {condition}"
        ));
    }

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
                        "\n in constraint overflow (cannot fit at minimum size): {}",
                        overflow.join(" ")
                    ));
                }
                if let Some(dormant) = tree["dormant_positions"].as_array() {
                    for slot in dormant {
                        rendered.push_str(&format!(
                            "\n dormant position {} kept for {} (expires {})",
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

#[cfg(any(windows, target_os = "macos"))]
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

/// Reports everything a release decision rests on (issue #63).
///
/// The JSON form is a selection of published fields under their published
/// names, not a reshaping of them: a caller that wants one of these facts
/// finds it spelled the same way in `mosaix state --json`. What this adds
/// is that the selection itself is the contract -- the ten things spec #45
/// requires status to expose -- so a client is not left to discover which
/// of the snapshot's fifty fields those are.
#[cfg(any(windows, target_os = "macos"))]
fn report_status(json: bool) {
    let Some(state) = published_persistence() else {
        eprintln!("mosaix: no agent is running");
        std::process::exit(1);
    };

    if json {
        let value = serde_json::json!({
            "mode": state["mode"],
            "conditions": state["conditions"],
            "primary_condition": state["primary_condition"],
            "tiling_mode": state["tiling_mode"],
            "display_count": state["display_count"],
            "focused_display": state["focused_display"],
            "container_trees": state["container_trees"],
            "constraint_overflow": state["constraint_overflow"],
            "workspaces": state["workspaces"],
            "workspace_switching": state["workspace_switching"],
            "workspace_switch": state["workspace_switch"],
            "parking_capability": state["parking_capability"],
            "parking_capability_reason": state["parking_capability_reason"],
            "revision": state["revision"],
            "persistence_status": state["persistence_status"],
            "persistence_reason": state["persistence_reason"],
            "last_durable_revision": state["last_durable_revision"],
            "undo_available": state["undo_available"],
            "undo_command": state["undo_command"],
            "undo_blocked_reason": state["undo_blocked_reason"],
            "recovery": state["recovery"],
            "recovery_required": state["recovery_required"],
            "recovery_actions": state["recovery_actions"],
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).expect("JSON value serializes")
        );
        return;
    }
    println!("{}", format_status(&state));
}

/// Reports whether undo would work right now, and why not if it would not.
///
/// Reads the agent's published state rather than asking undo to preflight,
/// because the agent already computed the same verdict there -- and because
/// a dry run must not be able to move a window by accident.
#[cfg(any(windows, target_os = "macos"))]
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

#[cfg(any(windows, target_os = "macos"))]
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
        WorkspaceCommandResult::Focused(WorkspaceFocusApplied::SwitchStarted {
            name,
            display_id,
            replaced,
            parking,
            restoring,
        }) => match replaced {
            Some(replaced) => format!(
                "switching display {} from {replaced} to {name}: parking {parking} window(s), restoring {restoring}",
                display_id.0
            ),
            None => format!(
                "switching display {} to {name}: parking {parking} window(s), restoring {restoring}",
                display_id.0
            ),
        },
        WorkspaceCommandResult::SwitchFailed(failed) if failed.compensated => format!(
            "mosaix: the switch to {} was cancelled and every moved window is back ({})",
            failed.name, failed.reason
        ),
        WorkspaceCommandResult::SwitchFailed(failed) => format!(
            "mosaix: the switch to {} failed and {} window(s) could not be put back ({}); workspace switching is blocked until `mosaix workspace restore-switch`",
            failed.name,
            failed.stranded_windows.len(),
            failed.reason
        ),
        WorkspaceCommandResult::Refused(refusal) => {
            format!("mosaix: {refusal} ({})", refusal.code())
        }
    }
}

/// The health condition a person should be told about first, if any.
///
/// Published state orders the conditions once and every client reads that
/// order rather than inventing one, so what the CLI leads with and what
/// any other interface leads with are the same fact.
fn format_conditions(state: &serde_json::Value) -> Option<String> {
    let conditions: Vec<&str> = state["conditions"]
        .as_array()
        .map(|conditions| {
            conditions
                .iter()
                .filter_map(|condition| condition.as_str())
                .collect()
        })
        .unwrap_or_default();
    let leading = *conditions.first()?;
    let described = match leading {
        "workspace_switch_degraded" => {
            "workspace-switch degraded: a failed switch left windows unaccounted for; switching is blocked until `mosaix workspace restore-switch`"
        }
        "persistence_degraded" => {
            "persistence degraded: live management continues, but nothing new is durable; see `mosaix persistence status`"
        }
        "degraded_tiling" => {
            "degraded tiling: automatic tiling continues, with some windows excluded by a placement failure"
        }
        other => return Some(format!("{other} (and {} more)", conditions.len() - 1)),
    };
    let rest = conditions.len() - 1;
    Some(match rest {
        0 => described.to_owned(),
        1 => format!(
            "{described}
  (1 other condition also holds)"
        ),
        more => format!(
            "{described}
  ({more} other conditions also hold)"
        ),
    })
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
    // A degraded switch outranks everything above it: switching is
    // blocked regardless of what the profile asks for, and the line that
    // says so has to be impossible to miss.
    let switch = &state["workspace_switch"];
    if let Some(degraded) = switch["degraded"].as_object() {
        let stranded = degraded["stranded_windows"]
            .as_array()
            .map(|windows| {
                windows
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        lines.insert(
            0,
            format!(
                "workspace switching: blocked; a failed switch left window(s) {stranded} unaccounted for ({}) -- run `mosaix workspace restore-switch`",
                degraded["reason"].as_str().unwrap_or("no reason")
            ),
        );
    }
    if let Some(in_flight) = switch["in_flight"].as_object() {
        lines.push(format!(
            "  switch in flight: display {} to {} ({})",
            in_flight["display_id"],
            in_flight["target"].as_str().unwrap_or("?"),
            in_flight["phase"].as_str().unwrap_or("?")
        ));
    }
    let recovery = &state["recovery"];
    if let Some(parked) = recovery["parked_windows"].as_array() {
        if !parked.is_empty() {
            lines.push(format!(
                "  parked windows: {}",
                parked
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
    }
    if let Some(failure) = recovery["last_parking_failure"].as_object() {
        lines.push(format!(
            "  last parking failure: {} of window {} failed: {}",
            failure["stage"].as_str().unwrap_or("?"),
            failure["window_id"],
            failure["reason"].as_str().unwrap_or("?")
        ));
    }
    lines.join("\n")
}

/// Renders the recovery section: what will not fix itself, and the
/// command that fixes it.
///
/// Distinct from `format_conditions`, which names what is wrong. A
/// condition can hold with nothing asked of the user, and a repair can be
/// outstanding with no condition holding -- windows a *previous* session
/// left parked are nobody's condition until someone puts them back.
fn format_recovery_actions(state: &serde_json::Value) -> String {
    let actions = state["recovery_actions"].as_array();
    let Some(actions) = actions.filter(|actions| !actions.is_empty()) else {
        return "recovery: nothing is waiting on you".to_owned();
    };
    let mut lines = vec![format!(
        "recovery: {} repair(s) waiting on you",
        actions.len()
    )];
    for action in actions {
        let reason = action["reason"].as_str().unwrap_or("?");
        let command = action["command"].as_str().unwrap_or("?");
        let described = match reason {
            "switch_degraded" => {
                "a failed switch left windows unaccounted for; switching is blocked until they are reconciled"
            }
            "parking_restore_failed" => {
                "a window could not be put back and is still parked off screen"
            }
            "startup_recovery_incomplete" => {
                "a previous session left windows parked that startup could not verify and put back"
            }
            "persistence_degraded" => {
                "nothing new is becoming durable, so parking and new undo entries are unavailable"
            }
            other => other,
        };
        lines.push(format!("  {described}"));
        let windows: Vec<String> = action["windows"]
            .as_array()
            .map(|windows| {
                windows
                    .iter()
                    .filter_map(|id| id.as_i64())
                    .map(|id| id.to_string())
                    .collect()
            })
            .unwrap_or_default();
        if !windows.is_empty() {
            lines.push(format!("    windows: {}", windows.join(" ")));
        }
        lines.push(format!("    run: {command}"));
    }
    lines.join("\n")
}

/// Renders the whole status report (issue #63).
///
/// A pure function of the snapshot, like every other formatter here, so
/// the wording is testable without an agent to talk to.
fn format_status(state: &serde_json::Value) -> String {
    let mut sections = Vec::new();

    let mode = state["mode"].as_str().unwrap_or("unknown");
    let mut agent = format!("agent: {mode}");
    if let Some(condition) = format_conditions(state) {
        agent.push_str(&format!("\n  {condition}"));
    }
    sections.push(agent);

    let tiling_mode = state["tiling_mode"].as_str().unwrap_or("unknown");
    let focused = match state["focused_display"].as_i64() {
        Some(display) => format!("display {display}"),
        None => "none".to_owned(),
    };
    sections.push(format!(
        "tiling: {tiling_mode} mode, {} display(s), focused display {focused}",
        state["display_count"].as_u64().unwrap_or(0)
    ));

    // Overflow is rolled up here rather than walked per tree: the question
    // this report answers is whether anything is overflowing at all.
    let overflow: Vec<String> = state["constraint_overflow"]
        .as_array()
        .map(|windows| {
            windows
                .iter()
                .filter_map(|id| id.as_i64())
                .map(|id| id.to_string())
                .collect()
        })
        .unwrap_or_default();
    sections.push(if overflow.is_empty() {
        "constraint overflow: none".to_owned()
    } else {
        format!(
            "constraint overflow: window(s) {} are visible and floating because the tree cannot fit them at minimum size",
            overflow.join(" ")
        )
    });

    let dormant: u64 = state["container_trees"]
        .as_array()
        .map(|trees| {
            trees
                .iter()
                .map(|tree| {
                    tree["dormant_positions"]
                        .as_array()
                        .map(|slots| slots.len() as u64)
                        .unwrap_or(0)
                })
                .sum()
        })
        .unwrap_or(0)
        + state["workspaces"]
            .as_array()
            .map(|workspaces| {
                workspaces
                    .iter()
                    .map(|workspace| workspace["dormant_positions"].as_u64().unwrap_or(0))
                    .sum::<u64>()
            })
            .unwrap_or(0);
    sections.push(format!("dormant positions: {dormant}"));

    sections.push(format_workspaces(state));
    sections.push(format_switching(state));

    // Parking capability is read from the top level rather than from the
    // switching section, because the switching section is absent exactly
    // when no profile requests switching -- and the capability still
    // governs every other parking path.
    let capability = state["parking_capability"].as_str().unwrap_or("unknown");
    let mut parking = format!(
        "parking capability: {capability} (emulated via public APIs; not native virtual desktops or Spaces)"
    );
    if let Some(reason) = state["parking_capability_reason"].as_str() {
        parking.push_str(&format!("\n  refused: {reason}"));
    }
    sections.push(parking);

    let durability = state["persistence_status"].as_str().unwrap_or("unknown");
    let mut durable = format!(
        "durability: {durability}, revision {} durable of {}",
        state["last_durable_revision"].as_u64().unwrap_or(0),
        state["revision"].as_u64().unwrap_or(0)
    );
    if let Some(reason) = state["persistence_reason"].as_str() {
        durable.push_str(&format!("\n  reason: {reason}"));
    }
    sections.push(durable);

    sections.push(
        match (
            state["undo_available"].as_bool().unwrap_or(false),
            state["undo_command"].as_str(),
            state["undo_blocked_reason"].as_str(),
        ) {
            (true, Some(command), _) => format!("undo: available, would reverse {command}"),
            (true, None, _) => "undo: available".to_owned(),
            (false, Some(command), Some(reason)) => {
                format!("undo: unavailable ({reason}); {command} is retained to retry")
            }
            (false, Some(command), None) => format!("undo: unavailable; next would be {command}"),
            (false, None, _) => "undo: nothing to undo".to_owned(),
        },
    );

    sections.push(format_recovery_actions(state));
    sections.join("\n")
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

#[cfg(any(windows, target_os = "macos"))]
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
        WorkspaceAction::Park { window, json } => {
            run_park_window(window, json);
            return;
        }
        WorkspaceAction::Restore { json } => {
            run_restore_parked(json);
            return;
        }
        WorkspaceAction::RestoreSwitch { json } => {
            run_restore_switch(json);
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

#[cfg(any(windows, target_os = "macos"))]
fn run_park_window(window: isize, json: bool) {
    use mosaix_ipc::{send_request, IpcRequest, IpcResponse};

    let data = match send_request(IpcRequest::ParkWindow { window_id: window }) {
        Ok(IpcResponse::Ok { data: Some(data) }) => data,
        Ok(IpcResponse::Ok { data: None }) => {
            eprintln!("mosaix: the agent answered without a parking result");
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
    let result: mosaix_domain::ParkWindowResult = match serde_json::from_value(data.clone()) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("mosaix: could not read the agent's parking result: {error}");
            std::process::exit(2);
        }
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&data).expect("JSON value serializes")
        );
    } else {
        match &result {
            mosaix_domain::ParkWindowResult::Requested { window_id } => println!(
                "parking requested for window {}; it leaves the screen once its recovery data is durable",
                window_id.0
            ),
            mosaix_domain::ParkWindowResult::Refused(refusal) => eprintln!("mosaix: {refusal}"),
        }
    }
    if !result.is_applied() {
        std::process::exit(1);
    }
}

/// Sends one request and returns the JSON the agent answered with.
///
/// The absent agent, the protocol disagreement and the answer that
/// carried no data all end the process the same way wherever they
/// happen, so every command that wants a payload asks through here
/// rather than restating twenty lines of matching.
#[cfg(any(windows, target_os = "macos"))]
fn agent_answer(request: mosaix_ipc::IpcRequest) -> serde_json::Value {
    use mosaix_ipc::{send_request, IpcResponse};

    match send_request(request) {
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
    }
}

/// Asks the agent to reconcile a degraded switch, and reports what it
/// answered.
///
/// Exits non-zero when nothing was reconciled, so a script can tell "the
/// condition is cleared" from "there was nothing to clear" without
/// parsing prose.
#[cfg(any(windows, target_os = "macos"))]
fn run_restore_switch(json: bool) {
    let data = agent_answer(mosaix_ipc::IpcRequest::RestoreWorkspaceSwitch);
    let result: mosaix_domain::WorkspaceSwitchRestoreResult =
        match serde_json::from_value(data.clone()) {
            Ok(result) => result,
            Err(error) => {
                eprintln!("mosaix: could not read the agent's restore result: {error}");
                std::process::exit(2);
            }
        };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&data).expect("JSON value serializes")
        );
    } else {
        match &result {
            mosaix_domain::WorkspaceSwitchRestoreResult::NotDegraded => {
                eprintln!("mosaix: no degraded workspace switch to reconcile");
            }
            mosaix_domain::WorkspaceSwitchRestoreResult::Reconciled { .. } => {
                println!("workspace switching is unblocked; nothing was left parked");
            }
            mosaix_domain::WorkspaceSwitchRestoreResult::Requested { windows } => {
                println!(
                    "restore requested for {} stranded window(s): {}",
                    windows.len(),
                    windows
                        .iter()
                        .map(|window| window.0.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
    }
    if matches!(
        result,
        mosaix_domain::WorkspaceSwitchRestoreResult::NotDegraded
    ) {
        std::process::exit(1);
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn run_restore_parked(json: bool) {
    let data = agent_answer(mosaix_ipc::IpcRequest::RestoreParkedWindows);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&data).expect("JSON value serializes")
        );
        return;
    }
    let parked: Vec<isize> = data["parked_windows"]
        .as_array()
        .map(|windows| {
            windows
                .iter()
                .filter_map(|w| w.as_i64())
                .map(|w| w as isize)
                .collect()
        })
        .unwrap_or_default();
    if parked.is_empty() {
        println!("no parked windows to restore");
    } else {
        println!(
            "restore requested for {} parked window(s): {}",
            parked.len(),
            parked
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        );
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

#[cfg(any(windows, target_os = "macos"))]
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

#[cfg(any(windows, target_os = "macos"))]
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

#[cfg(any(windows, target_os = "macos"))]
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
                    eprintln!("  now: {current_fingerprint}");
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
                                " candidate window {} scored {}",
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
                UndoRefusal::WorkspaceSwitchRefused { reason, .. } => {
                    eprintln!("  reason: {reason}");
                    eprintln!("  see `mosaix workspace switching`");
                }
                UndoRefusal::NothingToUndo => {}
            }
            std::process::exit(1);
        }
    }
}

/// The scored candidates a refusal has to show, whichever shape the
/// outcome took.
#[cfg(any(windows, target_os = "macos"))]
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
#[cfg(any(windows, target_os = "macos"))]
fn published_persistence() -> Option<serde_json::Value> {
    use mosaix_ipc::{send_request, IpcRequest, IpcResponse};

    match send_request(IpcRequest::GetState) {
        Ok(IpcResponse::Ok { data: Some(data) }) => Some(data),
        _ => None,
    }
}

/// Renders recovery outcomes for a person, one line per entry.
fn format_recovery(outcomes: &[mosaix_domain::RecoveryOutcome]) -> String {
    if outcomes.is_empty() {
        return "no parked windows to restore".to_owned();
    }
    let mut lines = Vec::with_capacity(outcomes.len() + 1);
    let restored = outcomes.iter().filter(|outcome| outcome.restored).count();
    lines.push(format!(
        "restored {restored} of {} parked window(s)",
        outcomes.len()
    ));
    for outcome in outcomes {
        let what = match (&outcome.verdict, outcome.restored, &outcome.failure) {
            (mosaix_domain::HandleVerdict::Verified, true, _) => "restored".to_owned(),
            (mosaix_domain::HandleVerdict::Verified, false, Some(failure)) => {
                format!("verified but not restored: {failure}")
            }
            (mosaix_domain::HandleVerdict::Verified, false, None) => {
                "verified but not restored".to_owned()
            }
            (mosaix_domain::HandleVerdict::Stale, ..) => {
                "left alone: the handle no longer names a window".to_owned()
            }
            (mosaix_domain::HandleVerdict::Reused { .. }, ..) => {
                "left alone: the handle now names a different window".to_owned()
            }
            (mosaix_domain::HandleVerdict::Ambiguous { claimants }, ..) => {
                format!("left alone: {claimants} entries claim this handle")
            }
        };
        lines.push(format!(
            "  entry {} window {} ({}): {what}",
            outcome.entry_id.0, outcome.native_handle, outcome.application_id.0
        ));
    }
    lines.join("\n")
}

#[cfg(any(windows, target_os = "macos"))]
fn run_restore_windows(force: bool, json: bool) {
    use mosaix_ipc::{send_request, IpcRequest, IpcResponse};

    if !force {
        if let Ok(IpcResponse::Ok { .. }) = send_request(IpcRequest::Ping) {
            eprintln!(
                "mosaix: an agent is running and owns the recovery ledger; \
                 stop it first, or pass --force to restore anyway"
            );
            std::process::exit(1);
        }
    }
    let Some(path) = mosaix_persistence::default_ledger_path() else {
        eprintln!("mosaix: this platform has no recovery ledger location");
        std::process::exit(2);
    };
    let mut ledger = match mosaix_persistence::RecoveryLedger::open(&path) {
        Ok(ledger) => ledger,
        Err(error) => {
            eprintln!("mosaix: could not open the recovery ledger: {error}");
            std::process::exit(2);
        }
    };
    // The two closures are the whole platform surface of out-of-process
    // recovery: what a recorded handle names now, and how a verified
    // window is put back. Each adapter answers both through public API.
    #[cfg(target_os = "macos")]
    use mosaix_platform_macos as platform;
    #[cfg(windows)]
    use mosaix_platform_windows as platform;

    let outcomes = match mosaix_persistence::recover_parked_windows(
        &mut ledger,
        |handle| platform::probe_handle(platform::WindowHandle(handle)),
        |entry| {
            platform::restore_window(platform::WindowHandle(entry.draft.native_handle), entry)
                .map_err(|error| error.to_string())
        },
    ) {
        Ok(outcomes) => outcomes,
        Err(error) => {
            eprintln!("mosaix: could not read the recovery ledger: {error}");
            std::process::exit(2);
        }
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&outcomes).expect("outcomes serialize")
        );
    } else {
        println!("{}", format_recovery(&outcomes));
    }
    if outcomes.iter().any(|outcome| {
        outcome.verdict == mosaix_domain::HandleVerdict::Verified && !outcome.restored
    }) {
        std::process::exit(1);
    }
}

#[cfg(any(windows, target_os = "macos"))]
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

#[cfg(not(any(windows, target_os = "macos")))]
fn main() {
    let _ = Cli::parse();
    eprintln!("mosaix currently only supports Windows and macOS");
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
            "arrangement: tree (active)\n  display 1: 11 12 13\n in constraint overflow (cannot fit at minimum size): 13"
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
            "arrangement: tree (active)\n  display 1: 11\n dormant position 1 kept for Code.exe (expires 1756604800)"
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
    fn the_leading_health_condition_is_the_one_published_state_ordered_first() {
        let state = serde_json::json!({
            "tiling_mode": "tree",
            "automatic_tiling_active": true,
            "automatic_tiling_suspended": false,
            "conditions": ["workspace_switch_degraded", "persistence_degraded"],
            "primary_condition": "workspace_switch_degraded",
        });

        let rendered = format_arrangement(&state);

        assert!(rendered.contains("workspace-switch degraded"), "{rendered}");
        assert!(
            rendered.contains("mosaix workspace restore-switch"),
            "{rendered}"
        );
        assert!(
            rendered.contains("(1 other condition also holds)"),
            "the other conditions are not hidden, only ranked: {rendered}"
        );
    }

    #[test]
    fn a_healthy_arrangement_report_mentions_no_condition() {
        let state = serde_json::json!({
            "tiling_mode": "tree",
            "automatic_tiling_active": true,
            "automatic_tiling_suspended": false,
            "conditions": [],
            "primary_condition": null,
        });

        assert_eq!(
            format_arrangement(&state),
            "arrangement: tree (active)
  no display is arranging windows yet"
        );
        assert_eq!(format_conditions(&state), None);
    }

    #[test]
    fn degraded_tiling_leads_only_when_nothing_more_serious_holds() {
        let state = serde_json::json!({ "conditions": ["degraded_tiling"] });

        let rendered = format_conditions(&state).expect("a condition holds");

        assert!(rendered.starts_with("degraded tiling:"), "{rendered}");
        assert!(!rendered.contains("also hold"), "{rendered}");
    }

    #[test]
    fn a_degraded_switch_leads_the_switching_report_and_names_the_way_out() {
        // Whatever the profile asks for, switching is blocked while a
        // failed compensation stands, so that is the first line.
        let state = serde_json::json!({
            "workspace_switching": {
                "status": "experimental",
                "reason": null,
                "profile_file": null,
                "displayed": {},
                "parking_capability": "verified",
            },
            "workspace_switch": {
                "in_flight": null,
                "degraded": {
                    "display_id": 1,
                    "target": "chat",
                    "outgoing": "dev",
                    "stranded_windows": [41, 42],
                    "reason": "the window would not come back",
                },
            },
            "recovery": {},
        });

        let rendered = format_switching(&state);

        assert!(
            rendered.starts_with("workspace switching: blocked;"),
            "{rendered}"
        );
        assert!(rendered.contains("41 42"), "{rendered}");
        assert!(
            rendered.contains("mosaix workspace restore-switch"),
            "{rendered}"
        );
    }

    #[test]
    fn a_switch_in_flight_is_reported_with_the_phase_it_is_in() {
        let state = serde_json::json!({
            "workspace_switching": {
                "status": "experimental",
                "reason": null,
                "profile_file": null,
                "displayed": {},
                "parking_capability": "verified",
            },
            "workspace_switch": {
                "in_flight": {
                    "display_id": 1,
                    "target": "chat",
                    "outgoing": "dev",
                    "phase": "parking",
                    "parked_windows": [],
                    "restored_windows": [],
                },
                "degraded": null,
            },
            "recovery": {},
        });

        assert!(
            format_switching(&state).contains("switch in flight: display 1 to chat (parking)"),
            "{}",
            format_switching(&state)
        );
    }

    #[test]
    fn a_switch_that_could_not_be_compensated_tells_the_user_what_to_run() {
        let result = mosaix_domain::WorkspaceCommandResult::SwitchFailed(
            mosaix_domain::WorkspaceSwitchFailed {
                name: mosaix_domain::WorkspaceName::new("chat").unwrap(),
                display_id: mosaix_domain::DisplayId(1),
                reason: "the window would not come back".to_owned(),
                compensated: false,
                stranded_windows: vec![mosaix_domain::WindowId(41)],
            },
        );

        let rendered = format_workspace_result(&result);

        assert!(
            rendered.contains("1 window(s) could not be put back"),
            "{rendered}"
        );
        assert!(rendered.contains("restore-switch"), "{rendered}");
    }

    #[test]
    fn a_compensated_switch_says_every_window_is_back() {
        let result = mosaix_domain::WorkspaceCommandResult::SwitchFailed(
            mosaix_domain::WorkspaceSwitchFailed {
                name: mosaix_domain::WorkspaceName::new("chat").unwrap(),
                display_id: mosaix_domain::DisplayId(1),
                reason: "the window refused to move".to_owned(),
                compensated: true,
                stranded_windows: Vec::new(),
            },
        );

        assert_eq!(
            format_workspace_result(&result),
            "mosaix: the switch to chat was cancelled and every moved window is back \
             (the window refused to move)"
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

    #[test]
    fn restore_windows_renders_each_verdict_and_the_count_restored() {
        let outcome = |id: i64, verdict: mosaix_domain::HandleVerdict, restored: bool| {
            mosaix_domain::RecoveryOutcome {
                entry_id: mosaix_domain::RecoveryEntryId(id),
                native_handle: 100 + id as isize,
                application_id: mosaix_domain::ApplicationId("code.exe".to_owned()),
                verdict,
                restored,
                failure: None,
            }
        };
        let outcomes = vec![
            outcome(1, mosaix_domain::HandleVerdict::Verified, true),
            outcome(2, mosaix_domain::HandleVerdict::Stale, false),
            outcome(
                3,
                mosaix_domain::HandleVerdict::Ambiguous { claimants: 2 },
                false,
            ),
        ];

        assert_eq!(
            format_recovery(&outcomes),
            "restored 1 of 3 parked window(s)\n  entry 1 window 101 (code.exe): restored\n  entry 2 window 102 (code.exe): left alone: the handle no longer names a window\n  entry 3 window 103 (code.exe): left alone: 2 entries claim this handle"
        );
        assert_eq!(format_recovery(&[]), "no parked windows to restore");
    }

    /// A snapshot of a healthy agent, in the shape `mosaix state --json`
    /// publishes it. Tests override only the fields they are about.
    fn healthy_status() -> serde_json::Value {
        serde_json::json!({
            "mode": "active",
            "conditions": [],
            "primary_condition": null,
            "revision": 42,
            "tiling_mode": "tree",
            "display_count": 2,
            "focused_display": 1,
            "container_trees": [],
            "constraint_overflow": [],
            "workspaces": [],
            "rule_workspace_refusals": [],
            "workspace_switching": {
                "status": "disabled",
                "reason": null,
                "profile_file": null,
                "displayed": {},
                "parking_capability": "verified",
            },
            "workspace_switch": {},
            "recovery": {},
            "parking_capability": "verified",
            "parking_capability_reason": null,
            "persistence_status": "healthy",
            "persistence_reason": null,
            "last_durable_revision": 42,
            "undo_available": false,
            "undo_command": null,
            "undo_blocked_reason": null,
            "recovery_required": false,
            "recovery_actions": [],
        })
    }

    #[test]
    fn status_reports_every_fact_a_release_decision_rests_on() {
        // Spec #45 names ten: tree mode, focused display, workspace pool,
        // displayed assignments, dormant positions, overflow windows,
        // durability revision, undo availability, parking capability, and
        // recovery-required status. This asserts each one is rendered.
        let mut state = healthy_status();
        state["workspaces"] = serde_json::json!([
            {
                "name": "dev",
                "origin": "configuration",
                "displayed_on": 1,
                "members": [11, 12],
                "last_focused_window": 11,
                "dormant_positions": 2,
            },
            {
                "name": "chat",
                "origin": "command",
                "displayed_on": null,
                "members": [],
                "last_focused_window": null,
                "dormant_positions": 0,
            },
        ]);
        state["constraint_overflow"] = serde_json::json!([12]);
        state["undo_available"] = serde_json::json!(true);
        state["undo_command"] = serde_json::json!("swap-left");

        let rendered = format_status(&state);

        assert!(rendered.contains("tiling: tree mode"), "{rendered}");
        assert!(rendered.contains("focused display display 1"), "{rendered}");
        assert!(rendered.contains("dev: display 1"), "{rendered}");
        assert!(rendered.contains("chat: hidden"), "{rendered}");
        assert!(rendered.contains("dormant positions: 2"), "{rendered}");
        assert!(
            rendered.contains("constraint overflow: window(s) 12"),
            "{rendered}"
        );
        assert!(rendered.contains("revision 42 durable of 42"), "{rendered}");
        assert!(
            rendered.contains("undo: available, would reverse swap-left"),
            "{rendered}"
        );
        assert!(
            rendered.contains("parking capability: verified"),
            "{rendered}"
        );
        assert!(
            rendered.contains("recovery: nothing is waiting on you"),
            "{rendered}"
        );
    }

    #[test]
    fn status_never_calls_parking_a_native_virtual_desktop() {
        // User story 97: the feature is described honestly wherever it is
        // described at all.
        let rendered = format_status(&healthy_status());

        assert!(
            rendered.contains("emulated via public APIs; not native virtual desktops or Spaces"),
            "{rendered}"
        );
    }

    #[test]
    fn a_refused_parking_site_is_reported_with_the_adapters_reason() {
        let mut state = healthy_status();
        state["parking_capability"] = serde_json::json!("refused");
        state["parking_capability_reason"] =
            serde_json::json!("no recoverable site beyond the virtual screen");

        let rendered = format_status(&state);

        assert!(
            rendered.contains("refused: no recoverable site beyond the virtual screen"),
            "{rendered}"
        );
    }

    #[test]
    fn status_names_each_repair_and_the_command_that_performs_it() {
        let mut state = healthy_status();
        state["recovery_required"] = serde_json::json!(true);
        state["recovery_actions"] = serde_json::json!([
            {
                "reason": "switch_degraded",
                "windows": [41, 42],
                "command": "mosaix workspace restore-switch",
            },
            {
                "reason": "startup_recovery_incomplete",
                "windows": [9],
                "command": "mosaix restore-windows",
            },
        ]);

        let rendered = format_recovery_actions(&state);

        assert!(
            rendered.starts_with("recovery: 2 repair(s) waiting on you"),
            "{rendered}"
        );
        assert!(rendered.contains("windows: 41 42"), "{rendered}");
        assert!(
            rendered.contains("run: mosaix workspace restore-switch"),
            "{rendered}"
        );
        assert!(
            rendered.contains("run: mosaix restore-windows"),
            "{rendered}"
        );
    }

    #[test]
    fn a_blocked_undo_says_the_transaction_is_kept_to_retry() {
        let mut state = healthy_status();
        state["undo_command"] = serde_json::json!("swap-left");
        state["undo_blocked_reason"] = serde_json::json!("topology_changed");

        let rendered = format_status(&state);

        assert!(
            rendered
                .contains("undo: unavailable (topology_changed); swap-left is retained to retry"),
            "{rendered}"
        );
    }

    #[test]
    fn status_leads_with_the_condition_every_other_client_leads_with() {
        let mut state = healthy_status();
        state["mode"] = serde_json::json!("degraded");
        state["conditions"] = serde_json::json!(["persistence_degraded", "degraded_tiling"]);
        state["primary_condition"] = serde_json::json!("persistence_degraded");

        let rendered = format_status(&state);

        let first = rendered.lines().next().expect("a first line");
        assert_eq!(first, "agent: degraded");
        assert!(
            rendered
                .lines()
                .nth(1)
                .expect("a condition line")
                .contains("persistence degraded:"),
            "{rendered}"
        );
    }

    #[test]
    fn status_renders_the_shape_a_healthy_agent_actually_sends() {
        // The optional sections serialize as `null`, not as `{}`, when
        // nothing is switching and nothing is parked. The fixture above
        // uses `{}`, so this pins the real shape rather than a convenient
        // one.
        let mut state = healthy_status();
        state["workspace_switch"] = serde_json::Value::Null;
        state["recovery"] = serde_json::Value::Null;
        state["workspace_switching"] = serde_json::Value::Null;

        let rendered = format_status(&state);

        assert!(
            rendered.contains("recovery: nothing is waiting on you"),
            "{rendered}"
        );
        assert!(
            rendered.contains("parking capability: verified"),
            "{rendered}"
        );
    }
}
