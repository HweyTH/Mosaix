//! Moving a window between displays: cycling to the adjacent monitor and
//! preserving a window's zone ratio when it lands there (architecture doc
//! section 20, "next-display" command).
//!
//! Both helpers here are stateless, like [`crate::zones`] -- they take the
//! current topology and bounds as input and return where the window should
//! go, leaving *tracking* the window's placement (so it can later be
//! restored) to whatever owns window state, per [`crate::zones`]'s module
//! docs.

use mosaix_domain::{Display, DisplayId, NormalizedRect, Rect};

/// Direction to cycle displays in, ordered left-to-right then top-to-bottom
/// by `full_bounds` origin (architecture doc section 20, "next-display"
/// command).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayDirection {
    Next,
    Prev,
}

/// Returns the display adjacent to `current` in `direction`, cycling
/// through `displays` ordered left-to-right then top-to-bottom by
/// `full_bounds` origin (ties broken by [`DisplayId`] so the order is
/// deterministic even for displays that report identical bounds). Wraps
/// around at either end.
///
/// Returns `None` if `current` isn't among `displays`, or if there's only
/// one display -- nothing to cycle to.
pub fn cycle_display(
    displays: &[Display],
    current: DisplayId,
    direction: DisplayDirection,
) -> Option<DisplayId> {
    if displays.len() < 2 {
        return None;
    }

    let mut ordered: Vec<&Display> = displays.iter().collect();
    ordered.sort_by_key(|d| (d.full_bounds.x, d.full_bounds.y, d.id.0));

    let index = ordered.iter().position(|d| d.id == current)?;
    let offset = match direction {
        DisplayDirection::Next => 1,
        DisplayDirection::Prev => ordered.len() - 1,
    };
    let next_index = (index + offset) % ordered.len();
    Some(ordered[next_index].id)
}

/// Moves `bounds` from `from_container` to `to_container`, preserving its
/// position and size as fractions of the container -- so a window snapped
/// to a half, quarter, or third zone on one display lands in the
/// equivalent zone on the other, and an unsnapped window keeps its
/// relative position and size (architecture doc section 20, "next-display"
/// command).
///
/// Resolved through [`NormalizedRect`], so a window that doesn't fit
/// `to_container` on an axis is clamped to it rather than hanging off the
/// edge, the same as every zone resolution in [`crate::zones`].
pub fn throw_preserving_ratio(bounds: Rect, from_container: Rect, to_container: Rect) -> Rect {
    NormalizedRect::from_rect(bounds, from_container).to_rect(to_container)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mosaix_domain::Rotation;

    fn display(id: isize, bounds: Rect) -> Display {
        Display {
            id: DisplayId(id),
            stable_fingerprint: format!("MON-{id}"),
            full_bounds: bounds,
            work_area: bounds,
            scale_factor: 1.0,
            rotation: Rotation::Landscape,
            is_primary: id == 1,
        }
    }

    #[test]
    fn cycle_display_next_moves_to_the_display_on_the_right() {
        let left = display(1, Rect::new(0, 0, 1920, 1080));
        let right = display(2, Rect::new(1920, 0, 1920, 1080));

        assert_eq!(
            cycle_display(&[left, right], DisplayId(1), DisplayDirection::Next),
            Some(DisplayId(2))
        );
    }

    #[test]
    fn cycle_display_prev_moves_to_the_display_on_the_left() {
        let left = display(1, Rect::new(0, 0, 1920, 1080));
        let right = display(2, Rect::new(1920, 0, 1920, 1080));

        assert_eq!(
            cycle_display(&[left, right], DisplayId(2), DisplayDirection::Prev),
            Some(DisplayId(1))
        );
    }

    #[test]
    fn cycle_display_next_wraps_around_from_the_rightmost_display() {
        let left = display(1, Rect::new(0, 0, 1920, 1080));
        let right = display(2, Rect::new(1920, 0, 1920, 1080));

        assert_eq!(
            cycle_display(&[left, right], DisplayId(2), DisplayDirection::Next),
            Some(DisplayId(1))
        );
    }

    #[test]
    fn cycle_display_prev_wraps_around_from_the_leftmost_display() {
        let left = display(1, Rect::new(0, 0, 1920, 1080));
        let right = display(2, Rect::new(1920, 0, 1920, 1080));

        assert_eq!(
            cycle_display(&[left, right], DisplayId(1), DisplayDirection::Prev),
            Some(DisplayId(2))
        );
    }

    #[test]
    fn cycle_display_orders_by_position_not_input_order_or_id() {
        // Deliberately out of position order and with descending ids, so a
        // naive "just walk the input slice" or "sort by id" implementation
        // would get this wrong.
        let right = display(1, Rect::new(1920, 0, 1920, 1080));
        let left = display(2, Rect::new(0, 0, 1920, 1080));

        assert_eq!(
            cycle_display(&[right, left], DisplayId(2), DisplayDirection::Next),
            Some(DisplayId(1)),
            "the physically-left display (id 2) should cycle to the physically-right one (id 1)"
        );
    }

    #[test]
    fn cycle_display_orders_ties_on_x_by_y_then_by_id() {
        let bottom = display(1, Rect::new(0, 1080, 1920, 1080));
        let top = display(2, Rect::new(0, 0, 1920, 1080));

        assert_eq!(
            cycle_display(&[bottom, top], DisplayId(2), DisplayDirection::Next),
            Some(DisplayId(1)),
            "the physically-top display should cycle to the physically-bottom one"
        );
    }

    #[test]
    fn cycle_display_cycles_through_three_displays_in_order() {
        let a = display(1, Rect::new(0, 0, 1920, 1080));
        let b = display(2, Rect::new(1920, 0, 1920, 1080));
        let c = display(3, Rect::new(3840, 0, 1920, 1080));
        let displays = [a, b, c];

        assert_eq!(
            cycle_display(&displays, DisplayId(1), DisplayDirection::Next),
            Some(DisplayId(2))
        );
        assert_eq!(
            cycle_display(&displays, DisplayId(2), DisplayDirection::Next),
            Some(DisplayId(3))
        );
        assert_eq!(
            cycle_display(&displays, DisplayId(3), DisplayDirection::Next),
            Some(DisplayId(1))
        );
    }

    #[test]
    fn cycle_display_returns_none_for_a_single_display() {
        let only = display(1, Rect::new(0, 0, 1920, 1080));
        assert_eq!(
            cycle_display(&[only], DisplayId(1), DisplayDirection::Next),
            None
        );
    }

    #[test]
    fn cycle_display_returns_none_when_current_is_not_in_the_topology() {
        let left = display(1, Rect::new(0, 0, 1920, 1080));
        let right = display(2, Rect::new(1920, 0, 1920, 1080));

        assert_eq!(
            cycle_display(&[left, right], DisplayId(99), DisplayDirection::Next),
            None
        );
    }

    #[test]
    fn throw_preserving_ratio_keeps_a_snapped_left_half_as_a_left_half_on_the_target() {
        let from = Rect::new(0, 0, 1920, 1080);
        let to = Rect::new(1920, 0, 2560, 1440);
        let left_half_on_from = Rect::new(0, 0, 960, 1080);

        assert_eq!(
            throw_preserving_ratio(left_half_on_from, from, to),
            Rect::new(1920, 0, 1280, 1440),
            "half-width, full-height on the source should stay half-width, full-height on the target"
        );
    }

    #[test]
    fn throw_preserving_ratio_keeps_an_unsnapped_windows_relative_position_and_size() {
        let from = Rect::new(0, 0, 1920, 1080);
        let to = Rect::new(1920, 0, 1920, 1080);
        // 25% in from the top-left, 50% of the container on each axis.
        let bounds = Rect::new(480, 270, 960, 540);

        assert_eq!(
            throw_preserving_ratio(bounds, from, to),
            Rect::new(2400, 270, 960, 540)
        );
    }

    #[test]
    fn throw_preserving_ratio_clamps_a_window_too_large_for_the_target() {
        let from = Rect::new(0, 0, 2560, 1440);
        let to = Rect::new(2560, 0, 1280, 720);
        let bounds = Rect::new(0, 0, 2560, 1440);

        assert_eq!(
            throw_preserving_ratio(bounds, from, to),
            to,
            "a window filling the source should fill (not overflow) a smaller target"
        );
    }

    #[test]
    fn throw_preserving_ratio_respects_a_container_offset_on_both_ends() {
        let from = Rect::new(-1920, 40, 1920, 1040);
        let to = Rect::new(0, 0, 1920, 1080);
        let right_half_on_from = Rect::new(-960, 40, 960, 1040);

        assert_eq!(
            throw_preserving_ratio(right_half_on_from, from, to),
            Rect::new(960, 0, 960, 1080)
        );
    }
}
