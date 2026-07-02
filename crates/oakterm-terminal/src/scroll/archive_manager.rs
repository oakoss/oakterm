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
        reply: SyncSender<io::Result<Vec<Row>>>,
    },
    Stats(SyncSender<ArchiveStats>),
    /// Finalize, delete all archive files, reply, and exit the loop.
    Shutdown(SyncSender<io::Result<()>>),
    /// Block the writer until the sender side drops — deterministic
    /// saturation for tests.
    #[cfg(test)]
    Stall(mpsc::Receiver<()>),
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
}

/// Channel and thread of a running writer; both live and die together.
struct WriterHandle {
    tx: SyncSender<WriterMsg>,
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
                            log_if_unheard(reply.send(core.read_rows(start, count)), "read");
                        }
                        WriterMsg::Stats(reply) => {
                            let _ = reply.send(core.stats());
                        }
                        WriterMsg::Shutdown(reply) => {
                            log_if_unheard(reply.send(core.shutdown()), "shutdown");
                            break;
                        }
                        #[cfg(test)]
                        WriterMsg::Stall(gate) => {
                            let _ = gate.recv();
                        }
                        #[cfg(test)]
                        #[allow(clippy::missing_panics_doc)]
                        WriterMsg::Panic => panic!("test-induced writer death"),
                    }
                }
            })?;
        Ok(Self {
            writer: Some(WriterHandle {
                tx,
                thread: writer_thread,
            }),
            session_dir,
            dropped_rows: 0,
            pending_gap: 0,
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

    /// Enqueue a query with a bounded wait on both the send (the mailbox
    /// may be full of batches) and the reply.
    fn query<T>(&self, build: impl FnOnce(SyncSender<T>) -> WriterMsg) -> io::Result<T> {
        let writer = self
            .writer
            .as_ref()
            .ok_or_else(|| io::Error::other("archive writer shut down"))?;
        let deadline = Instant::now() + QUERY_TIMEOUT;
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let mut msg = build(reply_tx);
        loop {
            match writer.tx.try_send(msg) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(returned)) => {
                    if Instant::now() >= deadline {
                        return Err(io::Error::other(
                            "archive writer did not accept a query within 10s",
                        ));
                    }
                    msg = returned;
                    thread::sleep(Duration::from_millis(10));
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    return Err(io::Error::other("archive writer thread exited"));
                }
            }
        }
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

    /// Read archived rows by absolute row index.
    ///
    /// Only reads from finalized segments. Call `seal_active_segment` first
    /// to make recently written rows available.
    ///
    /// Returns up to `count` rows starting from `start`. Returns an empty
    /// vec if no segment contains the requested range.
    ///
    /// # Errors
    ///
    /// Returns an error if reading from disk fails or the writer is dead
    /// or wedged.
    pub fn read_rows(&self, start: u64, count: usize) -> io::Result<Vec<Row>> {
        self.query(|reply| WriterMsg::Read {
            start,
            count,
            reply,
        })?
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

    /// Join a writer that is known to have exited (or is about to).
    fn join_writer(&mut self) {
        if let Some(writer) = self.writer.take() {
            drop(writer.tx);
            if writer.thread.join().is_err() {
                tracing::error!("archive writer thread panicked");
            }
        }
    }

    /// Abandon a wedged writer without joining — joining would hang
    /// teardown in exactly the scenario the query timeout exists to
    /// survive; dropping the channel lets it exit on its own.
    fn detach_writer(&mut self) {
        if let Some(writer) = self.writer.take() {
            drop(writer.tx);
            tracing::warn!(
                path = %self.session_dir.display(),
                "detaching wedged archive writer; queued rows may still be written, files left for orphan cleanup"
            );
        }
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

    /// Block the writer until the returned sender drops, so the mailbox
    /// can be filled deterministically.
    fn stall(mgr: &ArchiveManager) -> SyncSender<()> {
        let (gate_tx, gate_rx) = mpsc::sync_channel(0);
        mgr.writer
            .as_ref()
            .expect("writer running")
            .tx
            .send(WriterMsg::Stall(gate_rx))
            .expect("send stall");
        gate_tx
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
            let _gate = stall(&mgr);
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
            let _gate = stall(&mgr);
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
        assert!(mgr.read_rows(pre_gap, 1).unwrap().is_empty());
        // The post-gap batch lives at its true absolute position.
        assert_eq!(mgr.read_rows(pre_gap + gap, 10).unwrap().len(), 10);
    }

    #[test]
    fn read_archived_rows_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        let rows = make_rows(10, 40);
        mgr.archive_rows(rows.clone()).unwrap();
        mgr.flush_pending().unwrap();

        mgr.seal_active_segment().unwrap();

        let read_back = mgr.read_rows(0, 10).unwrap();
        assert_eq!(read_back.len(), 10);
        assert_eq!(read_back[0].cells[0].codepoint, 'A');
        assert_eq!(read_back[9].cells[0].codepoint, 'J');
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

        let result = mgr.read_rows(0, 1).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], row);
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
            let _gate = stall(&mgr);
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
    fn cleanup_orphans_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = ArchiveManager::cleanup_orphans(&dir.path().join("nonexistent"), "s");
        assert!(result.is_ok());
    }
}
