//! The real displays a layout can be previewed against.
//!
//! A saved layout's cells are fractions of a display's work area, so a
//! preview that does not use a real work area is showing the wrong shape:
//! a 16:9 canvas cannot tell a user what their cells will look like on a
//! 21:9 panel.
//!
//! Enumeration is the platform adapter's job, the same one
//! `mosaix-agent` uses. This module only reduces a `Display` to what the
//! editor shows.

use crate::editor::DisplaySummary;

/// Every display attached right now, primary first.
///
/// An empty result is possible and not an error -- off Windows there is
/// no adapter to ask, and on Windows enumeration can fail. The editor
/// falls back to a nominal display so the canvas still draws, and says so.
pub fn enumerate() -> Vec<DisplaySummary> {
    #[cfg(windows)]
    {
        match mosaix_platform_windows::enumerate_displays() {
            Ok(displays) => summarize(&displays),
            Err(error) => {
                tracing::warn!(%error, "could not enumerate displays for the layout preview");
                Vec::new()
            }
        }
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Reduces enumerated displays to the editor's view of them, primary
/// first and otherwise in enumeration order.
pub fn summarize(displays: &[mosaix_domain::Display]) -> Vec<DisplaySummary> {
    let mut summaries: Vec<(bool, DisplaySummary)> = displays
        .iter()
        .enumerate()
        .map(|(index, display)| {
            (
                display.is_primary,
                DisplaySummary {
                    name: if display.is_primary {
                        "Primary display".to_owned()
                    } else {
                        format!("Display {}", index + 1)
                    },
                    resolution: format!(
                        "{} × {}",
                        display.work_area.width, display.work_area.height
                    ),
                    scale_percent: (display.scale_factor * 100.0).round() as u16,
                    // The *work* area, not the full bounds: a layout's
                    // cells are laid over the area a window can occupy,
                    // so that is the rectangle the preview must match.
                    work_area_width: display.work_area.width,
                    work_area_height: display.work_area.height,
                },
            )
        })
        .collect();
    summaries.sort_by_key(|(is_primary, _)| !is_primary);
    summaries.into_iter().map(|(_, summary)| summary).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mosaix_domain::{DisplayId, Rect, Rotation};

    fn display(id: isize, x: i32, width: i32, scale: f64, primary: bool) -> mosaix_domain::Display {
        mosaix_domain::Display {
            id: DisplayId(id),
            stable_fingerprint: format!("MON-{id}"),
            full_bounds: Rect::new(x, 0, width, 1080),
            work_area: Rect::new(x, 40, width, 1040),
            scale_factor: scale,
            rotation: Rotation::Landscape,
            is_primary: primary,
        }
    }

    #[test]
    fn a_summary_carries_the_work_area_a_layout_is_laid_over() {
        let summaries = summarize(&[display(1, 0, 2560, 1.5, true)]);

        assert_eq!(summaries[0].work_area_width, 2560);
        assert_eq!(
            summaries[0].work_area_height, 1040,
            "the taskbar inset is part of the area a layout has to fit"
        );
        assert_eq!(summaries[0].resolution, "2560 × 1040");
        assert_eq!(summaries[0].scale_percent, 150);
    }

    #[test]
    fn the_primary_display_is_offered_first() {
        let summaries = summarize(&[
            display(1, 0, 1920, 1.0, false),
            display(2, 1920, 2560, 1.0, true),
        ]);

        assert_eq!(summaries[0].name, "Primary display");
        assert_eq!(summaries[0].work_area_width, 2560);
    }
}
