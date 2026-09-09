//! Snap preview overlay controller.
//!
//! One dedicated thread owns the single [`PreviewOverlay`] and serializes
//! flash-after-snap and drag-to-snap requests so they never fight over the
//! same layered window. Flash waits for the engine revision to advance past
//! the pre-send value (so a paused / circuit-open / no-op snap never
//! misleads); drag polls the cursor near work-area edges and commits via
//! [`Event::WindowPlaced`] on drop.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mosaix_domain::{DisplayId, Rect, WindowId};
use mosaix_engine::{Event, EventSender, StateReader};
use mosaix_layout::{apply_gaps, half_zone_at_edge, snap_to_half};
use mosaix_platform_windows::PreviewOverlay;

/// How long the post-snap flash stays visible.
const FLASH_DURATION: Duration = Duration::from_millis(600);
/// How long to wait for the engine to commit a snap before giving up.
const FLASH_WAIT_TIMEOUT: Duration = Duration::from_millis(250);
const FLASH_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Cursor poll rate while dragging (~30 Hz).
const DRAG_POLL_INTERVAL: Duration = Duration::from_millis(33);
/// Edge thickness (px) that triggers a half-zone drag preview.
const EDGE_THRESHOLD_PX: i32 = 24;

/// Requests the overlay controller can handle.
#[derive(Debug, Clone, Copy)]
pub enum OverlayRequest {
    /// A snap hotkey was just enqueued; flash once `EngineState.revision`
    /// exceeds `revision` (the value captured *before* the send).
    FlashAfterSnap { revision: u64 },
    /// An interactive move/resize began on `window_id`.
    DragStarted { window_id: WindowId },
    /// The interactive move/resize ended; commit if the cursor is in a zone.
    DragEnded { window_id: WindowId },
}

/// Starts the overlay controller thread. Drop the returned sender (or send
/// nothing after disconnect) and join the handle on shutdown.
pub fn start_overlay_controller(
    state_reader: StateReader,
    events: EventSender,
    overlay: PreviewOverlay,
) -> (Sender<OverlayRequest>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel();
    let join_handle = thread::spawn(move || run(state_reader, events, overlay, rx));
    (tx, join_handle)
}

fn run(
    state_reader: StateReader,
    events: EventSender,
    overlay: PreviewOverlay,
    rx: Receiver<OverlayRequest>,
) {
    enum Mode {
        Idle,
        Flashing { hide_at: Instant },
        Dragging { window_id: WindowId },
    }

    let mut mode = Mode::Idle;

    loop {
        match mode {
            Mode::Idle => match rx.recv() {
                Ok(OverlayRequest::FlashAfterSnap { revision }) => {
                    if let Some(bounds) = wait_for_committed_bounds(&state_reader, revision) {
                        overlay.show(bounds);
                        mode = Mode::Flashing {
                            hide_at: Instant::now() + FLASH_DURATION,
                        };
                    }
                }
                Ok(OverlayRequest::DragStarted { window_id }) => {
                    if events
                        .send(Event::InteractivePlacementStarted { window_id })
                        .is_err()
                    {
                        break;
                    }
                    mode = Mode::Dragging { window_id };
                }
                Ok(OverlayRequest::DragEnded { .. }) => {
                    // Spurious end with no start — ignore.
                }
                Err(_) => break,
            },

            Mode::Flashing { hide_at } => {
                let remaining = hide_at.saturating_duration_since(Instant::now());
                match rx.recv_timeout(remaining) {
                    Ok(OverlayRequest::FlashAfterSnap { revision }) => {
                        if let Some(bounds) = wait_for_committed_bounds(&state_reader, revision) {
                            overlay.show(bounds);
                            mode = Mode::Flashing {
                                hide_at: Instant::now() + FLASH_DURATION,
                            };
                        }
                    }
                    Ok(OverlayRequest::DragStarted { window_id }) => {
                        // Drag takes precedence over an in-progress flash.
                        overlay.hide();
                        if events
                            .send(Event::InteractivePlacementStarted { window_id })
                            .is_err()
                        {
                            break;
                        }
                        mode = Mode::Dragging { window_id };
                    }
                    Ok(OverlayRequest::DragEnded { .. }) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        overlay.hide();
                        mode = Mode::Idle;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }

            Mode::Dragging { window_id } => {
                match rx.recv_timeout(DRAG_POLL_INTERVAL) {
                    Ok(OverlayRequest::DragStarted { window_id: id }) => {
                        if events
                            .send(Event::InteractivePlacementStarted { window_id: id })
                            .is_err()
                        {
                            break;
                        }
                        mode = Mode::Dragging { window_id: id };
                    }
                    Ok(OverlayRequest::DragEnded { window_id: ended }) => {
                        let mut committed_manual_placement = false;
                        if let Some((display_id, bounds)) = zone_under_cursor(&state_reader) {
                            if ended == window_id {
                                if events
                                    .send(Event::WindowPlaced {
                                        window_id: ended,
                                        display_id,
                                        bounds,
                                    })
                                    .is_err()
                                {
                                    tracing::warn!("reducer stopped; overlay controller exiting");
                                    break;
                                }
                                committed_manual_placement = true;
                            }
                        }
                        if events
                            .send(Event::InteractivePlacementEnded {
                                window_id: ended,
                                committed_manual_placement,
                            })
                            .is_err()
                        {
                            break;
                        }
                        overlay.hide();
                        mode = Mode::Idle;
                    }
                    // Flash requests during drag are dropped — drag owns the overlay.
                    Ok(OverlayRequest::FlashAfterSnap { .. }) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let Some((_display_id, bounds)) = zone_under_cursor(&state_reader) {
                            overlay.show(bounds);
                        } else {
                            overlay.hide();
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        }
    }

    overlay.hide();
    overlay.stop();
}

/// Polls until `snapshot.revision > sent_revision` (or timeout), then returns
/// the focused window's committed placement bounds if any.
fn wait_for_committed_bounds(state_reader: &StateReader, sent_revision: u64) -> Option<Rect> {
    let deadline = Instant::now() + FLASH_WAIT_TIMEOUT;
    loop {
        let snapshot = state_reader.snapshot();
        if snapshot.revision > sent_revision {
            let window_id = snapshot.focused_window?;
            return snapshot.windows.get(&window_id).map(|p| p.bounds);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(FLASH_POLL_INTERVAL);
    }
}

/// The half-zone under the cursor (if any), already gap-insets applied,
/// plus the display it belongs to.
fn zone_under_cursor(state_reader: &StateReader) -> Option<(DisplayId, Rect)> {
    let (x, y) = mosaix_platform_windows::cursor_position().ok()?;
    let snapshot = state_reader.snapshot();
    let display = snapshot.displays.iter().find(|d| {
        let wa = d.work_area;
        x >= wa.x && y >= wa.y && x < wa.right() && y < wa.bottom()
    })?;
    let zone = half_zone_at_edge(display.work_area, (x, y), EDGE_THRESHOLD_PX)?;
    let raw = snap_to_half(display.work_area, zone);
    let bounds = apply_gaps(raw, display.work_area, snapshot.resolved_config.gaps);
    Some((display.id, bounds))
}
