//! macOS client for the user-private Unix domain socket transport.
use crate::protocol::{
    wrap_request, IpcEnvelope, IpcPayload, IpcRequest, IpcResponse, PROTOCOL_VERSION,
};
use crate::unix_socket::socket_path;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use thiserror::Error;

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
    let stream = UnixStream::connect(socket_path()).map_err(|_| IpcError::ConnectionFailed)?;
    let mut reader = BufReader::new(stream);
    let encoded = serde_json::to_string(&wrap_request(request))
        .map_err(|e| IpcError::Protocol(e.to_string()))?;
    reader
        .get_mut()
        .write_all(format!("{encoded}\n").as_bytes())?;
    reader.get_mut().flush()?;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let envelope: IpcEnvelope =
        serde_json::from_str(&line).map_err(|e| IpcError::Protocol(e.to_string()))?;
    if envelope.version != PROTOCOL_VERSION {
        return Err(IpcError::VersionMismatch {
            server_version: envelope.version,
        });
    }
    match envelope.payload {
        IpcPayload::Response(response) => Ok(response),
        IpcPayload::Request(_) => Err(IpcError::Protocol("expected an IPC response".into())),
    }
}
