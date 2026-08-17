//! Client request dispatch, split by message family (Spec-0001).

mod copy_mode;
mod input;
mod layout;
mod panes;
mod render;
mod scrollback;
mod search;
mod tabs;
mod workspaces;

use crate::pane::PaneManager;
use oakterm_protocol::frame::Frame;
use oakterm_protocol::message::{
    ErrorCode, ErrorMessage, MSG_CLOSE_PANE, MSG_CLOSE_TAB, MSG_CLOSE_WORKSPACE, MSG_CREATE_PANE,
    MSG_DETACH, MSG_ENTER_COPY_MODE, MSG_EXIT_COPY_MODE, MSG_FIND_PROMPT, MSG_FOCUS_PANE,
    MSG_GET_LAYOUT_TREE, MSG_GET_RENDER_UPDATE, MSG_GET_SCROLLBACK, MSG_KEY_INPUT, MSG_LIST_PANES,
    MSG_LIST_TABS, MSG_MOUSE_INPUT, MSG_MOVE_TAB, MSG_NEW_TAB, MSG_NEW_WORKSPACE, MSG_PING,
    MSG_PONG, MSG_RENAME_TAB, MSG_RENAME_WORKSPACE, MSG_RESIZE, MSG_RESIZE_PANE, MSG_SEARCH_CLOSE,
    MSG_SEARCH_NEXT, MSG_SEARCH_PREV, MSG_SEARCH_SCROLLBACK, MSG_SPLIT_PANE, MSG_SWAP_PANE,
    MSG_SWITCH_TAB, MSG_SWITCH_WORKSPACE, MSG_YANK_SELECTION,
};
use std::sync::Arc;
use tokio::sync::{Mutex, watch};
use tracing::{debug, error};

pub(crate) use copy_mode::release_client_pins;

/// Result of processing a client request.
pub(crate) enum RequestResult {
    Response(Frame),
    Detach,
    NoResponse,
}

/// Handle a single client request frame.
pub(crate) async fn handle_request(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
    dirty_tx: &watch::Sender<u64>,
) -> RequestResult {
    match frame.msg_type {
        MSG_KEY_INPUT => input::key_input(conn_id, frame, panes).await,
        MSG_MOUSE_INPUT => input::mouse_input(conn_id, frame, panes).await,
        MSG_RESIZE => input::resize(conn_id, frame, panes, dirty_tx).await,
        MSG_DETACH => RequestResult::Detach,
        MSG_GET_RENDER_UPDATE => render::get_render_update(conn_id, frame, panes).await,
        MSG_GET_SCROLLBACK => scrollback::get_scrollback(conn_id, frame, panes).await,
        MSG_FIND_PROMPT => search::find_prompt(conn_id, frame, panes).await,
        MSG_SEARCH_SCROLLBACK => search::search_scrollback(conn_id, frame, panes).await,
        MSG_SEARCH_NEXT => search::search_next(conn_id, frame, panes).await,
        MSG_SEARCH_PREV => search::search_prev(conn_id, frame, panes).await,
        MSG_SEARCH_CLOSE => search::search_close(panes).await,
        MSG_CREATE_PANE => panes::create_pane(conn_id, frame, panes).await,
        MSG_CLOSE_PANE => panes::close_pane(conn_id, frame, panes).await,
        MSG_FOCUS_PANE => panes::focus_pane(conn_id, frame, panes).await,
        MSG_LIST_PANES => panes::list_panes(conn_id, frame, panes).await,
        MSG_ENTER_COPY_MODE => copy_mode::enter_copy_mode(conn_id, frame, panes).await,
        MSG_EXIT_COPY_MODE => copy_mode::exit_copy_mode(conn_id, frame, panes).await,
        MSG_YANK_SELECTION => copy_mode::yank_selection(conn_id, frame, panes).await,
        MSG_SPLIT_PANE => layout::split_pane(conn_id, frame, panes).await,
        MSG_RESIZE_PANE => layout::resize_pane(conn_id, frame, panes).await,
        MSG_SWAP_PANE => layout::swap_pane(conn_id, frame, panes).await,
        MSG_GET_LAYOUT_TREE => layout::get_layout_tree(conn_id, frame, panes).await,
        MSG_NEW_TAB => tabs::new_tab(conn_id, frame, panes).await,
        MSG_CLOSE_TAB => tabs::close_tab(conn_id, frame, panes).await,
        MSG_SWITCH_TAB => tabs::switch_tab(conn_id, frame, panes).await,
        MSG_LIST_TABS => tabs::list_tabs(conn_id, frame, panes).await,
        MSG_MOVE_TAB => tabs::move_tab(conn_id, frame, panes).await,
        MSG_RENAME_TAB => tabs::rename_tab(conn_id, frame, panes).await,
        MSG_NEW_WORKSPACE => workspaces::new_workspace(conn_id, frame, panes).await,
        MSG_SWITCH_WORKSPACE => workspaces::switch_workspace(conn_id, frame, panes).await,
        MSG_RENAME_WORKSPACE => workspaces::rename_workspace(conn_id, frame, panes).await,
        MSG_CLOSE_WORKSPACE => workspaces::close_workspace(conn_id, frame, panes).await,
        MSG_PING => match Frame::new(MSG_PONG, frame.serial, vec![]) {
            Ok(f) => RequestResult::Response(f),
            Err(e) => {
                error!(conn_id, error = %e, "failed to create Pong frame");
                make_error_response(
                    conn_id,
                    frame.serial,
                    ErrorCode::InternalError,
                    "Pong frame error",
                )
            }
        },
        unknown => {
            // Spec-0001: ignore the frame — forward compatibility for
            // additive minor versions rests on this.
            debug!(conn_id, msg_type = unknown, "ignoring unknown message type");
            RequestResult::NoResponse
        }
    }
}

/// Build an error response frame, falling back to `NoResponse` if encoding fails.
pub(crate) fn make_error_response(
    conn_id: u64,
    serial: u32,
    code: ErrorCode,
    message: &str,
) -> RequestResult {
    let err = ErrorMessage {
        code: code as u32,
        message: message.to_string(),
    };
    match err.to_frame(serial) {
        Ok(f) => RequestResult::Response(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to encode error response");
            RequestResult::NoResponse
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::lock_live_pane;
    use oakterm_protocol::render::GetRenderUpdate;

    #[tokio::test]
    async fn handler_for_one_pane_unblocked_by_another_panes_lock() {
        let panes = Arc::new(Mutex::new(PaneManager::new()));
        let (a, b) = {
            let mut pm = panes.lock().await;
            let a = pm.create(80, 24, String::new(), String::new());
            let b = pm.create(80, 24, String::new(), String::new());
            (a, b)
        };
        // Pane B is mid-burst: its lock is held.
        let _held = lock_live_pane(&panes, b).await.unwrap();

        let (dirty_tx, _dirty_rx) = watch::channel(0u64);
        let req = GetRenderUpdate {
            pane_id: a,
            since_seqno: 0,
        };
        let frame = Frame::new(MSG_GET_RENDER_UPDATE, 1, req.encode()).unwrap();

        // The full handler path for pane A must not queue behind pane B.
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            handle_request(0, &frame, &panes, &dirty_tx),
        )
        .await
        .expect("pane A's request blocked behind pane B's lock");
        assert!(matches!(result, RequestResult::Response(_)));
    }
}
