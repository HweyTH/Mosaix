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

/// A container or a window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Node<L> {
    Leaf(L),
    Split {
        axis: SplitAxis,
        children: Vec<Child<L>>,
    },
}

/// The smallest weight a child may hold. A zero or negative weight would
/// make edge allocation meaningless, and a denormal one would round to an
/// invisible window.
pub const MIN_WEIGHT: f64 = 0.01;

impl<L> Node<L> {
    /// This node's leaves, left to right and top to bottom. This order is
    /// the arrangement's visual order.
    pub fn leaves(&self) -> Vec<&L> {
        let mut found = Vec::new();
        self.collect_leaves(&mut found);
        found
    }

    fn collect_leaves<'a>(&'a self, found: &mut Vec<&'a L>) {
        match self {
            Self::Leaf(leaf) => found.push(leaf),
            Self::Split { children, .. } => {
                for child in children {
                    child.node.collect_leaves(found);
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

/// One display's arrangement. An empty tree is a display with no tiled
/// windows, which is an ordinary state rather than an error.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tree<L> {
    root: Option<Node<L>>,
}

impl<L> Default for Tree<L> {
    fn default() -> Self {
        Self { root: None }
    }
}

/// The live arrangement, whose leaves are native handles.
pub type ContainerTree = Tree<WindowId>;

/// The durable arrangement, whose leaves are recognisable after a restart.
pub type PersistedTree = Tree<WindowEvidence>;

impl<L> Tree<L> {
    pub const fn new() -> Self {
        Self { root: None }
    }

    /// Builds a tree around an already-shaped node, then normalizes it.
    /// The way a persisted arrangement and a hand-written fixture both
    /// enter the type.
    pub fn from_root(root: Node<L>) -> Self {
        let mut tree = Self { root: Some(root) };
        tree.normalize();
        tree
    }

    pub const fn root(&self) -> Option<&Node<L>> {
        self.root.as_ref()
    }

    pub const fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// Every leaf, in visual order.
    pub fn leaves(&self) -> Vec<&L> {
        self.root.as_ref().map(Node::leaves).unwrap_or_default()
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

    /// Rebuilds the tree with each leaf replaced by `convert`, which is how
    /// a live tree becomes a durable one and back again.
    pub fn map_leaves<M>(&self, convert: &mut impl FnMut(&L) -> M) -> Tree<M> {
        Tree {
            root: self.root.as_ref().map(|node| map_node(node, convert)),
        }
    }

    /// Rebuilds the tree keeping only the leaves `convert` could translate.
    ///
    /// Restoring uses this: a persisted leaf whose window cannot be
    /// identified confidently is dropped rather than guessed at, and
    /// normalization closes the space it leaves behind.
    pub fn filter_map_leaves<M>(&self, convert: &mut impl FnMut(&L) -> Option<M>) -> Tree<M> {
        let mut rebuilt = Tree {
            root: self
                .root
                .as_ref()
                .and_then(|node| filter_map_node(node, convert)),
        };
        rebuilt.normalize();
        rebuilt
    }
}

fn map_node<L, M>(node: &Node<L>, convert: &mut impl FnMut(&L) -> M) -> Node<M> {
    match node {
        Node::Leaf(leaf) => Node::Leaf(convert(leaf)),
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
        Node::Leaf(leaf) => convert(leaf).map(Node::Leaf),
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
    pub fn contains(&self, leaf: &L) -> bool {
        self.leaves().into_iter().any(|held| held == leaf)
    }

    /// Adds `leaf` as the tree's only window. Does nothing if the tree
    /// already has one, so the caller cannot accidentally discard an
    /// arrangement.
    pub fn insert_first(&mut self, leaf: L) -> bool {
        if self.root.is_some() {
            return false;
        }
        self.root = Some(Node::Leaf(leaf));
        true
    }

    /// Replaces the leaf holding `target` with a container of `target` and
    /// `leaf` in equal shares, arranged along `axis`.
    ///
    /// This is the whole of BSP insertion as far as structure goes: the new
    /// window takes half of one existing window's space and nothing else in
    /// the tree moves (spec user story 28).
    pub fn split_leaf(&mut self, target: &L, axis: SplitAxis, leaf: L) -> bool {
        let Some(root) = self.root.as_mut() else {
            return false;
        };
        let inserted = split_within(root, target, axis, leaf);
        if inserted {
            self.normalize();
        }
        inserted
    }

    /// Removes the leaf holding `leaf`, closing the space it occupied.
    pub fn remove(&mut self, leaf: &L) -> bool {
        let Some(root) = self.root.as_mut() else {
            return false;
        };
        if matches!(root, Node::Leaf(held) if held == leaf) {
            self.root = None;
            return true;
        }
        let removed = remove_within(root, leaf);
        if removed {
            self.normalize();
        }
        removed
    }

    /// Exchanges the positions of two leaves, changing nothing else.
    ///
    /// Containers, axes, weights, and parentage all stay exactly as they
    /// were -- only the two occupants trade places (ADR 0026).
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

fn find_leaf<'a, L: PartialEq>(node: &'a Node<L>, wanted: &L) -> Option<&'a L> {
    match node {
        Node::Leaf(leaf) => (leaf == wanted).then_some(leaf),
        Node::Split { children, .. } => children
            .iter()
            .find_map(|child| find_leaf(&child.node, wanted)),
    }
}

fn swap_within<L: PartialEq + Clone>(node: &mut Node<L>, first: &L, second: &L) {
    match node {
        Node::Leaf(leaf) => {
            if leaf == first {
                *leaf = second.clone();
            } else if leaf == second {
                *leaf = first.clone();
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
    leaf: L,
) -> bool {
    match node {
        Node::Leaf(held) => {
            if held != target {
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
        .position(|child| matches!(&child.node, Node::Leaf(leaf) if leaf == wanted))
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
        tree.leaves().into_iter().map(|id| id.0).collect()
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
        let Some(Node::Split { children: before_children, .. }) = before.root() else {
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

        assert_eq!(tree, before);
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
        let mut tree = Tree {
            root: Some(Node::Split {
                axis: SplitAxis::Horizontal,
                children: vec![
                    Child {
                        weight: 1.0,
                        node: Node::Leaf(leaf(1)),
                    },
                    Child {
                        weight: 1.0,
                        node: Node::Split {
                            axis: SplitAxis::Horizontal,
                            children: vec![
                                Child {
                                    weight: 1.0,
                                    node: Node::Leaf(leaf(2)),
                                },
                                Child {
                                    weight: 1.0,
                                    node: Node::Leaf(leaf(3)),
                                },
                            ],
                        },
                    },
                ],
            }),
        };

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
        let mut tree = Tree {
            root: Some(Node::Split {
                axis: SplitAxis::Horizontal,
                children: vec![
                    Child {
                        weight: 1.0,
                        node: Node::Leaf(leaf(1)),
                    },
                    Child {
                        weight: 1.0,
                        node: Node::Split {
                            axis: SplitAxis::Vertical,
                            children: vec![
                                Child {
                                    weight: 1.0,
                                    node: Node::Leaf(leaf(2)),
                                },
                                Child {
                                    weight: 1.0,
                                    node: Node::Leaf(leaf(3)),
                                },
                            ],
                        },
                    },
                ],
            }),
        };

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
        let mut tree = Tree {
            root: Some(Node::Split {
                axis: SplitAxis::Horizontal,
                children: vec![
                    Child {
                        weight: 0.0,
                        node: Node::Leaf(leaf(1)),
                    },
                    Child {
                        weight: f64::NAN,
                        node: Node::Leaf(leaf(2)),
                    },
                    Child {
                        weight: -3.0,
                        node: Node::Leaf(leaf(3)),
                    },
                ],
            }),
        };

        tree.normalize();

        let Some(Node::Split { children, .. }) = tree.root() else {
            panic!("the root stays a container");
        };
        assert!(
            children.iter().all(|child| child.weight >= MIN_WEIGHT),
            "no window may be allocated zero or negative space"
        );
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
        let restored = tree.filter_map_leaves(&mut |id| (id.0 == 3).then_some(*id));

        assert_eq!(
            restored.leaves().into_iter().map(|id| id.0).collect::<Vec<_>>(),
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

        let as_text = tree.map_leaves(&mut |id| id.0.to_string());
        let json = serde_json::to_string(&as_text).expect("a tree serializes");
        let read_back: Tree<String> = serde_json::from_str(&json).expect("and deserializes");
        let live = read_back.map_leaves(&mut |text| WindowId(text.parse().expect("an id")));

        assert_eq!(live, tree, "structure, axes, and weights all survive");
    }
}
