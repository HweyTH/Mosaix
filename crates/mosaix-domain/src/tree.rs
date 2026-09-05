//! The container tree: a normalized hierarchy of weighted splits and
//! window leaves owning one display's tiled arrangement (ADR 0023).
//!
//! This module owns the *structure* only -- what a tree is, and the
//! operations that keep it normalized. Where its windows end up on screen
//! is `mosaix-layout`'s pure planner, and which leaf a new window splits is
//! that crate's insertion policy. Keeping the three apart is what lets the
//! planner be property-tested against trees that no policy would build.
//!
//! The tree is generic in what a live leaf holds because it has two lives.
//! In memory a leaf holds a [`WindowId`], which is a native handle and
//! means nothing after a restart. On disk a leaf holds
//! [`WindowEvidence`](crate::identity::WindowEvidence), which is how the
//! structure is recognised again in the next session.
//!
//! A leaf may also be *dormant* (CONTEXT.md "Dormant tree leaf"): its
//! window has closed or cannot be matched, but the slot is kept, holding
//! the evidence that would recognise the window if it came back. Dormant
//! leaves take no screen space; they are structure waiting for a window.

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

/// How long a dormant leaf is kept before it is pruned, in seconds. Seven
/// days, matching undo retention (spec user story 40). Fixed in the first
/// release.
pub const DORMANT_RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;

/// A slot whose window is gone for now: what would recognise the window
/// if it returned, and when the slot went dormant so it can expire.
///
/// The evidence is the same privacy-safe kind persistent undo keeps --
/// no title is ever captured (ADR 0024).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DormantPosition {
    pub evidence: WindowEvidence,
    /// Seconds since the Unix epoch at which the leaf went dormant.
    pub since_unix: i64,
}

impl DormantPosition {
    /// When retention will prune this position.
    pub const fn expires_unix(&self) -> i64 {
        self.since_unix.saturating_add(DORMANT_RETENTION_SECONDS)
    }

    pub const fn is_expired_at(&self, now_unix: i64) -> bool {
        self.expires_unix() <= now_unix
    }
}

/// What a leaf holds: a window, or the memory of one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Occupant<L> {
    Live(L),
    Dormant(DormantPosition),
}

impl<L> Occupant<L> {
    pub const fn live(&self) -> Option<&L> {
        match self {
            Self::Live(window) => Some(window),
            Self::Dormant(_) => None,
        }
    }

    pub const fn dormant(&self) -> Option<&DormantPosition> {
        match self {
            Self::Live(_) => None,
            Self::Dormant(position) => Some(position),
        }
    }
}

/// A slot in the tree, with the metadata the slot carries about how it
/// got there.
///
/// `inserted` is the tree's insertion sequence: each window that joins the
/// tree takes the next number, and the number stays with the slot rather
/// than the window, so a directional swap leaves it where it was (ADR
/// 0026). It is what makes "the newest window" a fact the planner can
/// read rather than a guess from geometry (spec user story 44), and it is
/// how a dormant position is named to the remove-position command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Leaf<L> {
    pub inserted: u64,
    pub occupant: Occupant<L>,
}

impl<L> Leaf<L> {
    /// The window this slot holds, if it holds one.
    pub const fn window(&self) -> Option<&L> {
        self.occupant.live()
    }

    pub const fn dormant(&self) -> Option<&DormantPosition> {
        self.occupant.dormant()
    }

    pub const fn is_live(&self) -> bool {
        matches!(self.occupant, Occupant::Live(_))
    }
}

/// A container or a leaf.
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
    /// A live leaf with no insertion history, for hand-built fixtures.
    /// [`Tree::from_root`] numbers such leaves in visual order.
    pub const fn window(window: L) -> Self {
        Self::Leaf(Leaf {
            inserted: 0,
            occupant: Occupant::Live(window),
        })
    }

    /// A dormant leaf with no insertion history, for hand-built fixtures.
    pub const fn dormant(position: DormantPosition) -> Self {
        Self::Leaf(Leaf {
            inserted: 0,
            occupant: Occupant::Dormant(position),
        })
    }

    /// This node's leaves, live and dormant, left to right and top to
    /// bottom. This order is the arrangement's visual order.
    pub fn leaves(&self) -> Vec<&Leaf<L>> {
        let mut found = Vec::new();
        self.collect_leaves(&mut found);
        found
    }

    /// This node's live windows, in visual order.
    pub fn windows(&self) -> Vec<&L> {
        self.leaves().into_iter().filter_map(Leaf::window).collect()
    }

    fn has_window(&self) -> bool {
        match self {
            Self::Leaf(leaf) => leaf.is_live(),
            Self::Split { children, .. } => children.iter().any(|child| child.node.has_window()),
        }
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
}

/// The smallest weight a child may hold. A zero or negative weight would
/// make edge allocation meaningless, and a denormal one would round to an
/// invisible window.
pub const MIN_WEIGHT: f64 = 0.01;

/// Which way along a container's axis a divider moves: toward the first
/// child (left or up) or toward the last (right or down).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Toward {
    Start,
    End,
}

/// What [`Tree::resize_toward`] changed: the windows on each side of the
/// divider it moved, and the container's weights before and after.
#[derive(Clone, Debug, PartialEq)]
pub struct DividerChange<L> {
    /// Every window in the child that gained space.
    pub grew: Vec<L>,
    /// Every window in the child that gave space up.
    pub shrank: Vec<L>,
    pub weights_before: Vec<f64>,
    pub weights_after: Vec<f64>,
}

/// What becomes of one leaf when a tree is rebuilt with
/// [`Tree::convert_leaves`].
pub enum LeafFate<M> {
    /// The slot holds a window.
    Live(M),
    /// The slot is kept, waiting for a window.
    Dormant(DormantPosition),
    /// The slot is dropped and normalization closes the space it held.
    Drop,
}

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

    /// Whether the tree has no leaves at all, live or dormant.
    pub const fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// Whether the tree currently holds a window.
    pub fn has_windows(&self) -> bool {
        self.root.as_ref().is_some_and(Node::has_window)
    }

    /// Every leaf, live and dormant, in visual order.
    pub fn leaves(&self) -> Vec<&Leaf<L>> {
        self.root.as_ref().map(Node::leaves).unwrap_or_default()
    }

    /// Every window, in visual order.
    pub fn windows(&self) -> Vec<&L> {
        self.root.as_ref().map(Node::windows).unwrap_or_default()
    }

    /// Every dormant slot, in visual order, each with its position number.
    pub fn dormant_positions(&self) -> Vec<(u64, &DormantPosition)> {
        self.leaves()
            .into_iter()
            .filter_map(|leaf| leaf.dormant().map(|position| (leaf.inserted, position)))
            .collect()
    }

    /// The number of windows the tree holds.
    pub fn len(&self) -> usize {
        self.windows().len()
    }

    /// Restores the tree's invariants: no empty containers, no container
    /// with a single child except the root, no nested container sharing its
    /// parent's axis, and every weight positive. Dormant leaves are leaves,
    /// so the containers that locate them are kept.
    pub fn normalize(&mut self) {
        let Some(root) = self.root.take() else {
            return;
        };
        self.root = normalize_node(root);
    }

    /// Rebuilds the tree with each leaf replaced by what `fate` says it
    /// becomes. Slots keep their insertion numbers, and normalization
    /// closes the space of anything dropped.
    ///
    /// This one operation is how a live tree becomes a durable one, how a
    /// durable one is recognised again after a restart, and how the
    /// planner sees only the windows that take up space.
    pub fn convert_leaves<M>(&self, fate: &mut impl FnMut(&Leaf<L>) -> LeafFate<M>) -> Tree<M> {
        let mut rebuilt = Tree {
            root: self.root.as_ref().and_then(|node| convert_node(node, fate)),
            next_insertion: self.next_insertion,
        };
        rebuilt.normalize();
        rebuilt
    }

    /// Rebuilds the tree with each window replaced by `convert`; dormant
    /// slots are kept as they are.
    pub fn map_windows<M>(&self, convert: &mut impl FnMut(&L) -> M) -> Tree<M> {
        self.convert_leaves(&mut |leaf| match &leaf.occupant {
            Occupant::Live(window) => LeafFate::Live(convert(window)),
            Occupant::Dormant(position) => LeafFate::Dormant(position.clone()),
        })
    }

    /// Rebuilds the tree keeping only the windows `convert` could
    /// translate; dormant slots are kept as they are.
    pub fn filter_map_windows<M>(&self, convert: &mut impl FnMut(&L) -> Option<M>) -> Tree<M> {
        self.convert_leaves(&mut |leaf| match &leaf.occupant {
            Occupant::Live(window) => match convert(window) {
                Some(converted) => LeafFate::Live(converted),
                None => LeafFate::Drop,
            },
            Occupant::Dormant(position) => LeafFate::Dormant(position.clone()),
        })
    }

    /// The tree as the planner sees it: dormant slots dropped and the
    /// space they held closed, so remaining windows use the whole
    /// arrangement (spec user story 42).
    pub fn live_projection(&self) -> Tree<L>
    where
        L: Clone,
    {
        self.convert_leaves(&mut |leaf| match &leaf.occupant {
            Occupant::Live(window) => LeafFate::Live(window.clone()),
            Occupant::Dormant(_) => LeafFate::Drop,
        })
    }

    /// Removes every dormant slot that has expired by `now_unix`, and
    /// answers how many. The pruning is the same whether the tree was
    /// live all along or just read back from disk, which is what makes
    /// expiry deterministic across a restart.
    pub fn prune_dormant(&mut self, now_unix: i64) -> usize {
        let before = self.leaves().len();
        let Some(root) = self.root.take() else {
            return 0;
        };
        self.root = remove_leaf_where(root, &|leaf: &Leaf<L>| {
            leaf.dormant()
                .is_some_and(|position| position.is_expired_at(now_unix))
        });
        self.normalize();
        before - self.leaves().len()
    }

    /// Removes the dormant slot numbered `position`, closing the space its
    /// ancestors held for it. `false` if no dormant slot has that number;
    /// a live slot is never removed this way.
    pub fn remove_position(&mut self, position: u64) -> bool {
        let is_target = |leaf: &Leaf<L>| leaf.inserted == position && !leaf.is_live();
        if !self.leaves().into_iter().any(is_target) {
            return false;
        }
        let Some(root) = self.root.take() else {
            return false;
        };
        self.root = remove_leaf_where(root, &is_target);
        self.normalize();
        true
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
            let count = found.len() as u64;
            for (number, leaf) in found.into_iter().enumerate() {
                leaf.inserted = number as u64;
            }
            self.next_insertion = count;
        } else if let Some(highest) = highest {
            self.next_insertion = self.next_insertion.max(highest + 1);
        }
    }
}

fn remove_leaf_where<L>(node: Node<L>, is_target: &impl Fn(&Leaf<L>) -> bool) -> Option<Node<L>> {
    match node {
        Node::Leaf(leaf) => {
            if is_target(&leaf) {
                None
            } else {
                Some(Node::Leaf(leaf))
            }
        }
        Node::Split { axis, children } => {
            let kept: Vec<Child<L>> = children
                .into_iter()
                .filter_map(|child| {
                    remove_leaf_where(child.node, is_target).map(|node| Child {
                        weight: child.weight,
                        node,
                    })
                })
                .collect();
            if kept.is_empty() {
                None
            } else {
                Some(Node::Split {
                    axis,
                    children: kept,
                })
            }
        }
    }
}

fn convert_node<L, M>(
    node: &Node<L>,
    fate: &mut impl FnMut(&Leaf<L>) -> LeafFate<M>,
) -> Option<Node<M>> {
    match node {
        Node::Leaf(leaf) => match fate(leaf) {
            LeafFate::Live(window) => Some(Node::Leaf(Leaf {
                inserted: leaf.inserted,
                occupant: Occupant::Live(window),
            })),
            LeafFate::Dormant(position) => Some(Node::Leaf(Leaf {
                inserted: leaf.inserted,
                occupant: Occupant::Dormant(position),
            })),
            LeafFate::Drop => None,
        },
        Node::Split { axis, children } => {
            let kept: Vec<Child<M>> = children
                .iter()
                .filter_map(|child| {
                    convert_node(&child.node, fate).map(|node| Child {
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
            .find(|leaf| leaf.window() == Some(window))
            .map(|leaf| leaf.inserted)
    }

    /// Adds `window` as the tree's only leaf. Does nothing if the tree
    /// already has one, so the caller cannot accidentally discard an
    /// arrangement.
    pub fn insert_first(&mut self, window: L) -> bool {
        if self.root.is_some() {
            return false;
        }
        let inserted = self.take_insertion();
        self.root = Some(Node::Leaf(Leaf {
            inserted,
            occupant: Occupant::Live(window),
        }));
        true
    }

    /// Makes `window` a sibling of the whole existing arrangement, in
    /// equal shares along `axis`.
    ///
    /// The insertion for a tree that has slots but no windows: every leaf
    /// is dormant, so there is no window to split, and the new one takes
    /// the whole display until a dormant slot is reclaimed. Does nothing
    /// to an empty tree; use [`Tree::insert_first`].
    pub fn split_root(&mut self, axis: SplitAxis, window: L) -> bool {
        let Some(existing) = self.root.take() else {
            return false;
        };
        let inserted = self.take_insertion();
        self.root = Some(Node::Split {
            axis,
            children: vec![
                Child {
                    weight: 1.0,
                    node: existing,
                },
                Child {
                    weight: 1.0,
                    node: Node::Leaf(Leaf {
                        inserted,
                        occupant: Occupant::Live(window),
                    }),
                },
            ],
        });
        self.normalize();
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
        let split = split_within(
            root,
            target,
            axis,
            Leaf {
                inserted,
                occupant: Occupant::Live(window),
            },
        );
        if split {
            self.normalize();
        }
        split
    }

    /// Removes the leaf holding `window`, closing the space it occupied.
    pub fn remove(&mut self, window: &L) -> bool {
        if !self.contains(window) {
            return false;
        }
        let Some(root) = self.root.take() else {
            return false;
        };
        self.root = remove_leaf_where(root, &|leaf: &Leaf<L>| leaf.window() == Some(window));
        self.normalize();
        true
    }

    /// Turns the leaf holding `window` dormant, keeping its slot and
    /// recording `position` -- the evidence that would recognise the
    /// window if it returned, and when it left. The slot's insertion
    /// number is unchanged. `false` if `window` is not in the tree.
    pub fn make_dormant(&mut self, window: &L, position: DormantPosition) -> bool {
        let Some(root) = self.root.as_mut() else {
            return false;
        };
        let mut found = Vec::new();
        root.leaves_mut(&mut found);
        match found.into_iter().find(|leaf| leaf.window() == Some(window)) {
            Some(leaf) => {
                leaf.occupant = Occupant::Dormant(position);
                true
            }
            None => false,
        }
    }

    /// Gives the dormant slot numbered `position` to `window`, which
    /// takes the slot's place in the arrangement. `false` if no dormant
    /// slot has that number.
    pub fn reclaim(&mut self, position: u64, window: L) -> bool {
        let Some(root) = self.root.as_mut() else {
            return false;
        };
        let mut found = Vec::new();
        root.leaves_mut(&mut found);
        match found
            .into_iter()
            .find(|leaf| leaf.inserted == position && !leaf.is_live())
        {
            Some(leaf) => {
                leaf.occupant = Occupant::Live(window);
                true
            }
            None => false,
        }
    }

    /// Exchanges the positions of two windows, changing nothing else.
    ///
    /// Containers, axes, weights, parentage, dormant slots, and each
    /// slot's insertion number all stay exactly as they were -- only the
    /// two occupants trade places (ADR 0026).
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

    /// Whether a divider on `axis` faces `toward` from `window`'s leaf.
    ///
    /// The divider is the one [`Tree::resize_toward`] would move: the
    /// nearest ancestor container on `axis` in which the child holding
    /// `window` has a sibling with a window on the `toward` side. The
    /// immediate parent is tried first, and only when it cannot satisfy
    /// the direction does the search climb outward (spec user story 36).
    /// A sibling holding only dormant slots takes no space and so is not
    /// a divider. `false` at a boundary of the arrangement, where no
    /// ancestor qualifies.
    pub fn has_divider_toward(&self, window: &L, axis: SplitAxis, toward: Toward) -> bool {
        let Some(root) = self.root.as_ref() else {
            return false;
        };
        let Some(path) = path_to(root, window) else {
            return false;
        };
        (0..path.len()).rev().any(|depth| {
            let Node::Split {
                axis: node_axis,
                children,
            } = node_at(root, &path[..depth])
            else {
                return false;
            };
            *node_axis == axis && sibling_toward(children, path[depth], toward).is_some()
        })
    }

    /// Moves the divider [`Tree::has_divider_toward`] describes by
    /// `fraction` of its container's visible extent, growing `window`'s
    /// side and shrinking the sibling's (CONTEXT.md "Tree resize").
    ///
    /// Only the two children on either side of the divider change share;
    /// the container's other children keep theirs, and its weights are
    /// renormalized to sum to one so that repeated resizes stay
    /// deterministic rather than drifting. `None` when no divider faces
    /// that way, or when the sibling would fall below [`MIN_WEIGHT`] --
    /// the tree is untouched in either case. Whether the resulting
    /// geometry respects every window's minimum size is the planner's
    /// question, not this one's.
    pub fn resize_toward(
        &mut self,
        window: &L,
        axis: SplitAxis,
        toward: Toward,
        fraction: f64,
    ) -> Option<DividerChange<L>>
    where
        L: Clone,
    {
        if !(fraction.is_finite() && fraction > 0.0) {
            return None;
        }
        let root = self.root.as_mut()?;
        let path = path_to(root, window)?;
        for depth in (0..path.len()).rev() {
            let Node::Split {
                axis: node_axis,
                children,
            } = node_at_mut(root, &path[..depth])
            else {
                continue;
            };
            if *node_axis != axis {
                continue;
            }
            let index = path[depth];
            let Some(sibling) = sibling_toward(children, index, toward) else {
                continue;
            };
            // Only children with windows take space, so the step is a
            // fraction of what is actually visible.
            let visible: f64 = children
                .iter()
                .filter(|child| child.node.has_window())
                .map(|child| child.weight)
                .sum();
            let total: f64 = children.iter().map(|child| child.weight).sum();
            let delta = fraction * visible;
            if children[sibling].weight - delta < MIN_WEIGHT * total {
                return None;
            }
            let weights_before: Vec<f64> = children.iter().map(|child| child.weight).collect();
            children[index].weight += delta;
            children[sibling].weight -= delta;
            let renormalized: f64 = children.iter().map(|child| child.weight).sum();
            for child in children.iter_mut() {
                child.weight /= renormalized;
            }
            return Some(DividerChange {
                grew: children[index]
                    .node
                    .windows()
                    .into_iter()
                    .cloned()
                    .collect(),
                shrank: children[sibling]
                    .node
                    .windows()
                    .into_iter()
                    .cloned()
                    .collect(),
                weights_before,
                weights_after: children.iter().map(|child| child.weight).collect(),
            });
        }
        None
    }
}

/// The index of the nearest child on the `toward` side of `index` that
/// holds a window, if any.
fn sibling_toward<L>(children: &[Child<L>], index: usize, toward: Toward) -> Option<usize> {
    match toward {
        Toward::Start => (0..index).rev().find(|j| children[*j].node.has_window()),
        Toward::End => (index + 1..children.len()).find(|j| children[*j].node.has_window()),
    }
}

/// The child indices leading from `node` to the leaf holding `wanted`.
fn path_to<L: PartialEq>(node: &Node<L>, wanted: &L) -> Option<Vec<usize>> {
    match node {
        Node::Leaf(leaf) => (leaf.window() == Some(wanted)).then(Vec::new),
        Node::Split { children, .. } => children.iter().enumerate().find_map(|(index, child)| {
            let mut path = path_to(&child.node, wanted)?;
            path.insert(0, index);
            Some(path)
        }),
    }
}

fn node_at<'a, L>(node: &'a Node<L>, path: &[usize]) -> &'a Node<L> {
    let mut current = node;
    for index in path {
        let Node::Split { children, .. } = current else {
            unreachable!("a path only descends through containers");
        };
        current = &children[*index].node;
    }
    current
}

fn node_at_mut<'a, L>(node: &'a mut Node<L>, path: &[usize]) -> &'a mut Node<L> {
    let mut current = node;
    for index in path {
        let Node::Split { children, .. } = current else {
            unreachable!("a path only descends through containers");
        };
        current = &mut children[*index].node;
    }
    current
}

fn swap_within<L: PartialEq + Clone>(node: &mut Node<L>, first: &L, second: &L) {
    match node {
        Node::Leaf(leaf) => {
            if leaf.window() == Some(first) {
                leaf.occupant = Occupant::Live(second.clone());
            } else if leaf.window() == Some(second) {
                leaf.occupant = Occupant::Live(first.clone());
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
            if held.window() != Some(target) {
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
                .position(|child| path_to(&child.node, target).is_some())
            else {
                return false;
            };
            split_within(&mut children[index].node, target, axis, leaf)
        }
    }
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
    use crate::id::ApplicationId;
    use crate::window::WindowRole;
    use crate::Rect;

    fn leaf(id: isize) -> WindowId {
        WindowId(id)
    }

    fn ids(tree: &ContainerTree) -> Vec<isize> {
        tree.windows().into_iter().map(|id| id.0).collect()
    }

    fn dormant_at(since_unix: i64) -> DormantPosition {
        DormantPosition {
            evidence: WindowEvidence {
                application_id: ApplicationId("Code.exe".to_owned()),
                executable_path: None,
                native_class: None,
                role: WindowRole::Normal,
                launch_order: 0,
                last_placement: Rect::new(0, 0, 100, 100),
                display_fingerprint: "DISPLAY1".to_owned(),
            },
            since_unix,
        }
    }

    /// H[ 1, V[ 2, 3 ] ]: window 3 sits under window 2 on the right.
    fn nested_tree() -> ContainerTree {
        let mut tree = ContainerTree::new();
        tree.insert_first(leaf(1));
        tree.split_leaf(&leaf(1), SplitAxis::Horizontal, leaf(2));
        tree.split_leaf(&leaf(2), SplitAxis::Vertical, leaf(3));
        tree
    }

    fn root_weights(tree: &ContainerTree) -> Vec<f64> {
        match tree.root() {
            Some(Node::Split { children, .. }) => children.iter().map(|c| c.weight).collect(),
            _ => Vec::new(),
        }
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
    fn resizing_moves_the_nearest_divider_that_faces_the_direction() {
        let mut tree = nested_tree();

        // Up from window 3: its parent is vertical and window 2 is above
        // it, so that divider moves and the root is untouched.
        let change = tree
            .resize_toward(&leaf(3), SplitAxis::Vertical, Toward::Start, 0.05)
            .expect("a divider faces up");

        assert_eq!(change.grew, vec![leaf(3)]);
        assert_eq!(change.shrank, vec![leaf(2)]);
        assert_eq!(change.weights_after, vec![0.45, 0.55]);
        assert_eq!(
            root_weights(&tree),
            vec![1.0, 1.0],
            "the outer split is untouched"
        );
    }

    #[test]
    fn resizing_climbs_outward_only_when_the_parent_cannot_face_that_way() {
        let mut tree = nested_tree();

        // Left from window 3: its parent is vertical, so the search climbs
        // to the horizontal root, where window 1 is on the left.
        let change = tree
            .resize_toward(&leaf(3), SplitAxis::Horizontal, Toward::Start, 0.05)
            .expect("the root divider faces left");

        assert_eq!(
            change.grew,
            vec![leaf(2), leaf(3)],
            "the whole subtree grows"
        );
        assert_eq!(change.shrank, vec![leaf(1)]);
        assert_eq!(change.weights_before, vec![1.0, 1.0]);
        assert_eq!(change.weights_after, vec![0.45, 0.55]);
    }

    #[test]
    fn resizing_at_a_boundary_is_refused_without_touching_the_tree() {
        let mut tree = nested_tree();
        let before = tree.clone();

        // Nothing is to the left of window 1, up from it, or down from it.
        assert!(!tree.has_divider_toward(&leaf(1), SplitAxis::Horizontal, Toward::Start));
        assert!(tree
            .resize_toward(&leaf(1), SplitAxis::Horizontal, Toward::Start, 0.05)
            .is_none());
        assert!(tree
            .resize_toward(&leaf(1), SplitAxis::Vertical, Toward::Start, 0.05)
            .is_none());
        assert!(tree
            .resize_toward(&leaf(1), SplitAxis::Vertical, Toward::End, 0.05)
            .is_none());
        assert!(tree
            .resize_toward(&leaf(99), SplitAxis::Horizontal, Toward::End, 0.05)
            .is_none());

        assert_eq!(tree, before);
    }

    #[test]
    fn resizing_only_touches_the_two_children_beside_the_divider() {
        let mut tree = Tree::from_root(Node::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                Child {
                    weight: 1.0,
                    node: Node::window(leaf(1)),
                },
                Child {
                    weight: 1.0,
                    node: Node::window(leaf(2)),
                },
                Child {
                    weight: 2.0,
                    node: Node::window(leaf(3)),
                },
            ],
        });

        let change = tree
            .resize_toward(&leaf(2), SplitAxis::Horizontal, Toward::End, 0.05)
            .expect("window 3 is to the right");

        assert_eq!(change.grew, vec![leaf(2)]);
        assert_eq!(change.shrank, vec![leaf(3)]);
        let weights = root_weights(&tree);
        assert!(
            (weights[0] - 0.25).abs() < 1e-9,
            "window 1 keeps its quarter"
        );
        assert!((weights[1] - 0.30).abs() < 1e-9);
        assert!((weights[2] - 0.45).abs() < 1e-9);
        assert!(
            (weights.iter().sum::<f64>() - 1.0).abs() < 1e-9,
            "renormalized"
        );
    }

    #[test]
    fn resizing_refuses_to_squeeze_a_sibling_below_the_minimum_weight() {
        let mut tree = Tree::from_root(Node::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                Child {
                    weight: 0.97,
                    node: Node::window(leaf(1)),
                },
                Child {
                    weight: 0.03,
                    node: Node::window(leaf(2)),
                },
            ],
        });
        let before = tree.clone();

        assert!(tree
            .resize_toward(&leaf(1), SplitAxis::Horizontal, Toward::End, 0.05)
            .is_none());
        assert_eq!(tree, before);
        assert!(
            tree.resize_toward(&leaf(1), SplitAxis::Horizontal, Toward::End, 0.01)
                .is_some(),
            "a smaller step that leaves the sibling visible is fine"
        );
    }

    #[test]
    fn repeated_resizes_are_deterministic_and_reversible() {
        let mut tree = nested_tree();
        let before = tree.clone();
        for _ in 0..3 {
            tree.resize_toward(&leaf(1), SplitAxis::Horizontal, Toward::End, 0.05);
        }
        for _ in 0..3 {
            tree.resize_toward(&leaf(2), SplitAxis::Horizontal, Toward::Start, 0.05);
        }
        let weights = root_weights(&tree);
        let original = root_weights(&before);
        // Both trees are renormalized forms of 1:1, so compare shares.
        let share = |w: &Vec<f64>| w[0] / (w[0] + w[1]);
        assert!((share(&weights) - share(&original)).abs() < 1e-9);
    }

    #[test]
    fn a_dormant_sibling_is_not_a_divider_and_takes_no_share_of_a_resize() {
        // H[ 1, dormant, 2 ]: from window 1, the divider to the right is
        // the one before window 2, and the step is 5% of what 1 and 2
        // share -- the dormant slot's weight is not part of the display.
        let mut tree = Tree::from_root(Node::Split {
            axis: SplitAxis::Horizontal,
            children: vec![
                Child {
                    weight: 1.0,
                    node: Node::window(leaf(1)),
                },
                Child {
                    weight: 1.0,
                    node: Node::dormant(dormant_at(0)),
                },
                Child {
                    weight: 1.0,
                    node: Node::window(leaf(2)),
                },
            ],
        });

        let change = tree
            .resize_toward(&leaf(1), SplitAxis::Horizontal, Toward::End, 0.05)
            .expect("window 2 is the divider to the right");

        assert_eq!(change.shrank, vec![leaf(2)]);
        // 1:1:1 with 5% of the visible 2.0 (= 0.1) moved: 1.1 : 1.0 : 0.9,
        // renormalized.
        let weights = root_weights(&tree);
        assert!((weights[0] / weights[2] - 1.1 / 0.9).abs() < 1e-9);
        assert!(
            (weights[1] / weights[2] - 1.0 / 0.9).abs() < 1e-9,
            "the dormant slot keeps its weight for when it is reclaimed"
        );
        assert!(!tree.has_divider_toward(&leaf(2), SplitAxis::Horizontal, Toward::End));
    }

    #[test]
    fn swapping_two_leaves_exchanges_only_their_occupants() {
        let mut tree = nested_tree();
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
        let tree = nested_tree();

        // Only window 3 could be identified after the restart.
        let restored = tree.filter_map_windows(&mut |id| (id.0 == 3).then_some(*id));

        assert_eq!(ids(&restored), vec![3]);
        assert!(
            matches!(restored.root(), Some(Node::Leaf(_))),
            "the containers that held the missing windows are gone"
        );
    }

    #[test]
    fn a_tree_round_trips_through_its_durable_form() {
        let mut tree = nested_tree();
        tree.make_dormant(&leaf(2), dormant_at(1_756_000_000));

        let as_text = tree.map_windows(&mut |id| id.0.to_string());
        let json = serde_json::to_string(&as_text).expect("a tree serializes");
        let read_back: Tree<String> = serde_json::from_str(&json).expect("and deserializes");
        let live = read_back.map_windows(&mut |text| WindowId(text.parse().expect("an id")));

        assert_eq!(
            live, tree,
            "structure, axes, weights, insertion numbers, and dormant slots all survive"
        );
        let mut continued = live;
        continued.split_leaf(&leaf(3), SplitAxis::Horizontal, leaf(4));
        assert_eq!(
            continued.insertion_of(&leaf(4)),
            Some(3),
            "the sequence continues where the stored tree left off"
        );
    }

    // ---- dormant positions (issue #53) ---------------------------------

    #[test]
    fn a_dormant_slot_keeps_its_place_and_number_but_holds_no_window() {
        let mut tree = nested_tree();

        assert!(tree.make_dormant(&leaf(2), dormant_at(100)));

        assert_eq!(ids(&tree), vec![1, 3], "window 2 is no longer a window");
        assert_eq!(tree.len(), 2);
        assert!(!tree.contains(&leaf(2)));
        assert_eq!(
            tree.dormant_positions()
                .into_iter()
                .map(|(position, dormant)| (position, dormant.since_unix))
                .collect::<Vec<_>>(),
            vec![(1, 100)],
            "the slot keeps insertion number 1 and remembers when it went dormant"
        );
        assert!(
            !tree.make_dormant(&leaf(2), dormant_at(200)),
            "a window that is not live cannot go dormant again"
        );
    }

    #[test]
    fn the_planner_projection_gives_a_dormant_slots_space_to_its_siblings() {
        let mut tree = nested_tree();
        tree.make_dormant(&leaf(2), dormant_at(100));

        let projection = tree.live_projection();

        assert_eq!(ids(&projection), vec![1, 3]);
        let Some(Node::Split { children, .. }) = projection.root() else {
            panic!("the root stays a container");
        };
        assert!(
            matches!(children[1].node, Node::Leaf(_)),
            "the vertical pair collapsed to window 3 alone, which takes the right half"
        );
        assert_eq!(
            tree.dormant_positions().len(),
            1,
            "projecting does not change the tree itself"
        );
    }

    #[test]
    fn reclaiming_gives_the_dormant_slot_to_the_returning_window() {
        let mut tree = nested_tree();
        let before = tree.clone();
        tree.make_dormant(&leaf(2), dormant_at(100));

        assert!(tree.reclaim(1, leaf(2)));

        assert_eq!(tree, before, "the arrangement is exactly as it was");
        assert!(!tree.reclaim(1, leaf(5)), "a live slot cannot be reclaimed");
        assert!(!tree.reclaim(42, leaf(5)), "nor an unknown one");
    }

    #[test]
    fn removing_a_dormant_position_closes_its_space_and_leaves_windows_alone() {
        let mut tree = nested_tree();
        tree.make_dormant(&leaf(2), dormant_at(100));

        assert!(tree.remove_position(1));

        assert_eq!(ids(&tree), vec![1, 3]);
        assert!(tree.dormant_positions().is_empty());
        assert!(
            !tree.remove_position(0),
            "a live slot is never removed through the position command"
        );
        assert_eq!(ids(&tree), vec![1, 3]);
    }

    #[test]
    fn dormant_slots_expire_after_seven_days_and_no_sooner() {
        let mut tree = nested_tree();
        tree.make_dormant(&leaf(2), dormant_at(1_000));
        tree.make_dormant(&leaf(3), dormant_at(2_000));

        assert_eq!(tree.prune_dormant(1_000 + DORMANT_RETENTION_SECONDS - 1), 0);
        assert_eq!(tree.dormant_positions().len(), 2);

        assert_eq!(tree.prune_dormant(1_000 + DORMANT_RETENTION_SECONDS), 1);
        assert_eq!(
            tree.dormant_positions()
                .into_iter()
                .map(|(position, _)| position)
                .collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(ids(&tree), vec![1], "the window is untouched");

        assert_eq!(tree.prune_dormant(2_000 + DORMANT_RETENTION_SECONDS), 1);
        assert!(
            matches!(tree.root(), Some(Node::Leaf(_))),
            "with every dormant slot gone the containers that located them are gone too"
        );
    }

    #[test]
    fn a_tree_of_only_dormant_slots_is_not_empty_and_takes_a_new_window_beside_them() {
        let mut tree = nested_tree();
        for id in [1, 2, 3] {
            tree.make_dormant(&leaf(id), dormant_at(0));
        }
        assert!(!tree.is_empty());
        assert!(!tree.has_windows());
        assert!(!tree.insert_first(leaf(9)), "the tree already has slots");

        assert!(tree.split_root(SplitAxis::Horizontal, leaf(9)));

        assert_eq!(ids(&tree), vec![9]);
        assert_eq!(tree.dormant_positions().len(), 3);
        assert_eq!(tree.insertion_of(&leaf(9)), Some(3));
        assert_eq!(
            ids(&tree.live_projection()),
            vec![9],
            "until a slot is reclaimed, the new window has the display to itself"
        );
    }
}
