//! Windows named-pipe transport for the local Mosaix agent.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::io::{FromRawHandle, IntoRawHandle, RawHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use mosaix_engine::{EventSender, StateReader};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE,
    HANDLE, HLOCAL,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows::Win32::Security::{
    GetTokenInformation, TokenUser, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_MODE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::handler::handle_request;
use crate::protocol::{wrap_response, IpcEnvelope, IpcPayload, IpcResponse, PROTOCOL_VERSION};

pub const PIPE_NAME_PREFIX: &str = r"\\.\pipe\mosaix-";
const MAX_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_REQUESTS_PER_CONNECTION: usize = 100;

/// The current user's private Mosaix pipe name.
pub fn pipe_name() -> String {
    let username = std::env::var("USERNAME").unwrap_or_else(|_| "default".to_owned());
    format!("{PIPE_NAME_PREFIX}{username}")
}

/// A blocking named-pipe server on its own thread. Each pipe instance has
/// an ACL granting full access only to the current process user's SID.
pub struct IpcServer {
    stop_flag: Arc<AtomicBool>,
    join_handle: Option<JoinHandle<()>>,
}

impl IpcServer {
    pub fn start(events: EventSender, state_reader: StateReader) -> std::io::Result<Self> {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let thread_flag = Arc::clone(&stop_flag);
        let join_handle = thread::Builder::new()
            .name("mosaix-ipc".to_owned())
            .spawn(move || server_loop(events, state_reader, thread_flag))?;
        Ok(Self {
            stop_flag,
            join_handle: Some(join_handle),
        })
    }

    pub fn stop(mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        // Wake a blocking ConnectNamedPipe so the server thread can observe
        // its stop flag. Failure merely means it was already between clients.
        let name = wide(&pipe_name());
        unsafe {
            let handle = CreateFileW(
                PCWSTR(name.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_MODE(0),
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                HANDLE::default(),
            );
            if let Ok(handle) = handle {
                if !handle.is_invalid() {
                    let _ = CloseHandle(handle);
                }
            }
        }
        if let Some(join_handle) = self.join_handle.take() {
            let _ = join_handle.join();
        }
    }
}

fn server_loop(events: EventSender, state_reader: StateReader, stop_flag: Arc<AtomicBool>) {
    let name = wide(&pipe_name());
    while !stop_flag.load(Ordering::SeqCst) {
        let security = match PipeSecurity::for_current_user() {
            Ok(security) => security,
            Err(error) => {
                tracing::error!(%error, "failed to create IPC security descriptor");
                break;
            }
        };
        let pipe = unsafe {
            CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                0,
                Some(&security.attributes),
            )
        };
        if pipe.is_invalid() {
            tracing::error!(error = ?unsafe { GetLastError() }, "failed to create IPC named pipe");
            thread::sleep(std::time::Duration::from_millis(100));
            continue;
        }

        let connected = unsafe { ConnectNamedPipe(pipe, None).is_ok() }
            || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
        if stop_flag.load(Ordering::SeqCst) {
            unsafe {
                let _ = CloseHandle(pipe);
            }
            break;
        }
        if connected {
            handle_client(pipe, &events, &state_reader);
        }
        unsafe {
            let _ = DisconnectNamedPipe(pipe);
            let _ = CloseHandle(pipe);
        }
    }
}

fn handle_client(pipe: HANDLE, events: &EventSender, state_reader: &StateReader) {
    let file = unsafe { File::from_raw_handle(pipe.0 as RawHandle) };
    let mut reader = BufReader::new(file);
    let mut request_count = 0;
    loop {
        request_count += 1;
        if request_count > MAX_REQUESTS_PER_CONNECTION {
            let _ = write_response(
                &mut reader,
                IpcResponse::Error {
                    message: "IPC request rate limit exceeded".to_owned(),
                },
            );
            break;
        }
        let mut bytes = Vec::with_capacity(1024);
        match reader
            .by_ref()
            .take((MAX_MESSAGE_BYTES + 1) as u64)
            .read_until(b'\n', &mut bytes)
        {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if bytes.len() > MAX_MESSAGE_BYTES || !bytes.ends_with(b"\n") {
            let _ = write_response(
                &mut reader,
                IpcResponse::Error {
                    message: "IPC message exceeds the 64 KiB limit or is not newline-delimited"
                        .to_owned(),
                },
            );
            break;
        }
        let response = match serde_json::from_slice::<IpcEnvelope>(&bytes) {
            Ok(envelope) if envelope.version != PROTOCOL_VERSION => IpcResponse::VersionMismatch {
                server_version: PROTOCOL_VERSION,
            },
            Ok(IpcEnvelope {
                payload: IpcPayload::Request(request),
                ..
            }) => handle_request(&request, events, state_reader),
            Ok(_) => IpcResponse::Error {
                message: "expected an IPC request".to_owned(),
            },
            Err(error) => IpcResponse::Error {
                message: format!("invalid IPC message: {error}"),
            },
        };
        if !write_response(&mut reader, response) {
            break;
        }
    }
    // The outer loop owns close/disconnect. Avoid File closing the handle
    // before that cleanup is performed.
    let file = reader.into_inner();
    let _ = file.into_raw_handle();
}

fn write_response(reader: &mut BufReader<File>, response: IpcResponse) -> bool {
    let Ok(encoded) = serde_json::to_string(&wrap_response(response)) else {
        return false;
    };
    reader
        .get_mut()
        .write_all(format!("{encoded}\n").as_bytes())
        .is_ok()
        && reader.get_mut().flush().is_ok()
}

struct PipeSecurity {
    descriptor: windows::Win32::Security::PSECURITY_DESCRIPTOR,
    attributes: SECURITY_ATTRIBUTES,
}

impl PipeSecurity {
    fn for_current_user() -> windows::core::Result<Self> {
        unsafe {
            let mut token = HANDLE::default();
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
            let mut size = 0;
            let _ = GetTokenInformation(token, TokenUser, None, 0, &mut size);
            let mut buffer = vec![0_u8; size as usize];
            GetTokenInformation(
                token,
                TokenUser,
                Some(buffer.as_mut_ptr().cast()),
                size,
                &mut size,
            )?;
            let user = &*(buffer.as_ptr().cast::<TOKEN_USER>());
            let mut sid = PWSTR::null();
            ConvertSidToStringSidW(user.User.Sid, &mut sid)?;
            let sid_text = sid.to_string()?;
            let _ = LocalFree(HLOCAL(sid.0.cast()));
            let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid_text})")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let mut descriptor = windows::Win32::Security::PSECURITY_DESCRIPTOR::default();
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                1,
                &mut descriptor,
                None,
            )?;
            let attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            };
            let _ = CloseHandle(token);
            Ok(Self {
                descriptor,
                attributes,
            })
        }
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(HLOCAL(self.descriptor.0));
        }
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
