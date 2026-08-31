//! Deterministic, aspect-aware Balanced grid planning.

use mosaix_domain::{allocate_edges, Rect};

/// Plans `members` cells that exactly cover `work_area` before gaps.
///
/// Rows are distributed as evenly as possible, so a non-factorable count
/// grows occupied cells instead of reserving an empty grid slot.  Column
/// count is selected by minimizing the difference between a cell's aspect
/// ratio and a square, which naturally prefers more columns on wide work
/// areas and more rows on tall ones.
pub fn plan_balanced_grid(work_area: Rect, members: usize) -> Vec<Rect> {
    if members == 0 || work_area.width <= 0 || work_area.height <= 0 {
        return Vec::new();
    }

    let columns = (1..=members)
        .min_by(|&left, &right| {
            let score = |columns: usize| {
                let rows = members.div_ceil(columns);
                let aspect = work_area.width as f64
                    / columns as f64
                    / (work_area.height as f64 / rows as f64);
                aspect.ln().abs()
            };
            score(left)
                .total_cmp(&score(right))
                .then_with(|| left.cmp(&right))
        })
        .expect("non-empty range");
    let rows = members.div_ceil(columns);
    let row_edges = allocate_edges(work_area.height, &vec![1.0; rows]);
    let base = members / rows;
    let extra = members % rows;

    let mut cells = Vec::with_capacity(members);
    for (row, &(top, bottom)) in row_edges.iter().enumerate() {
        let count = base + usize::from(row < extra);
        let column_edges = allocate_edges(work_area.width, &vec![1.0; count]);
        for (left, right) in column_edges {
            cells.push(Rect::new(
                work_area.x + left,
                work_area.y + top,
                right - left,
                bottom - top,
            ));
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balanced_grid_covers_a_negative_origin_without_empty_cells() {
        let work_area = Rect::new(-101, 7, 101, 99);
        let cells = plan_balanced_grid(work_area, 5);

        assert_eq!(cells.len(), 5);
        assert_eq!(cells[0], Rect::new(-101, 7, 34, 50));
        assert_eq!(cells[4], Rect::new(-50, 57, 50, 49));
        assert!(cells.iter().all(|cell| cell.width > 0 && cell.height > 0));
    }

    #[test]
    fn balanced_grid_prefers_columns_on_a_landscape_display() {
        let cells = plan_balanced_grid(Rect::new(0, 0, 1600, 800), 4);

        assert_eq!(
            cells,
            vec![
                Rect::new(0, 0, 800, 400),
                Rect::new(800, 0, 800, 400),
                Rect::new(0, 400, 800, 400),
                Rect::new(800, 400, 800, 400),
            ]
        );
    }
}
