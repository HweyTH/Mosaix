//! Experimental window parking through public APIs only (ADR 0023,
//! ADR 0029).
//!
//! A parked window is moved to a *parking site*: a position outside every
//! connected display, found beyond one edge of the virtual screen and
//! validated with `MonitorFromRect`, so that no monitor covers it. The
//! window keeps its show state, its style, its z-order slot, and its
//! taskbar and Alt-Tab presence; nothing here hides, cloaks, or
//! minimises it, and nothing here touches a private interface. That is
//! the whole point of the experiment: the only thing that changes is
//! where the window is, and where it was is in the recovery ledger.
//!
//! Site discovery is split in two so the choice is testable without a
//! monitor: [`plan_parking_sites`] is pure geometry over the displays the
//! adapter reported, and [`find_parking_site`] asks the live desktop
//! whether the planned block really is beyond every monitor. Both must
//! agree before a site is reported as verified.
//!
//! Every call is documented: `MonitorFromRect`, `MonitorFromWindow`,
//! `GetWindowPlacement`/`SetWindowPlacement`, `SetWindowPos`, and
//! `GetForegroundWindow`.

use mosaix_domain::recovery::ShowState;
use mosaix_domain::workspace::ParkingCapability;
use mosaix_domain::{topology_fingerprint, Display, Rect};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{MonitorFromRect, MonitorFromWindow, MONITOR_DEFAULTTONULL};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, IsIconic, IsWindow, IsZoomed, SetWindowPlacement, SetWindowPos,
    SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SW_SHOWNOACTIVATE,
    WINDOWPLACEMENT, WINDOWPLACEMENT_FLAGS,
};

use crate::events::WindowHandle;
use crate::{Result, WindowError};

/// How far beyond the virtual screen's edge a parked window is placed.
/// Large enough that a window's own frame and shadow cannot straddle the
/// edge; small enough to stay well inside the 16-bit coordinate range
/// window messages carry.
pub const PARKING_MARGIN: i32 = 256;

/// The side of the probe block validation checks beyond each edge: a
/// generous square, so a site is refused if anything at all is displayed
/// there, not only the pixels one window would occupy.
pub const PROBE_EXTENT: i32 = 4096;

/// Windows carries positions in 16-bit fields in several messages;
/// staying inside this keeps a parked window addressable everywhere.
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
/// parked on a refusal, and no other hiding mechanism is tried instead
/// (ADR 0023).
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
/// a monitor; [`find_parking_site`] adds the live check.
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

/// Whether the live desktop agrees that no monitor covers any part of
/// `site`'s probe block (`MonitorFromRect`). This is what catches a
/// display the enumeration missed.
pub fn verify_parking_site(site: &ParkingSite) -> bool {
    let probe = site.probe_rect();
    let rect = RECT {
        left: probe.x,
        top: probe.y,
        right: probe.x + probe.width,
        bottom: probe.y + probe.height,
    };
    unsafe { MonitorFromRect(&rect, MONITOR_DEFAULTTONULL) }.is_invalid()
}

/// Finds a validated parking site for `displays`, or refuses.
///
/// Each geometric candidate is checked live before it is accepted. A
/// candidate the geometry allows but the desktop refuses is skipped, and
/// when none survives the refusal is that every edge is covered: by the
/// live desktop's account, which is the one that matters.
pub fn find_parking_site(
    displays: &[Display],
) -> std::result::Result<ParkingSite, ParkingSiteRefusal> {
    plan_parking_sites(displays)?
        .into_iter()
        .find(verify_parking_site)
        .ok_or(ParkingSiteRefusal::EveryEdgeCovered)
}

/// What the adapter reports to the engine about parking for `displays`,
/// with the site itself for the executor to park against. `Refused`
/// carries the reason a person reads.
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

/// Moves `handle` to `site` without activating it, and verifies that no
/// monitor covers it afterwards.
///
/// A maximised window cannot simply be moved: the shell keeps it filling
/// its monitor. It is taken to its normal size at the site through
/// `SetWindowPlacement` with `SW_SHOWNOACTIVATE`, which is the one public
/// way to un-maximise without activation; the recovery entry already
/// holds its maximised state for the way back.
pub fn park_window(handle: WindowHandle, site: &ParkingSite) -> Result<ParkedAs> {
    let hwnd = HWND::from(handle);
    if !unsafe { IsWindow(hwnd) }.as_bool() {
        return Err(WindowError::InvalidWindow);
    }
    if unsafe { IsIconic(hwnd) }.as_bool() {
        return Ok(ParkedAs::LeftMinimized);
    }
    let parked_as = if unsafe { IsZoomed(hwnd) }.as_bool() {
        // Two steps, because the shell adjusts a normal position handed
        // to `SetWindowPlacement` so the window stays reachable, which
        // would leave it on a monitor. First it is taken to its normal
        // size where it already was, without activation; then it is
        // moved like any other normal window.
        let (normal, _) = crate::window_placement(handle).ok_or(WindowError::InvalidWindow)?;
        let placement = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            flags: WINDOWPLACEMENT_FLAGS(0),
            showCmd: SW_SHOWNOACTIVATE.0 as u32,
            ptMinPosition: POINT { x: -1, y: -1 },
            ptMaxPosition: POINT { x: -1, y: -1 },
            rcNormalPosition: RECT {
                left: normal.x,
                top: normal.y,
                right: normal.x + normal.width,
                bottom: normal.y + normal.height,
            },
        };
        unsafe { SetWindowPlacement(hwnd, &placement) }.map_err(WindowError::from)?;
        ParkedAs::Unmaximized
    } else {
        ParkedAs::Normal
    };
    let bounds = crate::window_bounds(hwnd)?;
    let (x, y) = site.position_for((bounds.width, bounds.height));
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        )
    }
    .map_err(WindowError::from)?;
    if !is_parked(handle) {
        return Err(WindowError::ParkingNotEffective);
    }
    Ok(parked_as)
}

/// Whether no monitor covers any part of `handle`'s window.
pub fn is_parked(handle: WindowHandle) -> bool {
    let hwnd = HWND::from(handle);
    unsafe { IsWindow(hwnd) }.as_bool()
        && unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONULL) }.is_invalid()
}

/// The window that owns the foreground right now, for measuring whether
/// a park or restore stole focus.
pub fn foreground() -> Option<WindowHandle> {
    let hwnd = unsafe { GetForegroundWindow() };
    (!hwnd.0.is_null()).then_some(WindowHandle::from(hwnd))
}

/// The show state of `handle` as the shell reports it right now.
pub fn show_state(handle: WindowHandle) -> Option<ShowState> {
    let hwnd = HWND::from(handle);
    if !unsafe { IsWindow(hwnd) }.as_bool() {
        return None;
    }
    Some(if unsafe { IsIconic(hwnd) }.as_bool() {
        ShowState::Minimized
    } else if unsafe { IsZoomed(hwnd) }.as_bool() {
        ShowState::Maximized
    } else {
        ShowState::Normal
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::create_test_window;
    use mosaix_domain::recovery::{RecoveryDraft, RecoveryEntry, RecoveryEntryId};
    use mosaix_domain::{ApplicationId, DisplayId, Rotation};
    use windows::Win32::UI::WindowsAndMessaging::{
        DestroyWindow, ShowWindow, SW_SHOWMAXIMIZED, SW_SHOWNOACTIVATE,
    };

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

    #[test]
    fn the_live_topology_yields_a_site_no_monitor_covers() {
        crate::test_support::ensure_dpi_awareness();
        let displays = crate::enumerate_displays().expect("displays enumerate");

        let site = find_parking_site(&displays).expect("this machine has a free edge");

        assert!(
            verify_parking_site(&site),
            "the site's probe block is beyond every monitor"
        );
        assert_eq!(site.topology_fingerprint, topology_fingerprint(&displays));
        let (capability, reported) = parking_capability(&displays);
        assert_eq!(capability, ParkingCapability::Verified);
        assert_eq!(reported, Some(site));
    }

    #[test]
    fn parking_moves_a_window_off_every_monitor_without_taking_the_foreground() {
        crate::test_support::ensure_dpi_awareness();
        let displays = crate::enumerate_displays().unwrap();
        let site = find_parking_site(&displays).unwrap();
        let hwnd = create_test_window();
        let handle = WindowHandle::from(hwnd);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        crate::move_resize_window(hwnd, Rect::new(100, 100, 320, 240)).unwrap();
        let foreground_before = foreground();

        let parked_as = park_window(handle, &site).unwrap();

        assert_eq!(parked_as, ParkedAs::Normal);
        assert!(is_parked(handle));
        assert_eq!(
            foreground(),
            foreground_before,
            "parking never activates the parked window"
        );
        assert_eq!(show_state(handle), Some(ShowState::Normal));
        let bounds = crate::window_bounds(hwnd).unwrap();
        assert_eq!(
            (bounds.width, bounds.height),
            (320, 240),
            "size is untouched"
        );
        unsafe { DestroyWindow(hwnd) }.unwrap();
    }

    #[test]
    fn a_parked_window_is_restored_to_its_recorded_geometry_by_the_recovery_path() {
        crate::test_support::ensure_dpi_awareness();
        let displays = crate::enumerate_displays().unwrap();
        let site = find_parking_site(&displays).unwrap();
        let hwnd = create_test_window();
        let handle = WindowHandle::from(hwnd);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        let visible = Rect::new(120, 80, 300, 200);
        crate::move_resize_window(hwnd, visible).unwrap();
        let (normal, state) = crate::window_placement(handle).unwrap();
        let live = crate::probe_handle(handle).unwrap();
        let entry = RecoveryEntry {
            id: RecoveryEntryId(1),
            draft: RecoveryDraft {
                session_id: "test".to_owned(),
                native_handle: handle.0,
                process: live.process,
                application_id: ApplicationId("test.exe".to_owned()),
                executable_path: None,
                native_class: live.native_class,
                original_display_fingerprint: displays[0].stable_fingerprint.clone(),
                visible_bounds: visible,
                normal_bounds: normal,
                show_state: state,
                recorded_at_unix: 0,
            },
            parked: true,
            restored: false,
        };

        park_window(handle, &site).unwrap();
        assert!(is_parked(handle));
        crate::restore_window(handle, &entry).unwrap();

        assert!(!is_parked(handle));
        assert_eq!(crate::window_bounds(hwnd).unwrap(), visible);
        unsafe { DestroyWindow(hwnd) }.unwrap();
    }

    #[test]
    fn a_maximized_window_is_parked_at_its_normal_size_and_re_maximizes_on_restore() {
        crate::test_support::ensure_dpi_awareness();
        let displays = crate::enumerate_displays().unwrap();
        let site = find_parking_site(&displays).unwrap();
        let hwnd = create_test_window();
        let handle = WindowHandle::from(hwnd);
        crate::move_resize_window(hwnd, Rect::new(200, 150, 400, 300)).unwrap();
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWMAXIMIZED);
        }
        assert_eq!(show_state(handle), Some(ShowState::Maximized));
        let (normal, state) = crate::window_placement(handle).unwrap();
        assert_eq!(state, ShowState::Maximized);
        let maximized_bounds = crate::window_bounds(hwnd).unwrap();
        let live = crate::probe_handle(handle).unwrap();
        let entry = RecoveryEntry {
            id: RecoveryEntryId(1),
            draft: RecoveryDraft {
                session_id: "test".to_owned(),
                native_handle: handle.0,
                process: live.process,
                application_id: ApplicationId("test.exe".to_owned()),
                executable_path: None,
                native_class: live.native_class,
                original_display_fingerprint: displays[0].stable_fingerprint.clone(),
                visible_bounds: maximized_bounds,
                normal_bounds: normal,
                show_state: ShowState::Maximized,
                recorded_at_unix: 0,
            },
            parked: true,
            restored: false,
        };

        let parked_as = park_window(handle, &site).unwrap();

        assert_eq!(parked_as, ParkedAs::Unmaximized);
        assert!(is_parked(handle));
        assert_eq!(show_state(handle), Some(ShowState::Normal));

        crate::restore_window(handle, &entry).unwrap();
        assert_eq!(show_state(handle), Some(ShowState::Maximized));
        assert!(!is_parked(handle));
        unsafe { DestroyWindow(hwnd) }.unwrap();
    }

    #[test]
    fn a_minimized_window_is_left_minimized_and_a_destroyed_one_is_refused() {
        crate::test_support::ensure_dpi_awareness();
        let displays = crate::enumerate_displays().unwrap();
        let site = find_parking_site(&displays).unwrap();
        let hwnd = create_test_window();
        let handle = WindowHandle::from(hwnd);
        unsafe {
            let _ = ShowWindow(
                hwnd,
                windows::Win32::UI::WindowsAndMessaging::SW_SHOWMINNOACTIVE,
            );
        }
        assert_eq!(show_state(handle), Some(ShowState::Minimized));

        assert_eq!(park_window(handle, &site).unwrap(), ParkedAs::LeftMinimized);
        assert_eq!(show_state(handle), Some(ShowState::Minimized));

        unsafe { DestroyWindow(hwnd) }.unwrap();
        assert!(matches!(
            park_window(handle, &site),
            Err(WindowError::InvalidWindow)
        ));
        assert!(!is_parked(handle));
        assert_eq!(show_state(handle), None);
    }
}
