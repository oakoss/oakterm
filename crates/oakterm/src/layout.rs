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

impl PixelRect {
    /// Whether a border rect separates panes side-by-side (a vertical
    /// 1px line) rather than stacked. Split borders are exactly 1px on
    /// their thin axis; this is the one place that encodes it.
    #[must_use]
    pub fn is_vertical_border(self) -> bool {
        self.width == 1
    }
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
            if b.is_vertical_border() {
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

/// Index of the border under the pixel position, if any. The 1px border
/// rect is expanded by `pad` on its thin axis so it is grabbable; the
/// long axis is exact. Overlapping grab zones resolve to the first
/// border in traversal order.
#[must_use]
pub fn border_at(geometry: &LayoutGeometry, x: f64, y: f64, pad: f64) -> Option<usize> {
    geometry.borders.iter().position(|b| {
        let (x0, y0) = (f64::from(b.x), f64::from(b.y));
        let (x1, y1) = (x0 + f64::from(b.width), y0 + f64::from(b.height));
        if b.is_vertical_border() {
            x >= x0 - pad && x < x1 + pad && y >= y0 && y < y1
        } else {
            y >= y0 - pad && y < y1 + pad && x >= x0 && x < x1
        }
    })
}

/// The two panes flanking a border, in layout order. A positive wire
/// `ResizePane` delta grows `before` (Spec-0001 0xA2), so the field
/// names carry the sign convention the drag code relies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlankedPanes {
    pub before: u32,
    pub after: u32,
}

/// The two panes flanking a border at the given cross-axis position
/// (`y` in window pixels for a vertical border, `x` for a horizontal
/// one). A border can span several panes on either side
/// (`H[A, V[B, C]]`); the cross position picks the pair the cursor is
/// actually between — Spec-0007 Resize sends exactly that pair.
/// `None` when either side has no pane there.
#[must_use]
pub fn border_panes(
    geometry: &LayoutGeometry,
    border_index: usize,
    cross: f64,
) -> Option<FlankedPanes> {
    let b = *geometry.borders.get(border_index)?;
    let vertical = b.is_vertical_border();
    let covers_cross = |r: PixelRect| {
        let (lo, len) = if vertical {
            (f64::from(r.y), f64::from(r.height))
        } else {
            (f64::from(r.x), f64::from(r.width))
        };
        cross >= lo && cross < lo + len
    };
    let touches_before = |r: PixelRect| {
        if vertical {
            r.x + r.width == b.x
        } else {
            r.y + r.height == b.y
        }
    };
    let touches_after = |r: PixelRect| {
        if vertical {
            r.x == b.x + 1
        } else {
            r.y == b.y + 1
        }
    };
    let find = |touches: &dyn Fn(PixelRect) -> bool| {
        geometry
            .panes
            .iter()
            .find(|p| touches(p.rect) && covers_cross(p.rect))
            .map(|p| p.pane_id)
    };
    Some(FlankedPanes {
        before: find(&touches_before)?,
        after: find(&touches_after)?,
    })
}

/// Direction for `focus_target` (Spec-0007 Focus Navigation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusDirection {
    Left,
    Right,
    Up,
    Down,
}

/// The pane to focus when moving from `focused_pane` in `direction`
/// (Spec-0007 Focus Navigation): project a ray from the focused pane's
/// center in the requested direction. Among panes beyond the focused
/// edge whose extent overlaps the pane's band, the nearest by edge
/// distance wins; ties break toward the pane the ray passes through
/// (smallest ray distance), then toward the smallest cross-axis
/// position for determinism. Purely geometric, not tree-structural.
/// `None` at the screen edge (no wrap) or when `focused_pane` is not
/// in the geometry.
#[must_use]
pub fn focus_target(
    geometry: &LayoutGeometry,
    focused_pane: u32,
    direction: FocusDirection,
) -> Option<u32> {
    let from = geometry
        .panes
        .iter()
        .find(|p| p.pane_id == focused_pane)?
        .rect;
    let row_overlap = |r: PixelRect| r.y < from.y + from.height && from.y < r.y + r.height;
    let col_overlap = |r: PixelRect| r.x < from.x + from.width && from.x < r.x + r.width;
    let ray_y = from.y + from.height / 2;
    let ray_x = from.x + from.width / 2;
    // Distance from the ray to the candidate's extent; 0 if the ray passes through it.
    let ray_dist =
        |lo: u32, len: u32, ray: u32| ray.clamp(lo, lo + len.saturating_sub(1)).abs_diff(ray);
    geometry
        .panes
        .iter()
        .filter(|p| p.pane_id != focused_pane)
        .filter_map(|p| {
            let r = p.rect;
            let (beyond, distance, ray, cross) = match direction {
                FocusDirection::Left => (
                    row_overlap(r) && r.x + r.width <= from.x,
                    from.x.saturating_sub(r.x + r.width),
                    ray_dist(r.y, r.height, ray_y),
                    r.y,
                ),
                FocusDirection::Right => (
                    row_overlap(r) && r.x >= from.x + from.width,
                    r.x.saturating_sub(from.x + from.width),
                    ray_dist(r.y, r.height, ray_y),
                    r.y,
                ),
                FocusDirection::Up => (
                    col_overlap(r) && r.y + r.height <= from.y,
                    from.y.saturating_sub(r.y + r.height),
                    ray_dist(r.x, r.width, ray_x),
                    r.x,
                ),
                FocusDirection::Down => (
                    col_overlap(r) && r.y >= from.y + from.height,
                    r.y.saturating_sub(from.y + from.height),
                    ray_dist(r.x, r.width, ray_x),
                    r.x,
                ),
            };
            beyond.then_some((distance, ray, cross, p.pane_id))
        })
        .min_by_key(|&(distance, ray, cross, _)| (distance, ray, cross))
        .map(|(_, _, _, pane_id)| pane_id)
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

    use super::FocusDirection::{Down, Left, Right, Up};

    #[test]
    fn focus_target_two_pane_horizontal() {
        let g = compute_layout(
            &container(
                LayoutDirection::Horizontal,
                vec![leaf(1), leaf(2)],
                vec![0.5, 0.5],
            ),
            CONTENT,
        );
        assert_eq!(focus_target(&g, 1, Right), Some(2));
        assert_eq!(focus_target(&g, 2, Left), Some(1));
        assert_eq!(focus_target(&g, 1, Left), None, "screen edge: no wrap");
        assert_eq!(focus_target(&g, 1, Up), None);
        assert_eq!(focus_target(&g, 1, Down), None);
    }

    #[test]
    fn focus_target_unknown_focused_pane_is_none() {
        let g = compute_layout(
            &container(
                LayoutDirection::Horizontal,
                vec![leaf(1), leaf(2)],
                vec![0.5, 0.5],
            ),
            CONTENT,
        );
        assert_eq!(focus_target(&g, 99, Right), None);
    }

    #[test]
    fn focus_target_full_ties_break_toward_smallest_cross_position() {
        // H[1, V[2, 3]] at even weights: panes 2 (top) and 3 (bottom)
        // are equidistant right of 1, and 1's center ray lands on their
        // shared border so ray distance ties too; the topmost wins.
        let g = compute_layout(
            &container(
                LayoutDirection::Horizontal,
                vec![
                    leaf(1),
                    container(
                        LayoutDirection::Vertical,
                        vec![leaf(2), leaf(3)],
                        vec![0.5, 0.5],
                    ),
                ],
                vec![0.5, 0.5],
            ),
            CONTENT,
        );
        assert_eq!(focus_target(&g, 1, Right), Some(2));
        assert_eq!(focus_target(&g, 2, Down), Some(3));
        assert_eq!(focus_target(&g, 3, Up), Some(2));
        assert_eq!(focus_target(&g, 3, Left), Some(1));
    }

    #[test]
    fn focus_target_follows_center_ray_in_asymmetric_splits() {
        // H[1, V[2, 3]] with the right column split 25/75: pane 1's
        // center ray passes through pane 3, so it wins over the
        // topmost-but-off-ray pane 2 (Spec-0007: the projected ray, not
        // band order, picks among equidistant panes).
        let g = compute_layout(
            &container(
                LayoutDirection::Horizontal,
                vec![
                    leaf(1),
                    container(
                        LayoutDirection::Vertical,
                        vec![leaf(2), leaf(3)],
                        vec![0.25, 0.75],
                    ),
                ],
                vec![0.5, 0.5],
            ),
            CONTENT,
        );
        assert_eq!(focus_target(&g, 1, Right), Some(3));
    }

    #[test]
    fn focus_target_nearest_beats_ray_and_cross_position() {
        // H[1, V[2, 3], 4]: from pane 1 moving right, panes 2 and 3 sit
        // one border away while pane 4 (full height, dead on the ray) is
        // a whole column farther. Edge distance is the primary key, so
        // pane 2 wins; an ordering that consulted the ray or cross
        // position first would pick 4.
        let g = compute_layout(
            &container(
                LayoutDirection::Horizontal,
                vec![
                    leaf(1),
                    container(
                        LayoutDirection::Vertical,
                        vec![leaf(2), leaf(3)],
                        vec![0.5, 0.5],
                    ),
                    leaf(4),
                ],
                vec![1.0, 1.0, 1.0],
            ),
            CONTENT,
        );
        assert_eq!(focus_target(&g, 1, Right), Some(2));
        assert_eq!(focus_target(&g, 4, Left), Some(2));
    }

    #[test]
    fn focus_target_excludes_corner_touching_panes() {
        // Left column split so pane 1's bottom edge exactly meets pane
        // 4's top: corner contact is not band overlap, so moving left
        // from 4 lands on 2.
        let g = compute_layout(
            &container(
                LayoutDirection::Horizontal,
                vec![
                    container(
                        LayoutDirection::Vertical,
                        vec![leaf(1), leaf(2)],
                        vec![301.0, 298.0],
                    ),
                    container(
                        LayoutDirection::Vertical,
                        vec![leaf(3), leaf(4)],
                        vec![0.5, 0.5],
                    ),
                ],
                vec![0.5, 0.5],
            ),
            CONTENT,
        );
        let pane = |id: u32| g.panes.iter().find(|p| p.pane_id == id).unwrap().rect;
        assert_eq!(
            pane(1).y + pane(1).height,
            pane(4).y,
            "fixture: corner contact between panes 1 and 4"
        );
        assert_eq!(focus_target(&g, 4, Left), Some(2));
    }

    // --- border hit-testing and pane pairs ---

    fn pair(before: u32, after: u32) -> FlankedPanes {
        FlankedPanes { before, after }
    }

    #[test]
    fn border_at_hits_within_pad_and_misses_outside() {
        // H[1, 2] over 800px: border at x = 400 (even split of 799).
        let g = compute_layout(
            &container(
                LayoutDirection::Horizontal,
                vec![leaf(1), leaf(2)],
                vec![0.5, 0.5],
            ),
            CONTENT,
        );
        let bx = f64::from(g.borders[0].x);
        assert_eq!(border_at(&g, bx, 300.0, 3.0), Some(0));
        assert_eq!(border_at(&g, bx - 3.0, 300.0, 3.0), Some(0), "pad left");
        assert_eq!(border_at(&g, bx + 3.9, 300.0, 3.0), Some(0), "pad right");
        assert_eq!(border_at(&g, bx - 10.0, 300.0, 3.0), None, "outside pad");
        assert_eq!(border_at(&g, bx, 700.0, 3.0), None, "outside long axis");
    }

    #[test]
    fn border_panes_returns_flanking_pair_in_layout_order() {
        let g = compute_layout(
            &container(
                LayoutDirection::Horizontal,
                vec![leaf(1), leaf(2)],
                vec![0.5, 0.5],
            ),
            CONTENT,
        );
        assert_eq!(border_panes(&g, 0, 300.0), Some(pair(1, 2)));
        assert_eq!(border_panes(&g, 9, 300.0), None, "unknown border");
    }

    #[test]
    fn border_at_hits_horizontal_border() {
        let g = compute_layout(
            &container(
                LayoutDirection::Vertical,
                vec![leaf(1), leaf(2)],
                vec![0.5, 0.5],
            ),
            CONTENT,
        );
        let by = f64::from(g.borders[0].y);
        assert_eq!(border_at(&g, 400.0, by, 3.0), Some(0));
        assert_eq!(border_at(&g, 400.0, by - 3.0, 3.0), Some(0), "pad above");
        assert_eq!(border_at(&g, 400.0, by + 3.9, 3.0), Some(0), "pad below");
        assert_eq!(border_at(&g, 400.0, by + 4.0, 3.0), None, "exact pad edge");
        assert_eq!(border_at(&g, 400.0, by - 10.0, 3.0), None, "outside pad");
        assert_eq!(border_at(&g, 800.0, by, 3.0), None, "long axis is exact");
    }

    #[test]
    fn border_at_overlapping_pads_pick_the_first_border() {
        // A 2px-wide three-way split: both borders sit 1px apart with
        // zero-width panes between them, so their pad zones overlap.
        let g = compute_layout(
            &container(
                LayoutDirection::Horizontal,
                vec![leaf(1), leaf(2), leaf(3)],
                vec![1.0, 1.0, 1.0],
            ),
            PixelRect {
                x: 0,
                y: 0,
                width: 2,
                height: 600,
            },
        );
        let shared = f64::from(g.borders[0].x);
        assert_eq!(border_at(&g, shared, 300.0, 3.0), Some(0));
        // Zero-width flanking panes never cover any cross position, so
        // this returns None; the assertion is that it doesn't panic.
        let _ = border_panes(&g, 0, 300.0);
    }

    #[test]
    fn border_panes_dead_row_on_the_inner_border() {
        // On the outer vertical border, the cursor row of the inner
        // horizontal border belongs to neither flanking pane: the 1px
        // row resolves no pair, and the rows either side pin the
        // transition.
        let g = compute_layout(
            &container(
                LayoutDirection::Horizontal,
                vec![
                    leaf(1),
                    container(
                        LayoutDirection::Vertical,
                        vec![leaf(2), leaf(3)],
                        vec![0.5, 0.5],
                    ),
                ],
                vec![0.5, 0.5],
            ),
            CONTENT,
        );
        let inner_y = f64::from(
            g.borders
                .iter()
                .find(|b| !b.is_vertical_border())
                .expect("inner border exists")
                .y,
        );
        assert_eq!(border_panes(&g, 0, inner_y), None);
        assert_eq!(border_panes(&g, 0, inner_y - 1.0), Some(pair(1, 2)));
        assert_eq!(border_panes(&g, 0, inner_y + 1.0), Some(pair(1, 3)));
    }

    #[test]
    fn border_panes_picks_the_pair_at_the_cross_position() {
        // H[1, V[2, 3]]: the full-height vertical border touches pane 1
        // on the left and panes 2 (top) / 3 (bottom) on the right — the
        // cursor's y picks which pair a drag adjusts (Spec-0007 Resize).
        let g = compute_layout(
            &container(
                LayoutDirection::Horizontal,
                vec![
                    leaf(1),
                    container(
                        LayoutDirection::Vertical,
                        vec![leaf(2), leaf(3)],
                        vec![0.5, 0.5],
                    ),
                ],
                vec![0.5, 0.5],
            ),
            CONTENT,
        );
        assert_eq!(border_panes(&g, 0, 100.0), Some(pair(1, 2)));
        assert_eq!(border_panes(&g, 0, 500.0), Some(pair(1, 3)));
        let horizontal = g
            .borders
            .iter()
            .position(|b| b.height == 1)
            .expect("inner border exists");
        assert_eq!(border_panes(&g, horizontal, 600.0), Some(pair(2, 3)));
    }

    #[test]
    fn focus_target_degenerate_zero_size_panes_do_not_panic() {
        // A 2px-wide three-way split saturates every pane to zero width;
        // focus navigation over coincident rects must stay total. The
        // winner among coincident panes is arbitrary but stable.
        let g = compute_layout(
            &container(
                LayoutDirection::Horizontal,
                vec![leaf(1), leaf(2), leaf(3)],
                vec![1.0, 1.0, 1.0],
            ),
            PixelRect {
                x: 0,
                y: 0,
                width: 2,
                height: 600,
            },
        );
        for direction in [Left, Right, Up, Down] {
            let _ = focus_target(&g, 2, direction);
        }
        assert!(focus_target(&g, 2, Left).is_some());
    }

    #[test]
    fn focus_target_requires_band_overlap() {
        // H[V[1, 2], V[3, 4]] quadrants: moving left from the bottom-right
        // pane must land on the bottom-left pane, never the top-left one.
        let quadrants = container(
            LayoutDirection::Horizontal,
            vec![
                container(
                    LayoutDirection::Vertical,
                    vec![leaf(1), leaf(2)],
                    vec![0.5, 0.5],
                ),
                container(
                    LayoutDirection::Vertical,
                    vec![leaf(3), leaf(4)],
                    vec![0.5, 0.5],
                ),
            ],
            vec![0.5, 0.5],
        );
        let g = compute_layout(&quadrants, CONTENT);
        assert_eq!(focus_target(&g, 4, Left), Some(2));
        assert_eq!(focus_target(&g, 4, Up), Some(3));
        assert_eq!(focus_target(&g, 1, Right), Some(3));
        assert_eq!(focus_target(&g, 1, Down), Some(2));
    }
}
