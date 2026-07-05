//! Read-only geometry queries on the layout tree: where panes sit in the
//! unit square, which edges they touch, and the extents the daemon needs to
//! convert wire-protocol cell deltas and minimum pane sizes into weight
//! space (Spec-0007, TREK-98).

use crate::layout::{Container, LayoutNode, PaneId, SplitDirection};
use crate::ops::LayoutError;

/// Minimum cross-axis overlap for two panes to count as sharing a border.
/// Genuine corner touches compute to exactly (or within float noise of)
/// zero; genuine sliver borders are far larger. Erring toward rejection is
/// safe — a border this thin cannot be meaningfully dragged.
pub(crate) const BORDER_OVERLAP_EPSILON: f32 = 1e-6;

/// Extents around the border between two panes, normalized to the whole
/// tree's `[0, 1]` space along the border container's axis. The daemon
/// derives the container's extent in cells from a pane's grid dimensions:
/// `container_cells = pane_cells / pane_extent * container_extent`.
///
/// For any tree maintained through the public ops, both extents are
/// finite with `0 < pane_extent <= container_extent <= 1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderExtents {
    /// The border container's split direction. `Horizontal` siblings share
    /// a vertical border, so cell deltas along it are columns; `Vertical`
    /// siblings share a horizontal border, so cell deltas are rows.
    pub axis: SplitDirection,
    /// `pane`'s extent along `axis`.
    pub pane_extent: f32,
    /// The border container's extent along `axis`.
    pub container_extent: f32,
}

/// Predicted extents after a split, each as a fraction of the pane's own
/// pre-split extent along the split axis. The daemon multiplies by each
/// pane's current cols/rows for the Spec-0007 minimum-size pre-check.
#[derive(Debug, Clone, PartialEq)]
pub struct SplitPreview {
    /// The new pane's extent, relative to the target's pre-split extent.
    /// Can exceed 1.0 when the target is a small sibling in a large
    /// same-direction container.
    pub new_pane_fraction: f32,
    /// The target's post-split extent.
    pub target_fraction: f32,
    /// Every other pane the split shrinks — the leaves of the target's
    /// same-direction container, which scale by `target_fraction` too.
    /// Empty when the split creates a new two-child container.
    pub shrunk_siblings: Vec<PaneId>,
}

impl LayoutNode {
    /// Geometry of the border shared by `pane` and `neighbor`, for
    /// converting a cell-space resize delta into weight space.
    ///
    /// # Errors
    /// [`LayoutError::PaneNotFound`] if either pane is absent;
    /// [`LayoutError::NotAdjacentSiblings`] if they do not share a border
    /// (same acceptance as [`LayoutNode::resize`]).
    pub fn border_extents(
        &self,
        pane: PaneId,
        neighbor: PaneId,
    ) -> Result<BorderExtents, LayoutError> {
        if !self.contains(pane) {
            return Err(LayoutError::PaneNotFound(pane));
        }
        if !self.contains(neighbor) {
            return Err(LayoutError::PaneNotFound(neighbor));
        }
        let LayoutNode::Container(c) = self else {
            // Single-leaf tree: pane == neighbor == leaf, no border.
            return Err(LayoutError::NotAdjacentSiblings { pane, neighbor });
        };
        let (axis, container_extent) = border_container_extent(c, 1.0, 1.0, pane, neighbor)?;
        // Membership was pre-validated; keep the query total anyway.
        let Some(r) = pane_rect(self, pane) else {
            return Err(LayoutError::PaneNotFound(pane));
        };
        let pane_extent = match axis {
            SplitDirection::Horizontal => r.x1 - r.x0,
            SplitDirection::Vertical => r.y1 - r.y0,
        };
        Ok(BorderExtents {
            axis,
            pane_extent,
            container_extent,
        })
    }

    /// Predict the extents [`LayoutNode::split`] would produce, without
    /// mutating the tree. Mirrors the split rules: a same-direction parent
    /// inserts a `1/(N+1)` sibling, anything else halves the target's slot.
    ///
    /// # Errors
    /// [`LayoutError::PaneNotFound`] if `target` is absent.
    pub fn split_preview(
        &self,
        target: PaneId,
        direction: SplitDirection,
    ) -> Result<SplitPreview, LayoutError> {
        if !self.contains(target) {
            return Err(LayoutError::PaneNotFound(target));
        }
        Ok(split_preview_in(self, target, direction))
    }
}

/// Descend to the container where the two panes' subtrees diverge,
/// tracking the current subtree's extent on both axes. Validation mirrors
/// `resize_at_border`; membership is pre-validated.
fn border_container_extent(
    c: &Container,
    width: f32,
    height: f32,
    pane: PaneId,
    neighbor: PaneId,
) -> Result<(SplitDirection, f32), LayoutError> {
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
        let child = &c.children[pane_idx];
        let LayoutNode::Container(inner) = &child.node else {
            // Both ids in one leaf means pane == neighbor: no border.
            return Err(LayoutError::NotAdjacentSiblings { pane, neighbor });
        };
        let (w, h) = match c.direction {
            SplitDirection::Horizontal => (width * child.weight, height),
            SplitDirection::Vertical => (width, height * child.weight),
        };
        return border_container_extent(inner, w, h, pane, neighbor);
    }
    if pane_idx.abs_diff(neighbor_idx) != 1
        || !panes_share_border(c, pane_idx, pane, neighbor_idx, neighbor)
    {
        return Err(LayoutError::NotAdjacentSiblings { pane, neighbor });
    }
    let extent = match c.direction {
        SplitDirection::Horizontal => width,
        SplitDirection::Vertical => height,
    };
    Ok((c.direction, extent))
}

/// Mirrors `split_in`'s descent. Membership is pre-validated.
fn split_preview_in(node: &LayoutNode, target: PaneId, direction: SplitDirection) -> SplitPreview {
    match node {
        LayoutNode::Leaf(_) => SplitPreview {
            new_pane_fraction: 0.5,
            target_fraction: 0.5,
            shrunk_siblings: Vec::new(),
        },
        LayoutNode::Container(c) => {
            if c.direction == direction {
                let target_pos = c
                    .children
                    .iter()
                    .position(|ch| matches!(ch.node, LayoutNode::Leaf(id) if id == target));
                if let Some(i) = target_pos {
                    #[allow(clippy::cast_precision_loss)]
                    let n = c.children.len() as f32;
                    let shrunk_siblings = c
                        .children
                        .iter()
                        .enumerate()
                        .filter(|(j, _)| *j != i)
                        .flat_map(|(_, ch)| ch.node.pane_ids())
                        .collect();
                    return SplitPreview {
                        new_pane_fraction: 1.0 / ((n + 1.0) * c.children[i].weight),
                        target_fraction: n / (n + 1.0),
                        shrunk_siblings,
                    };
                }
            }
            let child = c
                .children
                .iter()
                .find(|ch| ch.node.contains(target))
                .expect("membership pre-validated");
            split_preview_in(&child.node, target, direction)
        }
    }
}

/// Whether the two panes actually abut the border between adjacent sibling
/// subtrees. Adjacency of the subtrees is necessary but not sufficient: in
/// `H[V[A,B], V[C,D]]` panes A and D sit in adjacent columns yet only meet
/// at a corner. Each pane must touch the shared edge of its subtree
/// (checked structurally — weight arithmetic cannot distinguish a touching
/// pane from one separated by a sliver), and their extents along the
/// border must overlap.
pub(crate) fn panes_share_border(
    c: &Container,
    pane_idx: usize,
    pane: PaneId,
    neighbor_idx: usize,
    neighbor: PaneId,
) -> bool {
    let (first_idx, first_id, second_id) = if pane_idx < neighbor_idx {
        (pane_idx, pane, neighbor)
    } else {
        (neighbor_idx, neighbor, pane)
    };
    let first_node = &c.children[first_idx].node;
    let second_node = &c.children[first_idx + 1].node;

    if !touches_edge(first_node, first_id, c.direction, Edge::Trailing)
        || !touches_edge(second_node, second_id, c.direction, Edge::Leading)
    {
        return false;
    }

    let first = pane_rect(first_node, first_id).expect("membership pre-validated");
    let second = pane_rect(second_node, second_id).expect("membership pre-validated");
    let cross_overlap = match c.direction {
        SplitDirection::Horizontal => first.y1.min(second.y1) - first.y0.max(second.y0),
        SplitDirection::Vertical => first.x1.min(second.x1) - first.x0.max(second.x0),
    };
    cross_overlap > BORDER_OVERLAP_EPSILON
}

/// Which edge of a subtree a pane must touch, along a given axis.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Edge {
    Leading,
    Trailing,
}

/// Whether `target`'s leaf touches the given edge of `node` along `axis`:
/// in every container along the path whose direction is `axis`, the pane
/// must sit in the first (leading) or last (trailing) child. Structural,
/// so a sliver pane between `target` and the edge is never mistaken for
/// touching, no matter how thin.
pub(crate) fn touches_edge(
    node: &LayoutNode,
    target: PaneId,
    axis: SplitDirection,
    edge: Edge,
) -> bool {
    match node {
        LayoutNode::Leaf(id) => *id == target,
        LayoutNode::Container(c) => {
            let Some(pos) = c.children.iter().position(|ch| ch.node.contains(target)) else {
                return false;
            };
            if c.direction == axis {
                let required = match edge {
                    Edge::Leading => 0,
                    Edge::Trailing => c.children.len() - 1,
                };
                if pos != required {
                    return false;
                }
            }
            touches_edge(&c.children[pos].node, target, axis, edge)
        }
    }
}

/// A pane's bounds within its subtree, both axes normalized to `[0, 1]`.
#[derive(Clone, Copy)]
pub(crate) struct UnitRect {
    pub(crate) x0: f32,
    pub(crate) x1: f32,
    pub(crate) y0: f32,
    pub(crate) y1: f32,
}

pub(crate) fn pane_rect(node: &LayoutNode, target: PaneId) -> Option<UnitRect> {
    match node {
        LayoutNode::Leaf(id) if *id == target => Some(UnitRect {
            x0: 0.0,
            x1: 1.0,
            y0: 0.0,
            y1: 1.0,
        }),
        LayoutNode::Leaf(_) => None,
        LayoutNode::Container(c) => {
            let mut start = 0.0;
            for child in &c.children {
                let end = start + child.weight;
                if let Some(r) = pane_rect(&child.node, target) {
                    let span = end - start;
                    return Some(match c.direction {
                        SplitDirection::Horizontal => UnitRect {
                            x0: start + r.x0 * span,
                            x1: start + r.x1 * span,
                            ..r
                        },
                        SplitDirection::Vertical => UnitRect {
                            y0: start + r.y0 * span,
                            y1: start + r.y1 * span,
                            ..r
                        },
                    });
                }
                start = end;
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Child;

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
        assert_eq!(nodes.len(), weights.len(), "test setup: parallel arrays");
        LayoutNode::Container(Container {
            direction,
            children: nodes
                .into_iter()
                .zip(weights)
                .map(|(n, w)| Child::new(n, w))
                .collect(),
        })
    }

    #[test]
    fn border_extents_top_level_siblings() {
        let tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.25, 0.75]);
        let ext = tree.border_extents(A, B).unwrap();
        assert_eq!(ext.axis, Horizontal);
        assert!((ext.pane_extent - 0.25).abs() < 1e-6);
        assert!((ext.container_extent - 1.0).abs() < 1e-6);
    }

    #[test]
    fn border_extents_nested_container() {
        // H[A: 0.5, V[B, C]: 0.5] — B/C border is inside the V container,
        // which spans the full height but half the width.
        let tree = container(
            Horizontal,
            vec![
                leaf(A),
                container(Vertical, vec![leaf(B), leaf(C)], vec![0.5, 0.5]),
            ],
            vec![0.5, 0.5],
        );
        let ext = tree.border_extents(B, C).unwrap();
        assert_eq!(ext.axis, Vertical);
        assert!((ext.pane_extent - 0.5).abs() < 1e-6);
        assert!((ext.container_extent - 1.0).abs() < 1e-6);

        // A/B share the top-level vertical border; A spans the full width
        // of nothing nested, extents are along the horizontal axis.
        let ext = tree.border_extents(A, B).unwrap();
        assert_eq!(ext.axis, Horizontal);
        assert!((ext.pane_extent - 0.5).abs() < 1e-6);
        assert!((ext.container_extent - 1.0).abs() < 1e-6);
    }

    #[test]
    fn border_extents_deep_container_scales_extent() {
        // V[A: 0.4, H[B, C]: 0.6] — the B/C border container spans 60% of
        // the height and the full width; extent axis is horizontal.
        let tree = container(
            Vertical,
            vec![
                leaf(A),
                container(Horizontal, vec![leaf(B), leaf(C)], vec![0.3, 0.7]),
            ],
            vec![0.4, 0.6],
        );
        let ext = tree.border_extents(B, C).unwrap();
        assert_eq!(ext.axis, Horizontal);
        assert!((ext.pane_extent - 0.3).abs() < 1e-6);
        assert!((ext.container_extent - 1.0).abs() < 1e-6);
    }

    #[test]
    fn border_extents_diagonal_pair_rejected() {
        // H[V[A,B], V[C,D]]: A and D only meet at a corner.
        let tree = container(
            Horizontal,
            vec![
                container(Vertical, vec![leaf(A), leaf(B)], vec![0.5, 0.5]),
                container(Vertical, vec![leaf(C), leaf(D)], vec![0.5, 0.5]),
            ],
            vec![0.5, 0.5],
        );
        assert_eq!(
            tree.border_extents(A, D),
            Err(LayoutError::NotAdjacentSiblings {
                pane: A,
                neighbor: D
            })
        );
        // B (bottom-left) and C (top-right) are the other corner pair.
        assert_eq!(
            tree.border_extents(B, C),
            Err(LayoutError::NotAdjacentSiblings {
                pane: B,
                neighbor: C
            })
        );
        // A (top-left) and C (top-right) share the vertical border.
        let ext = tree.border_extents(A, C).unwrap();
        assert_eq!(ext.axis, Horizontal);
        assert!((ext.pane_extent - 0.5).abs() < 1e-6);
        assert!((ext.container_extent - 1.0).abs() < 1e-6);
    }

    #[test]
    fn border_extents_scales_nested_container_extent() {
        // V[A: 0.4, H[B: 0.5, V[C, D]: 0.5]: 0.6] — the C/D border
        // container spans 60% of the tree's height; C spans half of it.
        let tree = container(
            Vertical,
            vec![
                leaf(A),
                container(
                    Horizontal,
                    vec![
                        leaf(B),
                        container(Vertical, vec![leaf(C), leaf(D)], vec![0.5, 0.5]),
                    ],
                    vec![0.5, 0.5],
                ),
            ],
            vec![0.4, 0.6],
        );
        let ext = tree.border_extents(C, D).unwrap();
        assert_eq!(ext.axis, Vertical);
        assert!((ext.container_extent - 0.6).abs() < 1e-6);
        assert!((ext.pane_extent - 0.3).abs() < 1e-6);
    }

    #[test]
    fn border_extents_unknown_pane() {
        let tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        assert_eq!(tree.border_extents(A, D), Err(LayoutError::PaneNotFound(D)));
        assert_eq!(tree.border_extents(D, A), Err(LayoutError::PaneNotFound(D)));
    }

    #[test]
    fn border_extents_single_leaf_tree() {
        let tree = leaf(A);
        assert_eq!(
            tree.border_extents(A, A),
            Err(LayoutError::NotAdjacentSiblings {
                pane: A,
                neighbor: A
            })
        );
    }

    #[test]
    fn split_preview_bare_leaf_halves() {
        let tree = leaf(A);
        let p = tree.split_preview(A, Horizontal).unwrap();
        assert!((p.new_pane_fraction - 0.5).abs() < 1e-6);
        assert!((p.target_fraction - 0.5).abs() < 1e-6);
    }

    #[test]
    fn split_preview_cross_direction_halves() {
        let tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        let p = tree.split_preview(A, Vertical).unwrap();
        assert!((p.new_pane_fraction - 0.5).abs() < 1e-6);
        assert!((p.target_fraction - 0.5).abs() < 1e-6);
        assert!(
            p.shrunk_siblings.is_empty(),
            "a new two-child container shrinks no siblings"
        );
    }

    #[test]
    fn split_preview_same_direction_sibling_insert() {
        // Even 2-child container: newcomer gets 1/3 of the container, which
        // is 2/3 of the target's current half; target keeps 2/3 of its slot.
        let tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.5, 0.5]);
        let p = tree.split_preview(A, Horizontal).unwrap();
        assert!((p.new_pane_fraction - 2.0 / 3.0).abs() < 1e-6);
        assert!((p.target_fraction - 2.0 / 3.0).abs() < 1e-6);
        assert_eq!(p.shrunk_siblings, vec![B]);
    }

    #[test]
    fn split_preview_reports_leaves_of_container_siblings() {
        // H[A, V[B, C]]: inserting beside A shrinks the whole V subtree,
        // so both of its leaves are reported.
        let tree = container(
            Horizontal,
            vec![
                leaf(A),
                container(Vertical, vec![leaf(B), leaf(C)], vec![0.5, 0.5]),
            ],
            vec![0.5, 0.5],
        );
        let p = tree.split_preview(A, Horizontal).unwrap();
        assert_eq!(p.shrunk_siblings, vec![B, C]);
    }

    #[test]
    fn split_preview_small_sibling_newcomer_larger_than_target() {
        // Target holds 10% of the container; the newcomer's 1/3 share is
        // larger than the target's whole slot.
        let tree = container(Horizontal, vec![leaf(A), leaf(B)], vec![0.1, 0.9]);
        let p = tree.split_preview(A, Horizontal).unwrap();
        assert!((p.new_pane_fraction - 1.0 / (3.0 * 0.1)).abs() < 1e-5);
        assert!((p.target_fraction - 2.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn split_preview_matches_actual_split_extents() {
        // Preview must agree with the geometry split() produces.
        let mut tree = container(
            Horizontal,
            vec![leaf(A), leaf(B), leaf(C)],
            vec![0.2, 0.3, 0.5],
        );
        let before = pane_rect(&tree, B).unwrap();
        let p = tree.split_preview(B, Horizontal).unwrap();
        tree.split(B, D, Horizontal).unwrap();
        let target_after = pane_rect(&tree, B).unwrap();
        let new_after = pane_rect(&tree, D).unwrap();
        let before_w = before.x1 - before.x0;
        assert!(((target_after.x1 - target_after.x0) / before_w - p.target_fraction).abs() < 1e-5);
        assert!(((new_after.x1 - new_after.x0) / before_w - p.new_pane_fraction).abs() < 1e-5);
    }

    #[test]
    fn split_preview_unknown_pane() {
        let tree = leaf(A);
        assert_eq!(
            tree.split_preview(B, Horizontal),
            Err(LayoutError::PaneNotFound(B))
        );
    }
}
