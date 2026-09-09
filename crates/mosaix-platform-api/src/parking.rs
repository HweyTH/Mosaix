//! Recoverable window parking: the platform-neutral half.
//!
//! A parked window is moved to a *parking site*: a position outside every
//! connected display, beyond one edge of the virtual screen, so that no
//! display covers it. The window keeps its show state, its style, its
//! stacking slot, and its task-switcher presence; nothing here hides,
//! cloaks, or minimises it, and nothing here touches a private interface.
//!
//! Site discovery is split in two so the choice is testable without a
//! monitor. This module is the pure half: [`plan_parking_sites`] proposes
//! candidates from the displays an adapter reported, using geometry
//! alone. Each adapter supplies the other half -- asking the live window
//! server whether the planned block really is beyond every display -- and
//! both must agree before a site is reported as verified.
//!
//! The types live here rather than in either adapter so that Windows and
//! macOS answer the engine with one vocabulary, and so the geometry is
//! proved once.

use mosaix_domain::{topology_fingerprint, Display, Rect};
use serde::{Deserialize, Serialize};

/// How far beyond the virtual screen's edge a parked window is placed.
/// Large enough that a window's own frame and shadow cannot straddle the
/// edge; small enough to stay well inside the 16-bit coordinate range
/// window messages carry.
pub const PARKING_MARGIN: i32 = 256;

/// The side of the probe block validation checks beyond each edge: a
/// generous square, so a site is refused if anything at all is displayed
/// there, not only the pixels one window would occupy.
pub const PROBE_EXTENT: i32 = 4096;

/// The furthest a parking site may sit from the origin.
///
/// Windows carries positions in 16-bit fields in several messages, so
/// staying inside this keeps a parked window addressable everywhere.
/// macOS has no equivalent limit, and inherits the bound as a
/// deliberately conservative ceiling rather than an OS constraint: a site
/// this far out is already past every plausible display arrangement.
pub const COORDINATE_LIMIT: i32 = 30_000;

/// Which edge of the virtual screen the site lies beyond. Tried in this
/// order: right is the edge a left-to-right arrangement most often
/// leaves free, and bottom is second because a stacked arrangement
/// leaves it free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParkingEdge {
    Right,
    Bottom,
    Left,
    Top,
}

impl ParkingEdge {
    /// Every edge, in preference order.
    pub const ALL: [ParkingEdge; 4] = [
        ParkingEdge::Right,
        ParkingEdge::Bottom,
        ParkingEdge::Left,
        ParkingEdge::Top,
    ];

    pub const fn code(&self) -> &'static str {
        match self {
            Self::Right => "right",
            Self::Bottom => "bottom",
            Self::Left => "left",
            Self::Top => "top",
        }
    }
}

/// A validated place to park windows for one topology.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParkingSite {
    pub edge: ParkingEdge,
    /// The bounding box of every connected display, in screen
    /// coordinates.
    pub virtual_screen: Rect,
    /// The topology this site was validated for. A site is only as good
    /// as its topology: a display connected beyond this edge later would
    /// make the site visible, so the agent re-validates on every change.
    pub topology_fingerprint: String,
}

impl ParkingSite {
    /// Where a window of `size` goes so that none of it touches the
    /// virtual screen.
    pub fn position_for(&self, size: (i32, i32)) -> (i32, i32) {
        let (width, height) = size;
        let screen = self.virtual_screen;
        match self.edge {
            ParkingEdge::Right => (screen.x + screen.width + PARKING_MARGIN, screen.y),
            ParkingEdge::Bottom => (screen.x, screen.y + screen.height + PARKING_MARGIN),
            ParkingEdge::Left => (screen.x - PARKING_MARGIN - width, screen.y),
            ParkingEdge::Top => (screen.x, screen.y - PARKING_MARGIN - height),
        }
    }

    /// The probe rectangle validation checks against every monitor.
    pub fn probe_rect(&self) -> Rect {
        let (x, y) = self.position_for((PROBE_EXTENT, PROBE_EXTENT));
        Rect::new(x, y, PROBE_EXTENT, PROBE_EXTENT)
    }

    /// Whether the probe block stays inside the coordinate range window
    /// messages can carry.
    fn in_coordinate_range(&self) -> bool {
        let probe = self.probe_rect();
        probe.x.abs() < COORDINATE_LIMIT
            && probe.y.abs() < COORDINATE_LIMIT
            && (probe.x + probe.width).abs() < COORDINATE_LIMIT
            && (probe.y + probe.height).abs() < COORDINATE_LIMIT
    }

    /// Whether, by the reported geometry, no display overlaps the probe.
    fn clear_of(&self, displays: &[Display]) -> bool {
        let probe = self.probe_rect();
        !displays
            .iter()
            .any(|display| intersects(display.full_bounds, probe))
    }
}

/// Why no recoverable parking site exists for this topology. Nothing is
/// parked on a refusal, and no other hiding mechanism is tried instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParkingSiteRefusal {
    /// There is no display to be beyond the edge of.
    NoDisplays,
    /// Every edge of the virtual screen has a monitor beyond it, so a
    /// parked window would be visible somewhere.
    EveryEdgeCovered,
    /// The virtual screen reaches too close to the coordinate range that
    /// window messages carry for a site beyond it to be safe.
    CoordinateRangeExhausted,
}

impl ParkingSiteRefusal {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NoDisplays => "no_displays",
            Self::EveryEdgeCovered => "every_edge_covered",
            Self::CoordinateRangeExhausted => "coordinate_range_exhausted",
        }
    }
}

impl std::fmt::Display for ParkingSiteRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NoDisplays => "no display is connected",
            Self::EveryEdgeCovered => {
                "every edge of the virtual screen has a display beyond it, so a parked window would be visible"
            }
            Self::CoordinateRangeExhausted => {
                "the display arrangement reaches the edge of the coordinate range windows can be placed in"
            }
        })
    }
}

/// The bounding box of `displays`.
pub fn virtual_screen_of(displays: &[Display]) -> Option<Rect> {
    let first = displays.first()?.full_bounds;
    let (mut left, mut top) = (first.x, first.y);
    let (mut right, mut bottom) = (first.x + first.width, first.y + first.height);
    for display in displays {
        let bounds = display.full_bounds;
        left = left.min(bounds.x);
        top = top.min(bounds.y);
        right = right.max(bounds.x + bounds.width);
        bottom = bottom.max(bounds.y + bounds.height);
    }
    Some(Rect::new(left, top, right - left, bottom - top))
}

/// The candidate sites for `displays`, in preference order, by geometry
/// alone: every edge whose probe block stays in coordinate range and
/// overlaps no reported display. Pure, so the choice is testable without
/// a monitor; each adapter's `find_parking_site` adds the live check.
pub fn plan_parking_sites(
    displays: &[Display],
) -> std::result::Result<Vec<ParkingSite>, ParkingSiteRefusal> {
    let screen = virtual_screen_of(displays).ok_or(ParkingSiteRefusal::NoDisplays)?;
    let fingerprint = topology_fingerprint(displays);
    let mut any_in_range = false;
    let mut sites = Vec::new();
    for edge in ParkingEdge::ALL {
        let site = ParkingSite {
            edge,
            virtual_screen: screen,
            topology_fingerprint: fingerprint.clone(),
        };
        if !site.in_coordinate_range() {
            continue;
        }
        any_in_range = true;
        if site.clear_of(displays) {
            sites.push(site);
        }
    }
    if sites.is_empty() {
        return Err(if any_in_range {
            ParkingSiteRefusal::EveryEdgeCovered
        } else {
            ParkingSiteRefusal::CoordinateRangeExhausted
        });
    }
    Ok(sites)
}

fn intersects(bounds: Rect, other: Rect) -> bool {
    bounds.x < other.x + other.width
        && bounds.x + bounds.width > other.x
        && bounds.y < other.y + other.height
        && bounds.y + bounds.height > other.y
}

/// What parking did to a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParkedAs {
    /// Moved to the site as it was.
    Normal,
    /// Was maximised: taken to its normal size at the site, so it can be
    /// re-maximised where it returns.
    Unmaximized,
    /// Was minimised and left so: a minimised window occupies no screen
    /// and a restore would change an intentional state.
    LeftMinimized,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mosaix_domain::{DisplayId, Rotation};

    fn display(id: isize, bounds: Rect, primary: bool) -> Display {
        Display {
            id: DisplayId(id),
            stable_fingerprint: format!("D{id}"),
            full_bounds: bounds,
            work_area: bounds,
            scale_factor: 1.0,
            rotation: Rotation::Landscape,
            is_primary: primary,
        }
    }

    fn edges(sites: &[ParkingSite]) -> Vec<ParkingEdge> {
        sites.iter().map(|site| site.edge).collect()
    }

    #[test]
    fn the_virtual_screen_is_the_bounding_box_of_every_display() {
        let displays = [
            display(1, Rect::new(0, 0, 1920, 1080), true),
            display(2, Rect::new(-1920, 0, 1920, 1080), false),
        ];

        assert_eq!(
            virtual_screen_of(&displays),
            Some(Rect::new(-1920, 0, 3840, 1080))
        );
        assert_eq!(virtual_screen_of(&[]), None);
    }

    #[test]
    fn a_site_positions_a_window_wholly_beyond_its_edge() {
        let site = ParkingSite {
            edge: ParkingEdge::Left,
            virtual_screen: Rect::new(-1920, 0, 3840, 1080),
            topology_fingerprint: String::new(),
        };

        let (x, y) = site.position_for((800, 600));

        assert_eq!((x, y), (-1920 - PARKING_MARGIN - 800, 0));
        assert!(x + 800 < -1920, "the window ends before the screen begins");
    }

    #[test]
    fn side_by_side_displays_leave_every_edge_free_and_prefer_the_right() {
        let displays = [
            display(1, Rect::new(0, 0, 1920, 1080), true),
            display(2, Rect::new(-1920, 0, 1920, 1080), false),
        ];

        let sites = plan_parking_sites(&displays).unwrap();

        assert_eq!(
            edges(&sites),
            vec![
                ParkingEdge::Right,
                ParkingEdge::Bottom,
                ParkingEdge::Left,
                ParkingEdge::Top
            ]
        );
        assert_eq!(
            sites[0].topology_fingerprint,
            topology_fingerprint(&displays)
        );
    }

    #[test]
    fn a_display_beyond_an_edge_removes_that_edge_from_the_candidates() {
        // The bounding box is the whole 2x2 block, so the corner gaps are
        // inside it; every edge probe still clears. A display that sits
        // beyond the box's right edge, though, covers the right probe.
        let displays = [
            display(1, Rect::new(0, 0, 1920, 1080), true),
            display(2, Rect::new(1920 + PARKING_MARGIN, 0, 1920, 1080), false),
        ];
        // Pretend the second display was missed by the bounding box by
        // planning against the first alone, then checking against both.
        let site = ParkingSite {
            edge: ParkingEdge::Right,
            virtual_screen: displays[0].full_bounds,
            topology_fingerprint: String::new(),
        };

        assert!(
            !site.clear_of(&displays),
            "the right probe overlaps display 2"
        );
        assert!(site.clear_of(&displays[..1]));
    }

    #[test]
    fn a_site_validated_for_one_topology_is_stale_for_the_next() {
        // A display connected beyond the chosen edge would put parked
        // windows back in view. This is why the site is re-planned on
        // every topology change rather than carried over, and why it is
        // stamped with the topology it was validated for.
        let before = [display(1, Rect::new(0, 0, 1920, 1080), true)];
        let first = plan_parking_sites(&before).expect("one display leaves every edge free");
        assert_eq!(first[0].edge, ParkingEdge::Right);

        // A second display arrives inside the old right-edge probe block,
        // which is exactly the case that would leave a parked window
        // visible on it.
        let after = [
            display(1, Rect::new(0, 0, 1920, 1080), true),
            display(
                2,
                Rect::new(1920 + PARKING_MARGIN + 100, 0, 1920, 1080),
                false,
            ),
        ];

        assert!(
            !first[0].clear_of(&after),
            "the site chosen for the old topology now has a display on it"
        );

        let second = plan_parking_sites(&after).expect("the wider box still leaves an edge free");
        assert!(
            second[0].clear_of(&after),
            "re-planning yields a site clear of every display in the new topology"
        );
        assert_ne!(
            second[0].virtual_screen, first[0].virtual_screen,
            "the site moved with the virtual screen it was planned against"
        );
        assert_ne!(
            second[0].topology_fingerprint, first[0].topology_fingerprint,
            "the stamp is what makes a carried-over site recognisable as stale"
        );
    }

    #[test]
    fn no_displays_refuses_a_site() {
        assert_eq!(plan_parking_sites(&[]), Err(ParkingSiteRefusal::NoDisplays));
    }

    #[test]
    fn a_topology_at_the_coordinate_limit_refuses_rather_than_parking_out_of_range() {
        let span = COORDINATE_LIMIT * 2 - PARKING_MARGIN;
        let displays = [display(
            1,
            Rect::new(
                -COORDINATE_LIMIT + PARKING_MARGIN / 2,
                -COORDINATE_LIMIT + PARKING_MARGIN / 2,
                span,
                span,
            ),
            true,
        )];

        assert_eq!(
            plan_parking_sites(&displays),
            Err(ParkingSiteRefusal::CoordinateRangeExhausted)
        );
    }

    #[test]
    fn refusal_codes_are_distinct_and_edges_have_codes() {
        let codes = [
            ParkingSiteRefusal::NoDisplays.code(),
            ParkingSiteRefusal::EveryEdgeCovered.code(),
            ParkingSiteRefusal::CoordinateRangeExhausted.code(),
        ];
        let mut unique = codes.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), codes.len());
        assert_eq!(ParkingEdge::Right.code(), "right");
    }
}
