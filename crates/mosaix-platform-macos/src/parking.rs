//! Experimental window parking on macOS through public APIs only
//! (ADR 0023, ADR 0029).
//!
//! This is the native half of parking; the geometry, the site types, and
//! the refusal vocabulary are shared with Windows in
//! [`mosaix_platform_api::parking`]. What is platform-specific is who the
//! live authority is: on Windows it is `MonitorFromRect`, and here it is
//! the Core Graphics active display list, which is the window server's
//! own account of what is displayed where.
//!
//! Nothing here uses a private Spaces interface, injects into another
//! process, requires reduced System Integrity Protection, or falls back
//! to hiding or minimising a window. A window that cannot be parked
//! recoverably is left alone and the capability is reported as refused.

use core_graphics::display::CGDisplay;
use mosaix_domain::recovery::ShowState;
use mosaix_domain::workspace::ParkingCapability;
use mosaix_domain::{Display, Rect};

use crate::accessibility::{K_AX_FULL_SCREEN_ATTRIBUTE, K_AX_MINIMIZED_ATTRIBUTE};
use crate::events::WindowHandle;
use crate::flip_y;
use crate::recovery::{resolve_window, show_state_of, window_server_info};
use crate::{MacosError, Result};

// The platform-neutral half, re-exported so `parking::` reads the same on
// both adapters.
pub use mosaix_platform_api::parking::{
    plan_parking_sites, virtual_screen_of, ParkedAs, ParkingEdge, ParkingSite, ParkingSiteRefusal,
    COORDINATE_LIMIT, PARKING_MARGIN, PROBE_EXTENT,
};

/// Whether the live window server agrees that no display covers any part
/// of `site`'s probe block.
///
/// The Windows adapter asks `MonitorFromRect`, which answers for the
/// whole desktop in one call. Core Graphics has no such query, so this
/// asks the equivalent question of the active display list directly:
/// a site is verified only if every currently active display's bounds
/// miss the probe. Reading the list live is the point -- it is what
/// catches a display that the adapter's own enumeration missed.
pub fn verify_parking_site(site: &ParkingSite) -> bool {
    let Some(displays) = active_display_rects() else {
        // The window server would not say what is connected. A site that
        // cannot be checked is not a verified site.
        return false;
    };
    let probe = site.probe_rect();
    !displays
        .into_iter()
        .any(|display| intersects(display, probe))
}

/// Finds a validated parking site for `displays`, or refuses.
///
/// Each geometric candidate is checked against the live display list
/// before it is accepted, so a candidate the geometry allows but the
/// window server refuses is skipped.
pub fn find_parking_site(
    displays: &[Display],
) -> std::result::Result<ParkingSite, ParkingSiteRefusal> {
    plan_parking_sites(displays)?
        .into_iter()
        .find(verify_parking_site)
        .ok_or(ParkingSiteRefusal::EveryEdgeCovered)
}

/// What the adapter reports to the engine about parking for `displays`,
/// with the site itself for the executor to park against.
pub fn parking_capability(displays: &[Display]) -> (ParkingCapability, Option<ParkingSite>) {
    match find_parking_site(displays) {
        Ok(site) => (ParkingCapability::Verified, Some(site)),
        Err(refusal) => (
            ParkingCapability::Refused {
                reason: refusal.to_string(),
            },
            None,
        ),
    }
}

/// Moves `handle` to `site` without activating it, and verifies that no
/// display covers it afterwards.
///
/// A full-screen window cannot simply be moved -- macOS keeps it filling
/// its display -- so it is taken out of full screen first, and the
/// recovery entry already holds that state for the way back. A minimized
/// window is left alone: it occupies no screen already, and un-minimizing
/// it to park it would destroy a state the user chose.
///
/// Setting `AXPosition` does not raise the window, so unlike the Windows
/// adapter there is no `SWP_NOACTIVATE` equivalent to ask for -- not
/// activating is the default here.
pub fn park_window(handle: WindowHandle, site: &ParkingSite) -> Result<ParkedAs> {
    let window_id = u32::try_from(handle.0).map_err(|_| MacosError::WindowNotResolvable(0))?;
    let element = resolve_window(window_id)?;

    if element.bool_attribute(K_AX_MINIMIZED_ATTRIBUTE) == Some(true) {
        return Ok(ParkedAs::LeftMinimized);
    }
    let parked_as = if element.bool_attribute(K_AX_FULL_SCREEN_ATTRIBUTE) == Some(true) {
        element.set_bool_attribute(K_AX_FULL_SCREEN_ATTRIBUTE, false)?;
        ParkedAs::Unmaximized
    } else {
        ParkedAs::Normal
    };

    let (width, height) = element
        .size()
        .ok_or(MacosError::WindowNotResolvable(window_id))?;
    let (x, y) = site.position_for((width, height));
    element.set_position(x, y)?;

    if !is_parked(handle) {
        return Err(MacosError::ParkingNotEffective);
    }
    Ok(parked_as)
}

/// Whether no display covers any part of `handle`'s window.
///
/// The window server's own frame for the window is the authority, not the
/// position we asked for: an application is free to refuse or clamp a
/// move, and a park that did not take effect must not be reported as one.
pub fn is_parked(handle: WindowHandle) -> bool {
    let Ok(window_id) = u32::try_from(handle.0) else {
        return false;
    };
    let Some(info) = window_server_info(window_id) else {
        return false;
    };
    let Some(displays) = active_display_rects() else {
        return false;
    };
    // The window server reports the frame in the same Core Graphics
    // space the display list uses, so it takes the same conversion.
    let primary_height = CGDisplay::main().bounds().size.height.round() as i32;
    let window = Rect::new(
        info.bounds.x,
        flip_y(info.bounds.y, info.bounds.height, primary_height),
        info.bounds.width,
        info.bounds.height,
    );
    !displays
        .into_iter()
        .any(|display| intersects(display, window))
}

/// The window that owns the foreground right now, for measuring whether
/// a park or restore stole focus.
///
/// The window server returns on-screen windows front to back, so the
/// frontmost normal-layer window is the foreground one. This avoids
/// needing an AppKit main thread just to answer the question.
pub fn foreground() -> Option<WindowHandle> {
    crate::recovery::frontmost_window_id().map(|id| WindowHandle(id as isize))
}

/// The show state of `handle` as Accessibility reports it right now.
pub fn show_state(handle: WindowHandle) -> Option<ShowState> {
    let window_id = u32::try_from(handle.0).ok()?;
    let element = resolve_window(window_id).ok()?;
    Some(show_state_of(&element))
}

/// Every active display's bounds, in the same space
/// [`crate::enumerate_displays`] reports.
///
/// This conversion is the whole reason the helper exists. `Display`
/// values reach this module already flipped into Mosaix's top-left
/// space, and a parking site is planned in that space; comparing the
/// resulting probe against raw Core Graphics bounds would agree only on
/// a single-display machine, and silently disagree on the multi-display
/// arrangements parking exists to serve.
fn active_display_rects() -> Option<Vec<Rect>> {
    let ids = CGDisplay::active_displays().ok()?;
    let primary_height = CGDisplay::main().bounds().size.height.round() as i32;
    Some(
        ids.into_iter()
            .map(|id| cg_rect_to_mosaix_space(CGDisplay::new(id).bounds(), primary_height))
            .collect(),
    )
}

/// Converts a Core Graphics rectangle into Mosaix's coordinate space,
/// exactly as display enumeration does.
fn cg_rect_to_mosaix_space(rect: core_graphics::geometry::CGRect, primary_height: i32) -> Rect {
    let height = rect.size.height.round() as i32;
    Rect::new(
        rect.origin.x.round() as i32,
        flip_y(rect.origin.y.round() as i32, height, primary_height),
        rect.size.width.round() as i32,
        height,
    )
}

fn intersects(bounds: Rect, other: Rect) -> bool {
    bounds.x < other.x + other.width
        && bounds.x + bounds.width > other.x
        && bounds.y < other.y + other.height
        && bounds.y + bounds.height > other.y
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_live_topology_yields_a_site_no_display_covers() {
        let displays = crate::enumerate_displays().expect("displays enumerate");

        let site = find_parking_site(&displays).expect("this machine has a free edge");

        assert!(
            verify_parking_site(&site),
            "the site the planner chose survives the live check"
        );
    }

    #[test]
    fn a_site_inside_the_desktop_is_refused_by_the_live_check() {
        let displays = crate::enumerate_displays().expect("displays enumerate");
        let main = CGDisplay::main().bounds();
        // A site whose virtual screen is a single point at the origin puts
        // its probe block squarely over the main display.
        let covering = ParkingSite {
            edge: ParkingEdge::Right,
            virtual_screen: Rect::new(
                main.origin.x.round() as i32 - PROBE_EXTENT,
                main.origin.y.round() as i32,
                PROBE_EXTENT - PARKING_MARGIN,
                1,
            ),
            topology_fingerprint: String::new(),
        };

        assert!(
            !verify_parking_site(&covering),
            "a probe block over a live display is not a parking site"
        );
        assert!(!displays.is_empty(), "the test needs a display to cover");
    }

    #[test]
    fn a_display_is_converted_into_the_same_space_a_site_is_planned_in() {
        // The regression this guards: comparing a planned site against raw
        // Core Graphics bounds agrees on a single display and silently
        // disagrees on the stacked arrangements parking exists to serve.
        let primary_height = 1_080;
        let secondary = core_graphics::geometry::CGRect::new(
            &core_graphics::geometry::CGPoint::new(0.0, 1_080.0),
            &core_graphics::geometry::CGSize::new(1_920.0, 1_080.0),
        );

        let converted = cg_rect_to_mosaix_space(secondary, primary_height);

        assert_eq!(
            converted,
            Rect::new(0, -1_080, 1_920, 1_080),
            "a display above the primary one lands above it in Mosaix space too"
        );
        assert_eq!(
            cg_rect_to_mosaix_space(
                core_graphics::geometry::CGRect::new(
                    &core_graphics::geometry::CGPoint::new(0.0, 0.0),
                    &core_graphics::geometry::CGSize::new(1_920.0, 1_080.0),
                ),
                primary_height,
            ),
            Rect::new(0, 0, 1_920, 1_080),
            "the primary display maps onto itself"
        );
    }

    #[test]
    fn no_displays_refuses_rather_than_reporting_a_capability() {
        let (capability, site) = parking_capability(&[]);

        assert_eq!(site, None);
        assert!(matches!(capability, ParkingCapability::Refused { .. }));
    }
}
