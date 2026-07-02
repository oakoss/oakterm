//! The n-ary split-tree layout model (Spec-0007 Contract, ADR-0010).
//!
//! [`LayoutNode`] is the recursive tree of tiled panes: internal nodes are
//! [`Container`]s that arrange N weighted [`Child`]ren along one axis, and
//! leaves reference a pane by [`PaneId`]. After any structural mutation,
//! [`LayoutNode::flatten`] restores the canonical form — no single-child
//! containers, no same-direction nesting — that split, close, and resize
//! operations (TREK-97) assume.
//!
//! This is the in-memory model only; it carries no serde derives. The wire
//! protocol and session persistence serialize the Spec-0010 `SavedLayoutNode`
//! DTOs (EPIC-12), which own the documented JSON shape (parallel
//! `children`/`weights` arrays, lowercase directions) and convert to/from this
//! tree.

/// Tolerance for the "weights sum to 1.0" invariant. `f32` accumulation over a
/// handful of splits stays well inside this bound.
pub const WEIGHT_SUM_TOLERANCE: f32 = 0.001;

/// Opaque pane identifier, assigned by the daemon. `u32` on the wire (Spec-0001).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(pub u32);

/// Axis along which a [`Container`]'s children are arranged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    /// Children arranged left-to-right.
    Horizontal,
    /// Children arranged top-to-bottom.
    Vertical,
}

/// A node in the tiled layout tree: an internal [`Container`] or a pane leaf.
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutNode {
    Container(Container),
    Leaf(PaneId),
}

/// A weighted child of a [`Container`]. Pairing the weight with the node makes
/// a weight/child-count mismatch unrepresentable.
#[derive(Debug, Clone, PartialEq)]
pub struct Child {
    /// The child subtree.
    pub node: LayoutNode,

    /// Proportional size of this child; a container's child weights sum to 1.0.
    pub weight: f32,
}

/// An internal node arranging its weighted children along [`Container::direction`].
///
/// In canonical form (see [`LayoutNode::flatten`]) a container has at least two
/// children with positive weights that sum to 1.0, and never directly nests a
/// container of the same direction.
#[derive(Debug, Clone, PartialEq)]
pub struct Container {
    /// Split direction. Children are arranged along this axis.
    pub direction: SplitDirection,

    /// Ordered children, laid out first-to-last along `direction`.
    pub children: Vec<Child>,
}

/// A way in which a tree departs from the canonical-form invariants, reported
/// by [`LayoutNode::validate`].
#[derive(Debug, Clone, PartialEq)]
pub enum InvariantViolation {
    /// A container has fewer than two children.
    TooFewChildren { found: usize },
    /// A weight is NaN or infinite.
    NonFiniteWeight { index: usize },
    /// A weight is zero or negative.
    NonPositiveWeight { index: usize, weight: f32 },
    /// A container's weights do not sum to 1.0 within [`WEIGHT_SUM_TOLERANCE`].
    WeightsSumOutOfTolerance { sum: f32 },
    /// A container directly nests a container of the same direction.
    SameDirectionNesting,
}

impl Child {
    #[must_use]
    pub fn new(node: LayoutNode, weight: f32) -> Self {
        Self { node, weight }
    }
}

impl Container {
    /// Build a container whose `children` share the available space evenly.
    ///
    /// # Panics
    /// Panics if given fewer than two children — a container is meaningless
    /// below two, and callers building splits always have at least two.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn even(direction: SplitDirection, children: Vec<LayoutNode>) -> Self {
        assert!(
            children.len() >= 2,
            "a container needs at least two children, got {}",
            children.len()
        );
        let weight = 1.0 / children.len() as f32;
        let children = children
            .into_iter()
            .map(|node| Child::new(node, weight))
            .collect();
        Self {
            direction,
            children,
        }
    }
}

impl LayoutNode {
    /// A bare pane leaf.
    #[must_use]
    pub fn leaf(id: PaneId) -> Self {
        LayoutNode::Leaf(id)
    }

    #[must_use]
    pub fn is_leaf(&self) -> bool {
        matches!(self, LayoutNode::Leaf(_))
    }

    #[must_use]
    pub fn is_container(&self) -> bool {
        matches!(self, LayoutNode::Container(_))
    }

    /// Pane IDs of every leaf, in pre-order traversal following child order
    /// (left-to-right for horizontal containers, top-to-bottom for vertical).
    #[must_use]
    pub fn pane_ids(&self) -> Vec<PaneId> {
        let mut ids = Vec::new();
        self.collect_pane_ids(&mut ids);
        ids
    }

    fn collect_pane_ids(&self, out: &mut Vec<PaneId>) {
        match self {
            LayoutNode::Leaf(id) => out.push(*id),
            LayoutNode::Container(c) => {
                for child in &c.children {
                    child.node.collect_pane_ids(out);
                }
            }
        }
    }

    /// Restore the structural canonical form after a mutation.
    ///
    /// Bottom-up, this collapses single-child containers into their sole child
    /// (which inherits the container's weight in the parent) and merges a
    /// same-direction child container into its parent, scaling the merged
    /// grandchildren's weights by the child's weight. Idempotent, and total —
    /// it never panics.
    ///
    /// It restores only the structural invariants (no single-child containers,
    /// no same-direction nesting). Weight validity — positivity, finiteness,
    /// and the sum-to-1.0 bound — is preserved by the split/close/resize
    /// operations that redistribute weight, and is checked by
    /// [`validate`](Self::validate).
    #[must_use]
    pub fn flatten(self) -> LayoutNode {
        match self {
            LayoutNode::Leaf(_) => self,
            LayoutNode::Container(c) => flatten_container(c),
        }
    }

    /// Whether the tree satisfies every canonical-form invariant.
    #[must_use]
    pub fn is_canonical(&self) -> bool {
        self.validate().is_ok()
    }

    /// Check the canonical-form invariants, returning the first violation found.
    ///
    /// # Errors
    /// Returns an [`InvariantViolation`] describing the first container that
    /// breaks an invariant.
    pub fn validate(&self) -> Result<(), InvariantViolation> {
        let LayoutNode::Container(c) = self else {
            return Ok(());
        };

        if c.children.len() < 2 {
            return Err(InvariantViolation::TooFewChildren {
                found: c.children.len(),
            });
        }

        let mut sum = 0.0;
        for (index, child) in c.children.iter().enumerate() {
            let weight = child.weight;
            // NaN slips past `<= 0.0` (every NaN comparison is false), so reject
            // non-finite weights first.
            if !weight.is_finite() {
                return Err(InvariantViolation::NonFiniteWeight { index });
            }
            if weight <= 0.0 {
                return Err(InvariantViolation::NonPositiveWeight { index, weight });
            }
            sum += weight;
        }
        if (sum - 1.0).abs() >= WEIGHT_SUM_TOLERANCE {
            return Err(InvariantViolation::WeightsSumOutOfTolerance { sum });
        }

        for child in &c.children {
            if let LayoutNode::Container(inner) = &child.node {
                if inner.direction == c.direction {
                    return Err(InvariantViolation::SameDirectionNesting);
                }
            }
            child.node.validate()?;
        }
        Ok(())
    }
}

fn flatten_container(c: Container) -> LayoutNode {
    let direction = c.direction;

    let mut children: Vec<Child> = Vec::new();
    for child in c.children {
        // Flatten each child first so a same-direction merge sees a canonical
        // subtree and single-child collapses have already propagated upward.
        match child.node.flatten() {
            LayoutNode::Container(inner) if inner.direction == direction => {
                for grandchild in inner.children {
                    children.push(Child::new(
                        grandchild.node,
                        grandchild.weight * child.weight,
                    ));
                }
            }
            node => children.push(Child::new(node, child.weight)),
        }
    }

    if children.len() == 1 {
        return children.pop().expect("length checked to be 1").node;
    }

    LayoutNode::Container(Container {
        direction,
        children,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use SplitDirection::{Horizontal, Vertical};

    const A: PaneId = PaneId(1);
    const B: PaneId = PaneId(2);
    const C: PaneId = PaneId(3);
    const D: PaneId = PaneId(4);

    fn leaf(id: PaneId) -> LayoutNode {
        LayoutNode::Leaf(id)
    }

    fn container(
        direction: SplitDirection,
        nodes: Vec<LayoutNode>,
        weights: Vec<f32>,
    ) -> LayoutNode {
        assert_eq!(
            nodes.len(),
            weights.len(),
            "test setup: nodes and weights must be parallel"
        );
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

    /// Assert a node is a container whose weights match `expected` within tolerance.
    fn assert_weights(node: &LayoutNode, expected: &[f32]) {
        let LayoutNode::Container(c) = node else {
            panic!("expected a container, got {node:?}");
        };
        assert_eq!(c.children.len(), expected.len(), "weight count");
        for (child, want) in c.children.iter().zip(expected) {
            assert!(
                (child.weight - want).abs() < 1e-6,
                "weight {} != {want}",
                child.weight
            );
        }
    }

    #[test]
    fn leaf_flatten_is_identity() {
        assert_eq!(leaf(A).flatten(), leaf(A));
    }

    #[test]
    fn even_split_distributes_weight() {
        let c = Container::even(Horizontal, vec![leaf(A), leaf(B), leaf(C)]);
        let sum: f32 = c.children.iter().map(|ch| ch.weight).sum();
        assert!((sum - 1.0).abs() < WEIGHT_SUM_TOLERANCE);
        for child in &c.children {
            assert!((child.weight - 1.0 / 3.0).abs() < 1e-6);
        }
    }

    #[test]
    #[should_panic(expected = "at least two children")]
    fn even_split_rejects_single_child() {
        let _ = Container::even(Horizontal, vec![leaf(A)]);
    }

    #[test]
    fn canonical_tree_flatten_is_identity() {
        let tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        assert_eq!(tree.clone().flatten(), tree);
    }

    #[test]
    fn single_child_root_collapses_to_child() {
        let tree = container(Horizontal, vec![leaf(A)], vec![1.0]);
        assert_eq!(tree.flatten(), leaf(A));
    }

    #[test]
    fn single_child_container_collapses_and_child_inherits_weight() {
        // V[ A(0.3), H[B](0.7) ]  ->  V[ A(0.3), B(0.7) ]
        let inner = container(Horizontal, vec![leaf(B)], vec![1.0]);
        let tree = container(Vertical, vec![leaf(A), inner], vec![0.3, 0.7]);

        let flat = tree.flatten();
        assert_eq!(flat.pane_ids(), vec![A, B]);
        assert_weights(&flat, &[0.3, 0.7]);
        assert!(flat.is_canonical());
    }

    #[test]
    fn same_direction_child_merges_with_scaled_weights() {
        // Spec example: parent H[X(0.4), H[B,C](0.6)] -> H[X, B, C] = [0.4, 0.3, 0.3]
        let inner = container(Horizontal, vec![leaf(B), leaf(C)], vec![0.5, 0.5]);
        let tree = container(Horizontal, vec![leaf(A), inner], vec![0.4, 0.6]);

        let flat = tree.flatten();
        assert_eq!(flat.pane_ids(), vec![A, B, C]);
        assert_weights(&flat, &[0.4, 0.3, 0.3]);
        assert!(flat.is_canonical());
    }

    #[test]
    fn different_direction_nesting_is_preserved() {
        let inner = container(Vertical, vec![leaf(B), leaf(C)], vec![0.5, 0.5]);
        let tree = container(Horizontal, vec![leaf(A), inner], vec![0.5, 0.5]);

        let flat = tree.clone().flatten();
        assert_eq!(flat, tree, "opposite-direction nesting must not merge");
    }

    #[test]
    fn deeply_nested_same_direction_fully_flattens() {
        // H[A, H[B, H[C, D]]] with weights chaining 0.5 each level.
        let deepest = container(Horizontal, vec![leaf(C), leaf(D)], vec![0.5, 0.5]);
        let mid = container(Horizontal, vec![leaf(B), deepest], vec![0.5, 0.5]);
        let tree = container(Horizontal, vec![leaf(A), mid], vec![0.5, 0.5]);

        let flat = tree.flatten();
        assert_eq!(flat.pane_ids(), vec![A, B, C, D]);
        // A: 0.5, B: 0.25, C: 0.125, D: 0.125
        assert_weights(&flat, &[0.5, 0.25, 0.125, 0.125]);
        assert!(flat.is_canonical());
    }

    #[test]
    fn nested_single_child_same_direction_collapses_then_merges() {
        // H[A, H[H[B, C]]]: inner single-child H collapses to H[B,C], which then
        // merges into the root because both are horizontal.
        let bc = container(Horizontal, vec![leaf(B), leaf(C)], vec![0.5, 0.5]);
        let single = container(Horizontal, vec![bc], vec![1.0]);
        let tree = container(Horizontal, vec![leaf(A), single], vec![0.5, 0.5]);

        let flat = tree.flatten();
        assert_eq!(flat.pane_ids(), vec![A, B, C]);
        assert_weights(&flat, &[0.5, 0.25, 0.25]);
        assert!(flat.is_canonical());
    }

    #[test]
    fn same_direction_merge_in_first_position() {
        // H[ H[A,B](0.6), C(0.4) ] -> H[ A(0.3), B(0.3), C(0.4) ]
        let inner = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        let tree = container(Horizontal, vec![inner, leaf(C)], vec![0.6, 0.4]);

        let flat = tree.flatten();
        assert_eq!(flat.pane_ids(), vec![A, B, C]);
        assert_weights(&flat, &[0.3, 0.3, 0.4]);
        assert!(flat.is_canonical());
    }

    #[test]
    fn single_child_collapse_exposes_same_direction_merge() {
        // H[A, V[H[B,C]]]: the V collapses to its sole H child, which then
        // merges into the root H. Collapse and merge cross a direction change.
        let bc = container(Horizontal, vec![leaf(B), leaf(C)], vec![0.5, 0.5]);
        let single = container(Vertical, vec![bc], vec![1.0]);
        let tree = container(Horizontal, vec![leaf(A), single], vec![0.5, 0.5]);

        let flat = tree.flatten();
        assert_eq!(flat.pane_ids(), vec![A, B, C]);
        assert_weights(&flat, &[0.5, 0.25, 0.25]);
        assert!(flat.is_canonical());
    }

    #[test]
    fn multiple_sibling_same_direction_containers_merge_in_one_pass() {
        // H[ H[A,B](0.5), H[C,D](0.5) ] -> H[A,B,C,D] all 0.25.
        // This is the shape a close/redistribute (TREK-97) produces.
        let left = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        let right = container(Horizontal, vec![leaf(C), leaf(D)], vec![0.5, 0.5]);
        let tree = container(Horizontal, vec![left, right], vec![0.5, 0.5]);

        let flat = tree.flatten();
        assert_eq!(flat.pane_ids(), vec![A, B, C, D]);
        assert_weights(&flat, &[0.25, 0.25, 0.25, 0.25]);
        assert!(flat.is_canonical());
    }

    #[test]
    fn same_direction_merge_confined_to_surviving_nested_subtree() {
        // H[ A, V[ B, V[C,D] ] ] -> H[ A, V[B,C,D] ]; the outer H is untouched.
        // Proves flatten canonicalizes subtrees at depth, independent of the root.
        let inner_v = container(Vertical, vec![leaf(C), leaf(D)], vec![0.5, 0.5]);
        let outer_v = container(Vertical, vec![leaf(B), inner_v], vec![0.5, 0.5]);
        let tree = container(Horizontal, vec![leaf(A), outer_v], vec![0.5, 0.5]);

        let flat = tree.flatten();
        assert_eq!(flat.pane_ids(), vec![A, B, C, D]);
        assert!(flat.is_canonical());
        let LayoutNode::Container(root) = &flat else {
            panic!("expected a container");
        };
        assert_eq!(root.direction, Horizontal);
        assert_weights(&root.children[1].node, &[0.5, 0.25, 0.25]);
    }

    #[test]
    fn root_single_child_collapses_to_container_child() {
        // H[ V[A,B] ] -> V[A,B]
        let inner = container(Vertical, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        let tree = container(Horizontal, vec![inner.clone()], vec![1.0]);
        assert_eq!(tree.flatten(), inner);
    }

    #[test]
    fn flatten_is_idempotent() {
        let inner = container(Horizontal, vec![leaf(B), leaf(C)], vec![0.5, 0.5]);
        let tree = container(Horizontal, vec![leaf(A), inner], vec![0.4, 0.6]);

        let once = tree.flatten();
        let twice = once.clone().flatten();
        assert_eq!(once, twice);
    }

    #[test]
    fn flatten_is_idempotent_on_a_mixed_tree() {
        // Collapse + merge + opposite-direction preservation in one tree.
        let bc = container(Horizontal, vec![leaf(B), leaf(C)], vec![0.5, 0.5]);
        let single = container(Vertical, vec![bc], vec![1.0]);
        let right = container(Vertical, vec![leaf(C), leaf(D)], vec![0.5, 0.5]);
        let tree = container(
            Horizontal,
            vec![leaf(A), single, right],
            vec![0.4, 0.3, 0.3],
        );

        let once = tree.flatten();
        let twice = once.clone().flatten();
        assert_eq!(once, twice);
        assert!(once.is_canonical());
    }

    #[test]
    fn leaf_predicates_and_pane_ids() {
        let l = leaf(A);
        assert!(l.is_leaf());
        assert!(!l.is_container());
        assert_eq!(l.pane_ids(), vec![A]);

        let c = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        assert!(c.is_container());
        assert!(!c.is_leaf());
    }

    #[test]
    fn leaf_is_canonical() {
        assert!(leaf(A).is_canonical());
    }

    #[test]
    fn pane_ids_are_left_to_right() {
        let right = container(Vertical, vec![leaf(C), leaf(D)], vec![0.5, 0.5]);
        let tree = container(
            Horizontal,
            vec![leaf(A), leaf(B), right],
            vec![0.4, 0.3, 0.3],
        );
        assert_eq!(tree.pane_ids(), vec![A, B, C, D]);
    }

    #[test]
    fn validate_accepts_canonical_tree() {
        let tree = container(
            Horizontal,
            vec![leaf(A), leaf(B), leaf(C)],
            vec![0.4, 0.3, 0.3],
        );
        assert_eq!(tree.validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_too_few_children() {
        let tree = container(Horizontal, vec![leaf(A)], vec![1.0]);
        assert_eq!(
            tree.validate(),
            Err(InvariantViolation::TooFewChildren { found: 1 })
        );
    }

    #[test]
    fn validate_rejects_empty_container() {
        let tree = container(Horizontal, vec![], vec![]);
        assert_eq!(
            tree.validate(),
            Err(InvariantViolation::TooFewChildren { found: 0 })
        );
    }

    #[test]
    fn validate_recurses_into_nested_subtree() {
        // Root is canonical (H over a V child), but the V child's weights are
        // out of tolerance. validate must descend past the valid outer node.
        let bad_inner = container(Vertical, vec![leaf(B), leaf(C)], vec![0.5, 0.6]);
        let tree = container(Horizontal, vec![leaf(A), bad_inner], vec![0.5, 0.5]);
        let Err(InvariantViolation::WeightsSumOutOfTolerance { .. }) = tree.validate() else {
            panic!("expected the nested violation to surface");
        };
    }

    #[test]
    fn validate_tolerance_boundary() {
        // Just inside tolerance (|sum - 1.0| = 0.0005) is accepted.
        let ok = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5005]);
        assert_eq!(ok.validate(), Ok(()));

        // Just outside (|sum - 1.0| = 0.0011) is rejected.
        let bad = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5011]);
        assert!(matches!(
            bad.validate(),
            Err(InvariantViolation::WeightsSumOutOfTolerance { .. })
        ));
    }

    #[test]
    fn validate_distinguishes_negative_from_non_finite_weight() {
        let neg = container(Horizontal, vec![leaf(A), leaf(B)], vec![-0.5, 1.5]);
        assert_eq!(
            neg.validate(),
            Err(InvariantViolation::NonPositiveWeight {
                index: 0,
                weight: -0.5
            })
        );

        // NEG_INFINITY hits the finiteness check first, not the sign check.
        let neg_inf = container(
            Horizontal,
            vec![leaf(A), leaf(B)],
            vec![f32::NEG_INFINITY, 0.5],
        );
        assert_eq!(
            neg_inf.validate(),
            Err(InvariantViolation::NonFiniteWeight { index: 0 })
        );
    }

    #[test]
    fn validate_rejects_non_finite_weight() {
        let nan = container(Horizontal, vec![leaf(A), leaf(B)], vec![f32::NAN, 0.5]);
        assert_eq!(
            nan.validate(),
            Err(InvariantViolation::NonFiniteWeight { index: 0 })
        );

        let inf = container(Horizontal, vec![leaf(A), leaf(B)], vec![f32::INFINITY, 0.5]);
        assert_eq!(
            inf.validate(),
            Err(InvariantViolation::NonFiniteWeight { index: 0 })
        );
    }

    #[test]
    fn validate_rejects_non_positive_weight() {
        let tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![1.0, 0.0]);
        assert_eq!(
            tree.validate(),
            Err(InvariantViolation::NonPositiveWeight {
                index: 1,
                weight: 0.0
            })
        );
    }

    #[test]
    fn validate_rejects_weights_out_of_tolerance() {
        let tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.6]);
        let Err(InvariantViolation::WeightsSumOutOfTolerance { sum }) = tree.validate() else {
            panic!("expected sum-out-of-tolerance");
        };
        assert!((sum - 1.1).abs() < 1e-6);
    }

    #[test]
    fn validate_rejects_same_direction_nesting() {
        let inner = container(Horizontal, vec![leaf(B), leaf(C)], vec![0.5, 0.5]);
        let tree = container(Horizontal, vec![leaf(A), inner], vec![0.5, 0.5]);
        assert_eq!(
            tree.validate(),
            Err(InvariantViolation::SameDirectionNesting)
        );
    }
}
