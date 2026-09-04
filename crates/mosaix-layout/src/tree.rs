//! The pure container-tree planner and its insertion policy.
//!
//! Both are total functions of their inputs: no clock, no hash iteration,
//! no reducer state. Given the same tree and the same work area they
//! produce the same placements, byte for byte, which is what lets the
//! properties below be asserted rather than sampled hopefully.
//!
//! The planner guarantees, for any tree the domain considers normalized:
//! every leaf is placed exactly once, no two placements overlap, every
//! placement lies inside the work area, and with zero gaps the placements
//! tile the work area exactly.

use mosaix_domain::tree::{ContainerTree, Node, SplitAxis};
use mosaix_domain::{allocate_edges, Gaps, Rect, WindowId};

use crate::zones::apply_gaps;

/// Where a new window goes: which existing window's space it takes half
/// of, and which way that space is divided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Insertion {
    pub target: WindowId,
    pub axis: SplitAxis,
}

/// Places every window in `tree` inside `work_area`.
///
/// Gaps are applied last, and are reduced together toward zero rather than
/// allowed to consume a window: a display too small for the configured
/// decoration loses the decoration first (spec user story 43).
pub fn plan_tree(tree: &ContainerTree, work_area: Rect, gaps: Gaps) -> Vec<(WindowId, Rect)> {
    let raw = plan_tree_raw(tree, work_area);
    if raw.is_empty() {
        return raw;
    }
    let usable = usable_gaps(&raw, work_area, gaps);
    raw.into_iter()
        .map(|(window_id, rect)| (window_id, apply_gaps(rect, work_area, usable)))
        .collect()
}

/// The tiling before gaps: contiguous rectangles that exactly cover
/// `work_area`. Exposed because the properties that matter -- coverage and
/// non-overlap -- are properties of this, and gaps only ever shrink each
/// rectangle inside its own cell.
pub fn plan_tree_raw(tree: &ContainerTree, work_area: Rect) -> Vec<(WindowId, Rect)> {
    let mut placements = Vec::with_capacity(tree.len());
    if work_area.width <= 0 || work_area.height <= 0 {
        return placements;
    }
    if let Some(root) = tree.root() {
        tile(root, work_area, &mut placements);
    }
    placements
}

/// The largest fraction of `gaps` that leaves every cell with positive
/// area. Whole steps rather than a continuous search, so the result is
/// stable against float drift and easy to reason about.
fn usable_gaps(placements: &[(WindowId, Rect)], work_area: Rect, gaps: Gaps) -> Gaps {
    const STEPS: i32 = 8;
    for step in (0..=STEPS).rev() {
        let candidate = Gaps::new(gaps.outer * step / STEPS, gaps.inner * step / STEPS);
        let fits = placements.iter().all(|(_, rect)| {
            apply_gaps(*rect, work_area, candidate).has_positive_area()
        });
        if fits {
            return candidate;
        }
    }
    Gaps::new(0, 0)
}

fn tile(node: &Node<WindowId>, area: Rect, placements: &mut Vec<(WindowId, Rect)>) {
    match node {
        Node::Leaf(window_id) => placements.push((*window_id, area)),
        Node::Split { axis, children } => {
            let weights: Vec<f64> = children.iter().map(|child| child.weight).collect();
            if weights.is_empty() {
                return;
            }
            let extent = match axis {
                SplitAxis::Horizontal => area.width,
                SplitAxis::Vertical => area.height,
            };
            // `allocate_edges` is the workspace's one edge-allocation rule:
            // boundaries come from cumulative weight, so adjacent children
            // share an edge exactly rather than gapping or overlapping by a
            // rounded pixel.
            for (child, (start, end)) in children.iter().zip(allocate_edges(extent, &weights)) {
                let child_area = match axis {
                    SplitAxis::Horizontal => {
                        Rect::new(area.x + start, area.y, end - start, area.height)
                    }
                    SplitAxis::Vertical => {
                        Rect::new(area.x, area.y + start, area.width, end - start)
                    }
                };
                tile(&child.node, child_area, placements);
            }
        }
    }
}

/// Chooses the leaf a new window splits, from the arrangement as planned.
///
/// The focused window's leaf is the target when there is one, so insertion
/// follows attention. Without one, the largest leaf is split, and equal
/// areas are broken by visual order -- `placements` is in tree order, which
/// is that order -- so the result never depends on enumeration or hashing
/// (spec user stories 29 and 30).
pub fn choose_insertion(
    placements: &[(WindowId, Rect)],
    focused: Option<WindowId>,
) -> Option<Insertion> {
    let target = focused
        .filter(|window_id| {
            placements
                .iter()
                .any(|(candidate, _)| candidate == window_id)
        })
        .or_else(|| largest_leaf(placements))?;
    let rect = placements
        .iter()
        .find(|(window_id, _)| *window_id == target)
        .map(|(_, rect)| *rect)?;
    Some(Insertion {
        target,
        axis: longer_axis(rect),
    })
}

fn largest_leaf(placements: &[(WindowId, Rect)]) -> Option<WindowId> {
    placements
        .iter()
        .enumerate()
        // `max_by_key` keeps the last maximum, so the visual-order
        // tie-break is expressed by preferring the smaller index.
        .max_by_key(|(index, (_, rect))| {
            (
                (rect.width as i64) * (rect.height as i64),
                std::cmp::Reverse(*index),
            )
        })
        .map(|(_, (window_id, _))| *window_id)
}

/// Splitting "on the longer axis" halves the longer dimension: a wide
/// window becomes two side by side, a tall one becomes two stacked. A
/// square window splits side by side, which matches how displays are
/// usually shaped.
const fn longer_axis(rect: Rect) -> SplitAxis {
    if rect.width >= rect.height {
        SplitAxis::Horizontal
    } else {
        SplitAxis::Vertical
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mosaix_domain::tree::{Child, Tree};

    const WORK_AREA: Rect = Rect::new(0, 0, 1920, 1080);

    fn tree_of(ids: &[isize]) -> ContainerTree {
        let mut tree = ContainerTree::new();
        let Some((first, rest)) = ids.split_first() else {
            return tree;
        };
        tree.insert_first(WindowId(*first));
        for id in rest {
            let placements = plan_tree_raw(&tree, WORK_AREA);
            let insertion =
                choose_insertion(&placements, None).expect("a non-empty tree has a target");
            tree.split_leaf(&insertion.target, insertion.axis, WindowId(*id));
        }
        tree
    }

    fn overlaps(left: Rect, right: Rect) -> bool {
        left.x < right.right()
            && right.x < left.right()
            && left.y < right.bottom()
            && right.y < left.bottom()
    }

    /// A deterministic generator, so the properties below run over many
    /// shapes without a random seed making a failure unreproducible.
    fn shaped_tree(seed: u64, leaves: usize) -> ContainerTree {
        let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let mut next = move || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as usize
        };
        let mut tree = ContainerTree::new();
        tree.insert_first(WindowId(0));
        for id in 1..leaves as isize {
            let existing: Vec<WindowId> = tree.leaves().into_iter().copied().collect();
            let target = existing[next() % existing.len()];
            let axis = if next() % 2 == 0 {
                SplitAxis::Horizontal
            } else {
                SplitAxis::Vertical
            };
            tree.split_leaf(&target, axis, WindowId(id));
        }
        tree
    }

    #[test]
    fn an_empty_tree_places_nothing() {
        assert!(plan_tree(&ContainerTree::new(), WORK_AREA, Gaps::new(0, 0)).is_empty());
    }

    #[test]
    fn one_window_fills_the_work_area() {
        let tree = tree_of(&[1]);

        assert_eq!(
            plan_tree(&tree, WORK_AREA, Gaps::new(0, 0)),
            vec![(WindowId(1), WORK_AREA)]
        );
    }

    #[test]
    fn a_second_window_takes_half_of_a_wide_display_side_by_side() {
        let tree = tree_of(&[1, 2]);

        assert_eq!(
            plan_tree(&tree, WORK_AREA, Gaps::new(0, 0)),
            vec![
                (WindowId(1), Rect::new(0, 0, 960, 1080)),
                (WindowId(2), Rect::new(960, 0, 960, 1080)),
            ]
        );
    }

    #[test]
    fn a_third_window_splits_only_the_leaf_it_lands_in() {
        // The two existing windows are 960x1080, so the larger dimension is
        // height and the split stacks. The other half is untouched.
        let tree = tree_of(&[1, 2, 3]);

        assert_eq!(
            plan_tree(&tree, WORK_AREA, Gaps::new(0, 0)),
            vec![
                (WindowId(1), Rect::new(0, 0, 960, 540)),
                (WindowId(3), Rect::new(0, 540, 960, 540)),
                (WindowId(2), Rect::new(960, 0, 960, 1080)),
            ]
        );
    }

    #[test]
    fn insertion_follows_focus_when_a_tiled_window_has_it() {
        let tree = tree_of(&[1, 2]);
        let placements = plan_tree_raw(&tree, WORK_AREA);

        let insertion = choose_insertion(&placements, Some(WindowId(2))).expect("a target");

        assert_eq!(insertion.target, WindowId(2));
        assert_eq!(
            insertion.axis,
            SplitAxis::Vertical,
            "a 960x1080 leaf is taller than it is wide, so it stacks"
        );
    }

    #[test]
    fn focus_on_an_untiled_window_falls_back_to_the_largest_leaf() {
        let mut tree = ContainerTree::new();
        tree.insert_first(WindowId(1));
        tree.split_leaf(&WindowId(1), SplitAxis::Horizontal, WindowId(2));
        tree.split_leaf(&WindowId(2), SplitAxis::Vertical, WindowId(3));
        let placements = plan_tree_raw(&tree, WORK_AREA);

        let insertion =
            choose_insertion(&placements, Some(WindowId(404))).expect("a fallback target");

        assert_eq!(
            insertion.target,
            WindowId(1),
            "window 1 still holds a full half; the other two share the rest"
        );
    }

    #[test]
    fn equal_largest_leaves_are_broken_by_visual_order_not_by_id() {
        let tree = tree_of(&[7, 3]);
        let placements = plan_tree_raw(&tree, WORK_AREA);
        assert_eq!(placements[0].1.width * placements[0].1.height, 960 * 1080);
        assert_eq!(placements[1].1.width * placements[1].1.height, 960 * 1080);

        let insertion = choose_insertion(&placements, None).expect("a target");

        assert_eq!(
            insertion.target,
            WindowId(7),
            "the first in visual order wins a tie, regardless of window id"
        );
    }

    #[test]
    fn gaps_inset_every_window_and_shrink_none_of_them_away() {
        let tree = tree_of(&[1, 2]);

        let placements = plan_tree(&tree, WORK_AREA, Gaps::new(10, 4));

        assert_eq!(
            placements,
            vec![
                (WindowId(1), Rect::new(10, 10, 946, 1060)),
                (WindowId(2), Rect::new(964, 10, 946, 1060)),
            ]
        );
    }

    #[test]
    fn gaps_are_reduced_rather_than_letting_a_window_vanish() {
        // Eight windows in a strip far too narrow for a 200px gap.
        let tree = tree_of(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let cramped = Rect::new(0, 0, 400, 300);

        let placements = plan_tree(&tree, cramped, Gaps::new(200, 200));

        assert_eq!(placements.len(), 8);
        assert!(
            placements.iter().all(|(_, rect)| rect.has_positive_area()),
            "decoration is sacrificed before a window is: {placements:?}"
        );
    }

    // ---- properties -------------------------------------------------

    #[test]
    fn every_leaf_is_placed_exactly_once() {
        for seed in 0..40u64 {
            let leaves = 1 + (seed as usize % 12);
            let tree = shaped_tree(seed, leaves);
            let placements = plan_tree(&tree, WORK_AREA, Gaps::new(6, 3));

            let mut placed: Vec<isize> =
                placements.iter().map(|(window_id, _)| window_id.0).collect();
            placed.sort_unstable();
            let mut expected: Vec<isize> = tree.leaves().into_iter().map(|id| id.0).collect();
            expected.sort_unstable();
            assert_eq!(placed, expected, "seed {seed}");
        }
    }

    #[test]
    fn placements_never_overlap() {
        for seed in 0..40u64 {
            let leaves = 1 + (seed as usize % 12);
            let tree = shaped_tree(seed, leaves);
            let placements = plan_tree(&tree, WORK_AREA, Gaps::new(6, 3));

            for (index, (_, left)) in placements.iter().enumerate() {
                for (_, right) in placements.iter().skip(index + 1) {
                    assert!(
                        !overlaps(*left, *right),
                        "seed {seed}: {left:?} overlaps {right:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn every_placement_stays_inside_the_work_area() {
        for seed in 0..40u64 {
            let leaves = 1 + (seed as usize % 12);
            let tree = shaped_tree(seed, leaves);

            for area in [WORK_AREA, Rect::new(-1920, -200, 1280, 1024), Rect::new(3, 7, 801, 603)] {
                for (_, rect) in plan_tree(&tree, area, Gaps::new(6, 3)) {
                    assert!(
                        rect.x >= area.x
                            && rect.y >= area.y
                            && rect.right() <= area.right()
                            && rect.bottom() <= area.bottom(),
                        "seed {seed}: {rect:?} escapes {area:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn without_gaps_the_placements_cover_the_work_area_exactly() {
        for seed in 0..40u64 {
            let leaves = 1 + (seed as usize % 12);
            let tree = shaped_tree(seed, leaves);

            let covered: i64 = plan_tree(&tree, WORK_AREA, Gaps::new(0, 0))
                .iter()
                .map(|(_, rect)| (rect.width as i64) * (rect.height as i64))
                .sum();

            assert_eq!(
                covered,
                (WORK_AREA.width as i64) * (WORK_AREA.height as i64),
                "seed {seed}: a gapless tiling must leave no pixel unassigned"
            );
        }
    }

    #[test]
    fn planning_is_deterministic() {
        for seed in 0..40u64 {
            let tree = shaped_tree(seed, 1 + (seed as usize % 12));
            let first = plan_tree(&tree, WORK_AREA, Gaps::new(6, 3));
            let second = plan_tree(&tree, WORK_AREA, Gaps::new(6, 3));
            assert_eq!(first, second, "seed {seed}");
        }
    }

    #[test]
    fn odd_dimensions_and_one_pixel_boundaries_still_tile_exactly() {
        let tree = tree_of(&[1, 2, 3]);
        for area in [
            Rect::new(0, 0, 3, 3),
            Rect::new(0, 0, 1, 1),
            Rect::new(-7, -5, 101, 99),
        ] {
            let placements = plan_tree_raw(&tree, area);
            let covered: i64 = placements
                .iter()
                .map(|(_, rect)| (rect.width as i64) * (rect.height as i64))
                .sum();
            assert_eq!(
                covered,
                (area.width as i64) * (area.height as i64),
                "{area:?} was not exactly covered"
            );
        }
    }

    #[test]
    fn a_degenerate_work_area_places_nothing_rather_than_placing_nonsense() {
        let tree = tree_of(&[1, 2]);

        assert!(plan_tree(&tree, Rect::new(0, 0, 0, 1080), Gaps::new(0, 0)).is_empty());
        assert!(plan_tree(&tree, Rect::new(0, 0, 1920, -4), Gaps::new(0, 0)).is_empty());
    }

    #[test]
    fn normalization_does_not_oscillate() {
        for seed in 0..40u64 {
            let tree = shaped_tree(seed, 1 + (seed as usize % 12));
            let mut once = tree.clone();
            once.normalize();
            let mut twice = once.clone();
            twice.normalize();
            assert_eq!(once, twice, "seed {seed}: normalizing is idempotent");
        }
    }

    #[test]
    fn a_hand_built_unbalanced_tree_plans_the_weights_it_declares() {
        let tree = Tree::from_root(Node::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                Child {
                    weight: 3.0,
                    node: Node::Leaf(WindowId(1)),
                },
                Child {
                    weight: 1.0,
                    node: Node::Leaf(WindowId(2)),
                },
            ],
        });

        assert_eq!(
            plan_tree(&tree, WORK_AREA, Gaps::new(0, 0)),
            vec![
                (WindowId(1), Rect::new(0, 0, 1440, 1080)),
                (WindowId(2), Rect::new(1440, 0, 480, 1080)),
            ]
        );
    }
}
