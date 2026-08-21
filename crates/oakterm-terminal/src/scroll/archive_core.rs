//! Synchronous archive state machine: batching, segment rotation,
//! pruning, disk-space protection, and loss accounting per Spec-0004.
//!
//! Owned by value by the archive writer thread (see `archive_manager`);
//! nothing here is aware of threads or channels.

use crate::grid::row::Row;
use crate::scroll::archive::{ArchiveKey, SegmentReader, SegmentWriter};
use crate::scroll::storage::{DiskStorage, SegmentStorage};
use std::io::{self, Write};
use std::path::PathBuf;

/// Target uncompressed frame size in bytes.
const FRAME_TARGET_BYTES: usize = 64 * 1024;

struct FinalizedSegment {
    path: PathBuf,
    nonce_start: u64,
    first_row_index: u64,
    row_count: u64,
    disk_bytes: u64,
}

/// Snapshot of observable archive state, answered by the writer thread.
/// Deliberately not `Default`: the facade's degraded-state convention is
/// "assume paused" for an unreachable writer, the opposite of a default.
#[derive(Clone, Copy)]
pub(crate) struct ArchiveStats {
    pub total_archived_rows: u64,
    pub segment_count: usize,
    pub disk_bytes: u64,
    pub paused: bool,
    pub lost_rows: u64,
}

/// The segment currently being filled and its id — inseparable, so a
/// writer without an id is unrepresentable.
struct ActiveSegment<W: Write> {
    id: u32,
    writer: SegmentWriter<W>,
}

/// Archive state: batching, segment rotation, pruning, and disk-space
/// protection. All methods are synchronous. Generic over the storage
/// seam so tests can force each error channel; production uses
/// `DiskStorage`.
pub(crate) struct ArchiveCore<S: SegmentStorage = DiskStorage> {
    storage: S,
    key: Option<ArchiveKey>,
    session_dir: PathBuf,
    active: Option<ActiveSegment<S::Writer>>,
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
    /// Advanced by stored rows at finalize and by unfinalized losses, so
    /// segment indexing exposes gaps instead of misaligning.
    next_first_row_index: u64,
    /// Rows lost on the writer side: discarded while paused, or abandoned
    /// after a write error. Saturation drops are counted separately on the
    /// facade (`dropped_rows`).
    lost_rows: u64,
}

impl ArchiveCore<DiskStorage> {
    pub(crate) fn new(session_dir: PathBuf, max_disk_bytes: u64) -> io::Result<Self> {
        Self::with_storage(DiskStorage, session_dir, max_disk_bytes)
    }
}

impl<S: SegmentStorage> ArchiveCore<S> {
    pub(crate) fn with_storage(
        storage: S,
        session_dir: PathBuf,
        max_disk_bytes: u64,
    ) -> io::Result<Self> {
        storage.init_dir(&session_dir)?;
        Ok(Self {
            storage,
            key: Some(ArchiveKey::generate()?),
            session_dir,
            active: None,
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

    pub(crate) fn stats(&self) -> ArchiveStats {
        ArchiveStats {
            total_archived_rows: self.total_archived_rows,
            segment_count: self.segments.len(),
            disk_bytes: self.disk_bytes,
            paused: self.archiving_paused,
            lost_rows: self.lost_rows,
        }
    }

    fn segment_path(&self, id: u32) -> PathBuf {
        self.session_dir.join(format!("segment-{id:04}.bin"))
    }

    /// First absolute index not yet claimed by a finalized segment or an
    /// accounted gap. Reads entirely below it need no seal. Never exceeds
    /// the facade's `total_rows_received`; the difference is in-flight
    /// rows and gaps whose advance hasn't landed yet.
    pub(crate) fn finalized_boundary(&self) -> u64 {
        self.next_first_row_index
    }

    /// Disk-space check, rate-limited to once per second — flushes can run
    /// thousands of times per second under sustained scrolling.
    fn disk_space_ok(&mut self) -> bool {
        if let Some(last) = self.last_disk_check
            && last.elapsed() < std::time::Duration::from_secs(1)
        {
            return !self.archiving_paused;
        }
        self.last_disk_check = Some(std::time::Instant::now());
        self.storage.has_enough_space(&self.session_dir)
    }

    /// Account a segment that failed to finalize: its rows are lost, the
    /// file is dropped best-effort so untracked files can't accumulate
    /// past `max_disk_bytes`, and archiving pauses.
    fn discard_failed_segment(&mut self, id: u32, rows: u64) {
        self.lose_unfinalized(rows);
        let path = self.segment_path(id);
        if let Err(e) = self.storage.remove_file(&path) {
            tracing::warn!(error = %e, path = %path.display(), "could not remove failed segment");
        }
        self.recompute_total();
        self.pause("segment finalize error");
    }

    /// Batches rows and flushes to disk when the pending batch reaches
    /// ~64 KB. `preceding_gap` rows were dropped before this batch
    /// (saturation); the current segment is closed and the index base
    /// advanced so the gap stays visible to the read path.
    pub(crate) fn archive_rows(&mut self, rows: Vec<Row>, preceding_gap: u64) -> io::Result<()> {
        if preceding_gap > 0 {
            let sealed = self
                .flush_pending()
                .and_then(|()| self.seal_active_segment());
            // The gap advance must survive flush/seal errors — the facade
            // already consumed pending_gap, so a skipped advance would
            // silently misalign every later segment. The gap rows are
            // counted on the facade (dropped_rows), so only the base
            // moves here.
            self.next_first_row_index += preceding_gap;
            if let Err(e) = sealed {
                self.lose_unfinalized(rows.len() as u64);
                return Err(e);
            }
        }
        if self.archiving_paused {
            self.archiving_paused = !self.disk_space_ok();
            if self.archiving_paused {
                self.lose_unfinalized(rows.len() as u64);
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

    /// Count rows lost before they reached a finalized segment. Advances
    /// the index base — these rows never claimed their index range, so
    /// the base must move past them to keep gaps visible.
    ///
    /// Counterpart: `lose_finalized`, for rows whose base advance already
    /// happened at finalize. Picking the wrong one silently misaligns
    /// every later segment.
    fn lose_unfinalized(&mut self, count: u64) {
        self.lost_rows += count;
        self.next_first_row_index += count;
    }

    /// Count rows lost from already-finalized segments (era reset). Does
    /// NOT advance the index base — finalize already did when the
    /// segment was stamped; advancing again would double-shift.
    fn lose_finalized(&mut self, count: u64) {
        self.lost_rows += count;
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
        self.lose_unfinalized(lost_pending);
        self.pending_rows.clear();
        self.pending_bytes = 0;
        if let Some(active) = self.active.take() {
            let torn = active.writer.total_rows();
            self.lose_unfinalized(torn);
            match active.writer.finalize() {
                Ok((_, key)) => self.key = Some(key),
                Err(e) => {
                    tracing::warn!(error = %e, "could not salvage archive key from failed segment");
                }
            }
            // The torn segment's content is unreadable — drop the file so
            // untracked bytes can't accumulate past max_disk_bytes.
            let path = self.segment_path(active.id);
            if let Err(e) = self.storage.remove_file(&path) {
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
                    // No key at all: the segments are already unreadable,
                    // so count the loss now rather than at the next
                    // take_key — stats must not overreport while paused.
                    self.reset_era("archive key unrecoverable");
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
        self.lose_finalized(unreadable);
        for seg in self.segments.drain(..) {
            if let Err(e) = self.storage.remove_file(&seg.path) {
                tracing::warn!(error = %e, path = %seg.path.display(), "could not remove stale segment");
            }
        }
        self.disk_bytes = 0;
        tracing::warn!(reason, unreadable_rows = unreadable, "archive era reset");
    }

    fn recompute_total(&mut self) {
        self.total_archived_rows = self.segments.iter().map(|s| s.row_count).sum::<u64>()
            + self
                .active
                .as_ref()
                .map_or(0, |active| active.writer.total_rows());
    }

    /// Flush all pending rows as a single frame.
    pub(crate) fn flush_pending(&mut self) -> io::Result<()> {
        if self.pending_rows.is_empty() {
            return Ok(());
        }

        if !self.disk_space_ok() {
            // Losses must not shift a still-open segment: its
            // first_row_index is stamped at finalize, so close it before
            // advancing the index base. Failure routes through
            // discard_failed_segment, which accounts and pauses.
            if self.active.is_some()
                && let Err(e) = self.finalize_active_segment()
            {
                tracing::warn!(error = %e, "seal before pause failed");
            }
            let lost = self.pending_rows.len() as u64;
            self.lose_unfinalized(lost);
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
            .active
            .as_mut()
            .expect("ensure_writer succeeded")
            .writer
            .write_frame(rows)
        {
            self.abandon_after_error(rows.len() as u64);
            return Err(e);
        }

        let is_full = self.active.as_ref().expect("just wrote").writer.is_full();
        if is_full {
            self.finalize_active_segment()?;
        }

        self.prune_and_recompute();
        Ok(())
    }

    pub(crate) fn seal_active_segment(&mut self) -> io::Result<()> {
        if self.active.is_some() {
            self.finalize_active_segment()?;
            // Sealing adds the segment's bytes outside the write path's
            // prune check — enforce the disk cap here too, or a
            // read-triggered seal could hold the archive over it.
            self.prune_and_recompute();
        }
        Ok(())
    }

    /// Prune to the disk cap and refresh totals. A prune failure loses
    /// no rows and is deliberately not an error: flush/seal errors read
    /// as data loss to callers (the gap path discards its batch on
    /// them), and the cap retries on the next write or seal.
    fn prune_and_recompute(&mut self) {
        if let Err(e) = self.prune_if_needed() {
            tracing::warn!(error = %e, "archive prune failed; disk cap will retry");
        }
        self.recompute_total();
    }

    /// Read rows in `[start, start + count)` across all finalized
    /// segments, tagged with their absolute indices. Indices inside gaps
    /// (lost or dropped rows) produce no entry rather than misaligned
    /// rows, so callers must align by the returned indices. An unreadable
    /// segment fails the whole read: errors must stay distinguishable
    /// from gaps, because gaps render as permanently blank history while
    /// errors are retryable.
    pub(crate) fn read_range(&self, start: u64, count: usize) -> io::Result<Vec<(u64, Row)>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let end = start.saturating_add(count as u64);
        let key_ref = self.current_key()?;
        let mut out = Vec::new();
        for seg in &self.segments {
            let seg_end = seg.first_row_index + seg.row_count;
            if seg_end <= start || seg.first_row_index >= end {
                continue;
            }
            let from = start.max(seg.first_row_index);
            let to = end.min(seg_end);
            let local_start = from - seg.first_row_index;
            // to - from is bounded by count, which is a usize.
            let n = usize::try_from(to - from).unwrap_or(count);
            let rows = self
                .storage
                .read(&seg.path)
                .and_then(|data| {
                    SegmentReader::open(&data, key_ref, seg.nonce_start)
                        .and_then(|reader| reader.read_rows(local_start, n))
                })
                .map_err(|e| {
                    tracing::warn!(
                        error = %e,
                        path = %seg.path.display(),
                        "segment unreadable during range read"
                    );
                    e
                })?;
            out.extend(
                rows.into_iter()
                    .enumerate()
                    .map(|(i, row)| (from + i as u64, row)),
            );
        }
        // Consumers align by these indices; out-of-order entries would
        // make a gap indistinguishable from misalignment.
        debug_assert!(
            out.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "read_range indices must be strictly increasing"
        );
        Ok(out)
    }

    /// Get a reference to the encryption key, whether it's held by the
    /// manager or by the active writer.
    fn current_key(&self) -> io::Result<&ring::aead::LessSafeKey> {
        if let Some(key) = &self.key {
            Ok(key.key())
        } else if let Some(active) = &self.active {
            Ok(active.writer.key().key())
        } else {
            Err(io::Error::other("no archive key available"))
        }
    }

    /// Finalize the active writer and delete all archive files.
    pub(crate) fn shutdown(&mut self) -> io::Result<()> {
        // Errors here are non-fatal — cleanup still runs below.
        if let Err(e) = self.flush_pending() {
            tracing::warn!(error = %e, "flush_pending failed during shutdown");
        }
        if let Some(active) = self.active.take() {
            match active.writer.finalize() {
                Ok((_, key)) => self.key = Some(key),
                Err(e) => tracing::warn!(error = %e, "segment finalization failed during shutdown"),
            }
        }
        self.storage.remove_dir_all(&self.session_dir)?;
        self.segments.clear();
        self.disk_bytes = 0;
        self.total_archived_rows = 0;
        Ok(())
    }

    fn ensure_writer(&mut self) -> io::Result<()> {
        if self.active.is_none() {
            let id = self.next_segment_id;
            let file = self.storage.create(&self.segment_path(id))?;
            self.next_segment_id += 1;
            let writer = SegmentWriter::with_key(file, self.take_key()?);
            self.active = Some(ActiveSegment { id, writer });
        }
        Ok(())
    }

    fn finalize_active_segment(&mut self) -> io::Result<()> {
        let ActiveSegment { id: seg_id, writer } =
            self.active.take().expect("called with active writer");
        let nonce_start_for_segment = writer.nonce_start();
        let total_rows = writer.total_rows();
        let path = self.segment_path(seg_id);
        // BufWriter defers most IO errors to this flush point, so these
        // error paths are the common ENOSPC/EIO case: account the
        // segment's rows as lost, drop the file, and pause.
        let (buf_writer, key) = match writer.finalize() {
            Ok(v) => v,
            Err(e) => {
                self.discard_failed_segment(seg_id, total_rows);
                return Err(e);
            }
        };
        // Store the key the moment it exists again — the error paths below
        // must not lose it (prior segments become unreadable without it).
        self.key = Some(key);
        let file_size = match self.storage.finish(buf_writer, &path) {
            Ok(size) => size,
            Err(e) => {
                self.discard_failed_segment(seg_id, total_rows);
                return Err(e);
            }
        };

        // Index base, not the previous segment's end: lost/dropped rows
        // advance the base so gaps stay visible to the read path.
        let first_row_index = self.next_first_row_index;
        self.next_first_row_index += total_rows;

        // A base inside an existing segment means some loss site chose
        // lose_finalized where lose_unfinalized was required — the
        // misalignment the two conventions exist to prevent.
        debug_assert!(
            self.segments
                .last()
                .is_none_or(|prev| first_row_index >= prev.first_row_index + prev.row_count),
            "archive index base regressed into an existing segment"
        );

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
            self.storage.remove_file(&self.segments[0].path)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::row::Row;
    use crate::scroll::storage::fake::FakeStorage;

    fn fake_core(max_disk_bytes: u64) -> (ArchiveCore<FakeStorage>, FakeStorage) {
        let storage = FakeStorage::default();
        let core =
            ArchiveCore::with_storage(storage.clone(), PathBuf::from("/fake"), max_disk_bytes)
                .unwrap();
        (core, storage)
    }

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
    fn pending_batches_below_frame_target() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = ArchiveCore::new(dir.path().join("archive"), u64::MAX).unwrap();
        core.archive_rows(make_rows(5, 80), 0).unwrap();
        assert_eq!(core.stats().total_archived_rows, 0);
        assert!(!core.pending_rows.is_empty());
        core.flush_pending().unwrap();
        assert!(core.pending_rows.is_empty());
        assert_eq!(core.stats().total_archived_rows, 5);
    }

    #[test]
    fn gap_advances_index_base_and_seals() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = ArchiveCore::new(dir.path().join("archive"), u64::MAX).unwrap();
        core.archive_rows(make_rows(10, 80), 0).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();

        // A batch arriving after a 25-row gap starts past it.
        core.archive_rows(make_rows(10, 80), 25).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();

        assert!(
            core.read_range(10, 1).unwrap().is_empty(),
            "gap reads empty"
        );
        assert_eq!(core.read_range(35, 10).unwrap().len(), 10);
    }

    #[test]
    fn read_range_spans_segments_and_tags_indices() {
        let (mut core, _storage) = fake_core(u64::MAX);
        core.archive_rows(make_rows(10, 80), 0).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();
        let mut second = make_rows(10, 80);
        for row in &mut second {
            row.cells[0].codepoint = 'z';
        }
        core.archive_rows(second, 0).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();

        let rows = core.read_range(5, 10).unwrap();
        assert_eq!(rows.len(), 10);
        assert_eq!(rows.first().unwrap().0, 5);
        assert_eq!(rows.last().unwrap().0, 14);
        assert_eq!(rows[4].1.cells[0].codepoint, 'J', "index 9: segment 1");
        assert_eq!(rows[5].1.cells[0].codepoint, 'z', "index 10: segment 2");
    }

    #[test]
    fn read_range_errors_on_unreadable_segment() {
        let (mut core, storage) = fake_core(u64::MAX);
        core.archive_rows(make_rows(10, 80), 0).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();
        core.archive_rows(make_rows(10, 80), 0).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();

        // The read must fail rather than return the healthy tail as if
        // the corrupt range were a gap: errors are retryable, gaps are not.
        let seg0 = core.segment_path(0);
        storage.state.borrow_mut().files.insert(seg0, vec![0; 32]);
        assert!(core.read_range(0, 20).is_err());
    }

    #[test]
    fn seal_enforces_disk_cap() {
        // Any finalized segment exceeds this limit.
        let (mut core, _storage) = fake_core(64);
        core.archive_rows(make_rows(10, 80), 0).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();
        assert!(
            core.stats().disk_bytes <= 64,
            "read-triggered seals must prune, got {} bytes",
            core.stats().disk_bytes
        );
    }

    #[test]
    fn read_range_omits_gap_indices() {
        let (mut core, _storage) = fake_core(u64::MAX);
        core.archive_rows(make_rows(10, 80), 0).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();

        core.archive_rows(make_rows(10, 80), 25).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();

        let rows = core.read_range(5, 35).unwrap();
        let indices: Vec<u64> = rows.iter().map(|(i, _)| *i).collect();
        assert_eq!(indices, (5..10).chain(35..40).collect::<Vec<u64>>());
    }

    #[test]
    fn era_reset_counts_losses_without_moving_base() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = ArchiveCore::new(dir.path().join("archive"), u64::MAX).unwrap();
        core.archive_rows(make_rows(10, 80), 0).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();
        let base_before = core.next_first_row_index;

        core.key = None;
        core.reset_era("test");

        assert_eq!(core.stats().lost_rows, 10);
        assert_eq!(core.next_first_row_index, base_before);
        assert_eq!(core.stats().segment_count, 0);
        // The cleared range reads back as a gap under a fresh key.
        core.archive_rows(make_rows(10, 80), 0).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();
        assert!(core.read_range(0, 1).unwrap().is_empty());
        assert_eq!(core.read_range(base_before, 10).unwrap().len(), 10);
    }

    #[test]
    fn conservation_holds_through_pause_losses() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = ArchiveCore::new(dir.path().join("archive"), u64::MAX).unwrap();
        core.archive_rows(make_rows(10, 80), 0).unwrap();
        core.flush_pending().unwrap();

        core.pause("test");
        core.archive_rows(make_rows(7, 80), 0).unwrap();
        // Rate-limited disk check defers the probe, so the pause holds.
        let stats = core.stats();
        assert_eq!(stats.total_archived_rows + stats.lost_rows, 17);
    }

    #[test]
    fn write_error_loses_pending_and_resets_era() {
        let (mut core, storage) = fake_core(u64::MAX);
        core.archive_rows(make_rows(10, 80), 0).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();

        // The frame write fails, and so does the footer during key
        // salvage — the key is lost, forcing an era reset.
        storage.state.borrow_mut().fail_writes = true;
        core.archive_rows(make_rows(7, 80), 0).unwrap();
        assert!(core.flush_pending().is_err());

        let stats = core.stats();
        assert!(stats.paused);
        assert_eq!(stats.segment_count, 0, "era reset drops finalized segments");
        assert_eq!(stats.total_archived_rows + stats.lost_rows, 17);

        // The whole pre-reset range reads back as a gap.
        storage.state.borrow_mut().fail_writes = false;
        core.last_disk_check = None;
        core.archive_rows(make_rows(5, 80), 0).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();

        let stats = core.stats();
        assert_eq!(stats.total_archived_rows + stats.lost_rows, 22);
        assert!(core.read_range(0, 1).unwrap().is_empty());
        assert_eq!(core.read_range(17, 5).unwrap().len(), 5);
    }

    #[test]
    fn transient_write_error_salvages_key_and_keeps_prior_segments() {
        let (mut core, storage) = fake_core(u64::MAX);
        core.archive_rows(make_rows(10, 80), 0).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();

        // One write fails, then the salvage footer succeeds: the key
        // survives and prior segments must NOT be era-reset.
        storage.state.borrow_mut().fail_next_write = true;
        core.archive_rows(make_rows(7, 80), 0).unwrap();
        assert!(core.flush_pending().is_err());

        let stats = core.stats();
        assert!(stats.paused);
        assert_eq!(stats.segment_count, 1, "prior segment survives salvage");
        assert_eq!(stats.total_archived_rows + stats.lost_rows, 17);

        core.last_disk_check = None;
        core.archive_rows(make_rows(5, 80), 0).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();

        let stats = core.stats();
        assert_eq!(stats.total_archived_rows + stats.lost_rows, 22);
        // Pre-error history is still readable under the salvaged key.
        assert_eq!(core.read_range(0, 10).unwrap().len(), 10);
        assert_eq!(core.read_range(17, 5).unwrap().len(), 5);
    }

    #[test]
    fn create_failure_keeps_key_and_prior_segments() {
        let (mut core, storage) = fake_core(u64::MAX);
        core.archive_rows(make_rows(10, 80), 0).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();

        // create() fails before take_key runs — the key never leaves
        // the core, so no salvage and no era reset.
        storage.state.borrow_mut().fail_create = true;
        core.archive_rows(make_rows(7, 80), 0).unwrap();
        assert!(core.flush_pending().is_err());

        let stats = core.stats();
        assert!(stats.paused);
        assert_eq!(stats.segment_count, 1);
        assert_eq!(stats.total_archived_rows + stats.lost_rows, 17);

        storage.state.borrow_mut().fail_create = false;
        core.last_disk_check = None;
        core.archive_rows(make_rows(5, 80), 0).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();

        assert_eq!(core.read_range(0, 10).unwrap().len(), 10);
        assert_eq!(core.read_range(17, 5).unwrap().len(), 5);
    }

    #[test]
    fn seal_before_pause_failure_accounts_both_losses() {
        let (mut core, storage) = fake_core(u64::MAX);
        core.archive_rows(make_rows(10, 80), 0).unwrap();
        core.flush_pending().unwrap();

        // Low disk forces the seal, and the seal itself fails: the open
        // segment's rows and the pending rows are both lost, once each.
        {
            let mut flags = storage.state.borrow_mut();
            flags.low_disk = true;
            flags.fail_finish = true;
        }
        core.last_disk_check = None;
        core.archive_rows(make_rows(6, 80), 0).unwrap();
        core.flush_pending().unwrap();

        let stats = core.stats();
        assert!(stats.paused);
        assert_eq!(stats.total_archived_rows, 0);
        assert_eq!(stats.lost_rows, 16);

        {
            let mut flags = storage.state.borrow_mut();
            flags.low_disk = false;
            flags.fail_finish = false;
        }
        core.last_disk_check = None;
        core.archive_rows(make_rows(4, 80), 0).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();

        let stats = core.stats();
        assert_eq!(stats.total_archived_rows + stats.lost_rows, 20);
        assert_eq!(core.read_range(16, 4).unwrap().len(), 4);
    }

    #[test]
    fn finalize_failure_salvages_key_and_conserves() {
        let (mut core, storage) = fake_core(u64::MAX);
        core.archive_rows(make_rows(10, 80), 0).unwrap();
        core.flush_pending().unwrap();

        // The footer writes fine (key salvaged) but the close fails —
        // the deferred ENOSPC/EIO case.
        storage.state.borrow_mut().fail_finish = true;
        assert!(core.seal_active_segment().is_err());

        let stats = core.stats();
        assert!(stats.paused);
        assert_eq!(stats.segment_count, 0);
        assert_eq!(stats.total_archived_rows + stats.lost_rows, 10);

        // Same era after healing: the salvaged key reads the new segment
        // and the discarded range stays a gap.
        storage.state.borrow_mut().fail_finish = false;
        core.last_disk_check = None;
        core.archive_rows(make_rows(5, 80), 0).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();

        let stats = core.stats();
        assert_eq!(stats.total_archived_rows + stats.lost_rows, 15);
        assert!(core.read_range(0, 1).unwrap().is_empty());
        assert_eq!(core.read_range(10, 5).unwrap().len(), 5);
    }

    #[test]
    fn low_disk_seals_open_segment_before_pausing() {
        let (mut core, storage) = fake_core(u64::MAX);
        core.archive_rows(make_rows(10, 80), 0).unwrap();
        core.flush_pending().unwrap();

        storage.state.borrow_mut().low_disk = true;
        core.last_disk_check = None;
        core.archive_rows(make_rows(6, 80), 0).unwrap();
        core.flush_pending().unwrap();

        let stats = core.stats();
        assert!(stats.paused);
        assert_eq!(stats.total_archived_rows, 10);
        assert_eq!(stats.lost_rows, 6);
        // The open segment was sealed before the base advanced, so its
        // stamped index still resolves.
        assert_eq!(core.read_range(0, 10).unwrap().len(), 10);

        storage.state.borrow_mut().low_disk = false;
        core.last_disk_check = None;
        core.archive_rows(make_rows(4, 80), 0).unwrap();
        core.flush_pending().unwrap();
        core.seal_active_segment().unwrap();

        let stats = core.stats();
        assert_eq!(stats.total_archived_rows + stats.lost_rows, 20);
        assert_eq!(core.read_range(16, 4).unwrap().len(), 4);
    }

    #[test]
    fn prune_failure_keeps_metadata_consistent() {
        // Any finalized segment exceeds this limit, so every seal and
        // write tries to prune.
        let (mut core, storage) = fake_core(64);
        core.archive_rows(make_rows(10, 80), 0).unwrap();
        core.flush_pending().unwrap();

        storage.state.borrow_mut().fail_remove = true;
        // The over-limit segment finalizes; the prune failure is not a
        // seal failure — nothing was lost.
        core.seal_active_segment().unwrap();

        // A prune error loses nothing: metadata intact, totals current.
        let stats = core.stats();
        assert_eq!(stats.lost_rows, 0);
        assert_eq!(stats.segment_count, 1);
        assert_eq!(stats.total_archived_rows, 10);

        storage.state.borrow_mut().fail_remove = false;
        core.archive_rows(make_rows(6, 80), 0).unwrap();
        core.flush_pending().unwrap();

        // Retry pruned the oldest segment; its rows retire (not lost).
        let stats = core.stats();
        assert_eq!(stats.segment_count, 0);
        assert_eq!(stats.lost_rows, 0);
        assert_eq!(stats.total_archived_rows, 6);
    }
}
