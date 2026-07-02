//! Archive lifecycle manager: batching, segment rotation, pruning,
//! disk space protection, and cleanup per Spec-0004.
//!
//! Writes happen on a dedicated writer thread fed by a bounded queue so
//! compression and encryption never run inside the PTY read loop
//! (the 2026-07-02 parity benchmark measured the synchronous path at
//! ~5x ingest cost under sustained scrolling).

use crate::grid::row::Row;
use crate::scroll::archive::{ArchiveKey, SegmentReader, SegmentWriter};
use std::fs;
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

/// Target uncompressed frame size in bytes.
const FRAME_TARGET_BYTES: usize = 64 * 1024;

/// Writer queue depth in batches. A batch is one prune's worth of rows
/// (~10% of the hot buffer). Batches that find the queue full are dropped
/// (Spec-0004 overload policy), so this bounds memory, not latency.
const WRITER_QUEUE_BATCHES: usize = 4;

/// Minimum free disk space (1 GB) before archiving pauses.
const MIN_FREE_BYTES: u64 = 1024 * 1024 * 1024;

/// Minimum free disk percentage (5%) before archiving pauses.
const MIN_FREE_PERCENT: u64 = 5;

/// Metadata for a finalized segment file on disk.
struct FinalizedSegment {
    path: PathBuf,
    nonce_start: u64,
    first_row_index: u64,
    row_count: u64,
    disk_bytes: u64,
}

/// Result of draining the writer queue.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SyncOutcome {
    /// Every prior message was processed.
    Drained,
    /// The writer thread has exited; queued batches are lost.
    Dead,
    /// The writer did not drain within the deadline (alive but stuck).
    Wedged,
}

/// Message to the writer thread.
enum WriterMsg {
    /// Rows pruned from the hot buffer, to be batched and written.
    /// `preceding_gap` counts rows dropped (queue full) since the last
    /// enqueued batch; the core advances its index base by that amount so
    /// archived row indexing exposes the gap instead of misaligning.
    Batch { rows: Vec<Row>, preceding_gap: u64 },
    /// Barrier: acknowledged once every prior message is processed.
    Sync(SyncSender<()>),
}

/// Manages the cold disk archive for one pane's scrollback.
///
/// `archive_rows` enqueues to an internal writer thread and returns
/// immediately; every other method drains the queue first, so callers
/// observe the same state a synchronous implementation would show.
pub struct ArchiveManager {
    core: Arc<Mutex<ArchiveCore>>,
    writer: Option<WriterHandle>,
    session_dir: PathBuf,
    // Updated at enqueue time on the caller thread — the core is
    // writer-thread territory and the enqueue path must not lock it.
    // `archive_rows` taking `&mut self` is what keeps the sync() barrier
    // sound; don't relax these to atomics + `&self`.
    dropped_rows: u64,
    pending_gap: u64,
}

/// Channel and thread of a running writer; both live and die together.
struct WriterHandle {
    tx: SyncSender<WriterMsg>,
    thread: thread::JoinHandle<()>,
}

impl ArchiveManager {
    /// Create a new archive manager for the given session directory.
    ///
    /// Creates the directory with 0700 permissions if it doesn't exist,
    /// and spawns the writer thread.
    ///
    /// # Errors
    ///
    /// Returns an error if key generation, directory creation, or thread
    /// spawning fails.
    pub fn new(session_dir: PathBuf, max_disk_bytes: u64) -> io::Result<Self> {
        let core = Arc::new(Mutex::new(ArchiveCore::new(
            session_dir.clone(),
            max_disk_bytes,
        )?));
        let (tx, rx) = mpsc::sync_channel::<WriterMsg>(WRITER_QUEUE_BATCHES);
        let thread_core = Arc::clone(&core);
        let writer_thread = thread::Builder::new()
            .name("archive-writer".to_string())
            .spawn(move || {
                while let Ok(msg) = rx.recv() {
                    match msg {
                        WriterMsg::Batch {
                            rows,
                            preceding_gap,
                        } => {
                            let mut core = lock_core(&thread_core);
                            if let Err(e) = core.archive_rows(rows, preceding_gap) {
                                tracing::warn!(error = %e, "failed to archive pruned rows");
                            }
                        }
                        WriterMsg::Sync(ack) => {
                            let _ = ack.send(());
                        }
                    }
                }
            })?;
        Ok(Self {
            core,
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

    /// Rows dropped at enqueue because the writer was saturated (a subset
    /// of `lost_rows`).
    #[must_use]
    pub fn dropped_rows(&self) -> u64 {
        self.dropped_rows
    }

    /// Barrier: wait until the writer thread has processed every message
    /// sent before this call (`Drained` covers the already-shut-down case).
    /// Never call while holding the core lock — a queued batch ahead of
    /// the barrier would deadlock against it; use `synced_core` for the
    /// sync-then-lock pair.
    fn sync(&self) -> SyncOutcome {
        let Some(writer) = &self.writer else {
            return SyncOutcome::Drained;
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        // try_send: a blocking send on a full queue could wait forever on a
        // wedged writer, never reaching the ack timeout below.
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        let mut msg = WriterMsg::Sync(ack_tx);
        loop {
            match writer.tx.try_send(msg) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(returned)) => {
                    if std::time::Instant::now() >= deadline {
                        tracing::warn!("archive writer queue stayed full; skipping drain");
                        return SyncOutcome::Wedged;
                    }
                    msg = returned;
                    thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(mpsc::TrySendError::Disconnected(_)) => return SyncOutcome::Dead,
            }
        }
        // Bounded wait: a writer wedged on hung IO must not block pane
        // teardown forever.
        match ack_rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            Ok(()) => SyncOutcome::Drained,
            Err(mpsc::RecvTimeoutError::Disconnected) => SyncOutcome::Dead,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!("archive writer did not drain within 10s");
                SyncOutcome::Wedged
            }
        }
    }

    /// Drain the writer queue, then lock the core. The only correct way
    /// for facade methods to observe core state. Returns `None` when the
    /// writer is wedged holding the core lock — locking then would hang
    /// the caller past the drain timeout; infallible getters fall back to
    /// a best-effort default instead.
    fn synced_core(&self) -> Option<MutexGuard<'_, ArchiveCore>> {
        match self.sync() {
            SyncOutcome::Wedged => match self.core.try_lock() {
                Ok(guard) => Some(guard),
                Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => None,
            },
            SyncOutcome::Drained | SyncOutcome::Dead => Some(lock_core(&self.core)),
        }
    }

    /// Drain-then-lock for fallible operations: a dead writer is an error,
    /// not silently stale state.
    fn synced_core_checked(&self) -> io::Result<MutexGuard<'_, ArchiveCore>> {
        match self.sync() {
            SyncOutcome::Drained => Ok(lock_core(&self.core)),
            SyncOutcome::Dead => Err(io::Error::other("archive writer thread exited")),
            SyncOutcome::Wedged => Err(io::Error::other("archive writer did not drain within 10s")),
        }
    }

    /// Flush all pending rows as a single frame.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails or the writer thread has died.
    pub fn flush_pending(&mut self) -> io::Result<()> {
        self.synced_core_checked()?.flush_pending()
    }

    /// Finalize the active segment so all written rows become readable.
    /// A new segment is opened on the next write.
    ///
    /// # Errors
    ///
    /// Returns an error if finalization fails.
    pub fn seal_active_segment(&mut self) -> io::Result<()> {
        self.synced_core_checked()?.seal_active_segment()
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
    /// Returns an error if reading from disk fails.
    pub fn read_rows(&self, start: u64, count: usize) -> io::Result<Vec<Row>> {
        self.synced_core_checked()?.read_rows(start, count)
    }

    /// Total rows stored across all finalized segments (excludes pending).
    /// Best-effort 0 while the writer is wedged.
    #[must_use]
    pub fn total_archived_rows(&self) -> u64 {
        self.synced_core()
            .map_or(0, |core| core.total_archived_rows)
    }

    /// Number of finalized segments on disk. Best-effort 0 while the
    /// writer is wedged.
    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.synced_core().map_or(0, |core| core.segments.len())
    }

    /// Total bytes used by segment files on disk. Best-effort 0 while the
    /// writer is wedged.
    #[must_use]
    pub fn disk_bytes(&self) -> u64 {
        self.synced_core().map_or(0, |core| core.disk_bytes)
    }

    /// The session directory path.
    #[must_use]
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    /// Whether archiving is paused due to low disk space or write errors.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.synced_core().is_none_or(|core| core.archiving_paused)
    }

    /// Drain the queue, stop the writer thread, finalize the active
    /// segment, and delete all archive files.
    ///
    /// # Errors
    ///
    /// Returns an error if finalization or deletion fails, or if the
    /// writer is wedged — the core lock is in its hands, so cleanup
    /// would hang; orphan cleanup reclaims the directory later.
    pub fn shutdown(&mut self) -> io::Result<()> {
        if self.stop_writer() == SyncOutcome::Wedged {
            return Err(io::Error::other(
                "archive writer wedged; leaving cleanup to cleanup_orphans",
            ));
        }
        lock_core(&self.core).shutdown()
    }

    /// Close the channel and join the writer thread, draining first. A
    /// wedged writer (failed to drain, stuck holding the core lock) is
    /// detached, not joined — joining would hang teardown in exactly the
    /// scenario the drain timeout exists to survive; dropping the channel
    /// lets it exit on its own. A dead writer joins immediately.
    fn stop_writer(&mut self) -> SyncOutcome {
        let outcome = self.sync();
        if let Some(writer) = self.writer.take() {
            drop(writer.tx);
            if outcome == SyncOutcome::Wedged {
                tracing::warn!("detaching wedged archive writer thread");
            } else if writer.thread.join().is_err() {
                tracing::error!("archive writer thread panicked");
            }
        }
        outcome
    }

    /// Rows lost on any channel: saturation drops, paused discards, and
    /// write-error abandonment. Archive row indexing is non-contiguous
    /// whenever this is non-zero.
    #[must_use]
    pub fn lost_rows(&self) -> u64 {
        self.dropped_rows + self.synced_core().map_or(0, |core| core.lost_rows)
    }
}

impl Drop for ArchiveManager {
    fn drop(&mut self) {
        // Stop the thread but keep files: explicit shutdown deletes them;
        // a crash-dropped archive is reclaimed by cleanup_orphans.
        self.stop_writer();
    }
}

/// Lock the core, recovering from a poisoned lock (a panicking writer
/// thread must not take the daemon down with it).
fn lock_core(core: &Arc<Mutex<ArchiveCore>>) -> MutexGuard<'_, ArchiveCore> {
    core.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Archive state owned behind the mutex: batching, segment rotation,
/// pruning, and disk-space protection. All methods are synchronous.
struct ArchiveCore {
    key: Option<ArchiveKey>,
    session_dir: PathBuf,
    active_writer: Option<SegmentWriter<BufWriter<fs::File>>>,
    segments: Vec<FinalizedSegment>,
    pending_rows: Vec<Row>,
    pending_bytes: usize,
    disk_bytes: u64,
    max_disk_bytes: u64,
    next_segment_id: u32,
    archiving_paused: bool,
    total_archived_rows: u64,
    last_disk_check: Option<std::time::Instant>,
    /// Absolute pruned-row index where the next finalized segment starts.
    /// Advanced by stored rows at finalize and by lost/dropped rows, so
    /// segment indexing exposes gaps instead of misaligning.
    next_first_row_index: u64,
    /// Rows lost on the writer side: discarded while paused, or abandoned
    /// after a write error. Saturation drops are counted separately on the
    /// facade (`dropped_rows`).
    lost_rows: u64,
}

impl ArchiveCore {
    fn new(session_dir: PathBuf, max_disk_bytes: u64) -> io::Result<Self> {
        fs::create_dir_all(&session_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&session_dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            key: Some(ArchiveKey::generate()?),
            session_dir,
            active_writer: None,
            segments: Vec::new(),
            pending_rows: Vec::new(),
            pending_bytes: 0,
            disk_bytes: 0,
            max_disk_bytes,
            next_segment_id: 0,
            archiving_paused: false,
            total_archived_rows: 0,
            last_disk_check: None,
            next_first_row_index: 0,
            lost_rows: 0,
        })
    }

    /// Disk-space check, rate-limited to once per second — flushes can run
    /// thousands of times per second under sustained scrolling.
    fn disk_space_ok(&mut self) -> bool {
        if let Some(last) = self.last_disk_check {
            if last.elapsed() < std::time::Duration::from_secs(1) {
                return !self.archiving_paused;
            }
        }
        self.last_disk_check = Some(std::time::Instant::now());
        has_enough_disk_space(&self.session_dir)
    }

    /// Account a segment that failed to finalize: its rows are lost (the
    /// index base advances to keep gaps visible), the file is dropped
    /// best-effort so untracked files can't accumulate past
    /// `max_disk_bytes`, and archiving pauses.
    fn discard_failed_segment(&mut self, path: &Path, rows: u64) {
        self.lose_rows(rows);
        if let Err(e) = fs::remove_file(path) {
            tracing::warn!(error = %e, path = %path.display(), "could not remove failed segment");
        }
        self.recompute_total();
        self.pause("segment finalize error");
    }

    /// Batches rows and flushes to disk when the pending batch reaches
    /// ~64 KB. `preceding_gap` rows were dropped before this batch
    /// (saturation); the current segment is closed and the index base
    /// advanced so the gap stays visible to the read path.
    fn archive_rows(&mut self, rows: Vec<Row>, preceding_gap: u64) -> io::Result<()> {
        if preceding_gap > 0 {
            let sealed = self
                .flush_pending()
                .and_then(|()| self.seal_active_segment());
            // The gap advance must survive flush/seal errors — the facade
            // already consumed pending_gap, so a skipped advance would
            // silently misalign every later segment.
            self.next_first_row_index += preceding_gap;
            if let Err(e) = sealed {
                self.lose_rows(rows.len() as u64);
                return Err(e);
            }
        }
        if self.archiving_paused {
            self.archiving_paused = !self.disk_space_ok();
            if self.archiving_paused {
                self.lose_rows(rows.len() as u64);
                return Ok(());
            }
            tracing::info!(lost_rows = self.lost_rows, "archiving resumed");
        }
        for row in rows {
            self.pending_bytes += estimate_row_bytes(&row);
            self.pending_rows.push(row);
        }
        if self.pending_bytes >= FRAME_TARGET_BYTES {
            self.flush_pending()?;
        }
        Ok(())
    }

    /// Count rows the writer side could not store and keep the index base
    /// gap-visible.
    fn lose_rows(&mut self, count: u64) {
        self.lost_rows += count;
        self.next_first_row_index += count;
    }

    /// Pause archiving (idempotent), warn-logging the transition — a
    /// paused archive silently discards rows from then on, so the edge
    /// must be visible.
    fn pause(&mut self, reason: &str) {
        if !self.archiving_paused {
            self.archiving_paused = true;
            tracing::warn!(
                reason,
                lost_rows = self.lost_rows,
                "archiving paused; rows will be discarded until it resumes"
            );
        }
    }

    /// Abandon the active segment and pending rows after a write error.
    /// The torn segment's rows are counted lost; the session key is
    /// salvaged from the failed writer when possible. If the key cannot
    /// be recovered, prior segments are unreadable — the era resets
    /// (metadata cleared; files reclaimed at shutdown/orphan cleanup).
    fn abandon_after_error(&mut self, lost_pending: u64) {
        self.lose_rows(lost_pending);
        self.pending_rows.clear();
        self.pending_bytes = 0;
        if let Some(writer) = self.active_writer.take() {
            let torn = writer.total_rows();
            self.lose_rows(torn);
            match writer.finalize() {
                Ok((_, key)) => self.key = Some(key),
                Err(e) => {
                    tracing::warn!(error = %e, "could not salvage archive key from failed segment");
                }
            }
            // The torn segment's content is unreadable — drop the file so
            // untracked bytes can't accumulate past max_disk_bytes.
            let seg_id = self.next_segment_id - 1;
            let path = self.session_dir.join(format!("segment-{seg_id:04}.bin"));
            if let Err(e) = fs::remove_file(&path) {
                tracing::warn!(error = %e, path = %path.display(), "could not remove torn segment");
            }
        }
        if self.key.is_none() {
            match ArchiveKey::generate() {
                Ok(key) => {
                    self.key = Some(key);
                    self.reset_era("archive key lost");
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to regenerate archive key");
                }
            }
        }
        self.recompute_total();
        self.pause("write error");
    }

    /// Drop all segment metadata and files: a regenerated key cannot read
    /// them. Losses are counted and the index base keeps its position, so
    /// the cleared range reads back as a gap.
    fn reset_era(&mut self, reason: &str) {
        let unreadable: u64 = self.segments.iter().map(|s| s.row_count).sum();
        self.lost_rows += unreadable;
        for seg in self.segments.drain(..) {
            if let Err(e) = fs::remove_file(&seg.path) {
                tracing::warn!(error = %e, path = %seg.path.display(), "could not remove stale segment");
            }
        }
        self.disk_bytes = 0;
        tracing::warn!(reason, unreadable_rows = unreadable, "archive era reset");
    }

    fn recompute_total(&mut self) {
        self.total_archived_rows = self.segments.iter().map(|s| s.row_count).sum::<u64>()
            + self
                .active_writer
                .as_ref()
                .map_or(0, super::archive::SegmentWriter::total_rows);
    }

    /// Flush all pending rows as a single frame.
    fn flush_pending(&mut self) -> io::Result<()> {
        if self.pending_rows.is_empty() {
            return Ok(());
        }

        if !self.disk_space_ok() {
            // Losses must not shift a still-open segment: its
            // first_row_index is stamped at finalize, so close it before
            // advancing the index base. Failure routes through
            // discard_failed_segment, which accounts and pauses.
            if self.active_writer.is_some() {
                if let Err(e) = self.finalize_active_segment() {
                    tracing::warn!(error = %e, "seal before pause failed");
                }
            }
            let lost = self.pending_rows.len() as u64;
            self.lose_rows(lost);
            self.pending_rows.clear();
            self.pending_bytes = 0;
            self.pause("low disk space");
            return Ok(());
        }

        let rows: Vec<Row> = std::mem::take(&mut self.pending_rows);
        self.pending_bytes = 0;
        self.write_rows(&rows)
    }

    /// Write one frame of rows to the active segment, finalizing it when
    /// full. Each error site accounts its own losses: rows that never
    /// reached the writer abandon here; rows inside a failed finalize are
    /// counted by `discard_failed_segment`; a prune error loses nothing.
    ///
    /// # Panics
    ///
    /// Cannot panic: the active writer is guaranteed to exist after `ensure_writer`.
    fn write_rows(&mut self, rows: &[Row]) -> io::Result<()> {
        if let Err(e) = self.ensure_writer() {
            self.abandon_after_error(rows.len() as u64);
            return Err(e);
        }
        if let Err(e) = self
            .active_writer
            .as_mut()
            .expect("ensure_writer succeeded")
            .write_frame(rows)
        {
            self.abandon_after_error(rows.len() as u64);
            return Err(e);
        }

        let is_full = self.active_writer.as_ref().expect("just wrote").is_full();
        if is_full {
            self.finalize_active_segment()?;
        }

        self.prune_if_needed()?;
        self.recompute_total();
        Ok(())
    }

    fn seal_active_segment(&mut self) -> io::Result<()> {
        if self.active_writer.is_some() {
            self.finalize_active_segment()?;
        }
        Ok(())
    }

    fn read_rows(&self, start: u64, count: usize) -> io::Result<Vec<Row>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let key_ref = self.current_key()?;
        for seg in &self.segments {
            let seg_end = seg.first_row_index + seg.row_count;
            if start >= seg.first_row_index && start < seg_end {
                let data = fs::read(&seg.path)?;
                let reader = SegmentReader::open(&data, key_ref, seg.nonce_start)?;
                let local_start = start - seg.first_row_index;
                return reader.read_rows(local_start, count);
            }
        }
        Ok(Vec::new())
    }

    /// Get a reference to the encryption key, whether it's held by the
    /// manager or by the active writer.
    fn current_key(&self) -> io::Result<&ring::aead::LessSafeKey> {
        if let Some(key) = &self.key {
            Ok(key.key())
        } else if let Some(writer) = &self.active_writer {
            Ok(writer.key().key())
        } else {
            Err(io::Error::other("no archive key available"))
        }
    }

    /// Finalize the active writer and delete all archive files.
    fn shutdown(&mut self) -> io::Result<()> {
        // Flush pending rows and finalize the active segment if possible.
        // Errors are non-fatal — we still clean up the directory.
        if let Err(e) = self.flush_pending() {
            tracing::warn!(error = %e, "flush_pending failed during shutdown");
        }
        if let Some(writer) = self.active_writer.take() {
            match writer.finalize() {
                Ok((_, key)) => self.key = Some(key),
                Err(e) => tracing::warn!(error = %e, "segment finalization failed during shutdown"),
            }
        }
        if self.session_dir.exists() {
            fs::remove_dir_all(&self.session_dir)?;
        }
        self.segments.clear();
        self.disk_bytes = 0;
        self.total_archived_rows = 0;
        Ok(())
    }

    fn ensure_writer(&mut self) -> io::Result<()> {
        if self.active_writer.is_none() {
            let path = self
                .session_dir
                .join(format!("segment-{:04}.bin", self.next_segment_id));
            let file = BufWriter::new(fs::File::create(&path)?);
            self.next_segment_id += 1;
            let writer = SegmentWriter::with_key(file, self.take_key()?);
            self.active_writer = Some(writer);
        }
        Ok(())
    }
}

impl ArchiveManager {
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
        for entry in fs::read_dir(base_dir)? {
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
            // Skip directories whose owning process is still alive.
            #[cfg(unix)]
            if pid_is_alive(name_str) {
                continue;
            }
            if let Err(e) = fs::remove_dir_all(entry.path()) {
                last_error = Some(e);
            }
        }
        match last_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl ArchiveCore {
    fn finalize_active_segment(&mut self) -> io::Result<()> {
        let writer = self
            .active_writer
            .take()
            .expect("called with active writer");
        let nonce_start_for_segment =
            writer.key().nonce_counter() - u64::from(writer.frame_count());
        let total_rows = writer.total_rows();
        let seg_id = self.next_segment_id - 1;
        let path = self.session_dir.join(format!("segment-{seg_id:04}.bin"));
        // BufWriter defers most IO errors to this flush point, so these
        // error paths are the common ENOSPC/EIO case: account the
        // segment's rows as lost, drop the file, and pause.
        let (buf_writer, key) = match writer.finalize() {
            Ok(v) => v,
            Err(e) => {
                self.discard_failed_segment(&path, total_rows);
                return Err(e);
            }
        };
        // Store the key the moment it exists again — the error paths below
        // must not lose it (prior segments become unreadable without it).
        self.key = Some(key);
        let file_size = match buf_writer
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)
            .and_then(|inner| inner.metadata())
        {
            Ok(metadata) => metadata.len(),
            Err(e) => {
                self.discard_failed_segment(&path, total_rows);
                return Err(e);
            }
        };

        // Index base, not the previous segment's end: lost/dropped rows
        // advance the base so gaps stay visible to the read path.
        let first_row_index = self.next_first_row_index;
        self.next_first_row_index += total_rows;

        self.segments.push(FinalizedSegment {
            path,
            nonce_start: nonce_start_for_segment,
            first_row_index,
            row_count: total_rows,
            disk_bytes: file_size,
        });
        self.disk_bytes += file_size;
        Ok(())
    }

    /// Take the session key for a new segment writer, regenerating it if a
    /// prior failure lost it. A fresh key starts a new archive era: any
    /// segment metadata still present is unreadable under it and dropped.
    fn take_key(&mut self) -> io::Result<ArchiveKey> {
        if let Some(key) = self.key.take() {
            return Ok(key);
        }
        if !self.segments.is_empty() {
            self.reset_era("key regenerated for new writer");
        }
        ArchiveKey::generate()
    }

    fn prune_if_needed(&mut self) -> io::Result<()> {
        if self.disk_bytes <= self.max_disk_bytes {
            return Ok(());
        }
        let target = self.max_disk_bytes * 9 / 10;
        while self.disk_bytes > target && !self.segments.is_empty() {
            // Delete file first, then remove metadata. If deletion fails,
            // metadata stays consistent and the next prune attempt retries.
            if self.segments[0].path.exists() {
                fs::remove_file(&self.segments[0].path)?;
            }
            let removed = self.segments.remove(0);
            self.disk_bytes = self.disk_bytes.saturating_sub(removed.disk_bytes);
        }
        Ok(())
    }
}

/// Estimate serialized size of a row (for batching decisions).
fn estimate_row_bytes(row: &Row) -> usize {
    std::mem::size_of::<Row>() + row.cells.len() * std::mem::size_of::<crate::grid::cell::Cell>()
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

/// Check if the filesystem has enough free space for archiving.
fn has_enough_disk_space(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use rustix::fs::statvfs;
        let stat = match statvfs(path) {
            Ok(stat) => stat,
            Err(e) => {
                tracing::warn!(error = %e, "statvfs failed; treating as low disk space");
                return false;
            }
        };
        let free_bytes = stat.f_bavail.saturating_mul(stat.f_frsize);
        let total_bytes = stat.f_blocks.saturating_mul(stat.f_frsize);
        let min_percent_bytes = total_bytes / 100 * MIN_FREE_PERCENT;
        free_bytes >= MIN_FREE_BYTES.max(min_percent_bytes)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::cell::{Color, NamedColor};
    use crate::grid::row::Row;

    fn make_rows(count: usize, cols: usize) -> Vec<Row> {
        (0..count)
            .map(|i| {
                let mut r = Row::new(cols);
                #[allow(clippy::cast_possible_truncation)]
                {
                    r.cells[0].codepoint =
                        char::from_u32(u32::from(b'A') + (i as u32 % 26)).unwrap_or('?');
                }
                r
            })
            .collect()
    }

    #[test]
    fn small_batch_stays_pending() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        mgr.archive_rows(make_rows(5, 80)).unwrap();
        assert_eq!(mgr.total_archived_rows(), 0); // not flushed yet
        mgr.sync();
        assert!(!lock_core(&mgr.core).pending_rows.is_empty());
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
        mgr.flush_pending().unwrap();
        assert_eq!(mgr.total_archived_rows(), 5);
        assert!(lock_core(&mgr.core).pending_rows.is_empty());
    }

    #[test]
    fn segment_files_created() {
        let dir = tempfile::tempdir().unwrap();
        let archive_dir = dir.path().join("archive");
        let mut mgr = ArchiveManager::new(archive_dir.clone(), u64::MAX).unwrap();
        mgr.archive_rows(make_rows(150, 80)).unwrap();
        mgr.flush_pending().unwrap();
        // At least one segment file should exist (or active writer holds one)
        let files: Vec<_> = fs::read_dir(&archive_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "bin"))
            .collect();
        assert!(!files.is_empty(), "expected segment files on disk");
    }

    #[test]
    fn saturated_writer_drops_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        let sent = 10 * (WRITER_QUEUE_BATCHES as u64 + 2);
        {
            // Stall the writer thread by holding the core lock; the queue
            // fills and further batches must drop, not block.
            let core = Arc::clone(&mgr.core);
            let _stall = lock_core(&core);
            for _ in 0..WRITER_QUEUE_BATCHES + 2 {
                mgr.archive_rows(make_rows(10, 80)).unwrap();
            }
        }
        assert!(
            mgr.dropped_rows() >= 10,
            "expected at least one dropped batch, got {} dropped rows",
            mgr.dropped_rows()
        );
        // The writer resumes once the lock is released; queued batches land.
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
            let core = Arc::clone(&mgr.core);
            let _stall = lock_core(&core);
            for _ in 0..WRITER_QUEUE_BATCHES + 3 {
                mgr.archive_rows(make_rows(10, 80)).unwrap();
            }
        }
        let gap = mgr.dropped_rows();
        assert!(gap >= 10, "need drops to test gap indexing");

        // Drain the queue so the gap-carrying batch can't itself be dropped.
        assert!(mgr.sync() == SyncOutcome::Drained);
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
    fn shutdown_with_queued_batches_drains_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let archive_dir = dir.path().join("archive");
        let mut mgr = ArchiveManager::new(archive_dir.clone(), u64::MAX).unwrap();
        {
            let core = Arc::clone(&mgr.core);
            let _stall = lock_core(&core);
            for _ in 0..WRITER_QUEUE_BATCHES {
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
    fn read_archived_rows_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();
        let rows = make_rows(10, 40);
        mgr.archive_rows(rows.clone()).unwrap();
        mgr.flush_pending().unwrap();

        // Finalize so segment is readable
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
    fn segment_pruning() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = ArchiveManager::new(dir.path().join("archive"), u64::MAX).unwrap();

        // Write enough to create multiple finalized segments
        for _ in 0..10 {
            mgr.archive_rows(make_rows(200, 80)).unwrap();
        }
        mgr.flush_pending().unwrap();
        mgr.seal_active_segment().unwrap();

        let segments_before = mgr.segment_count();
        let bytes_before = mgr.disk_bytes();
        assert!(segments_before > 0, "expected finalized segments");

        // Now set a tight limit and trigger pruning
        {
            let mut core = lock_core(&mgr.core);
            core.max_disk_bytes = bytes_before / 2;
            core.prune_if_needed().unwrap();
        }

        assert!(
            mgr.disk_bytes() < bytes_before,
            "disk_bytes {} should be less than {bytes_before}",
            mgr.disk_bytes()
        );
        assert!(
            mgr.segment_count() < segments_before,
            "segments {} should be less than {segments_before}",
            mgr.segment_count()
        );
    }

    #[test]
    fn shutdown_deletes_files() {
        let dir = tempfile::tempdir().unwrap();
        let archive_dir = dir.path().join("archive");
        let mut mgr = ArchiveManager::new(archive_dir.clone(), u64::MAX).unwrap();
        mgr.archive_rows(make_rows(150, 80)).unwrap();
        mgr.flush_pending().unwrap();
        assert!(archive_dir.exists());

        mgr.shutdown().unwrap();
        assert!(!archive_dir.exists());
        assert_eq!(mgr.segment_count(), 0);
        assert_eq!(mgr.disk_bytes(), 0);
    }

    #[test]
    fn cleanup_orphans_deletes_stale() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        // Use {pid}-{timestamp} format. PID 999999999 won't be running.
        let current = "12345-1000000";
        let stale = "999999999-900000";
        fs::create_dir_all(base.join(current)).unwrap();
        fs::create_dir_all(base.join(stale)).unwrap();
        fs::write(base.join(format!("{stale}/segment-0000.bin")), b"data").unwrap();

        ArchiveManager::cleanup_orphans(base, current).unwrap();

        assert!(base.join(current).exists());
        assert!(!base.join(stale).exists());
    }

    #[test]
    fn cleanup_orphans_skips_unrecognised_names() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let current = "12345-1000000";
        fs::create_dir_all(base.join(current)).unwrap();
        fs::create_dir_all(base.join("not-a-pid")).unwrap();

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
