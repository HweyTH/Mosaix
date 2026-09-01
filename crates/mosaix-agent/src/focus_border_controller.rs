//! Drives [`FocusBorderOverlay`] from engine state.
//!
//! One dedicated thread owns the border window, the same shape
//! [`crate::overlay`] uses for the snap preview -- but with a different
//! trigger. The preview is *pushed* a request at the moment a hotkey fires
//! or a drag starts; the border has no such moment, because almost anything
//! can change it: focus, a reflow, a config reload, pause, the tiling
//! toggle, a display hotplug. Rather than teach every one of those call
//! sites about the border (and miss one), the controller watches
//! [`StateReader::revision`], which every committed mutation bumps.
//!
//! The poll is cheap by construction: `revision()` takes the lock and reads
//! a `u64`, and the full state is cloned only when that number moves. When
//! it does, the target is recomputed and pushed to the overlay only if it
//! differs from what is already on screen, so a burst of unrelated events
//! repaints nothing.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use mosaix_engine::StateReader;
use mosaix_platform_windows::{BorderPlacement, FocusBorderOverlay};

use crate::focus_border::{focus_border_target, FocusBorderTarget};

/// How often the controller checks whether engine state has moved. Fast
/// enough that the border keeps up with focus changes and grid reflows,
/// slow enough to be free when nothing is happening.
const POLL_INTERVAL: Duration = Duration::from_millis(33);

/// Starts the focus-border controller thread.
///
/// The controller takes no commands -- it is driven entirely by engine
/// state -- so the returned sender carries no payload and exists only as a
/// shutdown signal: drop it and the thread exits, then join the handle.
pub fn start_focus_border_controller(
    state_reader: StateReader,
    overlay: FocusBorderOverlay,
) -> (Sender<()>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let join_handle = thread::spawn(move || run(state_reader, overlay, rx));
    (tx, join_handle)
}

fn run(state_reader: StateReader, overlay: FocusBorderOverlay, rx: Receiver<()>) {
    // `None` rather than a real revision so the first pass always evaluates,
    // even against a still-untouched engine at revision 0.
    let mut last_revision: Option<u64> = None;
    let mut shown: Option<FocusBorderTarget> = None;

    // Runs until the sender is dropped; every tick in between is a poll.
    while let Ok(()) | Err(RecvTimeoutError::Timeout) = rx.recv_timeout(POLL_INTERVAL) {
        let revision = state_reader.revision();
        if last_revision == Some(revision) {
            continue;
        }
        last_revision = Some(revision);

        let target = focus_border_target(&state_reader.snapshot());
        if target == shown {
            continue;
        }
        match target {
            Some(target) => overlay.show(BorderPlacement {
                bounds: target.bounds,
                color: (target.color.r, target.color.g, target.color.b),
                thickness: target.thickness,
            }),
            None => overlay.hide(),
        }
        shown = target;
    }

    overlay.hide();
    overlay.stop();
}
