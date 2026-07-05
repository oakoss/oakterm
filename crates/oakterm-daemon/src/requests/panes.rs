//! Pane-management family: `CreatePane`, `ClosePane`, `FocusPane`,
//! `ListPanes` (0x90-0x95).

use super::{RequestResult, make_error_response};
use crate::pane::{PaneManager, PtyState, SharedPane};
use oakterm_protocol::frame::Frame;
use oakterm_protocol::message::{
    ClosePane, CreatePane, CreatePaneResponse, ErrorCode, FocusPane, ListPanesResponse,
    MSG_CLOSE_PANE_RESPONSE, PaneInfo,
};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

pub(super) async fn create_pane(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = CreatePane::decode(&frame.payload) else {
        warn!(conn_id, "malformed CreatePane payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed CreatePane",
        );
    };
    // Create the pane at a default size; GUI sends Resize immediately.
    let mut pm = panes.lock().await;
    let pane_id = pm.create(80, 24, msg.command.clone(), msg.cwd.clone());
    drop(pm);
    info!(conn_id, pane_id, command = %msg.command, cwd = %msg.cwd, "pane created");
    let resp = CreatePaneResponse { pane_id };
    match resp.to_frame(frame.serial) {
        Ok(f) => RequestResult::Response(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to encode CreatePaneResponse");
            make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InternalError,
                "CreatePaneResponse encode error",
            )
        }
    }
}

pub(super) async fn close_pane(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = ClosePane::decode(&frame.payload) else {
        warn!(conn_id, "malformed ClosePane payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed ClosePane",
        );
    };
    let mut pm = panes.lock().await;
    if pm.len() <= 1 {
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::InternalError,
            "cannot close the last pane",
        );
    }
    let Some(removed) = pm.remove(msg.pane_id) else {
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownPane,
            "unknown pane",
        );
    };
    drop(pm);
    // Tombstone under the pane lock: any handle cloned before the
    // removal observes `closed` once it acquires the lock. Taking
    // the state gives ownership of the cancel sender.
    let removed_state = {
        let mut removed = removed.lock().await;
        removed.closed = true;
        std::mem::replace(&mut removed.pty_state, PtyState::NotSpawned)
    };
    // Signal the read loop to exit promptly. Without this, an idle
    // shell (no output) would leave the loop blocked on readable()
    // forever; the loop only notices a removed pane on its next
    // successful read. Once the loop exits, dropping the Pty kills
    // and reaps the child via Pty::Drop.
    if let PtyState::Running { pid, cancel, .. } = removed_state {
        info!(
            conn_id,
            pane_id = msg.pane_id,
            pid,
            "pane closed, signalling PTY read loop"
        );
        // Best-effort: receiver is already gone if the loop exited
        // on its own (EOF, read error, or early-return during
        // AsyncFd setup). cancel_tx is uniquely owned by this
        // handler, so there's no other sender to race against.
        let _ = cancel.send(());
    } else {
        info!(conn_id, pane_id = msg.pane_id, "pane closed");
    }
    // Empty response confirms closure.
    match Frame::new(MSG_CLOSE_PANE_RESPONSE, frame.serial, vec![]) {
        Ok(f) => RequestResult::Response(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to create ClosePaneResponse frame");
            make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InternalError,
                "ClosePaneResponse frame error",
            )
        }
    }
}

pub(super) async fn focus_pane(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = FocusPane::decode(&frame.payload) else {
        warn!(conn_id, "malformed FocusPane payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed FocusPane",
        );
    };
    if !panes.lock().await.focus(msg.pane_id) {
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownPane,
            "unknown pane",
        );
    }
    debug!(conn_id, pane_id = msg.pane_id, "focus changed");
    // FocusPane is a push message (serial 0) per Spec-0001.
    RequestResult::NoResponse
}

pub(super) async fn list_panes(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let pane_list: Vec<(u32, SharedPane)> = panes.lock().await.snapshot();
    let mut infos: Vec<PaneInfo> = Vec::with_capacity(pane_list.len());
    for (id, pane) in pane_list {
        let pane = pane.lock().await;
        if pane.closed {
            continue;
        }
        let g = pane.screens.active_grid();
        let (pid, exit_code) = match &pane.pty_state {
            PtyState::Running { pid, .. } => (*pid, -1),
            PtyState::Exited { exit_code } => (0, *exit_code),
            PtyState::NotSpawned => (0, -1),
            PtyState::Failed(reason) => {
                debug!(pane_id = id, reason, "listing pane in failed state");
                (0, -1)
            }
        };
        infos.push(PaneInfo {
            pane_id: id,
            title: g.title.clone().unwrap_or_default(),
            cols: g.cols,
            rows: g.rows,
            pid,
            exit_code,
            cwd: pane.cwd.clone(),
        });
    }
    let resp = ListPanesResponse { panes: infos };
    match resp.to_frame(frame.serial) {
        Ok(f) => RequestResult::Response(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to encode ListPanesResponse");
            make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InternalError,
                "ListPanesResponse encode error",
            )
        }
    }
}
