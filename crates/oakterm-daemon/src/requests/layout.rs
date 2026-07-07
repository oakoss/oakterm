//! Split-topology family: `SplitPane`, `ResizePane`, `SwapPane`
//! (0xA0-0xA4). Layout mutations validate against the Spec-0007 tree; the
//! handlers convert the wire protocol's cell-space deltas and minimums
//! into the tree's normalized weight space.

use super::{RequestResult, make_error_response};
use crate::pane::{PaneManager, lock_live_pane};
use oakterm_mux::{BorderExtents, LayoutError, LayoutNode, SplitDirection, SplitPreview};
use oakterm_protocol::frame::Frame;
use oakterm_protocol::message::{
    ErrorCode, GetLayoutTree, LayoutDirection, LayoutTree, LayoutTreeNode, ResizePane,
    SplitDirection as WireSplitDirection, SplitPane, SplitPaneResponse, SwapPane, SwapPaneResponse,
};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

/// Spec-0007 minimum pane size: 2 columns wide, 1 row tall.
const MIN_COLS: f32 = 2.0;
const MIN_ROWS: f32 = 1.0;

fn layout_error_code(e: &LayoutError) -> ErrorCode {
    match e {
        LayoutError::PaneNotFound(_) => ErrorCode::UnknownPane,
        LayoutError::NotAdjacentSiblings { .. } => ErrorCode::LayoutRejected,
        // The daemon assigns fresh ids and computes the weights itself;
        // these two indicate a daemon bug, not a client error.
        LayoutError::PaneAlreadyPresent(_) | LayoutError::InvalidResizeParam => {
            ErrorCode::InternalError
        }
    }
}

fn layout_error_response(conn_id: u64, serial: u32, e: &LayoutError) -> RequestResult {
    warn!(conn_id, error = %e, "layout operation rejected");
    make_error_response(conn_id, serial, layout_error_code(e), &e.to_string())
}

/// Convert a cell-space resize delta and the Spec-0007 minimum pane size
/// into the border container's weight space: `(delta_weight, min_weight)`.
/// The wire delta is in cells along the border axis; the pane's own grid
/// extent anchors the cell↔unit conversion (`container_cells = pane_cells
/// / pane_extent * container_extent`). `None` when the geometry is
/// degenerate (non-finite or non-positive container extent).
fn resize_weights(ext: &BorderExtents, cols: u16, rows: u16, delta: i16) -> Option<(f32, f32)> {
    let (pane_cells, min_cells) = match ext.axis {
        SplitDirection::Horizontal => (f32::from(cols), MIN_COLS),
        SplitDirection::Vertical => (f32::from(rows), MIN_ROWS),
    };
    let container_cells = pane_cells / ext.pane_extent * ext.container_extent;
    if !container_cells.is_finite() || container_cells <= 0.0 {
        return None;
    }
    Some((
        f32::from(delta) / container_cells,
        min_cells / container_cells,
    ))
}

/// Read a live pane's grid dimensions, honoring the manager→pane lock
/// order. `None` when the pane is unknown or tombstoned.
async fn pane_dims(panes: &Arc<Mutex<PaneManager>>, pane_id: u32) -> Option<(u16, u16)> {
    let guard = lock_live_pane(panes, pane_id).await?;
    let g = guard.screens.active_grid();
    Some((g.cols, g.rows))
}

/// Spec-0007 Constraints: every pane a split produces or shrinks must stay
/// at or above the 2x1 minimum along the split axis — the new pane, the
/// shrunk target (`cols`/`rows` are the target's dims), and any sibling
/// scaled by a same-direction insert. `Some(reason)` names the violation.
async fn split_min_size_violation(
    panes: &Arc<Mutex<PaneManager>>,
    preview: &SplitPreview,
    direction: SplitDirection,
    cols: u16,
    rows: u16,
) -> Option<&'static str> {
    let (axis_cells, min_cells) = match direction {
        SplitDirection::Horizontal => (f32::from(cols), MIN_COLS),
        SplitDirection::Vertical => (f32::from(rows), MIN_ROWS),
    };
    if axis_cells * preview.new_pane_fraction < min_cells
        || axis_cells * preview.target_fraction < min_cells
    {
        return Some("split rejected: resulting pane below the 2x1 minimum");
    }
    for sibling in &preview.shrunk_siblings {
        // A sibling that vanishes mid-check is leaving the layout anyway.
        let Some((s_cols, s_rows)) = pane_dims(panes, sibling.0).await else {
            continue;
        };
        let s_cells = match direction {
            SplitDirection::Horizontal => f32::from(s_cols),
            SplitDirection::Vertical => f32::from(s_rows),
        };
        if s_cells * preview.target_fraction < min_cells {
            return Some("split rejected: a sibling pane would drop below the 2x1 minimum");
        }
    }
    None
}

pub(super) async fn split_pane(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = SplitPane::decode(&frame.payload) else {
        warn!(conn_id, "malformed SplitPane payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed SplitPane",
        );
    };
    let direction = match msg.direction {
        WireSplitDirection::Horizontal => SplitDirection::Horizontal,
        WireSplitDirection::Vertical => SplitDirection::Vertical,
    };
    let Some((cols, rows)) = pane_dims(panes, msg.pane_id).await else {
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownPane,
            "unknown pane",
        );
    };

    let preview = {
        let pm = panes.lock().await;
        match pm.split_preview(msg.pane_id, direction) {
            Ok(p) => p,
            Err(e) => return layout_error_response(conn_id, frame.serial, &e),
        }
    };
    if let Some(reason) = split_min_size_violation(panes, &preview, direction, cols, rows).await {
        warn!(conn_id, pane_id = msg.pane_id, cols, rows, reason);
        return make_error_response(conn_id, frame.serial, ErrorCode::LayoutRejected, reason);
    }
    let mut pm = panes.lock().await;
    // Same default size as CreatePane; the GUI sends Resize immediately.
    let new_pane_id = match pm.split_create(
        msg.pane_id,
        direction,
        80,
        24,
        msg.command.clone(),
        msg.cwd.clone(),
    ) {
        Ok(id) => id,
        // Reachable only if the target vanished between the dims read and
        // the manager lock (a racing ClosePane).
        Err(e) => return layout_error_response(conn_id, frame.serial, &e),
    };
    drop(pm);
    info!(
        conn_id,
        target = msg.pane_id,
        new_pane_id,
        direction = ?msg.direction,
        command = %msg.command,
        "pane split"
    );
    let resp = SplitPaneResponse { new_pane_id };
    match resp.to_frame(frame.serial) {
        Ok(f) => RequestResult::Response(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to encode SplitPaneResponse");
            make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InternalError,
                "SplitPaneResponse encode error",
            )
        }
    }
}

pub(super) async fn resize_pane(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = ResizePane::decode(&frame.payload) else {
        warn!(conn_id, "malformed ResizePane payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed ResizePane",
        );
    };
    let Some((cols, rows)) = pane_dims(panes, msg.pane_id).await else {
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownPane,
            "unknown pane",
        );
    };

    let mut pm = panes.lock().await;
    let ext = match pm.border_extents(msg.pane_id, msg.neighbor_pane_id) {
        Ok(e) => e,
        Err(e) => return layout_error_response(conn_id, frame.serial, &e),
    };
    let Some((delta_weight, min_weight)) = resize_weights(&ext, cols, rows, msg.delta) else {
        error!(
            conn_id,
            pane_id = msg.pane_id,
            neighbor = msg.neighbor_pane_id,
            pane_extent = ext.pane_extent,
            "degenerate layout geometry for resize"
        );
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::InternalError,
            "degenerate layout geometry",
        );
    };
    if let Err(e) = pm.resize_layout(msg.pane_id, msg.neighbor_pane_id, delta_weight, min_weight) {
        return layout_error_response(conn_id, frame.serial, &e);
    }
    drop(pm);
    debug!(
        conn_id,
        pane_id = msg.pane_id,
        neighbor = msg.neighbor_pane_id,
        delta = msg.delta,
        "pane border moved"
    );
    // ResizePane is a push message (serial 0) per Spec-0001.
    RequestResult::NoResponse
}

pub(super) async fn swap_pane(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = SwapPane::decode(&frame.payload) else {
        warn!(conn_id, "malformed SwapPane payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed SwapPane",
        );
    };
    if let Err(e) = panes.lock().await.swap_layout(msg.pane_id_a, msg.pane_id_b) {
        return layout_error_response(conn_id, frame.serial, &e);
    }
    info!(
        conn_id,
        pane_a = msg.pane_id_a,
        pane_b = msg.pane_id_b,
        "panes swapped"
    );
    match SwapPaneResponse.to_frame(frame.serial) {
        Ok(f) => RequestResult::Response(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to create SwapPaneResponse frame");
            make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InternalError,
                "SwapPaneResponse frame error",
            )
        }
    }
}

/// Convert the in-memory layout tree to the Spec-0001 wire DTO
/// (parallel-array `LayoutTreeNode` with live pane IDs at the leaves).
fn to_wire_tree(node: &LayoutNode) -> LayoutTreeNode {
    match node {
        LayoutNode::Leaf(pane_id) => LayoutTreeNode::Leaf { pane_id: pane_id.0 },
        LayoutNode::Container(c) => LayoutTreeNode::Container {
            direction: match c.direction {
                SplitDirection::Horizontal => LayoutDirection::Horizontal,
                SplitDirection::Vertical => LayoutDirection::Vertical,
            },
            children: c.children.iter().map(|ch| to_wire_tree(&ch.node)).collect(),
            weights: c.children.iter().map(|ch| ch.weight).collect(),
        },
    }
}

pub(super) async fn get_layout_tree(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = GetLayoutTree::decode(&frame.payload) else {
        warn!(conn_id, "malformed GetLayoutTree payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed GetLayoutTree",
        );
    };
    // The mux model has landed, but this still serves the active tab's tree
    // regardless of msg.tab_id; per-tab resolution is deferred until the tab
    // bar (TREK-107) needs to query background tabs.
    debug!(
        conn_id,
        workspace_id = msg.workspace_id,
        tab_id = msg.tab_id,
        "layout tree requested"
    );
    let tree = {
        let pm = panes.lock().await;
        pm.topology_snapshot().map(|s| to_wire_tree(&s.layout))
    };
    let Some(tree) = tree else {
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownPane,
            "no panes exist",
        );
    };
    match (LayoutTree { tree }).to_frame(frame.serial) {
        Ok(f) => RequestResult::Response(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to encode LayoutTree");
            make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InternalError,
                "LayoutTree encode error",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{resize_weights, to_wire_tree};
    use oakterm_mux::{BorderExtents, Child, Container, LayoutNode, PaneId, SplitDirection};
    use oakterm_protocol::message::{LayoutDirection, LayoutTreeNode};

    #[test]
    fn resize_weights_horizontal_uses_cols() {
        // 80-col pane at half the tree width: 160 cells across the full
        // container. +16 cells = +0.1 weight; 2-col minimum = 0.0125.
        let ext = BorderExtents {
            axis: SplitDirection::Horizontal,
            pane_extent: 0.5,
            container_extent: 1.0,
        };
        let (delta_weight, min_weight) = resize_weights(&ext, 80, 24, 16).unwrap();
        assert!((delta_weight - 0.1).abs() < 1e-6);
        assert!((min_weight - 2.0 / 160.0).abs() < 1e-6);
    }

    #[test]
    fn resize_weights_vertical_uses_rows() {
        // 24-row pane at 30% of a container spanning 60% of the tree:
        // container = 24 / 0.3 * 0.6 = 48 rows. -12 rows = -0.25 weight;
        // 1-row minimum = 1/48.
        let ext = BorderExtents {
            axis: SplitDirection::Vertical,
            pane_extent: 0.3,
            container_extent: 0.6,
        };
        let (delta_weight, min_weight) = resize_weights(&ext, 80, 24, -12).unwrap();
        assert!((delta_weight - (-0.25)).abs() < 1e-6);
        assert!((min_weight - 1.0 / 48.0).abs() < 1e-6);
    }

    #[test]
    fn resize_weights_sign_follows_delta() {
        let ext = BorderExtents {
            axis: SplitDirection::Horizontal,
            pane_extent: 0.5,
            container_extent: 1.0,
        };
        assert!(resize_weights(&ext, 80, 24, 5).unwrap().0 > 0.0);
        assert!(resize_weights(&ext, 80, 24, -5).unwrap().0 < 0.0);
        assert!(resize_weights(&ext, 80, 24, 0).unwrap().0.abs() < 1e-9);
    }

    #[test]
    fn to_wire_tree_leaf() {
        let wire = to_wire_tree(&LayoutNode::Leaf(PaneId(42)));
        assert_eq!(wire, LayoutTreeNode::Leaf { pane_id: 42 });
    }

    #[test]
    fn to_wire_tree_nested_container() {
        let tree = LayoutNode::Container(Container {
            direction: SplitDirection::Horizontal,
            children: vec![
                Child::new(LayoutNode::Leaf(PaneId(1)), 0.3),
                Child::new(
                    LayoutNode::Container(Container {
                        direction: SplitDirection::Vertical,
                        children: vec![
                            Child::new(LayoutNode::Leaf(PaneId(2)), 0.5),
                            Child::new(LayoutNode::Leaf(PaneId(3)), 0.5),
                        ],
                    }),
                    0.7,
                ),
            ],
        });
        let wire = to_wire_tree(&tree);
        assert_eq!(
            wire,
            LayoutTreeNode::Container {
                direction: LayoutDirection::Horizontal,
                children: vec![
                    LayoutTreeNode::Leaf { pane_id: 1 },
                    LayoutTreeNode::Container {
                        direction: LayoutDirection::Vertical,
                        children: vec![
                            LayoutTreeNode::Leaf { pane_id: 2 },
                            LayoutTreeNode::Leaf { pane_id: 3 },
                        ],
                        weights: vec![0.5, 0.5],
                    },
                ],
                weights: vec![0.3, 0.7],
            }
        );
    }

    #[test]
    fn resize_weights_rejects_degenerate_geometry() {
        let zero_extent = BorderExtents {
            axis: SplitDirection::Horizontal,
            pane_extent: 0.0,
            container_extent: 1.0,
        };
        assert!(resize_weights(&zero_extent, 80, 24, 5).is_none());

        let nan_extent = BorderExtents {
            axis: SplitDirection::Vertical,
            pane_extent: f32::NAN,
            container_extent: 1.0,
        };
        assert!(resize_weights(&nan_extent, 80, 24, 5).is_none());
    }
}
