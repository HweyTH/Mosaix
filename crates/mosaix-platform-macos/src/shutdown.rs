use crate::{MacosError, Result};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::OnceLock;
static SHUTDOWN_SENDER: OnceLock<SyncSender<()>> = OnceLock::new();
extern "C" fn signal_handler(_: libc::c_int) {
    if let Some(tx) = SHUTDOWN_SENDER.get() {
        let _ = tx.try_send(());
    }
}
pub fn register_shutdown_signal() -> Result<Receiver<()>> {
    let (tx, rx) = sync_channel(1);
    SHUTDOWN_SENDER
        .set(tx)
        .map_err(|_| MacosError::ShutdownHandlerAlreadyRegistered)?;
    // Casting a function item straight to `usize` is a lint the compiler
    // is right about: it goes through a data pointer, which is the only
    // cast guaranteed to preserve the address.
    let handler = signal_handler as *const () as usize;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
    Ok(rx)
}
