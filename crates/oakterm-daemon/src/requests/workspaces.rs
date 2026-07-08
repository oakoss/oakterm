//! Workspace family: `NewWorkspace`, `SwitchWorkspace`, `RenameWorkspace`,
//! `CloseWorkspace` (0xAC-0xAE, 0xB3-0xB4). Workspace state lives in the
//! Spec-0007 mux model behind `PaneManager`; the handlers translate wire
//! IDs and drive the model's workspace operations.

use super::{RequestResult, make_error_response};
use crate::pane::{PaneManager, SharedPane};
use oakterm_protocol::frame::Frame;
use oakterm_protocol::message::{
    CloseWorkspace, CloseWorkspaceResponse, ErrorCode, NewWorkspace, NewWorkspaceResponse,
    RenameWorkspace, SwitchWorkspace,
};
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

pub(super) async fn rename_workspace(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = RenameWorkspace::decode(&frame.payload) else {
        warn!(conn_id, "malformed RenameWorkspace payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed RenameWorkspace",
        );
    };
    if !panes
        .lock()
        .await
        .rename_workspace(msg.workspace_id, msg.name.clone())
    {
        warn!(
            conn_id,
            workspace_id = msg.workspace_id,
            "rename of unknown workspace"
        );
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownWorkspace,
            "unknown workspace",
        );
    }
    debug!(conn_id, workspace_id = msg.workspace_id, name = %msg.name, "workspace renamed");
    // RenameWorkspace is a push message (serial 0) per Spec-0001.
    RequestResult::NoResponse
}

pub(super) async fn close_workspace(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = CloseWorkspace::decode(&frame.payload) else {
        warn!(conn_id, "malformed CloseWorkspace payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed CloseWorkspace",
        );
    };
    // Remove every pane under one manager lock so a racing mutation can't
    // interleave with a half-closed workspace; the per-pane shutdowns
    // follow after release (manager->pane lock order).
    let (removed, missing): (Vec<(u32, SharedPane)>, usize) = {
        let mut pm = panes.lock().await;
        // Existence is checked before the last-workspace guard so an
        // unknown id reports UnknownWorkspace, not LayoutRejected.
        if !pm.workspace_exists(msg.workspace_id) {
            return make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::UnknownWorkspace,
                "unknown workspace",
            );
        }
        if pm.workspace_count() <= 1 {
            return make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::LayoutRejected,
                "cannot close the last workspace",
            );
        }
        let Some((removed, missing)) = pm.close_workspace(msg.workspace_id) else {
            // Unreachable: existence was checked under this same lock.
            error!(
                conn_id,
                workspace_id = msg.workspace_id,
                "close_workspace missed a workspace that exists; mux out of sync"
            );
            return make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InternalError,
                "workspace close failed",
            );
        };
        (removed, missing)
    };
    for (id, pane) in &removed {
        super::panes::shutdown_removed_pane(conn_id, *id, pane).await;
    }
    // A workspace pane absent from the map means the mux and pane map
    // desynced mid-close; report failure rather than a false success
    // (mirrors close_tab).
    if missing > 0 {
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::InternalError,
            "workspace close incomplete",
        );
    }
    info!(
        conn_id,
        workspace_id = msg.workspace_id,
        panes = removed.len(),
        "workspace closed"
    );
    match CloseWorkspaceResponse.to_frame(frame.serial) {
        Ok(f) => RequestResult::Response(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to create CloseWorkspaceResponse frame");
            make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InternalError,
                "CloseWorkspaceResponse frame error",
            )
        }
    }
}
