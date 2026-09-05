//! Graceful shutdown on SIGINT/SIGTERM.
//!
//! This is the path that gets a chance to restore parked windows, so a
//! signal that fails to stop the agent is not a tidiness problem: it
//! leaves SIGKILL as the only way out, and every stop then looks like a
//! crash to the recovery ledger (ADR 0023).
//!
//! The mechanism is the self-pipe trick, and the reason is worth stating
//! because the obvious alternative looks like it works. Sending on a
//! channel directly from the handler is not async-signal-safe, and it
//! fails in a specific way here: in a single-threaded process the signal
//! is delivered to the very thread parked on the receiver, and the
//! wakeup raised while that thread is inside the handler is lost when
//! BSD `signal()` restarts the interrupted wait. The handler completes,
//! the value is queued, and the receiver sleeps forever.
//!
//! So the handler does the one thing that is safe in that context --
//! `write(2)` of a single byte -- and an ordinary thread blocked on the
//! read end turns that byte into a normal cross-thread send.

use crate::{MacosError, Result};
use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::OnceLock;
use std::thread;

/// The write end of the self-pipe. Read from the signal handler, so it
/// is an atomic and nothing else.
static SHUTDOWN_WRITE_FD: AtomicI32 = AtomicI32::new(-1);
static REGISTERED: OnceLock<()> = OnceLock::new();

extern "C" fn signal_handler(_: libc::c_int) {
    let fd = SHUTDOWN_WRITE_FD.load(Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    // write(2) is async-signal-safe. Nothing else may be added here.
    let byte = 1u8;
    unsafe { libc::write(fd, &byte as *const u8 as *const c_void, 1) };
}

/// Registers the shutdown handlers and returns the receiver the agent
/// waits on. Only the first call in a process registers.
pub fn register_shutdown_signal() -> Result<Receiver<()>> {
    REGISTERED
        .set(())
        .map_err(|()| MacosError::ShutdownHandlerAlreadyRegistered)?;

    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(MacosError::EventLoopStartFailed);
    }
    let [read_fd, write_fd] = fds;
    // Published before the handler can run, so a signal arriving during
    // registration either sees a valid fd or no fd at all.
    SHUTDOWN_WRITE_FD.store(write_fd, Ordering::SeqCst);

    // Casting a function item straight to `usize` goes through a data
    // pointer, which is the cast that preserves the address.
    let handler = signal_handler as *const () as usize;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }

    let (sender, receiver) = sync_channel(1);
    thread::Builder::new()
        .name("mosaix-shutdown".to_owned())
        .spawn(move || {
            let mut byte = 0u8;
            loop {
                let read = unsafe { libc::read(read_fd, &mut byte as *mut u8 as *mut c_void, 1) };
                if read == 1 {
                    let _ = sender.try_send(());
                    break;
                }
                // A signal can interrupt the read itself; that is not the
                // end of the pipe, only a restart.
                if read < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                    continue;
                }
                break;
            }
        })
        .map_err(|_| MacosError::EventLoopStartFailed)?;

    Ok(receiver)
}
