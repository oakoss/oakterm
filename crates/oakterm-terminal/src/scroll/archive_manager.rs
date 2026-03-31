//! Archive lifecycle manager: batching, segment rotation, pruning,
//! disk space protection, and cleanup per Spec-0004.

use crate::grid::row::Row;
use crate::scroll::archive::{ArchiveKey, SegmentReader, SegmentWriter};
use std::fs;
use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};

/// Target uncompressed frame size in bytes.
const FRAME_TARGET_BYTES: usize = 64 * 1024;

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

/// Manages the cold disk archive for one pane's scrollback.
pub struct ArchiveManager {
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
}

impl ArchiveManager {
    /// Create a new archive manager for the given session directory.
    ///
    /// Creates the directory with 0700 permissions if it doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if key generation or directory creation fails.
    pub fn new(session_dir: PathBuf, max_disk_bytes: u64) -> io::Result<Self> {
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
        })
    }

    /// Accept pruned rows from the hot buffer. Batches internally and
    /// flushes to disk when the pending batch reaches ~64 KB.
    ///
    /// # Errors
    ///
    /// Returns an error if flushing to disk fails.
    pub fn archive_rows(&mut self, rows: Vec<Row>) -> io::Result<()> {
        if self.archiving_paused {
            self.archiving_paused = !has_enough_disk_space(&self.session_dir);
            if self.archiving_paused {
                return Ok(());
            }
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

    /// Flush all pending rows as a single frame.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    ///
    /// # Panics
    ///
    /// Cannot panic: the active writer is guaranteed to exist after `ensure_writer`.
    #[allow(clippy::cast_possible_truncation)]
    pub fn flush_pending(&mut self) -> io::Result<()> {
        if self.pending_rows.is_empty() {
            return Ok(());
        }

        if !has_enough_disk_space(&self.session_dir) {
            self.archiving_paused = true;
            self.pending_rows.clear();
            self.pending_bytes = 0;
            return Ok(());
        }

        self.ensure_writer()?;
        let rows: Vec<Row> = std::mem::take(&mut self.pending_rows);
        self.pending_bytes = 0;
        let row_count = rows.len() as u64;
        self.active_writer
            .as_mut()
            .expect("ensure_writer succeeded")
            .write_frame(&rows)?;

        let is_full = self.active_writer.as_ref().expect("just wrote").is_full();
        if is_full {
            self.finalize_active_segment()?;
        }

        self.total_archived_rows += row_count;
        self.prune_if_needed()?;
        Ok(())
    }

    /// Finalize the active segment so all written rows become readable.
    /// A new segment is opened on the next write.
    ///
    /// # Errors
    ///
    /// Returns an error if finalization fails.
    pub fn seal_active_segment(&mut self) -> io::Result<()> {
        if self.active_writer.is_some() {
            self.finalize_active_segment()?;
        }
        Ok(())
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

    /// Total rows stored across all finalized segments (excludes pending).
    #[must_use]
    pub fn total_archived_rows(&self) -> u64 {
        self.total_archived_rows
    }

    /// Number of finalized segments on disk.
    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Total bytes used by segment files on disk.
    #[must_use]
    pub fn disk_bytes(&self) -> u64 {
        self.disk_bytes
    }

    /// The session directory path.
    #[must_use]
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    /// Whether archiving is paused due to low disk space.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.archiving_paused
    }

    /// Finalize the active writer and delete all archive files.
    ///
    /// # Errors
    ///
    /// Returns an error if finalization or deletion fails.
    pub fn shutdown(&mut self) -> io::Result<()> {
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

    fn ensure_writer(&mut self) -> io::Result<()> {
        if self.active_writer.is_none() {
            let path = self
                .session_dir
                .join(format!("segment-{:04}.bin", self.next_segment_id));
            self.next_segment_id += 1;
            let file = BufWriter::new(fs::File::create(&path)?);
            let writer = SegmentWriter::with_key(file, self.take_key());
            self.active_writer = Some(writer);
        }
        Ok(())
    }

    fn finalize_active_segment(&mut self) -> io::Result<()> {
        let writer = self
            .active_writer
            .take()
            .expect("called with active writer");
        let nonce_start_for_segment =
            writer.key().nonce_counter() - u64::from(writer.frame_count());
        let total_rows = writer.total_rows();
        let (buf_writer, key) = writer.finalize()?;
        let inner = buf_writer
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)?;
        let metadata = inner.metadata()?;
        let file_size = metadata.len();

        self.key = Some(key);

        let seg_id = self.next_segment_id - 1;
        let path = self.session_dir.join(format!("segment-{seg_id:04}.bin"));

        let first_row_index = if let Some(last) = self.segments.last() {
            last.first_row_index + last.row_count
        } else {
            0
        };

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

    fn take_key(&mut self) -> ArchiveKey {
        self.key.take().expect("key taken while writer active")
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
        let Ok(stat) = statvfs(path) else {
            return false;
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
        assert!(!mgr.pending_rows.is_empty());
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
        assert!(mgr.pending_rows.is_empty());
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
        mgr.max_disk_bytes = bytes_before / 2;
        mgr.prune_if_needed().unwrap();

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
