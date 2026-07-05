//! Input family: `KeyInput`, `MouseInput`, `Resize` (0x64-0x66).

use super::{RequestResult, make_error_response};
use crate::pane::{PaneManager, PaneState, PtyState, build_command_spec, lock_live_pane};
use crate::pty_io::pty_read_loop;
use oakterm_protocol::frame::Frame;
use oakterm_protocol::input::{KeyInput, MouseInput, Resize};
use oakterm_protocol::message::ErrorCode;
use oakterm_terminal::grid::ScreenId;
use std::sync::Arc;
use tokio::sync::{Mutex, watch};
use tracing::{debug, error, info, warn};

/// Arrow key repeats per wheel tick for mode 1007 alt-screen scroll.
const ALT_SCROLL_LINES: usize = 3;

pub(super) async fn key_input(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = KeyInput::decode(&frame.payload) else {
        warn!(conn_id, "malformed KeyInput payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed KeyInput",
        );
    };
    let Some(pane) = lock_live_pane(panes, msg.pane_id).await else {
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownPane,
            "unknown pane",
        );
    };
    match pane.pty_state {
        PtyState::Running { fd, .. } => {
            drop(pane);
            if !msg.key_data.is_empty() {
                let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(fd) };
                if let Err(e) = rustix::io::write(borrowed, &msg.key_data) {
                    warn!(conn_id, error = %e, "PTY write failed");
                }
            }
        }
        PtyState::Exited { .. } | PtyState::Failed(_) => {
            debug!(conn_id, "KeyInput ignored: PTY not running");
        }
        PtyState::NotSpawned => {
            debug!(conn_id, "KeyInput ignored: PTY not spawned");
        }
    }
    RequestResult::NoResponse
}

pub(super) async fn mouse_input(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
) -> RequestResult {
    let Ok(msg) = MouseInput::decode(&frame.payload) else {
        warn!(conn_id, "malformed MouseInput payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed MouseInput",
        );
    };
    let Some(pane) = lock_live_pane(panes, msg.pane_id).await else {
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownPane,
            "unknown pane",
        );
    };
    if let PtyState::Running { fd, .. } = pane.pty_state {
        let g = pane.screens.active_grid();
        let sgr = g.modes.get(1006);
        let click = g.modes.get(1000);
        let cell_motion = g.modes.get(1002);
        let all_motion = g.modes.get(1003);
        let alt_scroll = g.modes.get(1007);
        let decckm = g.modes.get(1);
        let on_alt = pane.screens.active_screen() == ScreenId::Alternate;
        drop(pane);

        let mouse_reporting = click || cell_motion || all_motion;
        let shift_held = msg.modifiers & 4 != 0;
        let should_send = if shift_held {
            false
        } else {
            match msg.event_type {
                0 | 1 | 3 | 4 => mouse_reporting,
                2 => cell_motion || all_motion,
                _ => false,
            }
        };

        if should_send {
            let seq = encode_mouse_sgr(&msg, sgr);
            if !seq.is_empty() {
                let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(fd) };
                if let Err(e) = rustix::io::write(borrowed, seq.as_bytes()) {
                    warn!(conn_id, error = %e, "PTY mouse write failed");
                }
            }
        } else if (msg.event_type == 3 || msg.event_type == 4) && on_alt && alt_scroll {
            let arrow: &[u8] = match (msg.event_type, decckm) {
                (3, true) => b"\x1bOA",
                (3, false) => b"\x1b[A",
                (4, true) => b"\x1bOB",
                (_, _) => b"\x1b[B",
            };
            let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(fd) };
            for _ in 0..ALT_SCROLL_LINES {
                if let Err(e) = rustix::io::write(borrowed, arrow) {
                    warn!(conn_id, error = %e, "PTY alt-scroll write failed");
                    break;
                }
            }
        }
    }
    RequestResult::NoResponse
}

pub(super) async fn resize(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
    dirty_tx: &watch::Sender<u64>,
) -> RequestResult {
    let Ok(msg) = Resize::decode(&frame.payload) else {
        warn!(conn_id, "malformed Resize payload");
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "malformed Resize",
        );
    };
    let Some(mut pane) = lock_live_pane(panes, msg.pane_id).await else {
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownPane,
            "unknown pane",
        );
    };
    match pane.pty_state {
        PtyState::NotSpawned => {
            return spawn_pty(conn_id, frame.serial, &msg, pane, panes, dirty_tx);
        }
        PtyState::Running { fd, .. } => {
            let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(fd) };
            if let Err(e) = oakterm_pty::resize_fd(
                borrowed,
                msg.cols,
                msg.rows,
                msg.pixel_width,
                msg.pixel_height,
            ) {
                warn!(conn_id, error = %e, "PTY resize failed");
            } else {
                pane.screens.resize_all(msg.cols, msg.rows);
                pane.bump_dirty();
                // Notify clients so they fetch the resized grid immediately,
                // without waiting for the child process to produce output.
                let _ = dirty_tx.send(u64::from(msg.pane_id));
            }
        }
        PtyState::Failed(ref reason) => {
            warn!(conn_id, reason, "Resize ignored: PTY previously failed");
            return make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InternalError,
                &format!("PTY failed: {reason}"),
            );
        }
        PtyState::Exited { exit_code } => {
            debug!(conn_id, exit_code, "Resize ignored: PTY exited");
            return make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::PaneExited,
                "PTY has exited",
            );
        }
    }
    RequestResult::NoResponse
}

/// Spawn blocks briefly (~1-5ms for fork/exec) while holding this pane's
/// lock; other panes are unaffected.
fn spawn_pty(
    conn_id: u64,
    serial: u32,
    msg: &Resize,
    mut pane: tokio::sync::OwnedMutexGuard<PaneState>,
    panes: &Arc<Mutex<PaneManager>>,
    dirty_tx: &watch::Sender<u64>,
) -> RequestResult {
    let spec = match build_command_spec(&pane.command, &pane.cwd) {
        Ok(s) => s,
        Err(reason) => {
            error!(
                conn_id,
                pane_id = msg.pane_id,
                error = %reason,
                "malformed pane command"
            );
            let response_msg = format!("malformed command: {reason}");
            pane.pty_state = PtyState::Failed(reason);
            return make_error_response(
                conn_id,
                serial,
                ErrorCode::MalformedPayload,
                &response_msg,
            );
        }
    };
    info!(
        conn_id,
        pane_id = msg.pane_id,
        cols = msg.cols,
        rows = msg.rows,
        program = ?spec.program,
        args = ?spec.args,
        cwd = ?spec.cwd,
        "spawning PTY"
    );
    match oakterm_pty::spawn_command(
        spec,
        oakterm_pty::WinSize {
            cols: msg.cols,
            rows: msg.rows,
        },
    ) {
        Ok(pty) => {
            let fd = pty.master_raw_fd();
            let pid = pty.child_pid();
            let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
            pane.pty_state = PtyState::Running {
                fd,
                pid,
                cancel: cancel_tx,
            };
            pane.screens.resize_all(msg.cols, msg.rows);
            let pane_id = msg.pane_id;
            drop(pane);

            info!(pid, pane_id, "PTY spawned");

            let panes_clone = Arc::clone(panes);
            let dtx = dirty_tx.clone();
            tokio::spawn(pty_read_loop(pty, panes_clone, pane_id, dtx, cancel_rx));
            RequestResult::NoResponse
        }
        Err(e) => {
            error!(conn_id, error = %e, "failed to spawn PTY");
            pane.pty_state = PtyState::Failed(e.to_string());
            make_error_response(
                conn_id,
                serial,
                ErrorCode::InternalError,
                &format!("PTY spawn failed: {e}"),
            )
        }
    }
}

/// Encode a mouse event as an SGR escape sequence.
#[allow(clippy::match_same_arms)] // press/release intentionally share button encoding
fn encode_mouse_sgr(msg: &MouseInput, sgr: bool) -> String {
    // SGR button encoding: 0=left, 1=middle, 2=right, 64+=scroll
    let button = match msg.event_type {
        0 => msg.button,      // press
        1 => msg.button,      // release
        2 => 32 + msg.button, // motion (add 32)
        3 => 64,              // scroll up
        4 => 65,              // scroll down
        _ => return String::new(),
    };
    // Encode modifier bits: shift=4, alt=8, ctrl=16.
    let button = button | (msg.modifiers & 0x1C);
    // 1-based coordinates.
    let x = msg.x.saturating_add(1);
    let y = msg.y.saturating_add(1);

    if sgr {
        // SGR format: CSI < button ; x ; y M/m
        let suffix = if msg.event_type == 1 { 'm' } else { 'M' };
        format!("\x1b[<{button};{x};{y}{suffix}")
    } else {
        // Legacy X10 format (limited to 223 cols/rows).
        // Release is signaled by button=3 (no M/m distinction in X10).
        let legacy_button = if msg.event_type == 1 { 3 } else { button };
        let cx = ((x + 32).min(255)) as u8;
        let cy = ((y + 32).min(255)) as u8;
        let cb = legacy_button.saturating_add(32);
        format!("\x1b[M{}{}{}", cb as char, cx as char, cy as char)
    }
}
