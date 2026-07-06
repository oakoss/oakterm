//! Client-requested shutdown (Spec-0001 0x07): the dispatch/save barrier
//! and the save-then-signal protocol, split out from the connection loop
//! so the gate races are unit-testable without a socket.

use crate::pane::PaneManager;
use crate::session::save_session;
use oakterm_protocol::frame::Frame;
use oakterm_protocol::message::{
    ErrorCode, ErrorMessage, RequestShutdown, ShutdownAck, ShutdownAckStatus, ShutdownReason,
};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, RwLockWriteGuard, watch};
use tracing::{error, info, warn};

/// Serializes request dispatch against a shutdown save and carries the
/// daemon-termination signal.
///
/// Invariants (hardened over TREK-187's review cycles):
/// - Request handlers hold a dispatch permit (read) for the dispatch only,
///   never the response write, so a stalled reader cannot block a shutdown
///   save.
/// - A shutdown save holds the barrier (write), draining in-flight
///   mutations so none can be acknowledged after the snapshot and then lost
///   on exit. The write side also serializes concurrent `RequestShutdown`s.
/// - The termination signal is sent while the barrier is still held, so a
///   permit acquired afterward always observes it and drops its frame.
/// - Aborting a save (dropping the guard without `commit`) releases the
///   barrier without signalling, so normal dispatch resumes.
#[derive(Clone)]
pub(crate) struct ShutdownGate {
    term_tx: watch::Sender<Option<ShutdownReason>>,
    gate: Arc<RwLock<()>>,
}

/// Held for the duration of one request dispatch; drop before the response
/// write.
pub(crate) struct DispatchPermit<'a> {
    _guard: tokio::sync::RwLockReadGuard<'a, ()>,
}

/// Outcome of `ShutdownGate::try_begin_save`.
pub(crate) enum SaveAttempt<'a> {
    /// This caller won the barrier: perform the save, then `commit` on
    /// success or drop to abort.
    Proceed(SaveGuard<'a>),
    /// A concurrent request already saved and signalled termination;
    /// acknowledge idempotently without saving again.
    AlreadyInProgress,
}

/// Barrier guard for an in-progress shutdown save. Holds the write side so
/// concurrent `RequestShutdown`s serialize and in-flight dispatch drains.
pub(crate) struct SaveGuard<'a> {
    gate: &'a ShutdownGate,
    _barrier: RwLockWriteGuard<'a, ()>,
}

impl SaveGuard<'_> {
    /// Signal daemon termination with `reason`, then release the barrier.
    /// The signal is sent before the barrier drops so any reader waiting on
    /// a dispatch permit sees the terminating state and drops its frame.
    pub(crate) fn commit(self, reason: ShutdownReason) {
        // `run` holds a receiver for the daemon's lifetime; a send with zero
        // receivers means the client was acked but the daemon cannot exit.
        if self.gate.term_tx.send(Some(reason)).is_err() {
            error!("termination signal had no receivers; daemon will not exit");
        }
    }
}

impl ShutdownGate {
    pub(crate) fn new(term_tx: watch::Sender<Option<ShutdownReason>>) -> Self {
        Self {
            term_tx,
            gate: Arc::new(RwLock::new(())),
        }
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<Option<ShutdownReason>> {
        self.term_tx.subscribe()
    }

    /// Acquire a permit to dispatch one request. Returns `None` if a
    /// shutdown has been accepted — the caller drops the frame, since a
    /// mutation dispatched now would be acknowledged and then lost on exit.
    /// The permit covers dispatch only; drop it before the response write.
    pub(crate) async fn dispatch_permit(&self) -> Option<DispatchPermit<'_>> {
        let guard = self.gate.read().await;
        if self.term_tx.borrow().is_some() {
            None
        } else {
            Some(DispatchPermit { _guard: guard })
        }
    }

    /// Begin a shutdown save. Acquiring the write barrier drains in-flight
    /// dispatch and serializes against concurrent attempts. Returns
    /// `AlreadyInProgress` if a concurrent request already signalled
    /// termination.
    pub(crate) async fn try_begin_save(&self) -> SaveAttempt<'_> {
        let barrier = self.gate.write().await;
        if self.term_tx.borrow().is_some() {
            SaveAttempt::AlreadyInProgress
        } else {
            SaveAttempt::Proceed(SaveGuard {
                gate: self,
                _barrier: barrier,
            })
        }
    }
}

/// Per-connection handle for client-requested shutdown: the shared gate
/// plus the session state directory carried to the save.
#[derive(Clone)]
pub(crate) struct ShutdownCtx {
    pub(crate) gate: ShutdownGate,
    pub(crate) state_dir: Arc<std::path::PathBuf>,
}

/// Result of handling a `RequestShutdown` frame.
pub(crate) enum ShutdownOutcome {
    /// Shutdown accepted (or idempotently acknowledged): write `ack`, then
    /// stop reading this connection's pipelined frames — they would mutate
    /// state the saved session no longer reflects.
    Accepted { ack: Option<Frame> },
    /// Shutdown declined (malformed request or failed save): write
    /// `response` and keep dispatching.
    Declined { response: Option<Frame> },
}

/// Save-then-signal for `RequestShutdown` (Spec-0001 0x07). On accept the
/// termination signal is sent before the barrier releases, so the
/// requester's `Shutdown` push follows the ack (writes on one connection
/// are sequential). A failed save aborts the shutdown regardless of the
/// requested reason (ADR-0020): the daemon replies `save_failed` and keeps
/// running. The daemon never exits ack-less — an ack that fails to encode
/// also suppresses the signal.
pub(crate) async fn request_shutdown(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
    shutdown: &ShutdownCtx,
) -> ShutdownOutcome {
    let msg = match RequestShutdown::decode(&frame.payload) {
        Ok(m) => m,
        Err(e) => {
            // Spec-0001 error case: unknown reason or malformed payload —
            // do not shut down.
            warn!(conn_id, error = %e, "malformed RequestShutdown payload");
            let err = ErrorMessage {
                code: ErrorCode::MalformedPayload as u32,
                message: "malformed RequestShutdown".to_string(),
            };
            let response = match err.to_frame(frame.serial) {
                Ok(f) => Some(f),
                Err(e) => {
                    error!(conn_id, error = %e, "failed to encode error response");
                    None
                }
            };
            return ShutdownOutcome::Declined { response };
        }
    };

    let guard = match shutdown.gate.try_begin_save().await {
        SaveAttempt::AlreadyInProgress => {
            info!(conn_id, "shutdown already in progress, acknowledging");
            let ack = encode_ack(conn_id, frame.serial, ShutdownAckStatus::Accepted);
            return ShutdownOutcome::Accepted { ack };
        }
        SaveAttempt::Proceed(guard) => guard,
    };

    let status = match save_session(panes, &shutdown.state_dir).await {
        Ok(path) => {
            info!(
                conn_id,
                reason = ?msg.reason,
                path = %path.display(),
                "shutdown requested, session saved"
            );
            ShutdownAckStatus::Accepted
        }
        Err(e) => {
            error!(conn_id, error = %e, "session save failed, aborting shutdown");
            ShutdownAckStatus::SaveFailed
        }
    };
    let ack = encode_ack(conn_id, frame.serial, status);
    if status == ShutdownAckStatus::Accepted && ack.is_some() {
        guard.commit(msg.reason.broadcast_reason());
        ShutdownOutcome::Accepted { ack }
    } else {
        // Aborted: dropping `guard` releases the barrier so dispatch resumes.
        ShutdownOutcome::Declined { response: ack }
    }
}

fn encode_ack(conn_id: u64, serial: u32, status: ShutdownAckStatus) -> Option<Frame> {
    match (ShutdownAck { status }).to_frame(serial) {
        Ok(f) => Some(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to encode ShutdownAck");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns the gate plus a live receiver the caller must hold: a
    /// `watch` send is a no-op with zero receivers, and production always
    /// keeps one (`run`'s `term_rx`).
    fn gate() -> (ShutdownGate, watch::Receiver<Option<ShutdownReason>>) {
        let (term_tx, rx) = watch::channel(None);
        (ShutdownGate::new(term_tx), rx)
    }

    /// A save that commits flips the gate to terminating: a later attempt
    /// must not save again but acknowledge idempotently.
    #[tokio::test]
    async fn second_save_after_commit_is_idempotent() {
        let (gate, _rx) = gate();
        match gate.try_begin_save().await {
            SaveAttempt::Proceed(guard) => guard.commit(ShutdownReason::Clean),
            SaveAttempt::AlreadyInProgress => panic!("first attempt must proceed"),
        }
        assert!(matches!(
            gate.try_begin_save().await,
            SaveAttempt::AlreadyInProgress
        ));
    }

    /// Two concurrent attempts serialize under the write barrier: the
    /// second cannot proceed until the first releases, and once the first
    /// commits the second observes the terminating state.
    #[tokio::test]
    async fn concurrent_saves_serialize_second_sees_in_progress() {
        let (gate, _rx) = gate();
        let SaveAttempt::Proceed(guard) = gate.try_begin_save().await else {
            panic!("first attempt must proceed");
        };

        let other = gate.clone();
        let second = tokio::spawn(async move {
            matches!(other.try_begin_save().await, SaveAttempt::AlreadyInProgress)
        });

        // The barrier is held, so the spawned attempt cannot make progress
        // past `write().await`.
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(
            !second.is_finished(),
            "second attempt must block on the barrier"
        );

        guard.commit(ShutdownReason::Clean);
        assert!(
            second.await.unwrap(),
            "second attempt must see AlreadyInProgress"
        );
    }

    /// Once termination is signalled, a dispatch permit is denied even
    /// though the barrier is free — the re-check, not the lock, drops the
    /// frame.
    #[tokio::test]
    async fn dispatch_permit_denied_after_commit() {
        let (gate, _rx) = gate();
        assert!(
            gate.dispatch_permit().await.is_some(),
            "permit granted before shutdown"
        );
        match gate.try_begin_save().await {
            SaveAttempt::Proceed(guard) => guard.commit(ShutdownReason::Clean),
            SaveAttempt::AlreadyInProgress => panic!("first attempt must proceed"),
        }
        assert!(gate.dispatch_permit().await.is_none());
    }

    /// The idempotent branch at the `request_shutdown` level: after a
    /// committed shutdown, a second request gets `Accepted` with an
    /// Accepted-status ack and performs no save. Flipping this arm to
    /// `Declined` would let the second client keep dispatching pipelined
    /// frames after the save snapshot — the acked-then-lost-mutation bug.
    #[tokio::test]
    async fn second_request_shutdown_acks_without_saving() {
        use oakterm_protocol::message::RequestShutdownReason;

        let (gate, _rx) = gate();
        match gate.try_begin_save().await {
            SaveAttempt::Proceed(guard) => guard.commit(ShutdownReason::Clean),
            SaveAttempt::AlreadyInProgress => panic!("first attempt must proceed"),
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ShutdownCtx {
            gate,
            state_dir: Arc::new(dir.path().join("state")),
        };
        let panes = Arc::new(Mutex::new(PaneManager::new()));
        let frame = RequestShutdown {
            reason: RequestShutdownReason::Quit,
        }
        .to_frame(7)
        .expect("frame");

        let outcome = request_shutdown(1, &frame, &panes, &ctx).await;
        let ShutdownOutcome::Accepted { ack: Some(ack) } = outcome else {
            panic!("second RequestShutdown must ack idempotently");
        };
        let decoded = ShutdownAck::decode(&ack.payload).expect("ack decodes");
        assert_eq!(decoded.status, ShutdownAckStatus::Accepted);
        assert_eq!(ack.serial, 7);
        assert!(
            !ctx.state_dir.exists(),
            "idempotent ack must not write a session file"
        );
    }

    /// A dispatch permit blocks while a save holds the barrier; aborting the
    /// save (dropping the guard without committing) releases the barrier and
    /// grants the permit, so normal dispatch resumes.
    #[tokio::test]
    async fn aborted_save_releases_barrier_for_dispatch() {
        let (gate, _rx) = gate();
        let SaveAttempt::Proceed(guard) = gate.try_begin_save().await else {
            panic!("first attempt must proceed");
        };

        let other = gate.clone();
        let permit = tokio::spawn(async move { other.dispatch_permit().await.is_some() });

        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        assert!(
            !permit.is_finished(),
            "permit must block behind the save barrier"
        );

        drop(guard);
        assert!(
            permit.await.unwrap(),
            "aborted save must grant the permit (dispatch resumes)"
        );
    }
}
