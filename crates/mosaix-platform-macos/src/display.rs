use std::sync::mpsc::{self, Receiver, Sender};

use core_graphics::display::CGDisplay;
use mosaix_domain::{Display, DisplayId, Rect, Rotation};

use crate::{display_fingerprint, flip_y, MacosError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopologyEvent {
    Changed,
    WakeFromSleep,
}

/// Display watcher handle. The sender is retained by the platform callback
/// registration in the next integration layer; dropping it ends consumers.
pub struct DisplayWatcher {
    _sender: Sender<TopologyEvent>,
}
impl DisplayWatcher {
    pub fn stop(self) {}
}

fn rotation(degrees: f64) -> Rotation {
    match (degrees.round() as i32).rem_euclid(360) {
        90 => Rotation::Portrait,
        180 => Rotation::LandscapeFlipped,
        270 => Rotation::PortraitFlipped,
        _ => Rotation::Landscape,
    }
}

/// Enumerates active displays with Core Graphics. CG's global coordinates are
/// converted exactly once at this platform boundary.
pub fn enumerate_displays() -> Result<Vec<Display>> {
    let ids = CGDisplay::active_displays()
        .map_err(|e| MacosError::CoreGraphics(format!("CGGetActiveDisplayList failed: {e}")))?;
    let primary_height = CGDisplay::main().bounds().size.height.round() as i32;
    Ok(ids
        .into_iter()
        .map(|id| {
            let display = CGDisplay::new(id);
            let bounds = display.bounds();
            let width = bounds.size.width.round() as i32;
            let height = bounds.size.height.round() as i32;
            let x = bounds.origin.x.round() as i32;
            let y = flip_y(bounds.origin.y.round() as i32, height, primary_height);
            Display {
                id: DisplayId(id as isize),
                stable_fingerprint: display_fingerprint(
                    display.vendor_number(),
                    display.model_number(),
                    display.serial_number(),
                    width,
                    height,
                ),
                full_bounds: Rect::new(x, y, width, height),
                // AppKit's visibleFrame is intentionally isolated from Core
                // Graphics. Until the main-thread AppKit bridge supplies it, the
                // full bounds are a conservative usable area.
                work_area: Rect::new(x, y, width, height),
                scale_factor: 1.0,
                rotation: rotation(display.rotation()),
                is_primary: display.is_main(),
            }
        })
        .collect())
}

pub fn watch_display_topology() -> Result<(DisplayWatcher, Receiver<TopologyEvent>)> {
    let (tx, rx) = mpsc::channel();
    Ok((DisplayWatcher { _sender: tx }, rx))
}
