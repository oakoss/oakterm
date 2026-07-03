//! Storage seam for the archive core: the filesystem operations
//! `ArchiveCore` needs, behind a trait so tests can inject faults
//! (write errors, finalize failures, low disk) that would otherwise
//! require exhausting a real disk.

use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// Minimum free disk space (1 GB) before archiving pauses.
const MIN_FREE_BYTES: u64 = 1024 * 1024 * 1024;

/// Minimum free disk percentage (5%) before archiving pauses.
const MIN_FREE_PERCENT: u64 = 5;

/// Filesystem operations behind the archive core. One implementor is
/// the real disk; test fakes force each error channel at will.
///
/// Removal of a missing target is success, not an error — callers
/// retry removals after partial failures and must not trip on work
/// already done.
pub(crate) trait SegmentStorage {
    type Writer: Write;

    /// Create the session directory, private to the owning user.
    fn init_dir(&self, dir: &Path) -> io::Result<()>;

    fn create(&self, path: &Path) -> io::Result<Self::Writer>;

    /// Flush and close a finished segment writer, returning the stored
    /// size in bytes. `path` must be the path this writer was `create`d
    /// with. Failure here is the deferred-IO error site (ENOSPC/EIO
    /// surfacing at close).
    fn finish(&self, writer: Self::Writer, path: &Path) -> io::Result<u64>;

    fn remove_file(&self, path: &Path) -> io::Result<()>;

    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;

    fn remove_dir_all(&self, dir: &Path) -> io::Result<()>;

    fn has_enough_space(&self, dir: &Path) -> bool;
}

pub(crate) struct DiskStorage;

impl SegmentStorage for DiskStorage {
    type Writer = BufWriter<fs::File>;

    fn init_dir(&self, dir: &Path) -> io::Result<()> {
        fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    fn create(&self, path: &Path) -> io::Result<Self::Writer> {
        Ok(BufWriter::new(fs::File::create(path)?))
    }

    fn finish(&self, writer: Self::Writer, _path: &Path) -> io::Result<u64> {
        writer
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)
            .and_then(|inner| inner.metadata())
            .map(|metadata| metadata.len())
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        match fs::remove_file(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    fn remove_dir_all(&self, dir: &Path) -> io::Result<()> {
        match fs::remove_dir_all(dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }

    fn has_enough_space(&self, dir: &Path) -> bool {
        #[cfg(unix)]
        {
            use rustix::fs::statvfs;
            let stat = match statvfs(dir) {
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
            let _ = dir;
            true
        }
    }
}

/// In-memory storage with injectable faults. Each flag forces one
/// error channel; toggling between operations drives the core through
/// states a real disk only reaches when full or failing.
#[cfg(test)]
pub(crate) mod fake {
    use super::SegmentStorage;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    // Independent fault channels, not a state machine — tests combine them.
    #[allow(clippy::struct_excessive_bools)]
    #[derive(Default)]
    pub(crate) struct FakeState {
        pub(crate) files: HashMap<PathBuf, Vec<u8>>,
        /// Fail every `Write::write` — frame writes and, during key
        /// salvage, the seek-table footer, so this also drives the
        /// key-loss era reset.
        pub(crate) fail_writes: bool,
        /// Fail exactly one `Write::write`, then heal — a transient
        /// frame-write error whose salvage footer succeeds, the branch
        /// where the key survives and prior segments stay readable.
        pub(crate) fail_next_write: bool,
        /// Fail `finish` — the deferred ENOSPC/EIO-at-close site. The
        /// key has already been salvaged when this fires.
        pub(crate) fail_finish: bool,
        /// Fail `create` — fires before `take_key`, so the key must
        /// survive without a salvage.
        pub(crate) fail_create: bool,
        pub(crate) low_disk: bool,
        pub(crate) fail_remove: bool,
    }

    #[derive(Clone, Default)]
    pub(crate) struct FakeStorage {
        pub(crate) state: Rc<RefCell<FakeState>>,
    }

    pub(crate) struct FakeWriter {
        buf: Vec<u8>,
        state: Rc<RefCell<FakeState>>,
    }

    impl Write for FakeWriter {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            {
                let mut state = self.state.borrow_mut();
                if state.fail_next_write {
                    state.fail_next_write = false;
                    return Err(io::Error::other("injected transient write failure"));
                }
                if state.fail_writes {
                    return Err(io::Error::other("injected write failure"));
                }
            }
            self.buf.extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl SegmentStorage for FakeStorage {
        type Writer = FakeWriter;

        fn init_dir(&self, _dir: &Path) -> io::Result<()> {
            Ok(())
        }

        fn create(&self, path: &Path) -> io::Result<Self::Writer> {
            let mut state = self.state.borrow_mut();
            if state.fail_create {
                return Err(io::Error::other("injected create failure"));
            }
            // Materialize the file like `File::create` does, so removal
            // of a torn (never-finished) segment is observable.
            state.files.insert(path.to_path_buf(), Vec::new());
            Ok(FakeWriter {
                buf: Vec::new(),
                state: Rc::clone(&self.state),
            })
        }

        fn finish(&self, writer: Self::Writer, path: &Path) -> io::Result<u64> {
            let mut state = self.state.borrow_mut();
            if state.fail_finish {
                return Err(io::Error::other("injected finish failure"));
            }
            let size = u64::try_from(writer.buf.len()).expect("usize fits u64");
            state.files.insert(path.to_path_buf(), writer.buf);
            Ok(size)
        }

        fn remove_file(&self, path: &Path) -> io::Result<()> {
            let mut state = self.state.borrow_mut();
            if state.fail_remove {
                return Err(io::Error::other("injected remove failure"));
            }
            state.files.remove(path);
            Ok(())
        }

        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            self.state
                .borrow()
                .files
                .get(path)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such fake file"))
        }

        fn remove_dir_all(&self, dir: &Path) -> io::Result<()> {
            self.state
                .borrow_mut()
                .files
                .retain(|path, _| !path.starts_with(dir));
            Ok(())
        }

        fn has_enough_space(&self, _dir: &Path) -> bool {
            !self.state.borrow().low_disk
        }
    }
}
