//! Deterministic, aspect-aware Balanced grid planning.

use mosaix_domain::{allocate_edges, Rect};

/// Plans `members` cells that exactly cover `work_area` before gaps.
///
/// Rows are distributed as evenly as possible, so a non-factorable count
/// grows occupied cells instead of reserving an empty grid slot. Column
/// count is selected by minimizing the difference between a cell's aspect
/// ratio and a square, which naturally prefers more columns on wide work
/// areas and more rows on tall ones.
pub fn plan_balanced_grid(work_area: Rect, members: usize) -> Vec<Rect> {
    if members == 0 || work_area.width <= 0 || work_area.height <= 0 {
        return Vec::new();
    }

    let rows = (1..=members)
        .min_by(|&left, &right| {
            candidate_score(work_area, members, left)
                .0
                .total_cmp(&candidate_score(work_area, members, right).0)
                .then_with(|| {
                    candidate_score(work_area, members, left)
                        .1
                        .total_cmp(&candidate_score(work_area, members, right).1)
                })
                .then_with(|| left.cmp(&right))
        })
        .expect("non-empty range");
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

/// Returns the specification's lexicographic quality tuple: worst cell
/// distortion first, then total distortion. A final smaller-row-count tie
/// break is applied by the caller.
fn candidate_score(work_area: Rect, members: usize, rows: usize) -> (f64, f64) {
    let base = members / rows;
    let extra = members % rows;
    let row_height = work_area.height as f64 / rows as f64;
    let mut worst = 0.0_f64;
    let mut total = 0.0_f64;

    for row in 0..rows {
        let columns = base + usize::from(row < extra);
        let cell_width = work_area.width as f64 / columns as f64;
        let distortion = (cell_width / row_height).ln().abs();
        worst = worst.max(distortion);
        total += distortion * columns as f64;
    }

    (worst, total)
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
        let cells = plan_balanced_grid(Rect::new(0, 0, 1920, 1080), 4);

        assert_eq!(
            cells,
            vec![
                Rect::new(0, 0, 960, 540),
                Rect::new(960, 0, 960, 540),
                Rect::new(0, 540, 960, 540),
                Rect::new(960, 540, 960, 540),
            ]
        );
    }

    #[test]
    fn balanced_grid_prefers_rows_on_a_portrait_display() {
        let cells = plan_balanced_grid(Rect::new(0, 0, 900, 1600), 3);

        assert_eq!(cells.len(), 3);
        assert!(cells.iter().all(|cell| cell.width == 900));
        assert_eq!(cells.first().unwrap().y, 0);
        assert_eq!(cells.last().unwrap().bottom(), 1600);
    }

    #[test]
    fn balanced_grid_properties_hold_for_zero_to_one_hundred_members() {
        let work_areas = [
            Rect::new(0, 0, 1920, 1080),
            Rect::new(0, 0, 1080, 1920),
            Rect::new(-1601, -17, 1601, 901),
            Rect::new(3, 5, 17, 11),
        ];

        for work_area in work_areas {
            for members in 0..=100 {
                let cells = plan_balanced_grid(work_area, members);
                assert_eq!(cells.len(), members);
                assert!(cells.iter().all(|cell| work_area.contains(cell)));
                assert!(cells.iter().all(|cell| cell.has_positive_area()));

                for (index, cell) in cells.iter().enumerate() {
                    for other in &cells[index + 1..] {
                        let overlap_width = cell.right().min(other.right()) - cell.x.max(other.x);
                        let overlap_height =
                            cell.bottom().min(other.bottom()) - cell.y.max(other.y);
                        assert!(
                            overlap_width <= 0 || overlap_height <= 0,
                            "cells overlap for {members} members in {work_area:?}: {cell:?} and {other:?}"
                        );
                    }
                }

                let area: i64 = cells
                    .iter()
                    .map(|cell| i64::from(cell.width) * i64::from(cell.height))
                    .sum();
                assert_eq!(
                    area,
                    if members == 0 {
                        0
                    } else {
                        i64::from(work_area.width) * i64::from(work_area.height)
                    },
                    "cells do not cover work area for {members} members in {work_area:?}"
                );
            }
        }
    }
}
