//! Client-side layout geometry: converts the daemon's layout tree
//! (Spec-0007 weights) into pixel rectangles for rendering and border
//! drawing. Pure functions — no GPU or protocol state.

use oakterm_protocol::message::{LayoutDirection, LayoutTreeNode};

/// A pixel-space rectangle. Origin is the window's top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// A pane's computed screen area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneRect {
    pub pane_id: u32,
    pub rect: PixelRect,
}

/// Computed geometry for one layout tree: pane rectangles in traversal
/// order plus the 1px border segments between siblings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LayoutGeometry {
    pub panes: Vec<PaneRect>,
    pub borders: Vec<PixelRect>,
}

/// Walk the layout tree and compute pixel bounds per pane (Spec-0007
/// Pane Dimension Calculation): each container distributes its extent
/// among children by weight, minus 1px per internal border.
///
/// Child pixel sizes use cumulative rounding — child `i` spans
/// `round(cum_weight_i * available) - round(cum_weight_(i-1) * available)`
/// — so the children always sum exactly to the available extent and no
/// pixel drifts regardless of weight precision.
#[must_use]
pub fn compute_layout(tree: &LayoutTreeNode, content: PixelRect) -> LayoutGeometry {
    let mut geometry = LayoutGeometry::default();
    walk(tree, content, &mut geometry);
    geometry
}

fn walk(node: &LayoutTreeNode, rect: PixelRect, out: &mut LayoutGeometry) {
    match node {
        LayoutTreeNode::Leaf { pane_id } => out.panes.push(PaneRect {
            pane_id: *pane_id,
            rect,
        }),
        LayoutTreeNode::Container {
            direction,
            children,
            weights,
        } => {
            let border_count = u32::try_from(children.len().saturating_sub(1)).unwrap_or(0);
            let extent = match direction {
                LayoutDirection::Horizontal => rect.width,
                LayoutDirection::Vertical => rect.height,
            };
            let available = extent.saturating_sub(border_count);
            let total: f32 = weights.iter().sum();
            if total <= 0.0 || !total.is_finite() {
                // Wire validation rejects these weights; reaching this
                // means a producer bypassed it. Dropping the subtree
                // silently would make panes vanish untraceably.
                tracing::warn!(total, "degenerate layout weights; subtree not laid out");
                return;
            }

            let mut cursor = match direction {
                LayoutDirection::Horizontal => rect.x,
                LayoutDirection::Vertical => rect.y,
            };
            let mut cum_weight = 0.0f32;
            let mut prev_edge = 0u32;
            for (i, (child, weight)) in children.iter().zip(weights).enumerate() {
                cum_weight += weight / total;
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                // cum_weight is clamped to [0,1]; the product fits u32.
                let edge =
                    (f64::from(cum_weight.clamp(0.0, 1.0)) * f64::from(available)).round() as u32;
                // Monotonic for validated (positive) weights; saturate so a
                // bypassing producer degrades to a zero-width pane instead
                // of an underflow.
                let span = edge.saturating_sub(prev_edge);
                prev_edge = edge.max(prev_edge);

                let child_rect = match direction {
                    LayoutDirection::Horizontal => PixelRect {
                        x: cursor,
                        y: rect.y,
                        width: span,
                        height: rect.height,
                    },
                    LayoutDirection::Vertical => PixelRect {
                        x: rect.x,
                        y: cursor,
                        width: rect.width,
                        height: span,
                    },
                };
                walk(child, child_rect, out);
                cursor += span;

                if i + 1 < children.len() {
                    out.borders.push(match direction {
                        LayoutDirection::Horizontal => PixelRect {
                            x: cursor,
                            y: rect.y,
                            width: 1,
                            height: rect.height,
                        },
                        LayoutDirection::Vertical => PixelRect {
                            x: rect.x,
                            y: cursor,
                            width: rect.width,
                            height: 1,
                        },
                    });
                    cursor += 1;
                }
            }
        }
    }
}

/// Indices into `geometry.borders` of the segments adjacent to
/// `focused_pane`'s rect — the ones drawn in the focus highlight color.
/// A border is adjacent when it touches the pane's edge and overlaps its
/// extent along the border's own axis.
#[must_use]
pub fn focused_border_indices(geometry: &LayoutGeometry, focused_pane: u32) -> Vec<usize> {
    let Some(pane) = geometry.panes.iter().find(|p| p.pane_id == focused_pane) else {
        return Vec::new();
    };
    let r = pane.rect;
    geometry
        .borders
        .iter()
        .enumerate()
        .filter(|(_, b)| {
            if b.width == 1 {
                // Vertical border: touches the pane's left or right edge.
                let touches = b.x + 1 == r.x || b.x == r.x + r.width;
                let overlaps = b.y < r.y + r.height && r.y < b.y + b.height;
                touches && overlaps
            } else {
                // Horizontal border: touches the pane's top or bottom edge.
                let touches = b.y + 1 == r.y || b.y == r.y + r.height;
                let overlaps = b.x < r.x + r.width && r.x < b.x + b.width;
                touches && overlaps
            }
        })
        .map(|(i, _)| i)
        .collect()
}

/// Convert a pane's pixel rect to grid dimensions (Spec-0007: floor,
/// minimum 1x1).
#[must_use]
pub fn grid_dims(rect: PixelRect, cell_width: f32, cell_height: f32) -> (u16, u16) {
    let dim = |pixels: u32, cell: f32| -> u16 {
        if cell <= 0.0 || !cell.is_finite() {
            return 1;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        // floor of a positive ratio, clamped to the u16 grid range.
        let cells = (f64::from(pixels) / f64::from(cell)).floor() as u32;
        u16::try_from(cells.max(1)).unwrap_or(u16::MAX)
    };
    (dim(rect.width, cell_width), dim(rect.height, cell_height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(pane_id: u32) -> LayoutTreeNode {
        LayoutTreeNode::Leaf { pane_id }
    }

    fn container(
        direction: LayoutDirection,
        children: Vec<LayoutTreeNode>,
        weights: Vec<f32>,
    ) -> LayoutTreeNode {
        LayoutTreeNode::Container {
            direction,
            children,
            weights,
        }
    }

    const CONTENT: PixelRect = PixelRect {
        x: 0,
        y: 0,
        width: 800,
        height: 600,
    };

    #[test]
    fn single_leaf_fills_content() {
        let g = compute_layout(&leaf(0), CONTENT);
        assert_eq!(
            g.panes,
            vec![PaneRect {
                pane_id: 0,
                rect: CONTENT
            }]
        );
        assert!(g.borders.is_empty());
    }

    #[test]
    fn even_horizontal_split_shares_width_minus_border() {
        let tree = container(
            LayoutDirection::Horizontal,
            vec![leaf(0), leaf(1)],
            vec![0.5, 0.5],
        );
        let g = compute_layout(&tree, CONTENT);
        // 800px - 1 border = 799 available; cumulative rounding puts the
        // extra pixel on the first child (round(0.5 * 799) = 400).
        assert_eq!(
            g.panes,
            vec![
                PaneRect {
                    pane_id: 0,
                    rect: PixelRect {
                        x: 0,
                        y: 0,
                        width: 400,
                        height: 600
                    }
                },
                PaneRect {
                    pane_id: 1,
                    rect: PixelRect {
                        x: 401,
                        y: 0,
                        width: 399,
                        height: 600
                    }
                },
            ]
        );
        assert_eq!(
            g.borders,
            vec![PixelRect {
                x: 400,
                y: 0,
                width: 1,
                height: 600
            }]
        );
    }

    #[test]
    fn spans_sum_to_available_extent() {
        let tree = container(
            LayoutDirection::Horizontal,
            vec![leaf(0), leaf(1), leaf(2)],
            vec![0.333, 0.333, 0.334],
        );
        let g = compute_layout(&tree, CONTENT);
        let total_width: u32 = g.panes.iter().map(|p| p.rect.width).sum();
        assert_eq!(total_width, 800 - 2, "3 children, 2 borders");
        // Panes and borders tile the extent with no gaps or overlaps.
        assert_eq!(g.panes[0].rect.x, 0);
        assert_eq!(g.borders[0].x, g.panes[0].rect.x + g.panes[0].rect.width);
        assert_eq!(g.panes[1].rect.x, g.borders[0].x + 1);
        assert_eq!(g.borders[1].x, g.panes[1].rect.x + g.panes[1].rect.width);
        assert_eq!(g.panes[2].rect.x, g.borders[1].x + 1);
        assert_eq!(g.panes[2].rect.x + g.panes[2].rect.width, 800);
    }

    #[test]
    fn vertical_split_stacks_top_to_bottom() {
        let tree = container(
            LayoutDirection::Vertical,
            vec![leaf(0), leaf(1)],
            vec![0.25, 0.75],
        );
        let g = compute_layout(&tree, CONTENT);
        // 599 available: round(0.25 * 599) = 150.
        assert_eq!(g.panes[0].rect.height, 150);
        assert_eq!(g.panes[1].rect.height, 449);
        assert_eq!(g.panes[1].rect.y, 151);
        assert_eq!(
            g.borders,
            vec![PixelRect {
                x: 0,
                y: 150,
                width: 800,
                height: 1
            }]
        );
    }

    #[test]
    fn nested_split_offsets_inner_children() {
        // [pane 0 | [pane 1 / pane 2]]
        let tree = container(
            LayoutDirection::Horizontal,
            vec![
                leaf(0),
                container(
                    LayoutDirection::Vertical,
                    vec![leaf(1), leaf(2)],
                    vec![0.5, 0.5],
                ),
            ],
            vec![0.5, 0.5],
        );
        let g = compute_layout(&tree, CONTENT);
        assert_eq!(g.panes.len(), 3);
        let right_x = g.panes[1].rect.x;
        assert_eq!(
            g.panes[1].rect,
            PixelRect {
                x: right_x,
                y: 0,
                width: 399,
                height: 300
            }
        );
        assert_eq!(
            g.panes[2].rect,
            PixelRect {
                x: right_x,
                y: 301,
                width: 399,
                height: 299
            }
        );
        // One vertical border between columns, one horizontal inside the
        // right column spanning only that column's width.
        assert_eq!(g.borders.len(), 2);
        assert_eq!(
            g.borders[1],
            PixelRect {
                x: right_x,
                y: 300,
                width: 399,
                height: 1
            }
        );
    }

    #[test]
    fn content_origin_offsets_all_rects() {
        let content = PixelRect {
            x: 12,
            y: 8,
            width: 100,
            height: 50,
        };
        let tree = container(
            LayoutDirection::Horizontal,
            vec![leaf(0), leaf(1)],
            vec![0.5, 0.5],
        );
        let g = compute_layout(&tree, content);
        assert_eq!(g.panes[0].rect.x, 12);
        assert_eq!(g.panes[0].rect.y, 8);
        assert_eq!(g.panes[1].rect.x + g.panes[1].rect.width, 112);
    }

    #[test]
    fn unnormalized_weights_are_scaled() {
        // Daemon guarantees sum 1.0 within tolerance, but the client
        // normalizes defensively.
        let tree = container(
            LayoutDirection::Horizontal,
            vec![leaf(0), leaf(1)],
            vec![1.0, 1.0],
        );
        let g = compute_layout(&tree, CONTENT);
        assert_eq!(g.panes[0].rect.width + g.panes[1].rect.width, 799);
        assert!(g.panes[0].rect.width.abs_diff(g.panes[1].rect.width) <= 1);
    }

    #[test]
    fn tiny_extent_saturates_without_panic() {
        let tree = container(
            LayoutDirection::Horizontal,
            vec![leaf(0), leaf(1), leaf(2)],
            vec![0.33, 0.33, 0.34],
        );
        let g = compute_layout(
            &tree,
            PixelRect {
                x: 0,
                y: 0,
                width: 2,
                height: 10,
            },
        );
        // 2px minus 2 borders leaves 0 available; all spans are 0.
        assert_eq!(g.panes.len(), 3);
        let total: u32 = g.panes.iter().map(|p| p.rect.width).sum();
        assert_eq!(total, 0);
    }

    #[test]
    fn focused_border_indices_two_way_split() {
        let tree = container(
            LayoutDirection::Horizontal,
            vec![leaf(0), leaf(1)],
            vec![0.5, 0.5],
        );
        let g = compute_layout(&tree, CONTENT);
        assert_eq!(focused_border_indices(&g, 0), vec![0]);
        assert_eq!(focused_border_indices(&g, 1), vec![0]);
        assert!(focused_border_indices(&g, 99).is_empty());
    }

    #[test]
    fn focused_border_indices_nested_selects_adjacent_only() {
        // [pane 0 | [pane 1 / pane 2]]: pane 0 touches only the column
        // border; pane 1 touches the column border and the row border.
        let tree = container(
            LayoutDirection::Horizontal,
            vec![
                leaf(0),
                container(
                    LayoutDirection::Vertical,
                    vec![leaf(1), leaf(2)],
                    vec![0.5, 0.5],
                ),
            ],
            vec![0.5, 0.5],
        );
        let g = compute_layout(&tree, CONTENT);
        assert_eq!(focused_border_indices(&g, 0), vec![0]);
        assert_eq!(focused_border_indices(&g, 1), vec![0, 1]);
        assert_eq!(focused_border_indices(&g, 2), vec![0, 1]);
    }

    #[test]
    fn hostile_negative_weights_do_not_panic() {
        // Wire validation rejects these; compute_layout must still degrade
        // (zero-width spans) rather than underflow if a producer bypasses it.
        let tree = container(
            LayoutDirection::Horizontal,
            vec![leaf(0), leaf(1), leaf(2)],
            vec![1.0, -0.5, 0.5],
        );
        let g = compute_layout(&tree, CONTENT);
        assert_eq!(g.panes.len(), 3);
        let total: u32 = g.panes.iter().map(|p| p.rect.width).sum();
        assert!(total <= CONTENT.width);
    }

    #[test]
    fn many_children_tile_exactly() {
        let n: u32 = 10;
        #[allow(clippy::cast_precision_loss)] // tiny test values
        let weights: Vec<f32> = (0..n).map(|i| 1.0 / (i as f32 + 1.0)).collect();
        let children: Vec<LayoutTreeNode> = (0..n).map(leaf).collect();
        let tree = container(LayoutDirection::Horizontal, children, weights);
        let g = compute_layout(&tree, CONTENT);
        assert_eq!(g.panes.len(), n as usize);
        assert_eq!(g.borders.len(), n as usize - 1);
        // Panes and borders tile the extent exactly: contiguous, no
        // overlap, ending at the right edge.
        let mut cursor = CONTENT.x;
        for (i, p) in g.panes.iter().enumerate() {
            assert_eq!(p.rect.x, cursor, "pane {i} starts at the cursor");
            cursor += p.rect.width;
            if i < g.borders.len() {
                assert_eq!(g.borders[i].x, cursor, "border {i} follows pane {i}");
                cursor += 1;
            }
        }
        assert_eq!(cursor, CONTENT.x + CONTENT.width);
    }

    #[test]
    fn grid_dims_floors_and_clamps_to_minimum() {
        let rect = PixelRect {
            x: 0,
            y: 0,
            width: 100,
            height: 33,
        };
        assert_eq!(grid_dims(rect, 8.0, 16.0), (12, 2));
        let tiny = PixelRect {
            x: 0,
            y: 0,
            width: 3,
            height: 5,
        };
        assert_eq!(grid_dims(tiny, 8.0, 16.0), (1, 1));
        assert_eq!(grid_dims(rect, 0.0, -1.0), (1, 1));
    }
}
