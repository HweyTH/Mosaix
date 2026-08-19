//! Console shutdown-signal handling via `SetConsoleCtrlHandler`, so a
//! background agent can shut down cleanly (closing watchers, stopping the
//! reducer) instead of being killed mid-mutation when it receives Ctrl+C,
//! a console close, a logoff, or a system shutdown notification.

use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::OnceLock;

use windows::Win32::Foundation::BOOL;
use windows::Win32::System::Console::{
    SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT,
    CTRL_SHUTDOWN_EVENT,
};

use crate::{Result, WindowError};

static SHUTDOWN_SENDER: OnceLock<SyncSender<()>> = OnceLock::new();

unsafe extern "system" fn ctrl_handler(ctrl_type: u32) -> BOOL {
    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT
        | CTRL_SHUTDOWN_EVENT => {
            if let Some(sender) = SHUTDOWN_SENDER.get() {
                let _ = sender.try_send(());
            }
            BOOL(1)
        }
        _ => BOOL(0),
    }
}

/// Registers a console control handler and returns a receiver that yields
/// once when the process receives Ctrl+C, Ctrl+Break, a console close, a
/// logoff, or a system shutdown notification.
///
/// Must be called at most once per process -- this module supports a
/// single subscriber, not `SetConsoleCtrlHandler`'s general handler stack.
pub fn register_shutdown_signal() -> Result<Receiver<()>> {
    let (tx, rx) = sync_channel(1);

    // Register the OS-level handler before claiming the `OnceLock`: if this
    // fails, nothing has been mutated, so a caller can retry. Registering
    // in the other order would leave `SHUTDOWN_SENDER` permanently
    // occupied on failure, making every future call -- including a
    // legitimate retry -- report `ShutdownHandlerAlreadyRegistered` even
    // though no handler was ever actually installed.
    unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), true) }.map_err(WindowError::from)?;

    SHUTDOWN_SENDER
        .set(tx)
        .map_err(|_| WindowError::ShutdownHandlerAlreadyRegistered)?;

    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_handler_forwards_recognized_events_and_ignores_others() {
        let (tx, rx) = sync_channel(1);
        // Exercise the handler directly rather than through the
        // process-global `register_shutdown_signal` (which can only be
        // called once per process) by swapping in our own sender via the
        // same `OnceLock` -- safe here because tests run in this same
        // process and this is the only test that touches it.
        let _ = SHUTDOWN_SENDER.set(tx);

        assert_eq!(unsafe { ctrl_handler(0xDEAD_BEEF) }, BOOL(0), "unrecognized event should not be handled");
        assert!(rx.try_recv().is_err(), "unrecognized event must not signal shutdown");

        assert_eq!(unsafe { ctrl_handler(CTRL_C_EVENT) }, BOOL(1));
        assert!(rx.try_recv().is_ok(), "CTRL_C_EVENT must signal shutdown");
    }
}
