//! Saved-layout planner: turning a saved layout's normalized cells into
//! concrete rectangles for one display's work area.
//!
//! Sits alongside the zone planner in [`crate::zones`] and shares its
//! rules: cells resolve through [`NormalizedRect::to_rect`], the one
//! deterministic edge-allocation algorithm, and gap insetting stays a
//! separate post-processing step -- nothing here knows about
//! [`crate::apply_gaps`].
//!
//! Unlike a balanced grid, a saved layout's cells come from the user, so
//! this planner makes no claim that they tile the work area: they may
//! leave it partly uncovered, and they may overlap.

use mosaix_domain::{NormalizedRect, Rect};

/// Resolves each of `cells` against `work_area`, in order.
///
/// Cell *i* of the result is cell *i* of the layout, so a caller can zip
/// the result straight against the windows it means to place. An empty
/// cell list resolves to an empty plan rather than an error: rejecting an
/// empty layout, or one whose cells fall outside 0.0-1.0, belongs in
/// `mosaix-config`'s validation where the file and line are known. The
/// planner stays total either way.
pub fn resolve_saved_layout(work_area: Rect, cells: &[NormalizedRect]) -> Vec<Rect> {
    cells.iter().map(|cell| cell.to_rect(work_area)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(x: f64, y: f64, width: f64, height: f64) -> NormalizedRect {
        NormalizedRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn cells_resolve_to_rectangles_in_declaration_order() {
        let work_area = Rect::new(0, 0, 1920, 1080);

        let plan = resolve_saved_layout(
            work_area,
            &[
                cell(0.0, 0.0, 0.6, 1.0),
                cell(0.6, 0.0, 0.4, 0.5),
                cell(0.6, 0.5, 0.4, 0.5),
            ],
        );

        assert_eq!(
            plan,
            vec![
                Rect::new(0, 0, 1152, 1080),
                Rect::new(1152, 0, 768, 540),
                Rect::new(1152, 540, 768, 540),
            ]
        );
    }

    #[test]
    fn adjacent_cells_sharing_a_fraction_meet_exactly_on_an_odd_work_area() {
        let work_area = Rect::new(0, 0, 1365, 767);

        let plan = resolve_saved_layout(
            work_area,
            &[cell(0.0, 0.0, 0.5, 1.0), cell(0.5, 0.0, 0.5, 1.0)],
        );

        assert_eq!(plan[0].right(), plan[1].x, "no gap or overlap at the seam");
        assert_eq!(plan[1].right(), work_area.right());
    }

    #[test]
    fn cells_resolve_against_a_work_area_with_negative_coordinates() {
        let work_area = Rect::new(-1920, -1080, 1920, 1080);

        let plan = resolve_saved_layout(work_area, &[cell(0.5, 0.5, 0.5, 0.5)]);

        assert_eq!(plan, vec![Rect::new(-960, -540, 960, 540)]);
    }

    #[test]
    fn cells_resolve_against_a_scaled_work_area_offset_by_a_neighbouring_display() {
        // A 150%-scaled 2560x1440 panel to the right of a 1920-wide primary,
        // with a taskbar-sized work-area inset at the top.
        let work_area = Rect::new(1920, 40, 2560, 1400);

        let plan = resolve_saved_layout(work_area, &[cell(0.25, 0.0, 0.5, 1.0)]);

        assert_eq!(plan, vec![Rect::new(2560, 40, 1280, 1400)]);
    }

    #[test]
    fn an_empty_cell_list_resolves_to_an_empty_plan() {
        assert!(resolve_saved_layout(Rect::new(0, 0, 800, 600), &[]).is_empty());
    }

    #[test]
    fn cells_that_do_not_cover_the_work_area_are_resolved_as_written() {
        let plan = resolve_saved_layout(Rect::new(0, 0, 1000, 1000), &[cell(0.1, 0.1, 0.2, 0.2)]);

        assert_eq!(plan, vec![Rect::new(100, 100, 200, 200)]);
    }
}
