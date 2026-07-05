//! PTY read loop: feed child output to the VT parser and record exits.

use crate::pane::{PaneManager, PtyState, lock_live_pane};
use std::io;
use std::sync::Arc;
use tokio::sync::{Mutex, watch};
use tracing::{debug, error, info, warn};

/// Read PTY output, feed to VT parser, update the pane's screen buffer.
///
/// Exits when any of: the PTY hits EOF or a fatal read error; the pane is
/// removed and a subsequent read detects it; or `cancel_rx` fires (typically
/// from `ClosePane`). On exit, dropping `pty` runs `Pty::Drop`, which kills
/// and reaps the child. `cancel_rx` is the prompt-shutdown path for idle
/// shells that would otherwise leave the loop blocked on `readable()`.
pub(crate) async fn pty_read_loop(
    mut pty: oakterm_pty::Pty,
    panes: Arc<Mutex<PaneManager>>,
    pane_id: u32,
    dirty_tx: watch::Sender<u64>,
    mut cancel_rx: tokio::sync::oneshot::Receiver<()>,
) {
    use tokio::io::unix::AsyncFd;

    let raw_fd = pty.master_raw_fd();
    let pid = pty.child_pid();

    // Set non-blocking for tokio `AsyncFd`.
    let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(raw_fd) };
    match rustix::fs::fcntl_getfl(borrowed) {
        Ok(flags) => {
            if let Err(e) = rustix::fs::fcntl_setfl(borrowed, flags | rustix::fs::OFlags::NONBLOCK)
            {
                error!(error = %e, "failed to set PTY non-blocking");
                return;
            }
        }
        Err(e) => {
            error!(error = %e, "failed to get PTY fd flags");
            return;
        }
    }

    let Ok(async_fd) = AsyncFd::new(raw_fd) else {
        error!("failed to create AsyncFd for PTY");
        return;
    };

    debug!(pid, pane_id, "PTY read loop started");
    let mut buf = [0u8; 4096];

    let exit_reason = loop {
        // `biased` so a rapid close-after-spawn always wins over a pending
        // read — avoids one last guaranteed-stale read after cancellation.
        // Both branches are cancellation-safe: `oneshot::Receiver` and
        // `AsyncFd::readable` both document drop-safety.
        let read_ready = async_fd.readable();
        let read_outcome = tokio::select! {
            biased;
            _ = &mut cancel_rx => break "cancelled",
            ready = read_ready => ready,
        };

        let Ok(mut guard) = read_outcome else {
            break "readable poll failed";
        };

        match guard.try_io(|inner| {
            let fd = inner.get_ref();
            let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(*fd) };
            rustix::io::read(borrowed, &mut buf)
                .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
        }) {
            Ok(Ok(0)) => break "EOF",
            Ok(Ok(n)) => {
                // Re-lookup per read so pane removal is still detected;
                // the topology lock is held only for the map lookup.
                let Some(mut pane) = lock_live_pane(&panes, pane_id).await else {
                    warn!(pane_id, "pane removed while PTY read loop active, exiting");
                    break "pane removed";
                };
                let borrowed_wr = unsafe { rustix::fd::BorrowedFd::borrow_raw(raw_fd) };
                let mut pty_writer = FdWriter(borrowed_wr);
                pane.screens.process_bytes(&buf[..n], &mut pty_writer);
                pane.bump_dirty();
                drop(pane);
                // The seqno bump must happen-before this send: a client
                // woken by the watch reads the seqno next, and a stale
                // read would mark-seen and stall until unrelated output.
                let _ = dirty_tx.send(pane_id.into());
            }
            Ok(Err(e)) if e.kind() == io::ErrorKind::WouldBlock => {}
            Ok(Err(e)) => {
                warn!(error = %e, pane_id, "PTY read error");
                break "read error";
            }
            Err(_would_block) => {}
        }
    };

    let exit_code = capture_exit_code(&mut pty, pane_id, exit_reason);
    info!(pid, pane_id, exit_reason, ?exit_code, "PTY read loop ended");
    record_pane_exit(&panes, pane_id, exit_reason, exit_code).await;

    // Bump dirty so clients wake and detect the Exited state.
    let _ = dirty_tx.send(u64::MAX);
}

/// Capture the child's exit code based on how the read loop exited.
///
/// `try_wait()` first: a child that already exited (the common case for
/// EOF on a foreground command) reports its status without blocking. If
/// the child is still alive — e.g. a daemonized subprocess that closed
/// its stdio but kept running, or an I/O error while the child is
/// mid-write — send SIGKILL and then `wait()`. The kill bounds the wait
/// to the kernel's reap latency, preventing the read task from hanging
/// indefinitely. `cancelled` and `pane removed` skip waiting entirely;
/// the caller that signalled them owns teardown via `Pty::Drop`.
fn capture_exit_code(pty: &mut oakterm_pty::Pty, pane_id: u32, exit_reason: &str) -> Option<i32> {
    match exit_reason {
        "EOF" | "read error" | "readable poll failed" => {
            if let Some(code) = child_try_exit_code(pty, pane_id) {
                return Some(code);
            }
            pty.kill();
            child_exit_code(pty, pane_id)
        }
        _ => None,
    }
}

async fn record_pane_exit(
    panes: &Arc<Mutex<PaneManager>>,
    pane_id: u32,
    exit_reason: &str,
    exit_code: Option<i32>,
) {
    let pane = lock_live_pane(panes, pane_id).await;
    match (pane, exit_code) {
        (Some(mut pane), Some(code)) => {
            pane.pty_state = PtyState::Exited { exit_code: code };
        }
        (Some(mut pane), None) => {
            // Reader is gone but we never got a child status. Synthesize an
            // exit so clients aren't stuck waiting on PaneExited; -1 is
            // outside the 0-255 / 128+signal ranges produced by
            // exit_status_code, so it's distinguishable in logs.
            warn!(
                pane_id,
                exit_reason, "PTY read loop ended without child exit status"
            );
            pane.pty_state = PtyState::Exited { exit_code: -1 };
        }
        (None, _) if exit_reason == "cancelled" || exit_reason == "pane removed" => {
            // Expected on ClosePane and on internal pane removal: the
            // handler removes the pane from PaneManager *before* signalling
            // cancel, so by the time we reach this cleanup, the lookup is
            // None.
            debug!(
                pane_id,
                exit_reason, "PTY read loop ended; pane removed or closed"
            );
        }
        (None, Some(code)) => {
            warn!(
                pane_id,
                exit_code = code,
                exit_reason,
                "PTY exited but pane removed or closed"
            );
        }
        (None, None) => {
            warn!(
                pane_id,
                exit_reason, "PTY read loop ended; pane removed or closed, no exit status"
            );
        }
    }
}

fn child_exit_code(pty: &mut oakterm_pty::Pty, pane_id: u32) -> Option<i32> {
    match pty.wait() {
        Ok(status) => Some(exit_status_code(status)),
        Err(e) => {
            warn!(pane_id, error = %e, "failed to wait for PTY child exit status");
            None
        }
    }
}

fn child_try_exit_code(pty: &mut oakterm_pty::Pty, pane_id: u32) -> Option<i32> {
    match pty.try_wait() {
        Ok(Some(status)) => Some(exit_status_code(status)),
        Ok(None) => None,
        Err(e) => {
            warn!(pane_id, error = %e, "failed to poll PTY child exit status");
            None
        }
    }
}

fn exit_status_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;

    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(1)
}

/// Thin Write adapter for a borrowed file descriptor.
/// Retries on `WouldBlock` since the PTY fd is non-blocking for async reads.
struct FdWriter<'a>(rustix::fd::BorrowedFd<'a>);

impl std::io::Write for FdWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            match rustix::io::write(self.0, buf) {
                Ok(n) => return Ok(n),
                Err(e) if e == rustix::io::Errno::AGAIN => {
                    std::thread::yield_now();
                }
                Err(e) => return Err(io::Error::from_raw_os_error(e.raw_os_error())),
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
