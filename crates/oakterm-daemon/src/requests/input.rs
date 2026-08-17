//! Input family: `KeyInput`, `MouseInput`, `Resize` (0x64-0x66).

use super::{RequestResult, make_error_response};
use crate::pane::{PaneManager, PaneState, PtyState, WriterFd, build_command_spec, lock_live_pane};
use crate::pty_io::pty_read_loop;
use oakterm_protocol::frame::Frame;
use oakterm_protocol::input::{KeyInput, MouseInput, Resize};
use oakterm_protocol::message::ErrorCode;
use oakterm_terminal::grid::{MAX_GRID_DIMENSION, ScreenId};
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
    match &pane.pty_state {
        PtyState::Running { fd, .. } => {
            if !msg.key_data.is_empty() {
                if let Err(e) = rustix::io::write(fd, &msg.key_data) {
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
    if let PtyState::Running { fd, .. } = &pane.pty_state {
        let g = pane.screens.active_grid();
        let sgr = g.modes.get(1006);
        let click = g.modes.get(1000);
        let cell_motion = g.modes.get(1002);
        let all_motion = g.modes.get(1003);
        let alt_scroll = g.modes.get(1007);
        let decckm = g.modes.get(1);
        let on_alt = pane.screens.active_screen() == ScreenId::Alternate;

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
                if let Err(e) = rustix::io::write(fd, seq.as_bytes()) {
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
            for _ in 0..ALT_SCROLL_LINES {
                if let Err(e) = rustix::io::write(fd, arrow) {
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
    // Reject oversized dimensions before touching the grid or the PTY: a huge
    // Resize would otherwise drive a multi-terabyte grid allocation and OOM the
    // shared daemon. Rejecting (rather than clamping here) keeps the grid and
    // the PTY window size consistent — neither is changed on a bad request.
    if msg.cols > MAX_GRID_DIMENSION || msg.rows > MAX_GRID_DIMENSION {
        warn!(
            conn_id,
            cols = msg.cols,
            rows = msg.rows,
            max = MAX_GRID_DIMENSION,
            "Resize rejected: dimensions exceed the maximum"
        );
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::MalformedPayload,
            "resize dimensions exceed the maximum",
        );
    }
    let Some(mut pane) = lock_live_pane(panes, msg.pane_id).await else {
        return make_error_response(
            conn_id,
            frame.serial,
            ErrorCode::UnknownPane,
            "unknown pane",
        );
    };
    match &pane.pty_state {
        PtyState::NotSpawned => {
            return spawn_pty(conn_id, frame.serial, &msg, pane, panes, dirty_tx);
        }
        PtyState::Running { fd, .. } => {
            if let Err(e) =
                oakterm_pty::resize_fd(fd, msg.cols, msg.rows, msg.pixel_width, msg.pixel_height)
            {
                warn!(conn_id, error = %e, "PTY resize failed");
            } else {
                let before = pane.history_len();
                pane.screens.resize_all(msg.cols, msg.rows);
                let dropped = pane.invalidate_pins_after_resize(before);
                if dropped > 0 {
                    warn!(
                        conn_id,
                        pane_id = msg.pane_id,
                        pins = dropped,
                        "resize moved grid rows into scrollback; dropped copy mode pins"
                    );
                }
                pane.bump_dirty();
                // Notify clients so they fetch the resized grid immediately,
                // without waiting for the child process to produce output.
                let _ = dirty_tx.send(u64::from(msg.pane_id));
            }
        }
        PtyState::Failed(reason) => {
            warn!(conn_id, reason, "Resize ignored: PTY previously failed");
            return make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InternalError,
                &format!("PTY failed: {reason}"),
            );
        }
        PtyState::Exited { exit_code } => {
            debug!(
                conn_id,
                exit_code = *exit_code,
                "Resize ignored: PTY exited"
            );
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
            let write_fd = match WriterFd::new(&pty) {
                Ok(fd) => fd,
                Err(e) => {
                    // Dropping `pty` kills and reaps the child via `Pty::Drop`.
                    error!(conn_id, error = %e, "failed to prepare PTY master for writes");
                    pane.pty_state = PtyState::Failed(e.to_string());
                    return make_error_response(
                        conn_id,
                        serial,
                        ErrorCode::InternalError,
                        &format!("PTY write setup failed: {e}"),
                    );
                }
            };
            let pid = pty.child_pid();
            let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
            pane.pty_state = PtyState::Running {
                fd: write_fd,
                pid,
                cancel: cancel_tx,
            };
            let before = pane.history_len();
            pane.screens.resize_all(msg.cols, msg.rows);
            let dropped = pane.invalidate_pins_after_resize(before);
            if dropped > 0 {
                warn!(
                    conn_id,
                    pane_id = msg.pane_id,
                    pins = dropped,
                    "spawn resize moved grid rows into scrollback; dropped copy mode pins"
                );
            }
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
        0 => msg.button,                    // press
        1 => msg.button,                    // release
        2 => msg.button.saturating_add(32), // motion (add 32) — client-controlled byte
        3 => 64,                            // scroll up
        4 => 65,                            // scroll down
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
        // Saturating add: a client-controlled coordinate near u16::MAX would
        // otherwise overflow the `+ 32` (panic in debug, wrap in release)
        // before the .min(255) can cap it.
        let cx = (x.saturating_add(32).min(255)) as u8;
        let cy = (y.saturating_add(32).min(255)) as u8;
        let cb = legacy_button.saturating_add(32);
        format!("\x1b[M{}{}{}", cb as char, cx as char, cy as char)
    }
}

#[cfg(test)]
mod tests {
    use super::{encode_mouse_sgr, resize};
    use crate::pane::{PaneManager, lock_live_pane};
    use oakterm_protocol::input::{MouseInput, Resize};
    use std::sync::Arc;
    use tokio::sync::{Mutex, watch};

    /// A client can enter copy mode before the pane's first `Resize`, and
    /// that resize spawns the PTY through a separate path from the
    /// already-running one. It shrinks the grid the same way, so it owes
    /// the same pin invalidation.
    #[tokio::test]
    async fn the_spawning_resize_drops_pins_it_invalidates() {
        let panes = Arc::new(Mutex::new(PaneManager::new()));
        let pane_id = panes
            .lock()
            .await
            .create(80, 24, "/bin/sleep 60".to_string(), String::new());
        {
            let mut pane = lock_live_pane(&panes, pane_id).await.expect("pane");
            assert!(pane.pin_copy_mode(7).is_none());
        }

        let (dirty_tx, _dirty_rx) = watch::channel(0u64);
        let msg = Resize {
            pane_id,
            cols: 80,
            rows: 10,
            pixel_width: 0,
            pixel_height: 0,
        };
        resize(0, &msg.to_frame().expect("resize frame"), &panes, &dirty_tx).await;

        let pane = lock_live_pane(&panes, pane_id).await.expect("pane");
        assert!(pane.history_len() > 0, "shrink moved rows into scrollback");
        assert!(
            pane.copy_mode_pins.is_empty(),
            "the spawning resize must drop pins it invalidated"
        );
    }

    #[test]
    fn x10_mouse_encode_saturates_extreme_coordinates() {
        // Coordinates near u16::MAX must not overflow the X10 `+ 32` offset;
        // the byte saturates at 255 ('\u{ff}') instead.
        let msg = MouseInput {
            pane_id: 1,
            x: u16::MAX,
            y: u16::MAX,
            button: 0,
            event_type: 0,
            modifiers: 0,
        };
        let seq = encode_mouse_sgr(&msg, false);
        assert_eq!(
            seq,
            format!("\x1b[M{}{}{}", 32u8 as char, 255u8 as char, 255u8 as char)
        );
    }

    #[test]
    fn sgr_motion_button_saturates_extreme_button() {
        // event_type 2 (motion) adds 32 to a client-controlled button byte;
        // an extreme value must saturate rather than overflow u8.
        let msg = MouseInput {
            pane_id: 1,
            x: 0,
            y: 0,
            button: u8::MAX,
            event_type: 2,
            modifiers: 0,
        };
        // No panic; SGR form encodes the saturated button (255 | modifiers=0).
        let seq = encode_mouse_sgr(&msg, true);
        assert_eq!(seq, "\x1b[<255;1;1M");
    }

    #[test]
    fn x10_mouse_encode_offsets_normal_coordinates() {
        let msg = MouseInput {
            pane_id: 1,
            x: 9,
            y: 4,
            button: 0,
            event_type: 0,
            modifiers: 0,
        };
        // 1-based, then +32: x=(9+1)+32=42, y=(4+1)+32=37.
        let seq = encode_mouse_sgr(&msg, false);
        assert_eq!(
            seq,
            format!("\x1b[M{}{}{}", 32u8 as char, 42u8 as char, 37u8 as char)
        );
    }
}
