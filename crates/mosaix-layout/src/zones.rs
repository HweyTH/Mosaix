//! Manual zone planner: named half/quarter/third zones, centering, and
//! maximizing, each resolved against a container (typically a display's
//! work area) -- architecture doc section 20's "Focused-window halves,
//! quarters, thirds, center, maximize, restore" Phase 1 command set (minus
//! `restore`, which needs a remembered pre-snap size and belongs with
//! whatever tracks window state, not this stateless planner).
//!
//! Half and quarter zones are expressed as [`NormalizedRect`] fractions and
//! resolved with [`NormalizedRect::to_rect`], the "one deterministic
//! edge-allocation algorithm" (architecture doc section 10) that rounds
//! shared edges consistently -- so e.g. `LeftHalf` and `RightHalf` always
//! meet exactly, with no gap or overlap, even against an odd-sized
//! container. `0.5` is a literal fraction shared by every half/quarter
//! boundary, so resolving each zone independently is safe (see
//! [`NormalizedRect::to_rect`]'s docs).
//!
//! Thirds can't use that trick -- `1.0 / 3.0` isn't representable exactly,
//! so three independently-rounded thirds of e.g. a 100px span can fall a
//! pixel short of 100 (the exact failure mode [`allocate_edges`]'s docs
//! warn about). Third zones therefore go through [`allocate_edges`], which
//! rounds from *cumulative* weight instead, guaranteeing the three columns
//! are gapless and cover the container exactly.

use mosaix_domain::{allocate_edges, NormalizedRect, Rect};

/// A named half-zone a window can be snapped to (architecture doc section
/// 8.1, "Snap to named zone"; section 20, "Focused-window halves").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HalfZone {
    LeftHalf,
    RightHalf,
    TopHalf,
    BottomHalf,
}

impl HalfZone {
    /// This zone's bounds as fractions of the container.
    pub fn normalized(self) -> NormalizedRect {
        match self {
            HalfZone::LeftHalf => NormalizedRect {
                x: 0.0,
                y: 0.0,
                width: 0.5,
                height: 1.0,
            },
            HalfZone::RightHalf => NormalizedRect {
                x: 0.5,
                y: 0.0,
                width: 0.5,
                height: 1.0,
            },
            HalfZone::TopHalf => NormalizedRect {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 0.5,
            },
            HalfZone::BottomHalf => NormalizedRect {
                x: 0.0,
                y: 0.5,
                width: 1.0,
                height: 0.5,
            },
        }
    }
}

/// Resolves `zone` against `container` (typically a display's work area),
/// returning the bounds a window snapped to that zone should take.
pub fn snap_to_half(container: Rect, zone: HalfZone) -> Rect {
    zone.normalized().to_rect(container)
}

/// A named quarter-zone a window can be snapped to (architecture doc
/// section 20, "Focused-window ... quarters").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuarterZone {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl QuarterZone {
    /// This zone's bounds as fractions of the container.
    pub fn normalized(self) -> NormalizedRect {
        match self {
            QuarterZone::TopLeft => NormalizedRect {
                x: 0.0,
                y: 0.0,
                width: 0.5,
                height: 0.5,
            },
            QuarterZone::TopRight => NormalizedRect {
                x: 0.5,
                y: 0.0,
                width: 0.5,
                height: 0.5,
            },
            QuarterZone::BottomLeft => NormalizedRect {
                x: 0.0,
                y: 0.5,
                width: 0.5,
                height: 0.5,
            },
            QuarterZone::BottomRight => NormalizedRect {
                x: 0.5,
                y: 0.5,
                width: 0.5,
                height: 0.5,
            },
        }
    }
}

/// Resolves `zone` against `container` (typically a display's work area),
/// returning the bounds a window snapped to that zone should take.
pub fn snap_to_quarter(container: Rect, zone: QuarterZone) -> Rect {
    zone.normalized().to_rect(container)
}

/// A named third-zone a window can be snapped to, including the two-thirds
/// combos (architecture doc section 20, "Focused-window ... thirds").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThirdZone {
    LeftThird,
    CenterThird,
    RightThird,
    /// The left third plus the center third.
    LeftTwoThirds,
    /// The center third plus the right third.
    RightTwoThirds,
}

/// The container's width split into three gapless columns via
/// [`allocate_edges`], each as a `[start, end)` pixel range relative to
/// `container.x`.
fn third_columns(container: Rect) -> [(i32, i32); 3] {
    let segments = allocate_edges(container.width, &[1.0, 1.0, 1.0]);
    [segments[0], segments[1], segments[2]]
}

/// Resolves `zone` against `container` (typically a display's work area),
/// returning the bounds a window snapped to that zone should take. Spans
/// the container's full height; only the width is divided into thirds.
pub fn snap_to_third(container: Rect, zone: ThirdZone) -> Rect {
    let columns = third_columns(container);
    let (start, end) = match zone {
        ThirdZone::LeftThird => columns[0],
        ThirdZone::CenterThird => columns[1],
        ThirdZone::RightThird => columns[2],
        ThirdZone::LeftTwoThirds => (columns[0].0, columns[1].1),
        ThirdZone::RightTwoThirds => (columns[1].0, columns[2].1),
    };
    Rect::new(
        container.x + start,
        container.y,
        end - start,
        container.height,
    )
}

/// The horizontal direction a zone-snap hotkey cycles in (CONTEXT.md "Zone
/// cycle"). Vertical snapping (top/bottom) never cycles, so it has no
/// counterpart here -- it continues to resolve via [`snap_to_half`] alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HorizontalDirection {
    Left,
    Right,
}

/// A window's position within its zone cycle for a given
/// [`HorizontalDirection`] -- half, then the matching third, then the
/// matching two-thirds, wrapping back to half (CONTEXT.md "Cycle step").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CycleStep {
    Half,
    Third,
    TwoThirds,
}

impl CycleStep {
    /// The step a repeated same-direction press advances to, wrapping from
    /// `TwoThirds` back to `Half` rather than dead-ending.
    pub fn next(self) -> CycleStep {
        match self {
            CycleStep::Half => CycleStep::Third,
            CycleStep::Third => CycleStep::TwoThirds,
            CycleStep::TwoThirds => CycleStep::Half,
        }
    }
}

/// Resolves `direction`'s zone cycle at `step` against `container`
/// (typically a display's work area) -- half on [`CycleStep::Half`], the
/// matching third on [`CycleStep::Third`], the matching two-thirds on
/// [`CycleStep::TwoThirds`] (CONTEXT.md "Zone cycle"). Delegates to
/// [`snap_to_half`]/[`snap_to_third`], so it inherits their container-offset
/// handling and gapless thirds.
pub fn resolve_zone_cycle(container: Rect, direction: HorizontalDirection, step: CycleStep) -> Rect {
    match (direction, step) {
        (HorizontalDirection::Left, CycleStep::Half) => snap_to_half(container, HalfZone::LeftHalf),
        (HorizontalDirection::Right, CycleStep::Half) => snap_to_half(container, HalfZone::RightHalf),
        (HorizontalDirection::Left, CycleStep::Third) => snap_to_third(container, ThirdZone::LeftThird),
        (HorizontalDirection::Right, CycleStep::Third) => snap_to_third(container, ThirdZone::RightThird),
        (HorizontalDirection::Left, CycleStep::TwoThirds) => snap_to_third(container, ThirdZone::LeftTwoThirds),
        (HorizontalDirection::Right, CycleStep::TwoThirds) => snap_to_third(container, ThirdZone::RightTwoThirds),
    }
}

/// Centers a window of `window_size` (width, height) within `container`
/// (typically a display's work area), preserving the window's size unless
/// it's larger than `container` on an axis, in which case that axis is
/// clamped to `container`'s extent so the result never extends past the
/// container's edges (architecture doc section 20, "Focused-window ...
/// center").
pub fn center_on(container: Rect, window_size: (i32, i32)) -> Rect {
    debug_assert!(
        container.width >= 0 && container.height >= 0,
        "container must have non-negative size"
    );
    let (window_width, window_height) = window_size;
    debug_assert!(
        window_width >= 0 && window_height >= 0,
        "window size must be non-negative"
    );

    let width = window_width.max(0).min(container.width.max(0));
    let height = window_height.max(0).min(container.height.max(0));

    let x = container.x + (container.width - width) / 2;
    let y = container.y + (container.height - height) / 2;

    Rect::new(x, y, width, height)
}

/// Resizes/repositions a window to exactly fill `container` (typically a
/// display's work area) -- "maximize to work area" (architecture doc
/// section 20, "Focused-window ... maximize").
pub fn maximize_to_work_area(container: Rect) -> Rect {
    container
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK_AREA: Rect = Rect::new(0, 0, 1920, 1080);

    #[test]
    fn left_half_takes_the_left_half_of_the_container() {
        assert_eq!(
            snap_to_half(WORK_AREA, HalfZone::LeftHalf),
            Rect::new(0, 0, 960, 1080)
        );
    }

    #[test]
    fn right_half_takes_the_right_half_of_the_container() {
        assert_eq!(
            snap_to_half(WORK_AREA, HalfZone::RightHalf),
            Rect::new(960, 0, 960, 1080)
        );
    }

    #[test]
    fn top_half_takes_the_top_half_of_the_container() {
        assert_eq!(
            snap_to_half(WORK_AREA, HalfZone::TopHalf),
            Rect::new(0, 0, 1920, 540)
        );
    }

    #[test]
    fn bottom_half_takes_the_bottom_half_of_the_container() {
        assert_eq!(
            snap_to_half(WORK_AREA, HalfZone::BottomHalf),
            Rect::new(0, 540, 1920, 540)
        );
    }

    #[test]
    fn left_and_right_halves_are_contiguous_on_an_odd_width_container() {
        let container = Rect::new(0, 0, 1921, 1080);
        let left = snap_to_half(container, HalfZone::LeftHalf);
        let right = snap_to_half(container, HalfZone::RightHalf);

        assert_eq!(left.right(), right.x, "halves must share an exact edge");
        assert_eq!(
            left.width + right.width,
            container.width,
            "halves must cover the container exactly"
        );
    }

    #[test]
    fn top_and_bottom_halves_are_contiguous_on_an_odd_height_container() {
        let container = Rect::new(0, 0, 1920, 1081);
        let top = snap_to_half(container, HalfZone::TopHalf);
        let bottom = snap_to_half(container, HalfZone::BottomHalf);

        assert_eq!(top.bottom(), bottom.y, "halves must share an exact edge");
        assert_eq!(
            top.height + bottom.height,
            container.height,
            "halves must cover the container exactly"
        );
    }

    #[test]
    fn snapping_respects_a_container_offset_from_a_work_area_or_secondary_monitor() {
        // e.g. a work area shrunk by a taskbar, or a monitor left of the primary.
        let container = Rect::new(-1920, 40, 1920, 1040);

        assert_eq!(
            snap_to_half(container, HalfZone::LeftHalf),
            Rect::new(-1920, 40, 960, 1040)
        );
        assert_eq!(
            snap_to_half(container, HalfZone::RightHalf),
            Rect::new(-960, 40, 960, 1040)
        );
        assert_eq!(
            snap_to_half(container, HalfZone::TopHalf),
            Rect::new(-1920, 40, 1920, 520)
        );
        assert_eq!(
            snap_to_half(container, HalfZone::BottomHalf),
            Rect::new(-1920, 560, 1920, 520)
        );
    }

    #[test]
    fn top_left_quarter_takes_the_top_left_quarter_of_the_container() {
        assert_eq!(
            snap_to_quarter(WORK_AREA, QuarterZone::TopLeft),
            Rect::new(0, 0, 960, 540)
        );
    }

    #[test]
    fn top_right_quarter_takes_the_top_right_quarter_of_the_container() {
        assert_eq!(
            snap_to_quarter(WORK_AREA, QuarterZone::TopRight),
            Rect::new(960, 0, 960, 540)
        );
    }

    #[test]
    fn bottom_left_quarter_takes_the_bottom_left_quarter_of_the_container() {
        assert_eq!(
            snap_to_quarter(WORK_AREA, QuarterZone::BottomLeft),
            Rect::new(0, 540, 960, 540)
        );
    }

    #[test]
    fn bottom_right_quarter_takes_the_bottom_right_quarter_of_the_container() {
        assert_eq!(
            snap_to_quarter(WORK_AREA, QuarterZone::BottomRight),
            Rect::new(960, 540, 960, 540)
        );
    }

    #[test]
    fn quarters_tile_the_container_exactly_on_an_odd_sized_container() {
        let container = Rect::new(0, 0, 1921, 1081);
        let top_left = snap_to_quarter(container, QuarterZone::TopLeft);
        let top_right = snap_to_quarter(container, QuarterZone::TopRight);
        let bottom_left = snap_to_quarter(container, QuarterZone::BottomLeft);
        let bottom_right = snap_to_quarter(container, QuarterZone::BottomRight);

        assert_eq!(
            top_left.right(),
            top_right.x,
            "top quarters must share an exact edge"
        );
        assert_eq!(
            bottom_left.right(),
            bottom_right.x,
            "bottom quarters must share an exact edge"
        );
        assert_eq!(
            top_left.bottom(),
            bottom_left.y,
            "left quarters must share an exact edge"
        );
        assert_eq!(
            top_right.bottom(),
            bottom_right.y,
            "right quarters must share an exact edge"
        );
        assert_eq!(
            top_left.width + top_right.width,
            container.width,
            "quarters must cover the container's width exactly"
        );
        assert_eq!(
            top_left.height + bottom_left.height,
            container.height,
            "quarters must cover the container's height exactly"
        );
    }

    #[test]
    fn left_third_takes_the_leftmost_third_of_the_container() {
        assert_eq!(
            snap_to_third(WORK_AREA, ThirdZone::LeftThird),
            Rect::new(0, 0, 640, 1080)
        );
    }

    #[test]
    fn center_third_takes_the_middle_third_of_the_container() {
        assert_eq!(
            snap_to_third(WORK_AREA, ThirdZone::CenterThird),
            Rect::new(640, 0, 640, 1080)
        );
    }

    #[test]
    fn right_third_takes_the_rightmost_third_of_the_container() {
        assert_eq!(
            snap_to_third(WORK_AREA, ThirdZone::RightThird),
            Rect::new(1280, 0, 640, 1080)
        );
    }

    #[test]
    fn left_two_thirds_spans_the_left_and_center_thirds() {
        assert_eq!(
            snap_to_third(WORK_AREA, ThirdZone::LeftTwoThirds),
            Rect::new(0, 0, 1280, 1080)
        );
    }

    #[test]
    fn right_two_thirds_spans_the_center_and_right_thirds() {
        assert_eq!(
            snap_to_third(WORK_AREA, ThirdZone::RightTwoThirds),
            Rect::new(640, 0, 1280, 1080)
        );
    }

    #[test]
    fn thirds_are_gapless_and_cover_the_container_exactly_on_a_non_multiple_of_three_width() {
        // 100 doesn't divide evenly by 3 -- exactly the case allocate_edges
        // exists to handle without a rounding gap.
        let container = Rect::new(0, 0, 100, 50);
        let left = snap_to_third(container, ThirdZone::LeftThird);
        let center = snap_to_third(container, ThirdZone::CenterThird);
        let right = snap_to_third(container, ThirdZone::RightThird);

        assert_eq!(
            left.right(),
            center.x,
            "left and center thirds must share an exact edge"
        );
        assert_eq!(
            center.right(),
            right.x,
            "center and right thirds must share an exact edge"
        );
        assert_eq!(left.x, container.x);
        assert_eq!(right.right(), container.right());
        assert_eq!(
            left.width + center.width + right.width,
            container.width,
            "thirds must cover the container's width exactly, not fall short by rounding"
        );
    }

    #[test]
    fn snap_to_third_respects_a_container_offset() {
        let container = Rect::new(-1920, 40, 1920, 1040);

        assert_eq!(
            snap_to_third(container, ThirdZone::LeftThird),
            Rect::new(-1920, 40, 640, 1040)
        );
        assert_eq!(
            snap_to_third(container, ThirdZone::RightTwoThirds),
            Rect::new(-1280, 40, 1280, 1040)
        );
    }

    #[test]
    fn center_on_keeps_window_size_and_centers_it_in_the_container() {
        assert_eq!(
            center_on(WORK_AREA, (800, 600)),
            Rect::new(560, 240, 800, 600)
        );
    }

    #[test]
    fn center_on_truncates_toward_the_top_left_when_the_remainder_is_odd() {
        let container = Rect::new(0, 0, 1921, 1081);
        assert_eq!(
            center_on(container, (800, 600)),
            Rect::new(560, 240, 800, 600)
        );
    }

    #[test]
    fn center_on_clamps_a_window_larger_than_the_container_to_the_container() {
        assert_eq!(
            center_on(WORK_AREA, (3000, 2000)),
            Rect::new(0, 0, 1920, 1080),
            "a window that doesn't fit on an axis should fill that axis rather than overflow it"
        );
    }

    #[test]
    fn center_on_respects_a_container_offset() {
        let container = Rect::new(-1920, 40, 1920, 1040);
        assert_eq!(
            center_on(container, (800, 600)),
            Rect::new(-1360, 260, 800, 600)
        );
    }

    #[test]
    fn maximize_to_work_area_fills_the_container_exactly() {
        let container = Rect::new(-1920, 40, 1920, 1040);
        assert_eq!(maximize_to_work_area(container), container);
    }

    #[test]
    fn resolve_zone_cycle_left_half_matches_snap_to_half() {
        assert_eq!(
            resolve_zone_cycle(WORK_AREA, HorizontalDirection::Left, CycleStep::Half),
            Rect::new(0, 0, 960, 1080)
        );
    }

    #[test]
    fn resolve_zone_cycle_right_half_matches_snap_to_half() {
        assert_eq!(
            resolve_zone_cycle(WORK_AREA, HorizontalDirection::Right, CycleStep::Half),
            Rect::new(960, 0, 960, 1080)
        );
    }

    #[test]
    fn resolve_zone_cycle_left_third_matches_snap_to_third() {
        assert_eq!(
            resolve_zone_cycle(WORK_AREA, HorizontalDirection::Left, CycleStep::Third),
            Rect::new(0, 0, 640, 1080)
        );
    }

    #[test]
    fn resolve_zone_cycle_right_third_matches_snap_to_third() {
        assert_eq!(
            resolve_zone_cycle(WORK_AREA, HorizontalDirection::Right, CycleStep::Third),
            Rect::new(1280, 0, 640, 1080)
        );
    }

    #[test]
    fn resolve_zone_cycle_left_two_thirds_matches_snap_to_third() {
        assert_eq!(
            resolve_zone_cycle(WORK_AREA, HorizontalDirection::Left, CycleStep::TwoThirds),
            Rect::new(0, 0, 1280, 1080)
        );
    }

    #[test]
    fn resolve_zone_cycle_right_two_thirds_matches_snap_to_third() {
        assert_eq!(
            resolve_zone_cycle(WORK_AREA, HorizontalDirection::Right, CycleStep::TwoThirds),
            Rect::new(640, 0, 1280, 1080)
        );
    }

    #[test]
    fn resolve_zone_cycle_respects_a_container_offset() {
        let container = Rect::new(-1920, 40, 1920, 1040);

        assert_eq!(
            resolve_zone_cycle(container, HorizontalDirection::Left, CycleStep::Half),
            Rect::new(-1920, 40, 960, 1040)
        );
        assert_eq!(
            resolve_zone_cycle(container, HorizontalDirection::Right, CycleStep::TwoThirds),
            Rect::new(-1280, 40, 1280, 1040)
        );
    }

    #[test]
    fn resolve_zone_cycle_left_and_right_steps_are_gapless_and_cover_the_container_on_a_non_multiple_of_three_width() {
        let container = Rect::new(0, 0, 100, 50);
        let left_third = resolve_zone_cycle(container, HorizontalDirection::Left, CycleStep::Third);
        let left_two_thirds = resolve_zone_cycle(container, HorizontalDirection::Left, CycleStep::TwoThirds);
        let right_third = resolve_zone_cycle(container, HorizontalDirection::Right, CycleStep::Third);
        let right_two_thirds = resolve_zone_cycle(container, HorizontalDirection::Right, CycleStep::TwoThirds);

        assert_eq!(left_third.x, container.x);
        assert_eq!(left_two_thirds.x, container.x);
        assert_eq!(right_two_thirds.right(), container.right());
        assert_eq!(right_third.right(), container.right());
        assert_eq!(
            left_two_thirds.right(),
            right_third.x,
            "left two-thirds and right third must share an exact edge, not fall short by rounding"
        );
        assert_eq!(
            left_third.right(),
            right_two_thirds.x,
            "left third and right two-thirds must share an exact edge, not fall short by rounding"
        );
    }

    #[test]
    fn cycle_step_advances_half_to_third_to_two_thirds_and_wraps_back_to_half() {
        assert_eq!(CycleStep::Half.next(), CycleStep::Third);
        assert_eq!(CycleStep::Third.next(), CycleStep::TwoThirds);
        assert_eq!(CycleStep::TwoThirds.next(), CycleStep::Half);
    }
}
