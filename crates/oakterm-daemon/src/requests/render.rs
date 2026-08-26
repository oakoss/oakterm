//! Render family: `GetRenderUpdate` (0x71).

use super::{RequestResult, make_error_response};
use crate::pane::{PaneManager, lock_live_pane};
use crate::wire::row_to_wire;
use oakterm_protocol::frame::Frame;
use oakterm_protocol::message::{ErrorCode, MSG_RENDER_UPDATE};
use oakterm_protocol::render::{DirtyRow, GetRenderUpdate, RenderUpdate};
use oakterm_terminal::grid::ScreenId;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, warn};

pub(super) async fn get_render_update(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(req) = GetRenderUpdate::decode(&frame.payload) else {
        warn!(conn_id, "malformed GetRenderUpdate payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed GetRenderUpdate",
        );
    };
    let Some(pane) = lock_live_pane(panes, req.pane_id).await else {
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownPane,
            "unknown pane",
        );
    };
    let alt_screen = pane.screens.active_screen() == ScreenId::Alternate;
    let history_len = pane.history_len();
    let g = pane.screens.active_grid();
    // If since_seqno > g.seqno, the client is tracking a seqno from
    // a different grid (e.g., the alternate buffer before switching
    // back to primary). The seqno comparison is invalid — return all
    // rows for a full refresh. Fires once per grid transition.
    let dirty_indices: Vec<u16> = if req.since_seqno > g.seqno {
        debug!(
            conn_id,
            pane_id = req.pane_id,
            since_seqno = req.since_seqno,
            grid_seqno = g.seqno,
            "since_seqno exceeds grid seqno; returning full refresh"
        );
        (0..u16::try_from(g.lines.len()).unwrap_or(u16::MAX)).collect()
    } else {
        g.dirty_rows(req.since_seqno)
    };

    let dirty_rows: Vec<DirtyRow> = dirty_indices
        .iter()
        .filter_map(|&idx| {
            let row = g.lines.get(idx as usize)?;
            Some(row_to_wire(row, idx, &g.palette))
        })
        .collect();

    let (bg_r, bg_g, bg_b) = match g.dynamic_bg {
        Some(rgb) => (rgb.r, rgb.g, rgb.b),
        None => (0, 0, 0),
    };
    let update = RenderUpdate {
        pane_id: req.pane_id,
        seqno: g.seqno,
        cursor_x: g.cursor.col,
        cursor_y: g.cursor.row,
        cursor_style: g.cursor.style.to_wire(),
        cursor_visible: g.cursor.visible,
        bg_r,
        bg_g,
        bg_b,
        bracketed_paste: g.modes.get(2004),
        alt_screen,
        // Zero until TREK-236 populates the input-mode state (Spec-0011).
        input_flags: 0,
        kitty_kbd_flags: 0,
        history_len,
        dirty_rows,
    };

    match update.encode() {
        Ok(payload) => match Frame::new(MSG_RENDER_UPDATE, frame.serial, payload) {
            Ok(f) => RequestResult::Response(f),
            Err(e) => {
                error!(conn_id, error = %e, "failed to create RenderUpdate frame");
                make_error_response(
                    conn_id,
                    frame.serial,
                    ErrorCode::InternalError,
                    "RenderUpdate frame error",
                )
            }
        },
        Err(e) => {
            error!(conn_id, error = %e, "failed to encode RenderUpdate");
            make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InternalError,
                "RenderUpdate encode error",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::PaneManager;
    use oakterm_terminal::grid::row::Row;

    /// The reply carries the pane's history length (ADR-0025 clause 1).
    /// A regression to zero would be accepted as a valid base and pin
    /// every fill and yank at the top of history — silently.
    #[tokio::test]
    async fn a_render_update_reply_carries_the_history_length() {
        let panes = Arc::new(Mutex::new(PaneManager::new()));
        let pane_id = panes
            .lock()
            .await
            .create(80, 24, String::new(), String::new());
        {
            let mut pane = crate::pane::lock_live_pane(&panes, pane_id)
                .await
                .expect("pane");
            for _ in 0..3 {
                pane.screens.push_to_scrollback(Row::new(80));
            }
        }

        let req = GetRenderUpdate {
            pane_id,
            since_seqno: 0,
        };
        let frame = Frame::new(0x71, 9, req.encode()).expect("frame");
        let RequestResult::Response(reply) = get_render_update(1, &frame, &panes).await else {
            panic!("expected a RenderUpdate response");
        };
        let update =
            oakterm_protocol::render::RenderUpdate::decode(&reply.payload).expect("decode");
        assert_eq!(update.history_len, 3);
        assert_eq!((update.input_flags, update.kitty_kbd_flags), (0, 0));
    }
}
