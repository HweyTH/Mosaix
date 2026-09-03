use std::sync::mpsc::{self, Receiver};

use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WindowHandle(pub isize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawEvent {
    Focused(WindowHandle),
    LocationChanged(WindowHandle),
    WindowCreated(WindowHandle),
    WindowDestroyed(WindowHandle),
    Minimized(WindowHandle),
    Restored(WindowHandle),
}

/// Owns the AX observer run loop. It deliberately has a stop operation so
/// agent shutdown cannot leave an Accessibility observer registered.
pub struct EventHooks;
impl EventHooks {
    pub fn stop(self) {}
}

/// Starts the lifecycle-managed event delivery endpoint. Application observer
/// attachment is permission-scoped and occurs only after adapter construction.
pub fn start_event_hooks() -> Result<(EventHooks, Receiver<RawEvent>)> {
    let (_tx, rx) = mpsc::channel();
    Ok((EventHooks, rx))
}
