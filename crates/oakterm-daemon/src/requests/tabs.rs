//! Tab family: `NewTab`, `CloseTab`, `SwitchTab`, `ListTabs` (0xA7-0xAB,
//! 0xAF). Tab and workspace state lives in the Spec-0007 mux model behind
//! `PaneManager`; the handlers translate wire IDs and drive the model's
//! tab operations.

use super::{RequestResult, make_error_response};
use crate::pane::{PaneManager, SharedPane};
use oakterm_protocol::frame::Frame;
use oakterm_protocol::message::{
    CloseTab, CloseTabResponse, ErrorCode, MoveTab, NewTab, NewTabResponse, RenameTab, SwitchTab,
    TabEntry, TabList,
};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

pub(super) async fn new_tab(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = NewTab::decode(&frame.payload) else {
        warn!(conn_id, "malformed NewTab payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed NewTab",
        );
    };
    // The tab lands in the active workspace regardless of workspace_id:
    // the mux's new_tab op has no non-active-workspace target, so routing
    // waits on that op.
    debug!(
        conn_id,
        workspace_id = msg.workspace_id,
        "new tab requested"
    );
    // Same default size as CreatePane; the GUI sends Resize immediately.
    let created = panes
        .lock()
        .await
        .new_tab_create(80, 24, msg.command.clone(), msg.cwd.clone());
    let Some((tab_id, pane_id)) = created else {
        error!(conn_id, "mux refused the new tab");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::InternalError,
            "tab creation failed",
        );
    };
    info!(conn_id, tab_id, pane_id, command = %msg.command, "tab created");
    let resp = NewTabResponse { tab_id, pane_id };
    match resp.to_frame(frame.serial) {
        Ok(f) => RequestResult::Response(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to encode NewTabResponse");
            make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InternalError,
                "NewTabResponse encode error",
            )
        }
    }
}

pub(super) async fn close_tab(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = CloseTab::decode(&frame.payload) else {
        warn!(conn_id, "malformed CloseTab payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed CloseTab",
        );
    };
    // Remove every pane under one manager lock so a racing CreatePane or
    // SplitPane can't interleave with a half-closed tab; the per-pane
    // tombstones follow after release (manager->pane lock order).
    let mut missing = 0usize;
    let removed: Vec<(u32, SharedPane)> = {
        let mut pm = panes.lock().await;
        let Some(ids) = pm.tab_pane_ids(msg.tab_id) else {
            return make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::UnknownTab,
                "unknown tab",
            );
        };
        if ids.len() >= pm.len() {
            return make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::LayoutRejected,
                "cannot close the last tab",
            );
        }
        let mut removed = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(pane) = pm.remove(id) else {
                error!(
                    conn_id,
                    pane_id = id,
                    "tab pane missing from the pane map; mux out of sync"
                );
                debug_assert!(false, "tab pane {id} missing from the pane map");
                missing += 1;
                continue;
            };
            removed.push((id, pane));
        }
        removed
    };
    for (id, pane) in &removed {
        super::panes::shutdown_removed_pane(conn_id, *id, pane).await;
    }
    // A pane in the tab that was absent from the map means the mux and the
    // pane map desynced mid-close; the tab may still be live in the mux, so
    // report failure rather than a false success.
    if missing > 0 {
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::InternalError,
            "tab close incomplete",
        );
    }
    info!(
        conn_id,
        tab_id = msg.tab_id,
        panes = removed.len(),
        "tab closed"
    );
    match CloseTabResponse.to_frame(frame.serial) {
        Ok(f) => RequestResult::Response(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to create CloseTabResponse frame");
            make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InternalError,
                "CloseTabResponse frame error",
            )
        }
    }
}

pub(super) async fn switch_tab(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = SwitchTab::decode(&frame.payload) else {
        warn!(conn_id, "malformed SwitchTab payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed SwitchTab",
        );
    };
    if !panes.lock().await.switch_tab(msg.tab_id) {
        warn!(conn_id, tab_id = msg.tab_id, "switch to unknown tab");
        return make_error_response(conn_id, frame.serial, ErrorCode::UnknownTab, "unknown tab");
    }
    debug!(conn_id, tab_id = msg.tab_id, "active tab switched");
    // SwitchTab is a push message (serial 0) per Spec-0001.
    RequestResult::NoResponse
}

pub(super) async fn rename_tab(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = RenameTab::decode(&frame.payload) else {
        warn!(conn_id, "malformed RenameTab payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed RenameTab",
        );
    };
    if !panes.lock().await.rename_tab(msg.tab_id, msg.name.clone()) {
        warn!(conn_id, tab_id = msg.tab_id, "rename of unknown tab");
        return make_error_response(conn_id, frame.serial, ErrorCode::UnknownTab, "unknown tab");
    }
    debug!(conn_id, tab_id = msg.tab_id, name = %msg.name, "tab renamed");
    // RenameTab is a push message (serial 0) per Spec-0001.
    RequestResult::NoResponse
}

pub(super) async fn move_tab(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = MoveTab::decode(&frame.payload) else {
        warn!(conn_id, "malformed MoveTab payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed MoveTab",
        );
    };
    let new_index = msg.new_index as usize;
    if !panes.lock().await.move_tab(msg.tab_id, new_index) {
        warn!(conn_id, tab_id = msg.tab_id, "move of unknown tab");
        return make_error_response(conn_id, frame.serial, ErrorCode::UnknownTab, "unknown tab");
    }
    debug!(conn_id, tab_id = msg.tab_id, new_index, "tab moved");
    // MoveTab is a push message (serial 0) per Spec-0001.
    RequestResult::NoResponse
}

pub(super) async fn list_tabs(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let snapshot = panes.lock().await.tab_list_snapshot();
    // The transient empty state (no workspace yet) is a valid, empty list.
    let Some(snapshot) = snapshot else {
        let resp = TabList {
            workspace_id: 0,
            workspace_name: String::new(),
            active_tab: 0,
            tabs: vec![],
        };
        return tab_list_response(conn_id, frame.serial, &resp);
    };
    let mut tabs = Vec::with_capacity(snapshot.tabs.len());
    for entry in snapshot.tabs {
        let name = if entry.name.is_empty() {
            // Spec-0007: an unnamed tab shows its focused pane's title.
            if let Some(pane) = entry.pane {
                let pane = pane.lock().await;
                pane.screens.active_grid().title.clone().unwrap_or_default()
            } else {
                error!(
                    conn_id,
                    tab_id = entry.tab_id,
                    pane_id = entry.focused_pane,
                    "tab's focused pane missing from the pane map; mux out of sync"
                );
                debug_assert!(false, "tab's focused pane must be in the pane map");
                String::new()
            }
        } else {
            entry.name
        };
        tabs.push(TabEntry {
            tab_id: entry.tab_id,
            focused_pane: entry.focused_pane,
            name,
        });
    }
    let resp = TabList {
        workspace_id: snapshot.workspace_id,
        workspace_name: snapshot.workspace_name,
        active_tab: snapshot.active_tab,
        tabs,
    };
    tab_list_response(conn_id, frame.serial, &resp)
}

fn tab_list_response(conn_id: u64, serial: u32, resp: &TabList) -> RequestResult {
    match resp.to_frame(serial) {
        Ok(f) => RequestResult::Response(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to encode TabList");
            make_error_response(
                conn_id,
                serial,
                ErrorCode::InternalError,
                "TabList encode error",
            )
        }
    }
}
