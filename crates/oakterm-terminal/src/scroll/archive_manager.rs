//! Archive facade: a writer thread that owns the archive state, fed by a
//! bounded mailbox.
//!
//! Writes happen on the writer thread so compression and encryption never
//! run inside the PTY read loop (the 2026-07-02 parity benchmark measured
//! the synchronous path at ~5x ingest cost under sustained scrolling).
//! Reads are query messages answered on the same thread, so they observe
//! every previously enqueued write — the FIFO mailbox is the ordering
//! barrier.

use crate::grid::row::Row;
use crate::scroll::archive_core::{ArchiveCore, ArchiveStats};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Weak};
use std::thread;
use std::time::{Duration, Instant};

/// Mailbox depth in batches. A batch is one prune's worth of rows (~10%
/// of the hot buffer). Batches that find the queue full are dropped
/// (Spec-0004 overload policy), so this bounds memory, not latency.
const MAILBOX_DEPTH: usize = 4;

/// Bound on enqueueing a query and awaiting its reply. A writer that
/// can't respond within this is treated as wedged; callers get an error
/// (or a best-effort default) instead of hanging.
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);

/// How long teardown waits for a writer to notice the disconnect. Short
/// on purpose: the wait only covers a message already being processed,
/// and a writer that needs longer is abandoned rather than held onto.
const JOIN_GRACE: Duration = Duration::from_secs(1);

/// Message to the writer thread. Queries carry a reply channel; the
/// writer answers after processing everything queued before them.
enum WriterMsg {
    /// Rows pruned from the hot buffer. `preceding_gap` counts rows
    /// dropped (queue full) since the last enqueued batch; the core
    /// advances its index base by that amount so archived row indexing
    /// exposes the gap instead of misaligning.
    Batch {
        rows: Vec<Row>,
        preceding_gap: u64,
    },
    Flush(SyncSender<io::Result<()>>),
    Seal(SyncSender<io::Result<()>>),
    Read {
        start: u64,
        count: usize,
        reply: SyncSender<io::Result<Vec<(u64, Row)>>>,
    },
    Stats(SyncSender<ArchiveStats>),
    /// Finalize, delete all archive files, reply, and exit the loop.
    Shutdown(SyncSender<io::Result<()>>),
    /// Block the writer until `gate`'s sender drops — deterministic
    /// saturation for tests. `entered` acks that the writer is parked,
    /// so callers know later batches can't be drained early.
    #[cfg(any(test, feature = "test-hooks"))]
    Stall {
        entered: SyncSender<()>,
        gate: mpsc::Receiver<()>,
    },
    /// Arm a one-shot stall on the *next* read. Unlike `Stall`, this
    /// signals only once a read has actually arrived, giving tests a
    /// rendezvous on the read being in flight rather than a sleep.
    #[cfg(any(test, feature = "test-hooks"))]
    StallNextRead {
        entered: SyncSender<()>,
        gate: mpsc::Receiver<()>,
    },
    /// Kill the writer thread — deterministic dead-writer state for tests.
    #[cfg(test)]
    Panic,
}

/// Manages the cold disk archive for one pane's scrollback.
///
/// `archive_rows` enqueues to the writer thread and returns immediately;
/// every other method is a query answered after the queue drains, so
/// callers observe the same state a synchronous implementation would show.
pub struct ArchiveManager {
    writer: Option<WriterHandle>,
    session_dir: PathBuf,
    // Updated at enqueue time on the caller thread — archive state is
    // writer-thread territory and the enqueue path must not wait on it.
    // `archive_rows` taking `&mut self` is what keeps the queue ordering
    // sound; don't relax these to atomics + `&self`.
    dropped_rows: u64,
    pending_gap: u64,
    received_rows: u64,
}

/// Channel and thread of a running writer; both live and die together.
/// This is the only strong reference to the sender, so dropping it
/// disconnects the mailbox — that disconnect is the writer's exit signal,
/// and it is why [`ArchiveReader`] may only ever hold a `Weak`.
struct WriterHandle {
    tx: Arc<SyncSender<WriterMsg>>,
    thread: thread::JoinHandle<()>,
}

/// A query result whose caller stopped waiting still gets its error
/// logged — the dropped reply channel is exactly the degraded case
/// (ENOSPC at finalize, prune failure) where the detail matters most.
fn log_if_unheard<T>(send_result: Result<(), mpsc::SendError<io::Result<T>>>, op: &str) {
    if let Err(mpsc::SendError(Err(e))) = send_result {
        tracing::warn!(error = %e, op, "archive operation failed after caller stopped waiting");
    }
}

/// Enqueue `msg`, retrying while the mailbox is full of batches, giving up
/// at `deadline`.
///
/// The strong reference is taken per attempt and released before the
/// sleep. Holding it across the wait would keep the mailbox connected
/// while nothing is draining it, which is exactly how a caller here could
/// stall a teardown running on another thread.
fn send_with_deadline(
    tx: &Weak<SyncSender<WriterMsg>>,
    mut msg: WriterMsg,
    deadline: Instant,
) -> io::Result<()> {
    loop {
        let Some(sender) = tx.upgrade() else {
            return Err(io::Error::other("archive writer thread exited"));
        };
        match sender.try_send(msg) {
            Ok(()) => return Ok(()),
            Err(mpsc::TrySendError::Full(returned)) => {
                if Instant::now() >= deadline {
                    return Err(io::Error::other(
                        "archive writer did not accept a query within 10s",
                    ));
                }
                msg = returned;
                drop(sender);
                thread::sleep(Duration::from_millis(10));
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                return Err(io::Error::other("archive writer thread exited"));
            }
        }
    }
}

/// Enqueue a query with a bounded wait on both the send and the reply.
/// Every caller — manager and reader alike — goes through this, so the
/// wedged-writer contract has one definition.
///
/// The reply rides its own channel, so no reference to the mailbox is held
/// while waiting for it: a teardown may disconnect the writer mid-query,
/// and this call then ends on the reply channel closing.
fn query_writer<T>(
    tx: &Weak<SyncSender<WriterMsg>>,
    build: impl FnOnce(SyncSender<T>) -> WriterMsg,
) -> io::Result<T> {
    let deadline = Instant::now() + QUERY_TIMEOUT;
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    send_with_deadline(tx, build(reply_tx), deadline)?;
    match reply_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(value) => Ok(value),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(io::Error::other("archive writer thread exited"))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            Err(io::Error::other("archive writer did not reply within 10s"))
        }
    }
}

/// Read-only handle to a pane's archive writer.
///
/// `Send + 'static` and cloneable, so a read can be moved onto a blocking
/// task rather than run under whatever locks the caller holds. Reads
/// observe the same state the owning [`ArchiveManager`] would: the writer
/// answers them in mailbox order.
///
/// The reference is deliberately weak: teardown stops the writer by
/// dropping the manager's sender, so a reader that could hold it alive
/// would leak the thread. Methods fail fast once the manager is gone.
#[derive(Clone)]
pub struct ArchiveReader {
    tx: Weak<SyncSender<WriterMsg>>,
}

impl ArchiveReader {
    /// Read archived rows in `[start, start + count)`, tagged with their
    /// absolute indices — see [`ArchiveManager::read_range`] for the
    /// gap and alignment contract.
    ///
    /// # Errors
    ///
    /// Returns an error if reading from disk fails or the writer is dead
    /// or wedged.
    pub fn read_range(&self, start: u64, count: usize) -> io::Result<Vec<(u64, Row)>> {
        query_writer(&self.tx, |reply| WriterMsg::Read {
            start,
            count,
            reply,
        })?
    }

    /// Rows the writer itself lost: paused discards and write-error
    /// abandonment. Add [`ArchiveManager::dropped_rows`] for the total —
    /// enqueue-side drops are counted on the caller's thread, not here.
    /// A mailbox round-trip like any query; `None` whenever that query
    /// fails — writer gone, wedged, or unreachable.
    #[must_use]
    pub fn writer_lost_rows(&self) -> Option<u64> {
        query_writer(&self.tx, WriterMsg::Stats)
            .inspect_err(|e| tracing::debug!(error = %e, "archive stats query failed"))
            .ok()
            .map(|s| s.lost_rows)
    }
}

/// A read parked at the writer, from [`ArchiveManager::stall_next_read`].
/// Polling is non-blocking so async tests can await on it without tying
/// up a runtime worker.
#[cfg(any(test, feature = "test-hooks"))]
pub struct ReadStall {
    entered: mpsc::Receiver<()>,
    gate: SyncSender<()>,
    arrived: bool,
}

#[cfg(any(test, feature = "test-hooks"))]
impl ReadStall {
    /// Whether a read has reached the writer and parked. Latches, so it
    /// stays true once observed.
    pub fn read_arrived(&mut self) -> bool {
        self.arrived |= self.entered.try_recv().is_ok();
        self.arrived
    }

    /// Let the parked read proceed.
    pub fn release(self) {
        drop(self.gate);
    }
}

impl ArchiveManager {
    /// Create a new archive manager for the given session directory.
    ///
    /// Creates the directory with 0700 permissions if it doesn't exist,
    /// and spawns the writer thread that owns all archive state.
    ///
    /// # Errors
    ///
    /// Returns an error if key generation, directory creation, or thread
    /// spawning fails.
    pub fn new(session_dir: PathBuf, max_disk_bytes: u64) -> io::Result<Self> {
        let mut core = ArchiveCore::new(session_dir.clone(), max_disk_bytes)?;
        let (tx, rx) = mpsc::sync_channel::<WriterMsg>(MAILBOX_DEPTH);
        #[cfg(any(test, feature = "test-hooks"))]
        let mut stall_next_read: Option<(SyncSender<()>, mpsc::Receiver<()>)> = None;
        let writer_thread = thread::Builder::new()
            .name("archive-writer".to_string())
            .spawn(move || {
                while let Ok(msg) = rx.recv() {
                    match msg {
                        WriterMsg::Batch {
                            rows,
                            preceding_gap,
                        } => {
                            if let Err(e) = core.archive_rows(rows, preceding_gap) {
                                tracing::warn!(error = %e, "failed to archive pruned rows");
                            }
                        }
                        WriterMsg::Flush(reply) => {
                            log_if_unheard(reply.send(core.flush_pending()), "flush");
                        }
                        WriterMsg::Seal(reply) => {
                            log_if_unheard(reply.send(core.seal_active_segment()), "seal");
                        }
                        WriterMsg::Read {
                            start,
                            count,
                            reply,
                        } => {
                            #[cfg(any(test, feature = "test-hooks"))]
                            if let Some((entered, gate)) = stall_next_read.take() {
                                let _ = entered.send(());
                                let _ = gate.recv();
                            }
                            // Seal only when the window reaches unsealed
                            // rows, so reads of pure history don't churn
                            // segments or risk read-triggered loss on a
                            // failing disk. Flush and seal run
                            // independently: a flush failure (its rows
                            // count as lost) must not skip sealing rows
                            // already written.
                            if start.saturating_add(count as u64) > core.finalized_boundary() {
                                if let Err(e) = core.flush_pending() {
                                    tracing::warn!(error = %e, "flush before archive read failed");
                                }
                                if let Err(e) = core.seal_active_segment() {
                                    tracing::warn!(error = %e, "seal before archive read failed");
                                }
                            }
                            log_if_unheard(reply.send(core.read_range(start, count)), "read");
                        }
                        WriterMsg::Stats(reply) => {
                            let _ = reply.send(core.stats());
                        }
                        WriterMsg::Shutdown(reply) => {
                            log_if_unheard(reply.send(core.shutdown()), "shutdown");
                            break;
                        }
                        #[cfg(any(test, feature = "test-hooks"))]
                        WriterMsg::Stall { entered, gate } => {
                            let _ = entered.send(());
                            let _ = gate.recv();
                        }
                        #[cfg(any(test, feature = "test-hooks"))]
                        WriterMsg::StallNextRead { entered, gate } => {
                            stall_next_read = Some((entered, gate));
                        }
                        #[cfg(test)]
                        #[allow(clippy::missing_panics_doc)]
                        WriterMsg::Panic => panic!("test-induced writer death"),
                    }
                }
            })?;
        Ok(Self {
            writer: Some(WriterHandle {
                tx: Arc::new(tx),
                thread: writer_thread,
            }),
            session_dir,
            dropped_rows: 0,
            pending_gap: 0,
            received_rows: 0,
        })
    }

    /// Accept pruned rows from the hot buffer. Enqueues to the writer
    /// thread and returns without touching the disk.
    ///
    /// If the writer is saturated (queue full), the batch is dropped and
    /// counted (`dropped_rows`): ingest never waits on compression. The
    /// hot buffer is unaffected — only cold history beyond it gets a gap,
    /// and the index base advances so the gap stays visible. The read path
    /// must treat archive indexing as non-contiguous when `lost_rows() > 0`
    /// (Spec-0004 overload policy).
    ///
    /// # Errors
    ///
    /// Returns an error if the writer thread has exited.
    pub fn archive_rows(&mut self, rows: Vec<Row>) -> io::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let row_count = rows.len() as u64;
        self.received_rows += row_count;
        match self
            .writer
            .as_ref()
            .ok_or_else(|| io::Error::other("archive writer shut down"))?
            .tx
            .try_send(WriterMsg::Batch {
                rows,
                preceding_gap: self.pending_gap,
            }) {
            Ok(()) => {
                self.pending_gap = 0;
                Ok(())
            }
            Err(mpsc::TrySendError::Full(_)) => {
                self.pending_gap += row_count;
                self.dropped_rows += row_count;
                tracing::debug!(
                    dropped = row_count,
                    total_dropped = self.dropped_rows,
                    "archive writer saturated; dropping batch"
                );
                Ok(())
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                // Lost like a saturation drop — count it so the loss is
                // visible even though the caller also gets the error.
                self.dropped_rows += row_count;
                self.pending_gap += row_count;
                Err(io::Error::other("archive writer thread exited"))
            }
        }
    }

    fn query<T>(&self, build: impl FnOnce(SyncSender<T>) -> WriterMsg) -> io::Result<T> {
        let writer = self
            .writer
            .as_ref()
            .ok_or_else(|| io::Error::other("archive writer shut down"))?;
        // Borrowing `self` keeps the strong reference alive for the call,
        // so the downgrade here can always be upgraded back.
        query_writer(&Arc::downgrade(&writer.tx), build)
    }

    /// A handle that can read this archive without borrowing the manager,
    /// so callers can hand the read to another thread instead of holding
    /// their own locks across it. `None` once the writer is shut down.
    #[must_use]
    pub fn reader(&self) -> Option<ArchiveReader> {
        self.writer.as_ref().map(|w| ArchiveReader {
            tx: Arc::downgrade(&w.tx),
        })
    }

    /// Park the writer on the *next* read it receives, so tests can wait
    /// for a read to be genuinely in flight instead of sleeping and
    /// assuming. Unlike [`Self::stall_writer`], the signal proves the read
    /// reached the writer, which also orders anything enqueued afterwards
    /// behind it.
    ///
    /// # Panics
    ///
    /// Panics if the writer is already gone.
    #[cfg(any(test, feature = "test-hooks"))]
    #[must_use]
    pub fn stall_next_read(&self) -> ReadStall {
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (gate_tx, gate_rx) = mpsc::sync_channel(0);
        self.writer
            .as_ref()
            .expect("writer running")
            .tx
            .send(WriterMsg::StallNextRead {
                entered: entered_tx,
                gate: gate_rx,
            })
            .expect("send stall-next-read");
        ReadStall {
            entered: entered_rx,
            gate: gate_tx,
            arrived: false,
        }
    }

    /// Park the writer until the returned sender drops, so tests can
    /// exercise callers against a writer that will not answer. Returns
    /// once the writer is actually parked — without that rendezvous it
    /// could still drain queued work first.
    ///
    /// # Panics
    ///
    /// Panics if the writer is already gone.
    #[cfg(any(test, feature = "test-hooks"))]
    #[must_use]
    pub fn stall_writer(&self) -> SyncSender<()> {
        let (entered_tx, entered_rx) = mpsc::sync_channel(0);
        let (gate_tx, gate_rx) = mpsc::sync_channel(0);
        self.writer
            .as_ref()
            .expect("writer running")
            .tx
            .send(WriterMsg::Stall {
                entered: entered_tx,
                gate: gate_rx,
            })
            .expect("send stall");
        entered_rx.recv().expect("writer parked in stall");
        gate_tx
    }

    /// Observable state snapshot; `None` when the writer is dead or wedged.
    fn stats(&self) -> Option<ArchiveStats> {
        self.query(WriterMsg::Stats).ok()
    }

    /// Flush all pending rows as a single frame.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails or the writer is dead or wedged.
    pub fn flush_pending(&mut self) -> io::Result<()> {
        self.query(WriterMsg::Flush)?
    }

    /// Finalize the active segment so all written rows become readable.
    /// A new segment is opened on the next write.
    ///
    /// # Errors
    ///
    /// Returns an error if finalization fails or the writer is dead or
    /// wedged.
    pub fn seal_active_segment(&mut self) -> io::Result<()> {
        self.query(WriterMsg::Seal)?
    }

    /// Read archived rows in `[start, start + count)`, tagged with their
    /// absolute indices. The writer seals before reading when the window
    /// requires it, so every row enqueued before this call is visible —
    /// rows whose pre-read flush or seal fails are accounted lost and
    /// read as gaps. Indices inside gaps (dropped or lost rows) produce
    /// no entry — align by the returned indices, never by position
    /// (Spec-0004 overload policy).
    ///
    /// # Errors
    ///
    /// Returns an error if reading from disk fails or the writer is dead
    /// or wedged.
    pub fn read_range(&self, start: u64, count: usize) -> io::Result<Vec<(u64, Row)>> {
        self.reader()
            .ok_or_else(|| io::Error::other("archive writer shut down"))?
            .read_range(start, count)
    }

    /// Every row ever handed to `archive_rows`, whether stored, dropped,
    /// or lost. Combined with the hot buffer length this defines the
    /// absolute scrollback index space: archived history occupies
    /// `[0, total_rows_received)` and the hot buffer follows it.
    ///
    /// Deliberately counts rows lost at enqueue (saturation drops, dead
    /// writer): they were real pruned rows, so their indices must stay
    /// claimed as gaps — reordering the increment after the send would
    /// misalign every later row.
    #[must_use]
    pub fn total_rows_received(&self) -> u64 {
        self.received_rows
    }

    /// Total rows stored across all finalized segments (excludes pending).
    /// Best-effort 0 while the writer is dead or wedged.
    #[must_use]
    pub fn total_archived_rows(&self) -> u64 {
        self.stats().map_or(0, |s| s.total_archived_rows)
    }

    /// Number of finalized segments on disk. Best-effort 0 while the
    /// writer is dead or wedged.
    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.stats().map_or(0, |s| s.segment_count)
    }

    /// Total bytes used by segment files on disk. Best-effort 0 while the
    /// writer is dead or wedged.
    #[must_use]
    pub fn disk_bytes(&self) -> u64 {
        self.stats().map_or(0, |s| s.disk_bytes)
    }

    #[must_use]
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    /// Whether archiving is paused due to low disk space or write errors.
    /// Best-effort `true` while the writer is dead or wedged.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.stats().is_none_or(|s| s.paused)
    }

    /// Rows dropped at enqueue because the writer was saturated (a subset
    /// of `lost_rows`).
    #[must_use]
    pub fn dropped_rows(&self) -> u64 {
        self.dropped_rows
    }

    /// Rows lost on any channel: saturation drops, paused discards, and
    /// write-error abandonment. Archive row indexing is non-contiguous
    /// whenever this is non-zero. Best-effort (drops only) while the
    /// writer is dead or wedged.
    #[must_use]
    pub fn lost_rows(&self) -> u64 {
        self.dropped_rows + self.stats().map_or(0, |s| s.lost_rows)
    }

    /// Drain the queue, finalize the active segment, delete all archive
    /// files, and stop the writer thread. Idempotent: repeat calls after
    /// the writer is gone retry the deletion if a prior attempt failed,
    /// otherwise return `Ok`.
    ///
    /// # Errors
    ///
    /// Returns an error if finalization or deletion fails, or if the
    /// writer is wedged (cleanup then falls to `cleanup_orphans` — though
    /// a queued shutdown may still run and delete the files if the writer
    /// recovers). A dead writer is not an error: its files are removed
    /// directly.
    pub fn shutdown(&mut self) -> io::Result<()> {
        if self.writer.is_none() {
            if self.session_dir.exists() {
                std::fs::remove_dir_all(&self.session_dir)?;
            }
            return Ok(());
        }
        let result = self.query(WriterMsg::Shutdown);
        match result {
            Ok(inner) => {
                self.join_writer();
                inner
            }
            Err(e) => {
                // Dead writer: the thread (and the core it owned) are gone;
                // delete the files directly. Wedged writer: leave both the
                // thread and the files alone — cleanup_orphans reclaims.
                if self.writer_is_dead() {
                    self.join_writer();
                    if self.session_dir.exists() {
                        std::fs::remove_dir_all(&self.session_dir)?;
                    }
                    return Ok(());
                }
                self.detach_writer();
                Err(e)
            }
        }
    }

    /// Whether the writer thread has exited — distinguishes a dead writer
    /// (joinable, its core gone) from a wedged one (alive but stuck) after
    /// a query fails. Shared by `shutdown` and `Drop` so the two never
    /// classify the same state differently.
    fn writer_is_dead(&self) -> bool {
        self.writer.as_ref().is_some_and(|w| w.thread.is_finished())
    }

    /// Disconnect the mailbox and wait up to `grace` for the writer to
    /// notice and exit.
    ///
    /// A writer parked inside a blocking `write` or fsync sees neither the
    /// disconnect nor anything else until that syscall returns, so the
    /// wait is bounded and gives up rather than blocking teardown. An
    /// abandoned thread still exits on its own once it reaches `recv`;
    /// only the join is lost.
    fn stop_writer(&mut self, grace: Duration) {
        let Some(writer) = self.writer.take() else {
            return;
        };
        drop(writer.tx);
        let deadline = Instant::now() + grace;
        while !writer.thread.is_finished() {
            if Instant::now() >= deadline {
                tracing::warn!(
                    path = %self.session_dir.display(),
                    "archive writer did not exit; abandoning it, files left for orphan cleanup"
                );
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        if writer.thread.join().is_err() {
            tracing::error!("archive writer thread panicked");
        }
    }

    /// Stop a writer expected to be responsive or already gone.
    fn join_writer(&mut self) {
        self.stop_writer(JOIN_GRACE);
    }

    /// Abandon a writer already known to be wedged. Waiting is pointless
    /// in exactly the scenario the query timeout exists to survive.
    fn detach_writer(&mut self) {
        self.stop_writer(Duration::ZERO);
    }

    /// Delete orphaned archive directories that don't match the current session.
    ///
    /// Session directories are named `{pid}-{timestamp}`. On Unix, the PID
    /// prefix is checked for liveness before deleting. Directories with
    /// unrecognised names are left alone.
    ///
    /// Continues past individual deletion failures, returning the last error.
    ///
    /// # Errors
    ///
    /// Returns the last error encountered during cleanup, if any.
    pub fn cleanup_orphans(base_dir: &Path, current_session: &str) -> io::Result<()> {
        if !base_dir.exists() {
            return Ok(());
        }
        let mut last_error = None;
        for entry in std::fs::read_dir(base_dir)? {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            if name_str == current_session || !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            #[cfg(unix)]
            if pid_is_alive(name_str) {
                continue;
            }
            if let Err(e) = std::fs::remove_dir_all(entry.path()) {
                last_error = Some(e);
            }
        }
        match last_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl Drop for ArchiveManager {
    fn drop(&mut self) {
        // Stop the thread but keep files: explicit shutdown deletes them;
        // a crash-dropped archive is reclaimed by cleanup_orphans. A
        // responsive writer is joined (it exits when the channel closes);
        // a wedged one is detached rather than hanging teardown.
        if self.stats().is_some() || self.writer_is_dead() {
            self.join_writer();
        } else {
            self.detach_writer();
        }
    }
}

/// Check whether the process encoded in a `{pid}-{timestamp}` dir name is alive.
/// Returns `true` (assume alive) if the name doesn't match the expected format.
#[cfg(unix)]
fn pid_is_alive(dir_name: &str) -> bool {
    let Some(pid_str) = dir_name.split('-').next() else {
        return true;
    };
    let Ok(raw_pid) = pid_str.parse::<i32>() else {
        return true;
    };
    let Some(pid) = rustix::process::Pid::from_raw(raw_pid) else {
        return true;
    };
    rustix::process::test_kill_process(pid).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::cell::{Color, NamedColor};

    const STALL_BATCHES: usize = MAILBOX_DEPTH + 2;

    fn make_rows(count: usize, cols: usize) -> Vec<Row> {
        (0..count)
            .map(|i| {
                let mut r = Row::new(cols);
                #[allow(clippy::cast_possible_truncation)]
                let offset = (i % 26) as u8;
                r.cells[0].codepoint = char::from(b'A' + offset);
                r
            })
            .collect()
    }

    #[test]
    fn large_batch_triggers_flush() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        // ~650 bytes per 80-col row, need ~100 rows for 64 KB
        mgr.archive_rows(make_rows(150, 80)).unwrap();
        assert!(mgr.total_archived_rows() > 0);
    }

    #[test]
    fn explicit_flush() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        mgr.archive_rows(make_rows(5, 80)).unwrap();
        assert_eq!(mgr.total_archived_rows(), 0); // not flushed yet
        mgr.flush_pending().unwrap();
        assert_eq!(mgr.total_archived_rows(), 5);
    }

    #[test]
    fn saturated_writer_drops_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        let sent = 10 * STALL_BATCHES as u64;
        {
            let _gate = mgr.stall_writer();
            for _ in 0..STALL_BATCHES {
                mgr.archive_rows(make_rows(10, 80)).unwrap();
            }
        }
        assert!(
            mgr.dropped_rows() >= 10,
            "expected at least one dropped batch, got {} dropped rows",
            mgr.dropped_rows()
        );
        // The writer resumes once the gate drops; queued batches land.
        mgr.flush_pending().unwrap();
        assert!(mgr.total_archived_rows() > 0);
        // Conservation: every sent row is either stored or counted lost.
        assert_eq!(sent, mgr.total_archived_rows() + mgr.lost_rows());
    }

    #[test]
    fn dropped_batches_leave_index_gap() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        {
            let _gate = mgr.stall_writer();
            for _ in 0..=STALL_BATCHES {
                mgr.archive_rows(make_rows(10, 80)).unwrap();
            }
        }
        let gap = mgr.dropped_rows();
        assert!(gap >= 10, "need drops to test gap indexing");

        // Drain the queue (any query is a barrier) so the gap-carrying
        // batch can't itself be dropped.
        let _ = mgr.total_archived_rows();
        mgr.archive_rows(make_rows(10, 80)).unwrap();
        mgr.flush_pending().unwrap();
        mgr.seal_active_segment().unwrap();

        let stored = mgr.total_archived_rows();
        let pre_gap = stored - 10;
        // Indices inside the gap resolve to no rows, not misaligned rows.
        assert!(mgr.read_range(pre_gap, 1).unwrap().is_empty());
        // The post-gap batch lives at its true absolute position.
        assert_eq!(mgr.read_range(pre_gap + gap, 10).unwrap().len(), 10);
    }

    #[test]
    fn read_archived_rows_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        let rows = make_rows(10, 40);
        mgr.archive_rows(rows.clone()).unwrap();
        mgr.flush_pending().unwrap();

        mgr.seal_active_segment().unwrap();

        let read_back = mgr.read_range(0, 10).unwrap();
        assert_eq!(read_back.len(), 10);
        assert_eq!(read_back[0].1.cells[0].codepoint, 'A');
        assert_eq!(read_back[9].1.cells[0].codepoint, 'J');
        assert_eq!(read_back[9].0, 9);
    }

    #[test]
    fn read_range_seals_automatically() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        mgr.archive_rows(make_rows(10, 40)).unwrap();

        // No explicit flush or seal: the read observes the batch anyway.
        assert_eq!(mgr.read_range(0, 10).unwrap().len(), 10);
    }

    #[test]
    fn read_of_finalized_history_does_not_seal() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        mgr.archive_rows(make_rows(10, 40)).unwrap();
        mgr.flush_pending().unwrap();
        mgr.seal_active_segment().unwrap();
        mgr.archive_rows(make_rows(5, 40)).unwrap();

        // A window entirely within finalized history leaves the newer
        // rows unsealed — no read-triggered segment churn.
        assert_eq!(mgr.read_range(0, 10).unwrap().len(), 10);
        assert_eq!(mgr.segment_count(), 1);

        // A window reaching past the boundary still seals them.
        assert_eq!(mgr.read_range(10, 5).unwrap().len(), 5);
        assert_eq!(mgr.segment_count(), 2);
    }

    #[test]
    fn total_rows_received_counts_stored_and_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        let sent = 10 * STALL_BATCHES as u64;
        {
            let _gate = mgr.stall_writer();
            for _ in 0..STALL_BATCHES {
                mgr.archive_rows(make_rows(10, 80)).unwrap();
            }
        }
        assert_eq!(mgr.total_rows_received(), sent);
        assert!(mgr.dropped_rows() > 0, "need drops for this to mean much");
    }

    #[test]
    fn absolute_index_space_aligns_archive_and_hot() {
        use crate::scroll::HotBuffer;

        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        // Small enough that pushes prune, replicating push_to_scrollback.
        let mut hot = HotBuffer::new(64 * 1024);
        let mut pushed: u64 = 0;
        while mgr.total_rows_received() == 0 || pushed < 200 {
            let mut row = Row::new(80);
            #[allow(clippy::cast_possible_truncation)]
            let offset = (pushed % 26) as u8;
            row.cells[0].codepoint = char::from(b'a' + offset);
            let pruned = hot.push(row);
            if !pruned.is_empty() {
                mgr.archive_rows(pruned).unwrap();
            }
            pushed += 1;
        }

        // The two tiers partition the pushed history exactly.
        let received = mgr.total_rows_received();
        assert_eq!(received + hot.len() as u64, pushed);

        // Absolute index k is the k-th pushed row on both sides of the
        // archive/hot boundary.
        let archived = mgr.read_range(0, 1).unwrap();
        assert_eq!(archived[0].1.cells[0].codepoint, 'a');
        assert_eq!(
            hot.get(0).unwrap().cells[0].codepoint,
            char::from(b'a' + u8::try_from(received % 26).unwrap()),
        );
    }

    #[test]
    fn read_with_styled_rows() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        let mut row = Row::new(20);
        row.cells[0].codepoint = 'X';
        row.cells[0].fg = Color::Named(NamedColor::Red);
        mgr.archive_rows(vec![row.clone()]).unwrap();
        mgr.flush_pending().unwrap();
        mgr.seal_active_segment().unwrap();

        let result = mgr.read_range(0, 1).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, row);
    }

    #[test]
    fn segment_files_created() {
        let dir = tempfile::tempdir().unwrap();
        let archive_dir = dir.path().join("archive");
        let mut mgr = ArchiveManager::new(archive_dir.clone(), u64::MAX).unwrap();
        mgr.archive_rows(make_rows(150, 80)).unwrap();
        mgr.flush_pending().unwrap();
        mgr.seal_active_segment().unwrap();
        let segments = std::fs::read_dir(&archive_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("segment-"))
            .count();
        assert!(segments > 0);
    }

    #[test]
    fn segment_pruning() {
        let dir = tempfile::tempdir().unwrap();
        // Tight limit forces pruning during writes.
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), 64 * 1024).unwrap();
        for _ in 0..10 {
            mgr.archive_rows(make_rows(200, 80)).unwrap();
            mgr.flush_pending().unwrap();
            mgr.seal_active_segment().unwrap();
        }
        assert!(
            mgr.disk_bytes() <= 64 * 1024,
            "disk_bytes {} should be pruned below the limit",
            mgr.disk_bytes()
        );
        assert!(mgr.segment_count() > 0);
    }

    #[test]
    fn shutdown_deletes_files() {
        let dir = tempfile::tempdir().unwrap();
        let archive_dir = dir.path().join("archive");
        let mut mgr = ArchiveManager::new(archive_dir.clone(), u64::MAX).unwrap();
        mgr.archive_rows(make_rows(150, 80)).unwrap();
        mgr.flush_pending().unwrap();
        mgr.shutdown().unwrap();
        assert!(!archive_dir.exists());
        assert_eq!(mgr.segment_count(), 0);
        assert_eq!(mgr.disk_bytes(), 0);
    }

    #[test]
    fn shutdown_with_queued_batches_drains_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let archive_dir = dir.path().join("archive");
        let mut mgr = ArchiveManager::new(archive_dir.clone(), u64::MAX).unwrap();
        {
            let _gate = mgr.stall_writer();
            for _ in 0..MAILBOX_DEPTH {
                mgr.archive_rows(make_rows(10, 80)).unwrap();
            }
        }
        // Queue may still hold batches; shutdown must drain, not hang.
        mgr.shutdown().unwrap();
        assert!(!archive_dir.exists());
    }

    #[test]
    fn archive_rows_after_shutdown_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        mgr.shutdown().unwrap();
        assert!(mgr.archive_rows(make_rows(1, 80)).is_err());
    }

    #[test]
    fn shutdown_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        mgr.shutdown().unwrap();
        mgr.shutdown().unwrap();
    }

    #[test]
    fn shutdown_after_writer_death_deletes_files_directly() {
        let dir = tempfile::tempdir().unwrap();
        let archive_dir = dir.path().join("archive");
        let mut mgr = ArchiveManager::new(archive_dir.clone(), u64::MAX).unwrap();
        mgr.archive_rows(make_rows(150, 80)).unwrap();
        mgr.flush_pending().unwrap();
        mgr.seal_active_segment().unwrap();

        let writer = mgr.writer.as_ref().expect("writer running");
        writer.tx.send(WriterMsg::Panic).expect("send panic");
        while !writer.thread.is_finished() {
            thread::sleep(Duration::from_millis(1));
        }

        // Dead writer: enqueue errors and counts the loss.
        assert!(mgr.archive_rows(make_rows(10, 80)).is_err());
        assert_eq!(mgr.dropped_rows(), 10);

        // Shutdown deletes the files itself instead of erroring.
        mgr.shutdown().unwrap();
        assert!(!archive_dir.exists());
    }

    #[test]
    fn drop_without_shutdown_keeps_files() {
        let dir = tempfile::tempdir().unwrap();
        let archive_dir = dir.path().join("archive");
        let mut mgr = ArchiveManager::new(archive_dir.clone(), u64::MAX).unwrap();
        mgr.archive_rows(make_rows(150, 80)).unwrap();
        mgr.flush_pending().unwrap();
        mgr.seal_active_segment().unwrap();
        drop(mgr);
        let segments = std::fs::read_dir(&archive_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("segment-"))
            .count();
        assert!(segments > 0, "drop must retain files for orphan cleanup");
    }

    #[test]
    fn cleanup_orphans_removes_dead_session_dirs() {
        let dir = tempfile::tempdir().unwrap();
        // A dir named for a PID that cannot be alive (max pid + timestamp).
        let dead = dir.path().join("99999999-1234567890");
        std::fs::create_dir_all(&dead).unwrap();
        let live = dir.path().join("current-session");
        std::fs::create_dir_all(&live).unwrap();

        ArchiveManager::cleanup_orphans(dir.path(), "current-session").unwrap();
        assert!(!dead.exists());
        assert!(live.exists());
    }

    #[test]
    fn cleanup_orphans_skips_unrecognised_names() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let current = "12345-1000000";
        std::fs::create_dir_all(base.join(current)).unwrap();
        std::fs::create_dir_all(base.join("not-a-pid")).unwrap();

        ArchiveManager::cleanup_orphans(base, current).unwrap();

        // Unrecognised name should be left alone.
        assert!(base.join("not-a-pid").exists());
    }

    #[test]
    fn reader_sees_rows_the_manager_archived() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        mgr.archive_rows(make_rows(10, 40)).unwrap();
        let reader = mgr.reader().expect("writer running");

        // Same seal-before-read behaviour as the manager's own read.
        let rows = reader.read_range(0, 10).unwrap();
        assert_eq!(rows.len(), 10);
        assert_eq!(rows[9].0, 9);
        assert_eq!(rows[9].1.cells[0].codepoint, 'J');
    }

    #[test]
    fn reader_is_none_after_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        mgr.shutdown().unwrap();
        assert!(mgr.reader().is_none());
    }

    fn wait_for(within: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + within;
        while !cond() {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(1));
        }
        true
    }

    /// The invariant the whole reader design rests on: dropping the
    /// manager's sender must disconnect the mailbox even with readers
    /// outstanding, because that disconnect is the writer's only exit
    /// signal. A reader holding a strong sender would park the thread
    /// forever.
    #[test]
    fn a_reader_cannot_keep_the_writer_thread_alive() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        let reader = mgr.reader().expect("writer running");
        let writer = mgr.writer.take().expect("writer running");

        drop(writer.tx);

        assert!(
            wait_for(Duration::from_secs(5), || writer.thread.is_finished()),
            "a reader must not keep the writer thread alive"
        );
        writer.thread.join().expect("writer exited cleanly");
        assert!(reader.read_range(0, 1).is_err());
    }

    /// Detaching is for a writer already known to be wedged, so it must
    /// not wait — and with the mailbox full there is no way to ask the
    /// thread to stop, which is exactly why the exit signal has to be the
    /// disconnect rather than a message. An outstanding reader must not
    /// be able to undo it.
    #[test]
    fn detaching_a_wedged_writer_neither_waits_nor_leaves_a_way_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        let reader = mgr.reader().expect("writer running");
        let gate = mgr.stall_writer();
        for _ in 0..STALL_BATCHES {
            mgr.archive_rows(make_rows(10, 80)).unwrap();
        }

        let started = Instant::now();
        mgr.detach_writer();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "detaching a wedged writer must not wait on it"
        );

        let started = Instant::now();
        let err = reader.read_range(0, 1).expect_err("writer unreachable");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a reader whose manager is gone must fail fast, not wait out the query timeout"
        );
        assert!(
            err.to_string().contains("exited"),
            "unexpected error: {err}"
        );

        drop(gate);
    }

    /// A reader handed to a blocking task can outlive the pane it came
    /// from, and teardown must still finish.
    #[test]
    fn an_outstanding_reader_does_not_block_teardown() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        mgr.archive_rows(make_rows(10, 40)).unwrap();
        let reader = mgr.reader().expect("writer running");

        // Dropped on its own thread so a teardown that blocks fails here
        // with a diagnosis, rather than hanging until a CI timeout kills
        // the run and says nothing about which test wedged.
        let dropped = thread::spawn(move || drop(mgr));
        assert!(
            wait_for(Duration::from_secs(10), || dropped.is_finished()),
            "teardown blocked with a reader outstanding"
        );
        dropped.join().expect("teardown panicked");

        // The writer is gone; the stale handle errors instead of hanging.
        let err = reader.read_range(0, 10).expect_err("writer stopped");
        assert!(
            err.to_string().contains("exited"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn an_outstanding_reader_does_not_block_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let archive_dir = dir.path().join("archive");
        let mut mgr = ArchiveManager::new(archive_dir.clone(), u64::MAX).unwrap();
        mgr.archive_rows(make_rows(10, 40)).unwrap();
        let reader = mgr.reader().expect("writer running");

        mgr.shutdown().unwrap();

        assert!(!archive_dir.exists());
        assert!(reader.read_range(0, 10).is_err());
    }

    #[test]
    fn cleanup_orphans_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = ArchiveManager::cleanup_orphans(&dir.path().join("nonexistent"), "s");
        assert!(result.is_ok());
    }
}
