//! Workspace family: `NewWorkspace`, `SwitchWorkspace` (0xAC-0xAE).
//! Workspace state lives in the Spec-0007 mux model behind `PaneManager`;
//! the handlers translate wire IDs and drive the model's workspace
//! operations.

use super::{RequestResult, make_error_response};
use crate::pane::PaneManager;
use oakterm_protocol::frame::Frame;
use oakterm_protocol::message::{ErrorCode, NewWorkspace, NewWorkspaceResponse, SwitchWorkspace};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

pub(super) async fn new_workspace(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = NewWorkspace::decode(&frame.payload) else {
        warn!(conn_id, "malformed NewWorkspace payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed NewWorkspace",
        );
    };
    debug!(conn_id, name = %msg.name, "new workspace requested");
    // Same default size as CreatePane; the GUI sends Resize immediately.
    let created = panes
        .lock()
        .await
        .new_workspace_create(msg.name.clone(), 80, 24);
    let Some((workspace_id, tab_id, pane_id)) = created else {
        error!(conn_id, "mux refused the new workspace");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::InternalError,
            "workspace creation failed",
        );
    };
    info!(
        conn_id,
        workspace_id,
        tab_id,
        pane_id,
        name = %msg.name,
        "workspace created"
    );
    let resp = NewWorkspaceResponse {
        workspace_id,
        tab_id,
        pane_id,
    };
    match resp.to_frame(frame.serial) {
        Ok(f) => RequestResult::Response(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to encode NewWorkspaceResponse");
            make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InternalError,
                "NewWorkspaceResponse encode error",
            )
        }
    }
}

pub(super) async fn switch_workspace(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = SwitchWorkspace::decode(&frame.payload) else {
        warn!(conn_id, "malformed SwitchWorkspace payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed SwitchWorkspace",
        );
    };
    if !panes.lock().await.switch_workspace(msg.workspace_id) {
        warn!(
            conn_id,
            workspace_id = msg.workspace_id,
            "switch to unknown workspace"
        );
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownWorkspace,
            "unknown workspace",
        );
    }
    debug!(
        conn_id,
        workspace_id = msg.workspace_id,
        "active workspace switched"
    );
    // SwitchWorkspace is a push message (serial 0) per Spec-0001.
    RequestResult::NoResponse
}
