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
use mosaix_domain::{allocate_edges, Gaps, Rect, Size, WindowId};

use crate::zones::apply_gaps;

/// Where a new window goes: which existing window's space it takes half
/// of, and which way that space is divided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Insertion {
    pub target: WindowId,
    pub axis: SplitAxis,
}

/// What the planner produced for one display: where each arranged
/// window goes, which windows it could not fit, and the gaps it ended up
/// using.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreePlan {
    /// Every arranged window, in visual order, gaps already applied.
    pub placements: Vec<(WindowId, Rect)>,
    /// Windows whose leaves are kept but which the display cannot fit at
    /// their minimum size, newest insertion first (CONTEXT.md
    /// "Constraint-overflow window"). Empty whenever everything fits.
    pub overflow: Vec<WindowId>,
    /// The gaps actually applied: the configured ones when they fit, and
    /// a reduced fraction of them when decoration had to give way.
    pub gaps: Gaps,
}

/// Places every window in `tree` inside `work_area`, with no minimum-size
/// constraints. The shape [`plan_tree_constrained`] takes when nothing is
/// known about any window's minimum.
pub fn plan_tree(tree: &ContainerTree, work_area: Rect, gaps: Gaps) -> Vec<(WindowId, Rect)> {
    plan_tree_constrained(tree, work_area, gaps, |_| None).placements
}

/// Places every window in `tree` inside `work_area` that the display can
/// fit, honouring each window's minimum size.
///
/// The degradation order is fixed (spec user stories 43 and 44). Gaps are
/// reduced together toward zero first, so a display too small for the
/// configured decoration loses the decoration before it loses a window.
/// Only if the windows still cannot fit at zero gaps does the newest
/// inserted window leave the arrangement, and the rest are planned again
/// without it; that repeats until the remainder fits. Established windows
/// therefore keep their places, and the same tree on the same display
/// always overflows the same windows in the same order.
///
/// `minimum_size` answers `None` for a window whose minimum is unknown,
/// which is treated as needing positive area and nothing more -- an
/// unknown minimum is never guessed at.
pub fn plan_tree_constrained(
    tree: &ContainerTree,
    work_area: Rect,
    gaps: Gaps,
    minimum_size: impl Fn(WindowId) -> Option<Size>,
) -> TreePlan {
    let mut arranged = tree.clone();
    let mut overflow = Vec::new();
    loop {
        let raw = plan_tree_raw(&arranged, work_area);
        if raw.is_empty() {
            return TreePlan {
                placements: raw,
                overflow,
                gaps: Gaps::new(0, 0),
            };
        }
        if let Some(usable) = usable_gaps(&raw, work_area, gaps, &minimum_size) {
            return TreePlan {
                placements: raw
                    .into_iter()
                    .map(|(window_id, rect)| (window_id, apply_gaps(rect, work_area, usable)))
                    .collect(),
                overflow,
                gaps: usable,
            };
        }
        // Nothing fits even undecorated: the newest window gives its
        // space back. Visual order breaks a tie in insertion number, later
        // counting as newer, so the choice never depends on iteration.
        let Some(newest) = arranged
            .leaves()
            .into_iter()
            .enumerate()
            .max_by_key(|(index, leaf)| (leaf.inserted, *index))
            .map(|(_, leaf)| leaf.window)
        else {
            return TreePlan {
                placements: Vec::new(),
                overflow,
                gaps: Gaps::new(0, 0),
            };
        };
        arranged =
            arranged.filter_map_windows(&mut |window| (*window != newest).then_some(*window));
        overflow.push(newest);
    }
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

/// The largest fraction of `gaps` that leaves every cell at or above its
/// window's minimum size, or `None` when not even zero gaps do. Whole
/// steps rather than a continuous search, so the result is stable against
/// float drift and easy to reason about.
fn usable_gaps(
    placements: &[(WindowId, Rect)],
    work_area: Rect,
    gaps: Gaps,
    minimum_size: &impl Fn(WindowId) -> Option<Size>,
) -> Option<Gaps> {
    const STEPS: i32 = 8;
    (0..=STEPS)
        .rev()
        .map(|step| Gaps::new(gaps.outer * step / STEPS, gaps.inner * step / STEPS))
        .find(|candidate| {
            placements.iter().all(|(window_id, rect)| {
                let placed = apply_gaps(*rect, work_area, *candidate);
                minimum_size(*window_id)
                    .unwrap_or(Size::new(1, 1))
                    .fits_within(placed)
            })
        })
}

fn tile(node: &Node<WindowId>, area: Rect, placements: &mut Vec<(WindowId, Rect)>) {
    match node {
        Node::Leaf(leaf) => placements.push((leaf.window, area)),
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
            let existing: Vec<WindowId> = tree.windows().into_iter().copied().collect();
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

            let mut placed: Vec<isize> = placements
                .iter()
                .map(|(window_id, _)| window_id.0)
                .collect();
            placed.sort_unstable();
            let mut expected: Vec<isize> = tree.windows().into_iter().map(|id| id.0).collect();
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

            for area in [
                WORK_AREA,
                Rect::new(-1920, -200, 1280, 1024),
                Rect::new(3, 7, 801, 603),
            ] {
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
                    node: Node::window(WindowId(1)),
                },
                Child {
                    weight: 1.0,
                    node: Node::window(WindowId(2)),
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

    // ---- minimum sizes and constraint overflow (issue #54) ------------

    /// A minimum-size oracle over a fixed table, unknown for the rest.
    fn minimums(table: &[(isize, i32, i32)]) -> impl Fn(WindowId) -> Option<Size> + '_ {
        move |window_id| {
            table
                .iter()
                .find(|(id, _, _)| *id == window_id.0)
                .map(|(_, width, height)| Size::new(*width, *height))
        }
    }

    #[test]
    fn a_plan_with_no_known_minimums_matches_the_unconstrained_plan() {
        let tree = tree_of(&[1, 2, 3]);

        let plan = plan_tree_constrained(&tree, WORK_AREA, Gaps::new(10, 4), |_| None);

        assert_eq!(
            plan.placements,
            plan_tree(&tree, WORK_AREA, Gaps::new(10, 4))
        );
        assert!(plan.overflow.is_empty());
        assert_eq!(plan.gaps, Gaps::new(10, 4));
    }

    #[test]
    fn gaps_give_way_before_any_window_does() {
        // Two windows side by side on a 1000px display, each needing 490px.
        // With the configured gaps they would get 474px; with none, 500px.
        let tree = tree_of(&[1, 2]);
        let area = Rect::new(0, 0, 1000, 600);

        let plan = plan_tree_constrained(
            &tree,
            area,
            Gaps::new(20, 12),
            minimums(&[(1, 490, 100), (2, 490, 100)]),
        );

        assert!(
            plan.overflow.is_empty(),
            "both windows fit once gaps shrink"
        );
        assert!(
            plan.gaps.outer < 20 && plan.gaps.inner < 12,
            "decoration was sacrificed: {:?}",
            plan.gaps
        );
        for (window_id, rect) in &plan.placements {
            assert!(rect.width >= 490, "window {window_id:?} got {rect:?}");
        }
    }

    #[test]
    fn the_newest_window_overflows_first_and_the_rest_keep_their_places() {
        // Three windows in a row, each needing 400px of a 1000px display:
        // only two can fit. Window 3 was inserted last, so it goes.
        let tree = Tree::from_root(Node::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                Child {
                    weight: 1.0,
                    node: Node::window(WindowId(1)),
                },
                Child {
                    weight: 1.0,
                    node: Node::window(WindowId(2)),
                },
                Child {
                    weight: 1.0,
                    node: Node::window(WindowId(3)),
                },
            ],
        });
        let area = Rect::new(0, 0, 1000, 600);
        let table = [(1, 400, 100), (2, 400, 100), (3, 400, 100)];

        let plan = plan_tree_constrained(&tree, area, Gaps::new(0, 0), minimums(&table));

        assert_eq!(plan.overflow, vec![WindowId(3)]);
        assert_eq!(
            plan.placements,
            vec![
                (WindowId(1), Rect::new(0, 0, 500, 600)),
                (WindowId(2), Rect::new(500, 0, 500, 600)),
            ],
            "the established windows share the space the newest gave back"
        );
    }

    #[test]
    fn overflow_follows_the_newest_slot_which_a_swap_does_not_move() {
        // Window 9 arrived last and took half of window 1 space. Swapping
        // them exchanges only the occupants (ADR 0026): the slot that was
        // inserted last now holds window 1, and it is the slot that gives
        // its space back, so window 1 overflows.
        let mut tree = ContainerTree::new();
        tree.insert_first(WindowId(1));
        tree.split_leaf(&WindowId(1), SplitAxis::Horizontal, WindowId(2));
        tree.split_leaf(&WindowId(1), SplitAxis::Horizontal, WindowId(9));
        tree.swap_leaves(&WindowId(1), &WindowId(9));
        assert_eq!(
            tree.windows()
                .into_iter()
                .map(|id| id.0)
                .collect::<Vec<_>>(),
            vec![9, 1, 2]
        );
        let area = Rect::new(0, 0, 1000, 600);
        let table = [(1, 300, 100), (2, 300, 100), (9, 300, 100)];

        let plan = plan_tree_constrained(&tree, area, Gaps::new(0, 0), minimums(&table));

        assert_eq!(plan.overflow, vec![WindowId(1)]);
        assert_eq!(
            plan.placements,
            vec![
                (WindowId(9), Rect::new(0, 0, 333, 600)),
                (WindowId(2), Rect::new(333, 0, 667, 600)),
            ],
            "window 9 keeps the older slot and the space that slot regains"
        );
    }

    #[test]
    fn overflow_keeps_removing_the_newest_until_the_rest_fit() {
        let tree = tree_of(&[1, 2, 3, 4]);
        let area = Rect::new(0, 0, 1000, 600);
        // Everything needs the whole display: only the first survives.
        let table = [
            (1, 1000, 600),
            (2, 1000, 600),
            (3, 1000, 600),
            (4, 1000, 600),
        ];

        let plan = plan_tree_constrained(&tree, area, Gaps::new(0, 0), minimums(&table));

        assert_eq!(
            plan.overflow,
            vec![WindowId(4), WindowId(3), WindowId(2)],
            "newest first, one at a time"
        );
        assert_eq!(plan.placements, vec![(WindowId(1), area)]);
    }

    #[test]
    fn a_window_that_cannot_fit_the_display_at_all_overflows_alone() {
        let tree = tree_of(&[1]);

        let plan = plan_tree_constrained(
            &tree,
            Rect::new(0, 0, 800, 600),
            Gaps::new(0, 0),
            minimums(&[(1, 1200, 100)]),
        );

        assert_eq!(plan.overflow, vec![WindowId(1)]);
        assert!(plan.placements.is_empty());
    }

    #[test]
    fn an_unknown_minimum_is_never_guessed_at() {
        // A cramped strip of eight windows: with no minimums known, every
        // one is still placed, at positive area.
        let tree = tree_of(&[1, 2, 3, 4, 5, 6, 7, 8]);

        let plan = plan_tree_constrained(&tree, Rect::new(0, 0, 80, 60), Gaps::new(0, 0), |_| None);

        assert!(plan.overflow.is_empty());
        assert_eq!(plan.placements.len(), 8);
    }

    #[test]
    fn every_arranged_window_meets_its_minimum_size() {
        for seed in 0..40u64 {
            let leaves = 1 + (seed as usize % 12);
            let tree = shaped_tree(seed, leaves);
            // Minimums vary per window and per seed, some unknown.
            let minimum = |window_id: WindowId| {
                let n = (seed as i32 * 7 + window_id.0 as i32 * 13) % 5;
                (n != 0).then(|| Size::new(120 * n, 90 * n))
            };
            let plan = plan_tree_constrained(&tree, WORK_AREA, Gaps::new(6, 3), minimum);

            for (window_id, rect) in &plan.placements {
                assert!(
                    minimum(*window_id)
                        .unwrap_or(Size::new(1, 1))
                        .fits_within(*rect),
                    "seed {seed}: {window_id:?} at {rect:?} is below its minimum"
                );
            }
            let mut accounted: Vec<isize> = plan
                .placements
                .iter()
                .map(|(id, _)| id.0)
                .chain(plan.overflow.iter().map(|id| id.0))
                .collect();
            accounted.sort_unstable();
            let mut expected: Vec<isize> = tree.windows().into_iter().map(|id| id.0).collect();
            expected.sort_unstable();
            assert_eq!(
                accounted, expected,
                "seed {seed}: every window is either arranged or overflowed, never both"
            );
        }
    }

    #[test]
    fn overflow_order_is_stable_and_never_oscillates() {
        for seed in 0..40u64 {
            let tree = shaped_tree(seed, 1 + (seed as usize % 12));
            let minimum = |_: WindowId| Some(Size::new(700, 500));
            let first = plan_tree_constrained(&tree, WORK_AREA, Gaps::new(6, 3), minimum);
            let second = plan_tree_constrained(&tree, WORK_AREA, Gaps::new(6, 3), minimum);
            assert_eq!(first, second, "seed {seed}");

            // Overflow is newest-first: insertion numbers descend.
            let numbers: Vec<u64> = first
                .overflow
                .iter()
                .map(|id| {
                    tree.insertion_of(id)
                        .expect("an overflowed window is in the tree")
                })
                .collect();
            assert!(
                numbers.windows(2).all(|pair| pair[0] > pair[1]),
                "seed {seed}: overflow {numbers:?} is not newest-first"
            );

            // And re-planning the tree with the overflowed windows removed
            // reproduces the arranged placements exactly: the arrangement
            // the survivors got is the one they would have got alone.
            let survivors =
                tree.filter_map_windows(&mut |id| (!first.overflow.contains(id)).then_some(*id));
            let alone = plan_tree_constrained(&survivors, WORK_AREA, Gaps::new(6, 3), minimum);
            assert_eq!(alone.placements, first.placements, "seed {seed}");
            assert!(alone.overflow.is_empty(), "seed {seed}");
        }
    }
}
