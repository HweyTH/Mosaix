//! Synchronous client for the agent's local named pipe.
//!
//! Two shapes of client share one implementation. [`send_request`] is the
//! one-shot the CLI uses: connect, ask, hang up. [`IpcConnection`] is the
//! held connection the settings application uses instead, kept open for
//! its window's lifetime -- required because hotkey-capture suspension is
//! bounded by that connection's lifetime, so the agent recovers when the
//! application dies and the OS closes the pipe handle.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::{FromRawHandle, RawHandle};

use thiserror::Error;
use windows::core::PCWSTR;
use windows::core::HRESULT;
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, GENERIC_READ, GENERIC_WRITE, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_MODE, OPEN_EXISTING,
};

use crate::pipe::pipe_name;
use crate::protocol::{decode_response, wrap_request, IpcRequest, IpcResponse, ProtocolError};

#[derive(Error, Debug)]
pub enum IpcError {
    #[error("connection failed: mosaix may not be running")]
    ConnectionFailed,
    #[error(
        "connection refused: the mosaix agent is running at a different privilege level.          Start the agent and this command with the same elevation."
    )]
    AccessDenied,
    #[error("the mosaix agent closed the connection")]
    ConnectionClosed,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("version mismatch: server is v{server_version}")]
    VersionMismatch { server_version: u32 },
}

impl From<ProtocolError> for IpcError {
    fn from(error: ProtocolError) -> Self {
        match error {
            ProtocolError::VersionMismatch { peer_version } => IpcError::VersionMismatch {
                server_version: peer_version,
            },
            ProtocolError::Malformed(message) => IpcError::Protocol(message),
        }
    }
}

/// The error a failed `CreateFileW` on the agent's pipe reports.
///
/// `ERROR_ACCESS_DENIED` means the pipe is there and this process may not
/// open it, which in practice means the agent is elevated and the caller
/// is not, or the reverse. Reporting that as "mosaix may not be running"
/// sent people looking for a stopped agent that was in fact running.
///
/// Split out from [`IpcConnection::connect`] so the mapping can be tested
/// without a second privilege level to run the tests under.
fn connect_error(code: HRESULT) -> IpcError {
    if code == HRESULT::from_win32(ERROR_ACCESS_DENIED.0) {
        IpcError::AccessDenied
    } else {
        IpcError::ConnectionFailed
    }
}

/// An open connection to the agent, usable for as many requests as the
/// holder needs. Dropping it closes the pipe handle, which is what tells
/// the agent the client is gone.
#[derive(Debug)]
pub struct IpcConnection {
    reader: BufReader<File>,
}

impl IpcConnection {
    /// Opens a connection to the current user's agent.
    /// [`IpcError::ConnectionFailed`] means no agent is listening.
    pub fn connect() -> Result<Self, IpcError> {
        let name: Vec<u16> = pipe_name().encode_utf16().chain(Some(0)).collect();
        let handle = unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                HANDLE::default(),
            )
        }
        .map_err(|error| connect_error(error.code()))?;
        if handle.is_invalid() {
            return Err(IpcError::ConnectionFailed);
        }
        // The `File` now owns the handle and closes it on drop.
        let file = unsafe { File::from_raw_handle(handle.0 as RawHandle) };
        Ok(Self {
            reader: BufReader::new(file),
        })
    }

    /// Sends one request and waits for its response.
    ///
    /// An agent that exited mid-session shows up as end-of-stream, which
    /// is reported as [`IpcError::ConnectionClosed`] -- never as a
    /// success, and never as an indefinite wait.
    pub fn send(&mut self, request: IpcRequest) -> Result<IpcResponse, IpcError> {
        let encoded = serde_json::to_string(&wrap_request(request))
            .map_err(|error| IpcError::Protocol(error.to_string()))?;
        self.reader
            .get_mut()
            .write_all(format!("{encoded}\n").as_bytes())?;
        self.reader.get_mut().flush()?;

        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Err(IpcError::ConnectionClosed);
        }
        decode_response(line.as_bytes()).map_err(IpcError::from)
    }
}

/// Sends one request over a connection opened and closed for it alone --
/// what the CLI does, where the process itself is the session.
pub fn send_request(request: IpcRequest) -> Result<IpcResponse, IpcError> {
    IpcConnection::connect()?.send(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY};

    /// An elevated agent and an unelevated caller produce this, and it
    /// used to be reported as "mosaix may not be running" -- which sent
    /// people looking for a stopped agent that was running the whole
    /// time.
    #[test]
    fn access_denied_names_the_privilege_mismatch_rather_than_a_missing_agent() {
        let error = connect_error(HRESULT::from_win32(ERROR_ACCESS_DENIED.0));

        assert!(matches!(error, IpcError::AccessDenied));
        let rendered = error.to_string();
        assert!(
            rendered.contains("privilege level"),
            "the message must say what is actually wrong: {rendered}"
        );
        assert!(
            !rendered.contains("may not be running"),
            "and must not send the reader after a stopped agent: {rendered}"
        );
    }

    /// Everything else keeps the message it had. A missing pipe really
    /// does mean no agent is listening.
    #[test]
    fn every_other_failure_still_reads_as_a_missing_agent() {
        for code in [ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY] {
            assert!(matches!(
                connect_error(HRESULT::from_win32(code.0)),
                IpcError::ConnectionFailed
            ));
        }
    }
}
