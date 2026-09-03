//! IPC protocol definitions and local transport implementations.

#[cfg(windows)]
pub mod client;
pub mod handler;
#[cfg(windows)]
pub mod pipe;
pub mod protocol;

#[cfg(windows)]
pub use client::{send_request, IpcConnection, IpcError};
pub use handler::{
    handle_request, CaptureHold, ConfigError, ConfigStore, HotkeyBindingSnapshot, StateSnapshot,
    UnavailableConfigStore,
};
#[cfg(windows)]
pub use pipe::{pipe_name, IpcServer, PIPE_NAME_PREFIX};
pub use protocol::*;
