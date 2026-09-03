//! User-private Unix-domain socket transport for macOS.
use crate::handler::handle_request;
use crate::protocol::{wrap_response, IpcEnvelope, IpcPayload, IpcResponse, PROTOCOL_VERSION};
use mosaix_engine::{EventSender, StateReader};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

const MAX_MESSAGE_BYTES: usize = 64 * 1024;
pub fn socket_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library/Application Support/Mosaix/mosaix.sock")
}
pub struct IpcServer {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    path: PathBuf,
}
impl IpcServer {
    pub fn start(events: EventSender, state: StateReader) -> std::io::Result<Self> {
        let path = socket_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if path.exists() {
            fs::remove_file(&path)?;
        }
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let join = thread::Builder::new()
            .name("mosaix-ipc".into())
            .spawn(move || {
                while !flag.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => serve(stream, &events, &state),
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(std::time::Duration::from_millis(25))
                        }
                        Err(e) => {
                            tracing::error!(%e, "macOS IPC accept failed");
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            stop,
            join: Some(join),
            path,
        })
    }
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = fs::remove_file(self.path);
    }
}
fn serve(stream: UnixStream, events: &EventSender, state: &StateReader) {
    let mut reader = BufReader::new(stream);
    let mut bytes = Vec::new();
    if reader
        .by_ref()
        .take((MAX_MESSAGE_BYTES + 1) as u64)
        .read_until(b'\n', &mut bytes)
        .is_err()
        || bytes.len() > MAX_MESSAGE_BYTES
    {
        return;
    }
    let response = match serde_json::from_slice::<IpcEnvelope>(&bytes) {
        Ok(envelope) if envelope.version != PROTOCOL_VERSION => IpcResponse::VersionMismatch {
            server_version: PROTOCOL_VERSION,
        },
        Ok(IpcEnvelope {
            payload: IpcPayload::Request(request),
            ..
        }) => handle_request(&request, events, state),
        Ok(_) => IpcResponse::Error {
            message: "expected an IPC request".into(),
        },
        Err(e) => IpcResponse::Error {
            message: format!("invalid IPC message: {e}"),
        },
    };
    if let Ok(encoded) = serde_json::to_string(&wrap_response(response)) {
        let _ = reader
            .get_mut()
            .write_all(format!("{encoded}\n").as_bytes());
        let _ = reader.get_mut().flush();
    }
}
