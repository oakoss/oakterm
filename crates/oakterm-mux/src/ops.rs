//! Structural operations on the layout tree: split, close, resize, swap
//! (Spec-0007 Behavior, TREK-97).
//!
//! All operations mutate in place and leave the tree untouched on error, so
//! the daemon never loses a tab's layout to a rejected request. Resize works
//! in normalized weight space — the caller converts the wire protocol's pixel
//! delta (`delta_weight = delta_pixels / container_pixel_extent`) and minimum
//! pane size to weights, because only the daemon knows the pixel geometry.

use crate::geometry::panes_share_border;
use crate::layout::{Child, Container, LayoutNode, PaneId, SplitDirection};

/// Why a layout operation was rejected. The tree is unchanged.
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutError {
    /// The referenced pane is not in the tree.
    PaneNotFound(PaneId),
    /// A split would insert a pane ID the tree already contains.
    PaneAlreadyPresent(PaneId),
    /// Resize requires the two panes to share a border: adjacent sibling
    /// subtrees of one container, with each pane touching the shared edge
    /// (Spec-0001 `ResizePane` error case).
    NotAdjacentSiblings { pane: PaneId, neighbor: PaneId },
    /// A resize parameter was NaN, infinite, or a negative `min_weight`.
    InvalidResizeParam,
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayoutError::PaneNotFound(id) => write!(f, "pane {} not in the layout tree", id.0),
            LayoutError::PaneAlreadyPresent(id) => {
                write!(f, "pane {} is already in the layout tree", id.0)
            }
            LayoutError::NotAdjacentSiblings { pane, neighbor } => write!(
                f,
                "panes {} and {} do not share a resizable border",
                pane.0, neighbor.0
            ),
            LayoutError::InvalidResizeParam => {
                write!(f, "resize parameter was NaN, infinite, or negative")
            }
        }
    }
}

impl std::error::Error for LayoutError {}

/// Result of [`LayoutNode::close`]. Discarding it loses the last-pane
/// signal, leaving a tab open that should have closed.
#[must_use]
#[derive(Debug, Clone, PartialEq)]
pub enum CloseOutcome {
    /// The target was the only pane. The tree is unchanged; per Spec-0007 the
    /// tab (not the pane slot) should close.
    LastPane,
    /// The pane was removed and the tree re-canonicalized. Focus should move
    /// to the nearest sibling (the pane to the left/above when one exists).
    Removed { focus_hint: PaneId },
}

impl LayoutNode {
    /// Whether any leaf references `id`.
    #[must_use]
    pub fn contains(&self, id: PaneId) -> bool {
        match self {
            LayoutNode::Leaf(leaf) => *leaf == id,
            LayoutNode::Container(c) => c.children.iter().any(|ch| ch.node.contains(id)),
        }
    }

    /// Split `target`, inserting `new_pane` beside it (Spec-0007 Split).
    ///
    /// If `target`'s parent container already has `direction`, `new_pane` is
    /// inserted as a sibling directly after it: the newcomer gets weight
    /// `1/(N+1)` and existing siblings scale by `N/(N+1)`. Otherwise the
    /// target leaf becomes a two-child container in `direction`, splitting
    /// its weight evenly.
    ///
    /// # Errors
    /// [`LayoutError::PaneNotFound`] if `target` is absent;
    /// [`LayoutError::PaneAlreadyPresent`] if `new_pane` already exists.
    pub fn split(
        &mut self,
        target: PaneId,
        new_pane: PaneId,
        direction: SplitDirection,
    ) -> Result<(), LayoutError> {
        if self.contains(new_pane) {
            return Err(LayoutError::PaneAlreadyPresent(new_pane));
        }
        if !self.contains(target) {
            return Err(LayoutError::PaneNotFound(target));
        }
        split_in(self, target, new_pane, direction);
        Ok(())
    }

    /// Close `target`: remove its leaf, redistribute its weight
    /// proportionally among the remaining siblings, and re-canonicalize
    /// (Spec-0007 Close).
    ///
    /// # Errors
    /// [`LayoutError::PaneNotFound`] if `target` is absent.
    pub fn close(&mut self, target: PaneId) -> Result<CloseOutcome, LayoutError> {
        match self {
            LayoutNode::Leaf(leaf) => {
                if *leaf == target {
                    Ok(CloseOutcome::LastPane)
                } else {
                    Err(LayoutError::PaneNotFound(target))
                }
            }
            LayoutNode::Container(c) => {
                let LeafRemoval::Removed { hint } = remove_leaf(c, target) else {
                    return Err(LayoutError::PaneNotFound(target));
                };
                // Placeholder is unobservable: flatten is total (never
                // panics), so the real tree is always written back.
                let tree = std::mem::replace(self, LayoutNode::Leaf(target));
                *self = tree.flatten();
                // A canonical tree always yields a sibling hint; the
                // fallbacks keep close total on degenerate input (a
                // single-child container, or a subtree with no leaves).
                let focus_hint = hint
                    .or_else(|| self.pane_ids().first().copied())
                    .unwrap_or(target);
                Ok(CloseOutcome::Removed { focus_hint })
            }
        }
    }

    /// Move the boundary between `pane` and `neighbor` by `delta_weight`
    /// (positive grows `pane`), clamping so neither sibling drops below
    /// `min_weight` (Spec-0007 Resize).
    ///
    /// The two panes must share a border: they live in adjacent sibling
    /// subtrees of one container and each touches the shared edge. The
    /// adjustment applies to the two subtree weights — when a sibling is a
    /// container, its inner panes shrink proportionally and can end up
    /// below `min_weight`; per-pane minimums are not enforced through
    /// subtrees (Spec-0007 clamps sibling weights, not descendant panes).
    ///
    /// # Errors
    /// [`LayoutError::PaneNotFound`] if either pane is absent;
    /// [`LayoutError::NotAdjacentSiblings`] if they do not share a border;
    /// [`LayoutError::InvalidResizeParam`] on NaN, infinite, or negative
    /// parameters.
    pub fn resize(
        &mut self,
        pane: PaneId,
        neighbor: PaneId,
        delta_weight: f32,
        min_weight: f32,
    ) -> Result<(), LayoutError> {
        if !delta_weight.is_finite() || !min_weight.is_finite() || min_weight < 0.0 {
            return Err(LayoutError::InvalidResizeParam);
        }
        if !self.contains(pane) {
            return Err(LayoutError::PaneNotFound(pane));
        }
        if !self.contains(neighbor) {
            return Err(LayoutError::PaneNotFound(neighbor));
        }
        let LayoutNode::Container(c) = self else {
            // Single-leaf tree: both panes exist, so pane == neighbor == leaf,
            // which cannot be adjacent siblings.
            return Err(LayoutError::NotAdjacentSiblings { pane, neighbor });
        };
        resize_at_border(c, pane, neighbor, delta_weight, min_weight)
    }

    /// Exchange the positions of two panes. Each takes the other's slot and
    /// weight; the structure is otherwise untouched.
    ///
    /// # Errors
    /// [`LayoutError::PaneNotFound`] if either pane is absent.
    pub fn swap(&mut self, a: PaneId, b: PaneId) -> Result<(), LayoutError> {
        if !self.contains(a) {
            return Err(LayoutError::PaneNotFound(a));
        }
        if !self.contains(b) {
            return Err(LayoutError::PaneNotFound(b));
        }
        swap_leaves(self, a, b);
        Ok(())
    }
}

/// Apply the split at the target leaf. Membership is pre-validated, so the
/// target is always found.
fn split_in(node: &mut LayoutNode, target: PaneId, new_pane: PaneId, direction: SplitDirection) {
    match node {
        LayoutNode::Leaf(leaf) => {
            debug_assert_eq!(*leaf, target, "split_in reached a non-target leaf");
            // Rule 2: bare leaf (or parent of a different direction) becomes
            // a two-child container splitting the slot evenly.
            *node = LayoutNode::Container(Container {
                direction,
                children: vec![
                    Child::new(LayoutNode::Leaf(target), 0.5),
                    Child::new(LayoutNode::Leaf(new_pane), 0.5),
                ],
            });
        }
        LayoutNode::Container(c) => {
            // Rule 1: the target is a direct leaf child of a same-direction
            // container — insert the newcomer as its sibling.
            if c.direction == direction {
                let target_pos = c
                    .children
                    .iter()
                    .position(|ch| matches!(ch.node, LayoutNode::Leaf(id) if id == target));
                if let Some(i) = target_pos {
                    #[allow(clippy::cast_precision_loss)]
                    let n = c.children.len() as f32;
                    for ch in &mut c.children {
                        ch.weight *= n / (n + 1.0);
                    }
                    c.children.insert(
                        i + 1,
                        Child::new(LayoutNode::Leaf(new_pane), 1.0 / (n + 1.0)),
                    );
                    return;
                }
            }
            let child = c
                .children
                .iter_mut()
                .find(|ch| ch.node.contains(target))
                .expect("membership pre-validated");
            split_in(&mut child.node, target, new_pane, direction);
        }
    }
}

/// Outcome of [`remove_leaf`]: distinguishes "target not in this subtree"
/// from "removed, but no sibling could provide a hint" (the latter only on
/// degenerate, non-canonical trees).
enum LeafRemoval {
    NotFound,
    Removed { hint: Option<PaneId> },
}

/// Remove the leaf for `target` from the subtree, redistributing its weight
/// proportionally among its former siblings. The focus hint (the nearest
/// sibling pane, preferring the one to the left/above) is computed from the
/// pre-removal neighbors so it stays total on degenerate trees.
fn remove_leaf(c: &mut Container, target: PaneId) -> LeafRemoval {
    let target_pos = c
        .children
        .iter()
        .position(|ch| matches!(ch.node, LayoutNode::Leaf(id) if id == target));
    if let Some(i) = target_pos {
        let hint = if i > 0 {
            // Prefer the sibling before the closed pane; its last leaf is
            // the pane spatially nearest the vacated slot.
            c.children[i - 1].node.pane_ids().last().copied()
        } else {
            c.children
                .get(i + 1)
                .and_then(|ch| ch.node.pane_ids().first().copied())
        };
        c.children.remove(i);
        renormalize(&mut c.children);
        return LeafRemoval::Removed { hint };
    }
    for child in &mut c.children {
        if let LayoutNode::Container(inner) = &mut child.node {
            if let LeafRemoval::Removed { hint } = remove_leaf(inner, target) {
                return LeafRemoval::Removed { hint };
            }
        }
    }
    LeafRemoval::NotFound
}

/// Scale weights to sum to 1.0. Falls back to even weights if the sum is
/// degenerate (defensive: unreachable for a canonical input tree).
fn renormalize(children: &mut [Child]) {
    if children.is_empty() {
        return;
    }
    let sum: f32 = children.iter().map(|ch| ch.weight).sum();
    if sum.is_finite() && sum > 0.0 {
        for ch in children {
            ch.weight /= sum;
        }
    } else {
        #[allow(clippy::cast_precision_loss)]
        let even = 1.0 / children.len() as f32;
        for ch in children {
            ch.weight = even;
        }
    }
}

/// Floor applied to the two adjusted sibling weights regardless of the
/// caller's `min_weight`. Without it, `min_weight` of 0.0 (a degenerate
/// pixel extent) or below one f32 ulp lets a weight land on exactly 0.0,
/// breaking the positive-weights invariant with no error.
const MIN_SIBLING_WEIGHT: f32 = 1e-4;

/// Descend to the container where `pane` and `neighbor` part ways and adjust
/// the boundary between their subtrees. Membership is pre-validated.
fn resize_at_border(
    c: &mut Container,
    pane: PaneId,
    neighbor: PaneId,
    delta_weight: f32,
    min_weight: f32,
) -> Result<(), LayoutError> {
    let pane_idx = c
        .children
        .iter()
        .position(|ch| ch.node.contains(pane))
        .expect("membership pre-validated");
    let neighbor_idx = c
        .children
        .iter()
        .position(|ch| ch.node.contains(neighbor))
        .expect("membership pre-validated");

    if pane_idx == neighbor_idx {
        let LayoutNode::Container(inner) = &mut c.children[pane_idx].node else {
            // Both ids in one leaf means pane == neighbor: no border.
            return Err(LayoutError::NotAdjacentSiblings { pane, neighbor });
        };
        return resize_at_border(inner, pane, neighbor, delta_weight, min_weight);
    }
    if pane_idx.abs_diff(neighbor_idx) != 1 {
        return Err(LayoutError::NotAdjacentSiblings { pane, neighbor });
    }
    if !panes_share_border(c, pane_idx, pane, neighbor_idx, neighbor) {
        return Err(LayoutError::NotAdjacentSiblings { pane, neighbor });
    }

    // Clamp the delta so neither sibling drops below the floor. A sibling
    // already below it (degenerate input) is never pushed lower.
    let floor = min_weight.max(MIN_SIBLING_WEIGHT);
    let grow = c.children[pane_idx].weight;
    let shrink = c.children[neighbor_idx].weight;
    let lo = -(grow - floor).max(0.0);
    let hi = (shrink - floor).max(0.0);
    let applied = delta_weight.clamp(lo, hi);

    c.children[pane_idx].weight += applied;
    c.children[neighbor_idx].weight -= applied;
    renormalize(&mut c.children);
    Ok(())
}

fn swap_leaves(node: &mut LayoutNode, a: PaneId, b: PaneId) {
    match node {
        LayoutNode::Leaf(id) => {
            if *id == a {
                *id = b;
            } else if *id == b {
                *id = a;
            }
        }
        LayoutNode::Container(c) => {
            for child in &mut c.children {
                swap_leaves(&mut child.node, a, b);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use SplitDirection::{Horizontal, Vertical};

    const A: PaneId = PaneId(1);
    const B: PaneId = PaneId(2);
    const C: PaneId = PaneId(3);
    const D: PaneId = PaneId(4);
    const NEW: PaneId = PaneId(99);

    fn leaf(id: PaneId) -> LayoutNode {
        LayoutNode::Leaf(id)
    }

    fn container(
        direction: SplitDirection,
        nodes: Vec<LayoutNode>,
        weights: Vec<f32>,
    ) -> LayoutNode {
        assert_eq!(nodes.len(), weights.len(), "test setup: parallel arrays");
        let children = nodes
            .into_iter()
            .zip(weights)
            .map(|(node, weight)| Child::new(node, weight))
            .collect();
        LayoutNode::Container(Container {
            direction,
            children,
        })
    }

    fn assert_weights(node: &LayoutNode, expected: &[f32]) {
        let LayoutNode::Container(c) = node else {
            panic!("expected a container, got {node:?}");
        };
        assert_eq!(c.children.len(), expected.len(), "child count");
        for (child, want) in c.children.iter().zip(expected) {
            assert!(
                (child.weight - want).abs() < 1e-6,
                "weight {} != {want}",
                child.weight
            );
        }
    }

    // --- split ---

    #[test]
    fn split_bare_leaf_builds_even_container() {
        let mut tree = leaf(A);
        tree.split(A, NEW, Horizontal).unwrap();

        assert_eq!(tree.pane_ids(), vec![A, NEW]);
        assert_weights(&tree, &[0.5, 0.5]);
        assert!(tree.is_canonical());
    }

    #[test]
    fn split_same_direction_inserts_sibling_after_target() {
        // Spec example: H[A: 0.5, B: 0.5] split B horizontally -> thirds.
        let mut tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        tree.split(B, NEW, Horizontal).unwrap();

        assert_eq!(tree.pane_ids(), vec![A, B, NEW]);
        assert_weights(&tree, &[1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0]);
        assert!(tree.is_canonical());
    }

    #[test]
    fn split_same_direction_scales_uneven_weights() {
        // [0.5, 0.25, 0.25] split A: existing scale by 3/4, newcomer gets 1/4.
        let mut tree = container(
            Horizontal,
            vec![leaf(A), leaf(B), leaf(C)],
            vec![0.5, 0.25, 0.25],
        );
        tree.split(A, NEW, Horizontal).unwrap();

        assert_eq!(tree.pane_ids(), vec![A, NEW, B, C]);
        assert_weights(&tree, &[0.375, 0.25, 0.1875, 0.1875]);
        assert!(tree.is_canonical());
    }

    #[test]
    fn split_different_direction_nests_a_container() {
        let mut tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        tree.split(B, NEW, Vertical).unwrap();

        assert_eq!(tree.pane_ids(), vec![A, B, NEW]);
        assert_weights(&tree, &[0.5, 0.5]);
        let LayoutNode::Container(root) = &tree else {
            panic!("expected container");
        };
        let LayoutNode::Container(inner) = &root.children[1].node else {
            panic!("expected nested container");
        };
        assert_eq!(inner.direction, Vertical);
        assert_weights(&root.children[1].node, &[0.5, 0.5]);
        assert!(tree.is_canonical());
    }

    #[test]
    fn split_uses_parent_not_grandparent_direction() {
        // V[A, H[B, C]]: splitting B vertically nests under B's H parent even
        // though the V grandparent matches the requested direction.
        let inner = container(Horizontal, vec![leaf(B), leaf(C)], vec![0.5, 0.5]);
        let mut tree = container(Vertical, vec![leaf(A), inner], vec![0.5, 0.5]);
        tree.split(B, NEW, Vertical).unwrap();

        assert_eq!(tree.pane_ids(), vec![A, B, NEW, C]);
        assert!(tree.is_canonical());
        let LayoutNode::Container(root) = &tree else {
            panic!("expected container");
        };
        let LayoutNode::Container(h) = &root.children[1].node else {
            panic!("expected H container");
        };
        assert!(
            matches!(&h.children[0].node, LayoutNode::Container(v) if v.direction == Vertical),
            "B's slot should hold a new vertical container"
        );
    }

    #[test]
    fn split_deep_same_direction_target() {
        // H[A, V[B, C]] split C vertically: C sits in a same-direction (V)
        // parent, so it gains a sibling there.
        let inner = container(Vertical, vec![leaf(B), leaf(C)], vec![0.5, 0.5]);
        let mut tree = container(Horizontal, vec![leaf(A), inner], vec![0.5, 0.5]);
        tree.split(C, NEW, Vertical).unwrap();

        assert_eq!(tree.pane_ids(), vec![A, B, C, NEW]);
        let LayoutNode::Container(root) = &tree else {
            panic!("expected container");
        };
        assert_weights(&root.children[1].node, &[1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0]);
        assert!(tree.is_canonical());
    }

    #[test]
    fn split_rejects_unknown_target() {
        let mut tree = leaf(A);
        let before = tree.clone();
        assert_eq!(
            tree.split(B, NEW, Horizontal),
            Err(LayoutError::PaneNotFound(B))
        );
        assert_eq!(tree, before, "tree must be unchanged on error");
    }

    #[test]
    fn split_rejects_duplicate_new_pane() {
        let mut tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        let before = tree.clone();
        assert_eq!(
            tree.split(A, B, Vertical),
            Err(LayoutError::PaneAlreadyPresent(B))
        );
        assert_eq!(
            tree.split(A, A, Vertical),
            Err(LayoutError::PaneAlreadyPresent(A))
        );
        assert_eq!(tree, before);
    }

    // --- close ---

    #[test]
    fn close_last_pane_reports_and_keeps_tree() {
        let mut tree = leaf(A);
        assert_eq!(tree.close(A), Ok(CloseOutcome::LastPane));
        assert_eq!(tree, leaf(A));
    }

    #[test]
    fn close_redistributes_weight_proportionally() {
        // [0.25, 0.5, 0.25]: closing B leaves A and C at 0.5 each.
        let mut tree = container(
            Horizontal,
            vec![leaf(A), leaf(B), leaf(C)],
            vec![0.25, 0.5, 0.25],
        );
        let outcome = tree.close(B).unwrap();

        assert_eq!(outcome, CloseOutcome::Removed { focus_hint: A });
        assert_eq!(tree.pane_ids(), vec![A, C]);
        assert_weights(&tree, &[0.5, 0.5]);
        assert!(tree.is_canonical());
    }

    #[test]
    fn close_first_child_hints_next_sibling() {
        let mut tree = container(
            Horizontal,
            vec![leaf(A), leaf(B), leaf(C)],
            vec![0.4, 0.3, 0.3],
        );
        let outcome = tree.close(A).unwrap();
        assert_eq!(outcome, CloseOutcome::Removed { focus_hint: B });
    }

    #[test]
    fn close_first_child_with_container_successor_hints_its_first_leaf() {
        // H[A, V[B, C]]: closing A hints B, the first leaf of the successor.
        let inner = container(Vertical, vec![leaf(B), leaf(C)], vec![0.5, 0.5]);
        let mut tree = container(Horizontal, vec![leaf(A), inner], vec![0.5, 0.5]);
        let outcome = tree.close(A).unwrap();
        assert_eq!(outcome, CloseOutcome::Removed { focus_hint: B });
    }

    #[test]
    fn close_hints_deep_left_siblings_last_leaf() {
        // H[ V[A, H[B, C]], D ]: closing D hints C — the last leaf of the
        // left sibling at depth 2.
        let bc = container(Horizontal, vec![leaf(B), leaf(C)], vec![0.5, 0.5]);
        let left = container(Vertical, vec![leaf(A), bc], vec![0.5, 0.5]);
        let mut tree = container(Horizontal, vec![left, leaf(D)], vec![0.5, 0.5]);
        let outcome = tree.close(D).unwrap();
        assert_eq!(outcome, CloseOutcome::Removed { focus_hint: C });
    }

    #[test]
    fn close_prefers_left_siblings_last_leaf() {
        // H[ V[A, B], C ]: closing C hints B — the bottom pane of the left
        // sibling, spatially nearest the vacated slot.
        let inner = container(Vertical, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        let mut tree = container(Horizontal, vec![inner, leaf(C)], vec![0.5, 0.5]);
        let outcome = tree.close(C).unwrap();
        assert_eq!(outcome, CloseOutcome::Removed { focus_hint: B });
    }

    #[test]
    fn close_collapses_two_child_container_to_leaf() {
        let mut tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        let outcome = tree.close(B).unwrap();

        assert_eq!(outcome, CloseOutcome::Removed { focus_hint: A });
        assert_eq!(tree, leaf(A));
    }

    #[test]
    fn close_collapse_inherits_weight_and_merges_same_direction() {
        // H[ A(0.5), V[ B(0.5), H[C, D](0.5) ](0.5) ]: closing B collapses the
        // V to its surviving H child, which merges into the root.
        let cd = container(Horizontal, vec![leaf(C), leaf(D)], vec![0.5, 0.5]);
        let v = container(Vertical, vec![leaf(B), cd], vec![0.5, 0.5]);
        let mut tree = container(Horizontal, vec![leaf(A), v], vec![0.5, 0.5]);

        let outcome = tree.close(B).unwrap();

        assert_eq!(outcome, CloseOutcome::Removed { focus_hint: C });
        assert_eq!(tree.pane_ids(), vec![A, C, D]);
        assert_weights(&tree, &[0.5, 0.25, 0.25]);
        assert!(tree.is_canonical());
    }

    #[test]
    fn close_deep_leaf_leaves_ancestors_untouched() {
        // H[A, V[B, C]]: closing C collapses the V; root keeps its weights.
        let inner = container(Vertical, vec![leaf(B), leaf(C)], vec![0.7, 0.3]);
        let mut tree = container(Horizontal, vec![leaf(A), inner], vec![0.6, 0.4]);

        let outcome = tree.close(C).unwrap();

        assert_eq!(outcome, CloseOutcome::Removed { focus_hint: B });
        assert_eq!(tree.pane_ids(), vec![A, B]);
        assert_weights(&tree, &[0.6, 0.4]);
        assert!(tree.is_canonical());
    }

    #[test]
    fn close_rejects_unknown_pane() {
        let mut tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        let before = tree.clone();
        assert_eq!(tree.close(C), Err(LayoutError::PaneNotFound(C)));
        assert_eq!(tree, before);

        let mut single = leaf(A);
        assert_eq!(single.close(B), Err(LayoutError::PaneNotFound(B)));
    }

    // --- resize ---

    #[test]
    fn resize_moves_weight_between_siblings() {
        let mut tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        tree.resize(A, B, 0.1, 0.05).unwrap();
        assert_weights(&tree, &[0.6, 0.4]);
        assert!(tree.is_canonical());
    }

    #[test]
    fn resize_negative_delta_shrinks_pane() {
        let mut tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        tree.resize(A, B, -0.2, 0.05).unwrap();
        assert_weights(&tree, &[0.3, 0.7]);
    }

    #[test]
    fn resize_reversed_pair_grows_the_named_pane() {
        // `pane` on the right of `neighbor`: positive delta still grows `pane`.
        let mut tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        tree.resize(B, A, 0.1, 0.05).unwrap();
        assert_weights(&tree, &[0.4, 0.6]);
    }

    #[test]
    fn resize_clamps_at_neighbor_minimum() {
        let mut tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        tree.resize(A, B, 0.9, 0.1).unwrap();
        assert_weights(&tree, &[0.9, 0.1]);
    }

    #[test]
    fn resize_clamps_at_pane_minimum() {
        let mut tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        tree.resize(A, B, -0.9, 0.1).unwrap();
        assert_weights(&tree, &[0.1, 0.9]);
    }

    #[test]
    fn resize_below_floor_sibling_is_never_pushed_lower() {
        // A already sits below the floor; shrinking it further is a no-op.
        let mut tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.05, 0.95]);
        tree.resize(A, B, -0.5, 0.1).unwrap();
        assert_weights(&tree, &[0.05, 0.95]);
    }

    #[test]
    fn resize_border_between_subtrees_adjusts_subtree_weights() {
        // H[A, V[B, C]]: the border between A and either of B/C is the root's;
        // resizing adjusts A vs the V subtree.
        let inner = container(Vertical, vec![leaf(B), leaf(C)], vec![0.5, 0.5]);
        let mut tree = container(Horizontal, vec![leaf(A), inner], vec![0.5, 0.5]);

        tree.resize(A, C, 0.2, 0.05).unwrap();
        assert_weights(&tree, &[0.7, 0.3]);
        let LayoutNode::Container(root) = &tree else {
            panic!("expected container");
        };
        assert_weights(&root.children[1].node, &[0.5, 0.5]);
    }

    #[test]
    fn resize_descends_to_the_common_container() {
        // H[A, V[B, C]]: B/C share the inner V's border; the root is untouched.
        let inner = container(Vertical, vec![leaf(B), leaf(C)], vec![0.5, 0.5]);
        let mut tree = container(Horizontal, vec![leaf(A), inner], vec![0.6, 0.4]);

        tree.resize(B, C, 0.25, 0.05).unwrap();
        assert_weights(&tree, &[0.6, 0.4]);
        let LayoutNode::Container(root) = &tree else {
            panic!("expected container");
        };
        assert_weights(&root.children[1].node, &[0.75, 0.25]);
    }

    #[test]
    fn resize_rejects_non_adjacent_siblings() {
        let mut tree = container(
            Horizontal,
            vec![leaf(A), leaf(B), leaf(C)],
            vec![0.4, 0.3, 0.3],
        );
        let before = tree.clone();
        assert_eq!(
            tree.resize(A, C, 0.1, 0.05),
            Err(LayoutError::NotAdjacentSiblings {
                pane: A,
                neighbor: C
            })
        );
        assert_eq!(tree, before);
    }

    #[test]
    fn resize_rejects_same_pane_pair() {
        let mut tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        assert_eq!(
            tree.resize(A, A, 0.1, 0.05),
            Err(LayoutError::NotAdjacentSiblings {
                pane: A,
                neighbor: A
            })
        );

        let mut single = leaf(A);
        assert_eq!(
            single.resize(A, A, 0.1, 0.05),
            Err(LayoutError::NotAdjacentSiblings {
                pane: A,
                neighbor: A
            })
        );
    }

    #[test]
    fn resize_rejects_unknown_panes() {
        let mut tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        assert_eq!(
            tree.resize(C, A, 0.1, 0.05),
            Err(LayoutError::PaneNotFound(C))
        );
        assert_eq!(
            tree.resize(A, C, 0.1, 0.05),
            Err(LayoutError::PaneNotFound(C))
        );
    }

    #[test]
    fn resize_rejects_invalid_params() {
        let mut tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        let before = tree.clone();
        assert_eq!(
            tree.resize(A, B, f32::NAN, 0.05),
            Err(LayoutError::InvalidResizeParam)
        );
        assert_eq!(
            tree.resize(A, B, f32::INFINITY, 0.05),
            Err(LayoutError::InvalidResizeParam)
        );
        assert_eq!(
            tree.resize(A, B, 0.1, f32::NAN),
            Err(LayoutError::InvalidResizeParam)
        );
        assert_eq!(
            tree.resize(A, B, 0.1, -0.1),
            Err(LayoutError::InvalidResizeParam)
        );
        assert_eq!(tree, before);
    }

    #[test]
    fn resize_zero_min_weight_keeps_weights_positive() {
        // A degenerate pixel extent yields min_weight = 0.0; the internal
        // floor must keep the shrinking sibling strictly positive.
        let mut tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        tree.resize(A, B, 0.9, 0.0).unwrap();

        assert!(tree.is_canonical(), "weights must stay positive");
        let LayoutNode::Container(c) = &tree else {
            panic!("expected container");
        };
        assert!(c.children[1].weight > 0.0);
    }

    #[test]
    fn resize_sub_ulp_min_weight_keeps_weights_positive() {
        // min_weight below one f32 ulp of the weight would round the
        // remainder to exactly 0.0 without the internal floor.
        let mut tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        tree.resize(A, B, 0.9, 1e-10).unwrap();

        assert!(tree.is_canonical());
        let LayoutNode::Container(c) = &tree else {
            panic!("expected container");
        };
        assert!(c.children[1].weight > 0.0);
    }

    #[test]
    fn resize_rejects_diagonal_pane_pair() {
        // H[V[A,B], V[C,D]]: A (top-left) and D (bottom-right) sit in
        // adjacent columns but meet only at a corner — no shared border.
        let left = container(Vertical, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        let right = container(Vertical, vec![leaf(C), leaf(D)], vec![0.5, 0.5]);
        let mut tree = container(Horizontal, vec![left, right], vec![0.5, 0.5]);
        let before = tree.clone();

        assert_eq!(
            tree.resize(A, D, 0.1, 0.05),
            Err(LayoutError::NotAdjacentSiblings {
                pane: A,
                neighbor: D
            })
        );
        assert_eq!(
            tree.resize(B, C, 0.1, 0.05),
            Err(LayoutError::NotAdjacentSiblings {
                pane: B,
                neighbor: C
            })
        );
        assert_eq!(tree, before);
    }

    #[test]
    fn resize_accepts_border_sharing_pairs_across_subtrees() {
        // Same grid: A/C share the top half of the column border, B/D the
        // bottom half. Both pairs may drag it.
        let left = container(Vertical, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        let right = container(Vertical, vec![leaf(C), leaf(D)], vec![0.5, 0.5]);
        let mut tree = container(Horizontal, vec![left, right], vec![0.5, 0.5]);

        tree.resize(A, C, 0.1, 0.05).unwrap();
        assert_weights(&tree, &[0.6, 0.4]);
        tree.resize(B, D, -0.1, 0.05).unwrap();
        assert_weights(&tree, &[0.5, 0.5]);
    }

    #[test]
    fn resize_rejects_pane_separated_from_border_by_a_sliver() {
        // H[ V[ H[A(0.999), S(0.001)](0.5), B(0.5) ], C ]: a sliver pane S
        // sits between A and the column border, so A must not be treated as
        // touching it — the structural check ignores how thin S is.
        const S: PaneId = PaneId(50);
        let a_s = container(Horizontal, vec![leaf(A), leaf(S)], vec![0.999, 0.001]);
        let left = container(Vertical, vec![a_s, leaf(B)], vec![0.5, 0.5]);
        let mut tree = container(Horizontal, vec![left, leaf(C)], vec![0.5, 0.5]);
        let before = tree.clone();

        assert_eq!(
            tree.resize(A, C, 0.1, 0.05),
            Err(LayoutError::NotAdjacentSiblings {
                pane: A,
                neighbor: C
            })
        );
        assert_eq!(tree, before);

        // The sliver itself does share the border.
        tree.resize(S, C, 0.1, 0.05).unwrap();
        assert_weights(&tree, &[0.6, 0.4]);
    }

    #[test]
    fn resize_rejects_pane_away_from_the_border() {
        // V[ H[A, V[B, C]], D ]: only C reaches the border with D; B sits
        // above C and does not touch it.
        let bc = container(Vertical, vec![leaf(B), leaf(C)], vec![0.5, 0.5]);
        let top = container(Horizontal, vec![leaf(A), bc], vec![0.5, 0.5]);
        let mut tree = container(Vertical, vec![top, leaf(D)], vec![0.5, 0.5]);

        assert_eq!(
            tree.resize(B, D, 0.1, 0.05),
            Err(LayoutError::NotAdjacentSiblings {
                pane: B,
                neighbor: D
            })
        );
        tree.resize(C, D, 0.1, 0.05).unwrap();
        assert_weights(&tree, &[0.6, 0.4]);
    }

    #[test]
    fn resize_subtree_squeeze_clamps_the_sibling_not_its_panes() {
        // Documented limitation: the clamp floors the V subtree's weight at
        // min_weight, so its inner panes end up at min_weight / 2 each.
        let inner = container(Vertical, vec![leaf(B), leaf(C)], vec![0.5, 0.5]);
        let mut tree = container(Horizontal, vec![leaf(A), inner], vec![0.5, 0.5]);

        tree.resize(A, B, 0.9, 0.1).unwrap();

        assert_weights(&tree, &[0.9, 0.1]);
        let LayoutNode::Container(root) = &tree else {
            panic!("expected container");
        };
        assert_weights(&root.children[1].node, &[0.5, 0.5]);
    }

    // --- swap ---

    #[test]
    fn swap_exchanges_slots_and_keeps_weights() {
        let inner = container(Vertical, vec![leaf(B), leaf(C)], vec![0.7, 0.3]);
        let mut tree = container(Horizontal, vec![leaf(A), inner], vec![0.6, 0.4]);

        tree.swap(A, C).unwrap();

        assert_eq!(tree.pane_ids(), vec![C, B, A]);
        assert_weights(&tree, &[0.6, 0.4]);
        let LayoutNode::Container(root) = &tree else {
            panic!("expected container");
        };
        assert_weights(&root.children[1].node, &[0.7, 0.3]);
        assert!(tree.is_canonical());
    }

    #[test]
    fn swap_same_pane_is_a_no_op() {
        let mut tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        let before = tree.clone();
        tree.swap(A, A).unwrap();
        assert_eq!(tree, before);
    }

    #[test]
    fn swap_rejects_unknown_pane() {
        let mut tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        let before = tree.clone();
        assert_eq!(tree.swap(A, C), Err(LayoutError::PaneNotFound(C)));
        assert_eq!(tree.swap(C, A), Err(LayoutError::PaneNotFound(C)));
        assert_eq!(tree, before);
    }

    // --- cross-op ---

    #[test]
    fn split_then_close_round_trips() {
        let mut tree = leaf(A);
        tree.split(A, B, Horizontal).unwrap();
        tree.split(B, C, Vertical).unwrap();

        assert_eq!(tree.close(C), Ok(CloseOutcome::Removed { focus_hint: B }));
        assert_eq!(tree.close(B), Ok(CloseOutcome::Removed { focus_hint: A }));
        assert_eq!(tree, leaf(A));
        assert_eq!(tree.close(A), Ok(CloseOutcome::LastPane));
    }

    #[test]
    fn many_splits_stay_canonical_and_sum_to_one() {
        let mut tree = leaf(PaneId(0));
        for i in 1..32u32 {
            let target = PaneId(i - 1);
            let dir = if i % 2 == 0 { Horizontal } else { Vertical };
            tree.split(target, PaneId(i), dir).unwrap();
            assert!(
                tree.is_canonical(),
                "tree lost canonical form after split {i}"
            );
        }
        assert_eq!(tree.pane_ids().len(), 32);
    }

    /// Deterministic LCG so the stress sequence is reproducible.
    fn lcg(state: &mut u64) -> u32 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        u32::try_from(*state >> 33).expect("31-bit value")
    }

    #[test]
    fn random_op_sequences_keep_the_tree_canonical() {
        // Exercises the N/(N+1) split scaling, close renormalization, and
        // resize clamping under accumulated f32 drift — shapes the
        // hand-written tests' power-of-two weights never reach.
        let mut rng: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut tree = leaf(PaneId(0));
        let mut live = vec![PaneId(0)];
        let mut next_id = 1u32;

        for step in 0..500 {
            let op = lcg(&mut rng) % 3;
            if op == 0 && live.len() < 64 {
                let target = live[lcg(&mut rng) as usize % live.len()];
                let dir = if lcg(&mut rng) % 2 == 0 {
                    Horizontal
                } else {
                    Vertical
                };
                let id = PaneId(next_id);
                next_id += 1;
                tree.split(target, id, dir).unwrap();
                live.push(id);
            } else if op == 1 && live.len() > 1 {
                let target = live.swap_remove(lcg(&mut rng) as usize % live.len());
                let CloseOutcome::Removed { .. } = tree.close(target).unwrap() else {
                    panic!("more than one pane was live");
                };
            } else if live.len() > 1 {
                let a = live[lcg(&mut rng) as usize % live.len()];
                let b = live[lcg(&mut rng) as usize % live.len()];
                #[allow(clippy::cast_precision_loss)]
                let delta = (lcg(&mut rng) % 200) as f32 / 1000.0 - 0.1;
                match tree.resize(a, b, delta, 0.02) {
                    Ok(()) | Err(LayoutError::NotAdjacentSiblings { .. }) => {}
                    Err(e) => panic!("unexpected resize error at step {step}: {e}"),
                }
            }
            assert!(
                tree.is_canonical(),
                "tree lost canonical form at step {step}: {:?}",
                tree.validate()
            );
            assert_eq!(tree.pane_ids().len(), live.len(), "pane count at {step}");
        }
    }
}
