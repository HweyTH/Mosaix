//! The container tree: a normalized hierarchy of weighted splits and
//! window leaves owning one display's tiled arrangement (ADR 0023).
//!
//! This module owns the *structure* only -- what a tree is, and the
//! operations that keep it normalized. Where its windows end up on screen
//! is `mosaix-layout`'s pure planner, and which leaf a new window splits is
//! that crate's insertion policy. Keeping the three apart is what lets the
//! planner be property-tested against trees that no policy would build.
//!
//! The tree is generic in what a leaf holds because it has two lives. In
//! memory a leaf holds a [`WindowId`], which is a native handle and means
//! nothing after a restart. On disk a leaf holds
//! [`WindowEvidence`](crate::identity::WindowEvidence), which is how the
//! structure is recognised again in the next session.

use serde::{Deserialize, Serialize};

use crate::id::WindowId;
use crate::identity::WindowEvidence;

/// How a container arranges its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitAxis {
    /// Children sit side by side, left to right.
    Horizontal,
    /// Children stack, top to bottom.
    Vertical,
}

impl SplitAxis {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
        }
    }
}

/// One child of a container, with its share of the container's extent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Child<L> {
    /// Share of the parent's extent along its axis. Always positive;
    /// [`Tree::normalize`] clamps anything else.
    pub weight: f64,
    pub node: Node<L>,
}

/// A window's slot in the tree, with the metadata the slot carries about
/// how it got there.
///
/// `inserted` is the tree's insertion sequence: each window that joins the
/// tree takes the next number, and the number stays with the slot rather
/// than the window, so a directional swap leaves it where it was (ADR
/// 0026). It is what makes "the newest window" a fact the planner can
/// read rather than a guess from geometry (spec user story 44).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Leaf<L> {
    pub inserted: u64,
    pub window: L,
}

/// A container or a window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Node<L> {
    Leaf(Leaf<L>),
    Split {
        axis: SplitAxis,
        children: Vec<Child<L>>,
    },
}

impl<L> Node<L> {
    /// A leaf with no insertion history, for hand-built fixtures.
    /// [`Tree::from_root`] numbers such leaves in visual order.
    pub const fn window(window: L) -> Self {
        Self::Leaf(Leaf {
            inserted: 0,
            window,
        })
    }

    /// This node's leaves, left to right and top to bottom. This order is
    /// the arrangement's visual order.
    pub fn leaves(&self) -> Vec<&Leaf<L>> {
        let mut found = Vec::new();
        self.collect_leaves(&mut found);
        found
    }

    fn collect_leaves<'a>(&'a self, found: &mut Vec<&'a Leaf<L>>) {
        match self {
            Self::Leaf(leaf) => found.push(leaf),
            Self::Split { children, .. } => {
                for child in children {
                    child.node.collect_leaves(found);
                }
            }
        }
    }

    fn leaves_mut<'a>(&'a mut self, found: &mut Vec<&'a mut Leaf<L>>) {
        match self {
            Self::Leaf(leaf) => found.push(leaf),
            Self::Split { children, .. } => {
                for child in children {
                    child.node.leaves_mut(found);
                }
            }
        }
    }

    fn leaf_count(&self) -> usize {
        match self {
            Self::Leaf(_) => 1,
            Self::Split { children, .. } => {
                children.iter().map(|child| child.node.leaf_count()).sum()
            }
        }
    }
}

/// The smallest weight a child may hold. A zero or negative weight would
/// make edge allocation meaningless, and a denormal one would round to an
/// invisible window.
pub const MIN_WEIGHT: f64 = 0.01;

/// One display's arrangement. An empty tree is a display with no tiled
/// windows, which is an ordinary state rather than an error.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tree<L> {
    root: Option<Node<L>>,
    /// The insertion number the next window to join will take. Stored
    /// with the tree so numbering continues across a restart rather than
    /// restarting from zero and colliding with the slots already held.
    #[serde(default)]
    next_insertion: u64,
}

impl<L> Default for Tree<L> {
    fn default() -> Self {
        Self {
            root: None,
            next_insertion: 0,
        }
    }
}

/// The live arrangement, whose leaves are native handles.
pub type ContainerTree = Tree<WindowId>;

/// The durable arrangement, whose leaves are recognisable after a restart.
pub type PersistedTree = Tree<WindowEvidence>;

impl<L> Tree<L> {
    pub const fn new() -> Self {
        Self {
            root: None,
            next_insertion: 0,
        }
    }

    /// Builds a tree around an already-shaped node, then normalizes it.
    /// The way a persisted arrangement and a hand-written fixture both
    /// enter the type.
    ///
    /// Leaves whose insertion numbers collide -- a fixture built from
    /// [`Node::window`] gives every leaf zero -- are renumbered in visual
    /// order, so the sequence is unique whatever the source.
    pub fn from_root(root: Node<L>) -> Self {
        let mut tree = Self {
            root: Some(root),
            next_insertion: 0,
        };
        tree.normalize();
        tree.renumber_if_ambiguous();
        tree
    }

    pub const fn root(&self) -> Option<&Node<L>> {
        self.root.as_ref()
    }

    pub const fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// Every leaf, in visual order.
    pub fn leaves(&self) -> Vec<&Leaf<L>> {
        self.root.as_ref().map(Node::leaves).unwrap_or_default()
    }

    /// Every window, in visual order.
    pub fn windows(&self) -> Vec<&L> {
        self.leaves().into_iter().map(|leaf| &leaf.window).collect()
    }

    pub fn len(&self) -> usize {
        self.root.as_ref().map_or(0, Node::leaf_count)
    }

    /// Restores the tree's invariants: no empty containers, no container
    /// with a single child except the root, no nested container sharing its
    /// parent's axis, and every weight positive.
    pub fn normalize(&mut self) {
        let Some(root) = self.root.take() else {
            return;
        };
        self.root = normalize_node(root);
    }

    /// Rebuilds the tree with each window replaced by `convert`, which is
    /// how a live tree becomes a durable one and back again. Slots keep
    /// their insertion numbers.
    pub fn map_windows<M>(&self, convert: &mut impl FnMut(&L) -> M) -> Tree<M> {
        Tree {
            root: self.root.as_ref().map(|node| map_node(node, convert)),
            next_insertion: self.next_insertion,
        }
    }

    /// Rebuilds the tree keeping only the windows `convert` could
    /// translate.
    ///
    /// Restoring uses this: a persisted leaf whose window cannot be
    /// identified confidently is dropped rather than guessed at, and
    /// normalization closes the space it leaves behind.
    pub fn filter_map_windows<M>(&self, convert: &mut impl FnMut(&L) -> Option<M>) -> Tree<M> {
        let mut rebuilt = Tree {
            root: self
                .root
                .as_ref()
                .and_then(|node| filter_map_node(node, convert)),
            next_insertion: self.next_insertion,
        };
        rebuilt.normalize();
        rebuilt
    }

    fn take_insertion(&mut self) -> u64 {
        let number = self.next_insertion;
        self.next_insertion += 1;
        number
    }

    fn renumber_if_ambiguous(&mut self) {
        let mut seen = std::collections::HashSet::new();
        let ambiguous = self.leaves().iter().any(|leaf| !seen.insert(leaf.inserted));
        let highest = self.leaves().iter().map(|leaf| leaf.inserted).max();
        if ambiguous {
            let mut found = Vec::new();
            if let Some(root) = self.root.as_mut() {
                root.leaves_mut(&mut found);
            }
            for (number, leaf) in found.into_iter().enumerate() {
                leaf.inserted = number as u64;
            }
            self.next_insertion = self.len() as u64;
        } else if let Some(highest) = highest {
            self.next_insertion = self.next_insertion.max(highest + 1);
        }
    }
}

fn map_node<L, M>(node: &Node<L>, convert: &mut impl FnMut(&L) -> M) -> Node<M> {
    match node {
        Node::Leaf(leaf) => Node::Leaf(Leaf {
            inserted: leaf.inserted,
            window: convert(&leaf.window),
        }),
        Node::Split { axis, children } => Node::Split {
            axis: *axis,
            children: children
                .iter()
                .map(|child| Child {
                    weight: child.weight,
                    node: map_node(&child.node, convert),
                })
                .collect(),
        },
    }
}

fn filter_map_node<L, M>(
    node: &Node<L>,
    convert: &mut impl FnMut(&L) -> Option<M>,
) -> Option<Node<M>> {
    match node {
        Node::Leaf(leaf) => convert(&leaf.window).map(|window| {
            Node::Leaf(Leaf {
                inserted: leaf.inserted,
                window,
            })
        }),
        Node::Split { axis, children } => {
            let kept: Vec<Child<M>> = children
                .iter()
                .filter_map(|child| {
                    filter_map_node(&child.node, convert).map(|node| Child {
                        weight: child.weight,
                        node,
                    })
                })
                .collect();
            if kept.is_empty() {
                None
            } else {
                Some(Node::Split {
                    axis: *axis,
                    children: kept,
                })
            }
        }
    }
}

impl<L: PartialEq> Tree<L> {
    pub fn contains(&self, window: &L) -> bool {
        self.windows().into_iter().any(|held| held == window)
    }

    /// The insertion number of the slot `window` holds.
    pub fn insertion_of(&self, window: &L) -> Option<u64> {
        self.leaves()
            .into_iter()
            .find(|leaf| leaf.window == *window)
            .map(|leaf| leaf.inserted)
    }

    /// Adds `window` as the tree's only one. Does nothing if the tree
    /// already has one, so the caller cannot accidentally discard an
    /// arrangement.
    pub fn insert_first(&mut self, window: L) -> bool {
        if self.root.is_some() {
            return false;
        }
        let inserted = self.take_insertion();
        self.root = Some(Node::Leaf(Leaf { inserted, window }));
        true
    }

    /// Replaces the leaf holding `target` with a container of `target` and
    /// `window` in equal shares, arranged along `axis`.
    ///
    /// This is the whole of BSP insertion as far as structure goes: the new
    /// window takes half of one existing window's space and nothing else in
    /// the tree moves (spec user story 28).
    pub fn split_leaf(&mut self, target: &L, axis: SplitAxis, window: L) -> bool {
        if !self.contains(target) {
            return false;
        }
        let inserted = self.take_insertion();
        let Some(root) = self.root.as_mut() else {
            return false;
        };
        let split = split_within(root, target, axis, Leaf { inserted, window });
        if split {
            self.normalize();
        }
        split
    }

    /// Removes the leaf holding `window`, closing the space it occupied.
    pub fn remove(&mut self, window: &L) -> bool {
        let Some(root) = self.root.as_mut() else {
            return false;
        };
        if matches!(root, Node::Leaf(held) if held.window == *window) {
            self.root = None;
            return true;
        }
        let removed = remove_within(root, window);
        if removed {
            self.normalize();
        }
        removed
    }

    /// Exchanges the positions of two windows, changing nothing else.
    ///
    /// Containers, axes, weights, parentage, and each slot's insertion
    /// number all stay exactly as they were -- only the two occupants
    /// trade places (ADR 0026).
    pub fn swap_leaves(&mut self, first: &L, second: &L) -> bool
    where
        L: Clone,
    {
        if first == second || !self.contains(first) || !self.contains(second) {
            return false;
        }
        let Some(root) = self.root.as_mut() else {
            return false;
        };
        // One pass, so the second exchange cannot undo the first.
        swap_within(root, first, second);
        true
    }
}

fn find_leaf<'a, L: PartialEq>(node: &'a Node<L>, wanted: &L) -> Option<&'a Leaf<L>> {
    match node {
        Node::Leaf(leaf) => (leaf.window == *wanted).then_some(leaf),
        Node::Split { children, .. } => children
            .iter()
            .find_map(|child| find_leaf(&child.node, wanted)),
    }
}

fn swap_within<L: PartialEq + Clone>(node: &mut Node<L>, first: &L, second: &L) {
    match node {
        Node::Leaf(leaf) => {
            if leaf.window == *first {
                leaf.window = second.clone();
            } else if leaf.window == *second {
                leaf.window = first.clone();
            }
        }
        Node::Split { children, .. } => {
            for child in children.iter_mut() {
                swap_within(&mut child.node, first, second);
            }
        }
    }
}

fn split_within<L: PartialEq>(
    node: &mut Node<L>,
    target: &L,
    axis: SplitAxis,
    leaf: Leaf<L>,
) -> bool {
    match node {
        Node::Leaf(held) => {
            if held.window != *target {
                return false;
            }
            // `Node` is not `Default`, so the existing leaf is moved out
            // through a temporary split rather than replaced in place.
            let existing = std::mem::replace(
                node,
                Node::Split {
                    axis,
                    children: Vec::new(),
                },
            );
            *node = Node::Split {
                axis,
                children: vec![
                    Child {
                        weight: 1.0,
                        node: existing,
                    },
                    Child {
                        weight: 1.0,
                        node: Node::Leaf(leaf),
                    },
                ],
            };
            true
        }
        Node::Split { children, .. } => {
            let Some(index) = children
                .iter()
                .position(|child| child_holds(&child.node, target))
            else {
                return false;
            };
            split_within(&mut children[index].node, target, axis, leaf)
        }
    }
}

fn child_holds<L: PartialEq>(node: &Node<L>, wanted: &L) -> bool {
    find_leaf(node, wanted).is_some()
}

fn remove_within<L: PartialEq>(node: &mut Node<L>, wanted: &L) -> bool {
    let Node::Split { children, .. } = node else {
        return false;
    };
    if let Some(index) = children
        .iter()
        .position(|child| matches!(&child.node, Node::Leaf(leaf) if leaf.window == *wanted))
    {
        children.remove(index);
        return true;
    }
    children
        .iter_mut()
        .any(|child| remove_within(&mut child.node, wanted))
}

fn normalize_node<L>(node: Node<L>) -> Option<Node<L>> {
    match node {
        Node::Leaf(leaf) => Some(Node::Leaf(leaf)),
        Node::Split { axis, children } => {
            let mut kept: Vec<Child<L>> = Vec::with_capacity(children.len());
            for child in children {
                let weight = if child.weight.is_finite() && child.weight >= MIN_WEIGHT {
                    child.weight
                } else {
                    MIN_WEIGHT
                };
                let Some(node) = normalize_node(child.node) else {
                    continue;
                };
                // A container sharing its parent's axis adds no structure,
                // only a level of nesting, so its children are hoisted with
                // their shares scaled into the space it held.
                match node {
                    Node::Split {
                        axis: inner_axis,
                        children: inner,
                    } if inner_axis == axis => {
                        let total: f64 = inner.iter().map(|child| child.weight).sum();
                        for inner_child in inner {
                            kept.push(Child {
                                weight: weight * inner_child.weight / total,
                                node: inner_child.node,
                            });
                        }
                    }
                    node => kept.push(Child { weight, node }),
                }
            }
            match kept.len() {
                0 => None,
                // A container with one child is that child, wherever it
                // sits -- including at the root, where flattening it is
                // what turns a two-window tree back into a one-window tree.
                1 => Some(kept.pop().expect("length checked").node),
                _ => Some(Node::Split {
                    axis,
                    children: kept,
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(id: isize) -> WindowId {
        WindowId(id)
    }

    fn ids(tree: &ContainerTree) -> Vec<isize> {
        tree.windows().into_iter().map(|id| id.0).collect()
    }

    #[test]
    fn an_empty_tree_has_no_windows() {
        let tree = ContainerTree::new();
        assert!(tree.is_empty());
        assert_eq!(tree.len(), 0);
        assert!(ids(&tree).is_empty());
    }

    #[test]
    fn the_first_window_becomes_the_whole_arrangement() {
        let mut tree = ContainerTree::new();

        assert!(tree.insert_first(leaf(1)));

        assert_eq!(ids(&tree), vec![1]);
        assert!(
            !tree.insert_first(leaf(2)),
            "inserting a first window twice would discard the arrangement"
        );
        assert_eq!(ids(&tree), vec![1]);
    }

    #[test]
    fn splitting_a_leaf_puts_the_new_window_beside_it_in_equal_shares() {
        let mut tree = ContainerTree::new();
        tree.insert_first(leaf(1));

        assert!(tree.split_leaf(&leaf(1), SplitAxis::Horizontal, leaf(2)));

        assert_eq!(ids(&tree), vec![1, 2]);
        let Some(Node::Split { axis, children }) = tree.root() else {
            panic!("the root is now a container");
        };
        assert_eq!(*axis, SplitAxis::Horizontal);
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].weight, children[1].weight);
    }

    #[test]
    fn splitting_a_nested_leaf_leaves_every_other_slot_alone() {
        let mut tree = ContainerTree::new();
        tree.insert_first(leaf(1));
        tree.split_leaf(&leaf(1), SplitAxis::Horizontal, leaf(2));
        let before = tree.clone();

        assert!(tree.split_leaf(&leaf(2), SplitAxis::Vertical, leaf(3)));

        assert_eq!(ids(&tree), vec![1, 2, 3]);
        let Some(Node::Split { children, .. }) = tree.root() else {
            panic!("the root is a container");
        };
        let Some(Node::Split {
            children: before_children,
            ..
        }) = before.root()
        else {
            panic!("the root was a container");
        };
        assert_eq!(
            children[0], before_children[0],
            "the untouched sibling keeps its node, weight, and identity"
        );
    }

    #[test]
    fn splitting_an_absent_window_changes_nothing() {
        let mut tree = ContainerTree::new();
        tree.insert_first(leaf(1));
        let before = tree.clone();

        assert!(!tree.split_leaf(&leaf(99), SplitAxis::Horizontal, leaf(2)));

        assert_eq!(tree, before, "not even the insertion sequence advances");
    }

    #[test]
    fn each_window_takes_the_next_insertion_number_and_keeps_it() {
        let mut tree = ContainerTree::new();
        tree.insert_first(leaf(1));
        tree.split_leaf(&leaf(1), SplitAxis::Horizontal, leaf(2));
        tree.split_leaf(&leaf(2), SplitAxis::Vertical, leaf(3));

        assert_eq!(tree.insertion_of(&leaf(1)), Some(0));
        assert_eq!(tree.insertion_of(&leaf(2)), Some(1));
        assert_eq!(tree.insertion_of(&leaf(3)), Some(2));

        // Removing a window does not hand its number to the next arrival.
        tree.remove(&leaf(2));
        tree.split_leaf(&leaf(3), SplitAxis::Horizontal, leaf(4));
        assert_eq!(
            tree.insertion_of(&leaf(4)),
            Some(3),
            "insertion numbers are never reused, so newest stays newest"
        );
    }

    #[test]
    fn removing_the_last_window_empties_the_tree() {
        let mut tree = ContainerTree::new();
        tree.insert_first(leaf(1));

        assert!(tree.remove(&leaf(1)));

        assert!(tree.is_empty());
    }

    #[test]
    fn removing_one_of_two_windows_flattens_the_container_away() {
        let mut tree = ContainerTree::new();
        tree.insert_first(leaf(1));
        tree.split_leaf(&leaf(1), SplitAxis::Horizontal, leaf(2));

        assert!(tree.remove(&leaf(1)));

        assert_eq!(ids(&tree), vec![2]);
        assert!(
            matches!(tree.root(), Some(Node::Leaf(_))),
            "a container with one child is that child"
        );
    }

    #[test]
    fn removing_an_absent_window_reports_that_it_did_nothing() {
        let mut tree = ContainerTree::new();
        tree.insert_first(leaf(1));

        assert!(!tree.remove(&leaf(99)));
        assert_eq!(ids(&tree), vec![1]);
    }

    #[test]
    fn normalization_hoists_a_container_that_shares_its_parents_axis() {
        let mut tree = Tree::from_root(Node::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                Child {
                    weight: 1.0,
                    node: Node::window(leaf(1)),
                },
                Child {
                    weight: 1.0,
                    node: Node::Split {
                        axis: SplitAxis::Horizontal,
                        children: vec![
                            Child {
                                weight: 1.0,
                                node: Node::window(leaf(2)),
                            },
                            Child {
                                weight: 1.0,
                                node: Node::window(leaf(3)),
                            },
                        ],
                    },
                },
            ],
        });

        tree.normalize();

        let Some(Node::Split { children, .. }) = tree.root() else {
            panic!("the root stays a container");
        };
        assert_eq!(children.len(), 3, "the redundant level is gone");
        assert_eq!(ids(&tree), vec![1, 2, 3], "visual order is unchanged");
        // The hoisted pair shared half the width, so each now holds a
        // quarter -- the geometry the nesting described is preserved.
        assert!((children[0].weight - 1.0).abs() < 1e-9);
        assert!((children[1].weight - 0.5).abs() < 1e-9);
        assert!((children[2].weight - 0.5).abs() < 1e-9);
    }

    #[test]
    fn normalization_does_not_hoist_a_container_on_the_other_axis() {
        let mut tree = Tree::from_root(Node::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                Child {
                    weight: 1.0,
                    node: Node::window(leaf(1)),
                },
                Child {
                    weight: 1.0,
                    node: Node::Split {
                        axis: SplitAxis::Vertical,
                        children: vec![
                            Child {
                                weight: 1.0,
                                node: Node::window(leaf(2)),
                            },
                            Child {
                                weight: 1.0,
                                node: Node::window(leaf(3)),
                            },
                        ],
                    },
                },
            ],
        });

        tree.normalize();

        let Some(Node::Split { children, .. }) = tree.root() else {
            panic!("the root stays a container");
        };
        assert_eq!(
            children.len(),
            2,
            "a differently-oriented container carries real structure"
        );
    }

    #[test]
    fn normalization_clamps_a_weight_that_would_erase_a_window() {
        let tree = Tree::from_root(Node::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                Child {
                    weight: 0.0,
                    node: Node::window(leaf(1)),
                },
                Child {
                    weight: f64::NAN,
                    node: Node::window(leaf(2)),
                },
                Child {
                    weight: -3.0,
                    node: Node::window(leaf(3)),
                },
            ],
        });

        let Some(Node::Split { children, .. }) = tree.root() else {
            panic!("the root stays a container");
        };
        assert!(
            children.iter().all(|child| child.weight >= MIN_WEIGHT),
            "no window may be allocated zero or negative space"
        );
    }

    #[test]
    fn a_fixture_with_unnumbered_leaves_is_numbered_in_visual_order() {
        let tree = Tree::from_root(Node::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                Child {
                    weight: 1.0,
                    node: Node::window(leaf(7)),
                },
                Child {
                    weight: 1.0,
                    node: Node::window(leaf(3)),
                },
            ],
        });

        assert_eq!(tree.insertion_of(&leaf(7)), Some(0));
        assert_eq!(tree.insertion_of(&leaf(3)), Some(1));

        // And the sequence continues past what the fixture declared.
        let mut tree = tree;
        tree.split_leaf(&leaf(3), SplitAxis::Vertical, leaf(9));
        assert_eq!(tree.insertion_of(&leaf(9)), Some(2));
    }

    #[test]
    fn swapping_two_leaves_exchanges_only_their_occupants() {
        let mut tree = ContainerTree::new();
        tree.insert_first(leaf(1));
        tree.split_leaf(&leaf(1), SplitAxis::Horizontal, leaf(2));
        tree.split_leaf(&leaf(2), SplitAxis::Vertical, leaf(3));
        let before = tree.clone();

        assert!(tree.swap_leaves(&leaf(1), &leaf(3)));

        assert_eq!(ids(&tree), vec![3, 2, 1]);
        assert_eq!(
            tree.insertion_of(&leaf(3)),
            Some(0),
            "the slot keeps its insertion number; the window moved into it"
        );
        // Structure is untouched: mapping the swap back reproduces the
        // original tree exactly, weights and axes included.
        let mut restored = tree.clone();
        assert!(restored.swap_leaves(&leaf(3), &leaf(1)));
        assert_eq!(restored, before);
    }

    #[test]
    fn swapping_with_an_absent_or_identical_window_does_nothing() {
        let mut tree = ContainerTree::new();
        tree.insert_first(leaf(1));
        tree.split_leaf(&leaf(1), SplitAxis::Horizontal, leaf(2));
        let before = tree.clone();

        assert!(!tree.swap_leaves(&leaf(1), &leaf(1)));
        assert!(!tree.swap_leaves(&leaf(1), &leaf(99)));

        assert_eq!(tree, before);
    }

    #[test]
    fn dropping_unidentifiable_leaves_closes_the_space_they_held() {
        let mut tree = ContainerTree::new();
        tree.insert_first(leaf(1));
        tree.split_leaf(&leaf(1), SplitAxis::Horizontal, leaf(2));
        tree.split_leaf(&leaf(2), SplitAxis::Vertical, leaf(3));

        // Only window 3 could be identified after the restart.
        let restored = tree.filter_map_windows(&mut |id| (id.0 == 3).then_some(*id));

        assert_eq!(
            restored
                .windows()
                .into_iter()
                .map(|id| id.0)
                .collect::<Vec<_>>(),
            vec![3]
        );
        assert!(
            matches!(restored.root(), Some(Node::Leaf(_))),
            "the containers that held the missing windows are gone"
        );
    }

    #[test]
    fn a_tree_round_trips_through_its_durable_form() {
        let mut tree = ContainerTree::new();
        tree.insert_first(leaf(1));
        tree.split_leaf(&leaf(1), SplitAxis::Horizontal, leaf(2));
        tree.split_leaf(&leaf(2), SplitAxis::Vertical, leaf(3));

        let as_text = tree.map_windows(&mut |id| id.0.to_string());
        let json = serde_json::to_string(&as_text).expect("a tree serializes");
        let read_back: Tree<String> = serde_json::from_str(&json).expect("and deserializes");
        let live = read_back.map_windows(&mut |text| WindowId(text.parse().expect("an id")));

        assert_eq!(
            live, tree,
            "structure, axes, weights, and insertion numbers all survive"
        );
        let mut continued = live;
        continued.split_leaf(&leaf(3), SplitAxis::Horizontal, leaf(4));
        assert_eq!(
            continued.insertion_of(&leaf(4)),
            Some(3),
            "the sequence continues where the stored tree left off"
        );
    }
}
