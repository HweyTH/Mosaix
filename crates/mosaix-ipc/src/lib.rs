//! IPC protocol definitions and local transport implementations.

#[cfg(windows)]
pub mod client;
#[cfg(target_os = "macos")]
pub mod client_unix;
pub mod handler;
#[cfg(windows)]
pub mod pipe;
pub mod protocol;
#[cfg(target_os = "macos")]
pub mod unix_socket;

#[cfg(windows)]
pub use client::{send_request, IpcConnection, IpcError};
#[cfg(target_os = "macos")]
pub use client_unix::{send_request, IpcError};
pub use handler::{
    handle_request, CaptureHold, ConfigError, ConfigStore, ContainerTreeSnapshot,
    DormantPositionSnapshot, HotkeyBindingSnapshot, HotkeyProbe, HotkeyVerdict,
    LayoutSourceSnapshot, ProbeOutcome, StateSnapshot, UnavailableConfigStore,
    UnavailableHotkeyProbe,
};
#[cfg(windows)]
pub use pipe::{pipe_name, IpcServer, PIPE_NAME_PREFIX};
pub use protocol::*;
#[cfg(target_os = "macos")]
pub use unix_socket::{socket_path, IpcServer};
