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
    State {
        #[arg(long)]
        json: bool,
    },
    Ping,
}

#[derive(Debug, Subcommand)]
enum LayoutAction {
    /// Apply a saved layout to the focused window's display. Fails with
    /// the agent's own reason if no managed window is focused or no layout
    /// carries that name.
    Apply {
        /// The layout's name, as declared under `[layouts]` in config.
        name: String,
    },
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

#[cfg(not(windows))]
fn main() {
    let _ = Cli::parse();
    eprintln!("mosaix currently only supports Windows");
    std::process::exit(2);
}
