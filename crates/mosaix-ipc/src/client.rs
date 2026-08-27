//! Synchronous client for the agent's local named pipe.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::{FromRawHandle, IntoRawHandle, RawHandle};

use thiserror::Error;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_MODE, OPEN_EXISTING,
};

use crate::pipe::pipe_name;
use crate::protocol::{
    wrap_request, IpcEnvelope, IpcPayload, IpcRequest, IpcResponse, PROTOCOL_VERSION,
};

#[derive(Error, Debug)]
pub enum IpcError {
    #[error("connection failed: mosaix may not be running")]
    ConnectionFailed,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("version mismatch: server is v{server_version}")]
    VersionMismatch { server_version: u32 },
}

pub fn send_request(request: IpcRequest) -> Result<IpcResponse, IpcError> {
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
    .map_err(|_| IpcError::ConnectionFailed)?;
    if handle.is_invalid() {
        return Err(IpcError::ConnectionFailed);
    }
    let file = unsafe { File::from_raw_handle(handle.0 as RawHandle) };
    let mut reader = BufReader::new(file);
    let encoded = serde_json::to_string(&wrap_request(request))
        .map_err(|error| IpcError::Protocol(error.to_string()))?;
    reader
        .get_mut()
        .write_all(format!("{encoded}\n").as_bytes())?;
    reader.get_mut().flush()?;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let file = reader.into_inner();
    let _ = file.into_raw_handle();
    unsafe {
        let _ = CloseHandle(handle);
    }

    let envelope: IpcEnvelope =
        serde_json::from_str(&line).map_err(|error| IpcError::Protocol(error.to_string()))?;
    if envelope.version != PROTOCOL_VERSION {
        return Err(IpcError::VersionMismatch {
            server_version: envelope.version,
        });
    }
    match envelope.payload {
        IpcPayload::Response(response) => Ok(response),
        IpcPayload::Request(_) => Err(IpcError::Protocol("expected an IPC response".to_owned())),
    }
}
