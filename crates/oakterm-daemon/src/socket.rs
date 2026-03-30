//! Unix socket path resolution and listener setup per Spec-0001.

use std::io;
use std::path::PathBuf;

/// Resolve the daemon socket path per Spec-0001.
///
/// - Linux: `$XDG_RUNTIME_DIR/oakterm/socket`
/// - macOS: `$TMPDIR/oakterm-<uid>/socket`
///
/// Creates the parent directory with `0700` permissions.
///
/// # Errors
/// Returns an error if the runtime directory cannot be determined or created.
pub fn socket_path() -> io::Result<PathBuf> {
    let dir = socket_dir()?;
    Ok(dir.join("socket"))
}

/// RAII guard holding an exclusive flock on the startup lock file.
#[non_exhaustive]
pub struct StartupLock {
    _file: std::fs::File,
}

/// Acquire the exclusive startup lock. Blocks until available.
/// Prevents two GUI clients from racing to spawn the daemon.
///
/// # Errors
/// Returns an error if the lock file cannot be created or locked.
pub fn acquire_startup_lock() -> io::Result<StartupLock> {
    let dir = socket_dir()?;
    let path = dir.join("lock");
    let file = std::fs::File::create(&path)?;

    // Try non-blocking first; if contended, log and block.
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => return Ok(StartupLock { _file: file }),
        Err(e) if e == rustix::io::Errno::WOULDBLOCK => {
            tracing::info!(
                lock_path = %path.display(),
                "waiting for another oakterm to finish starting",
            );
        }
        Err(e) => return Err(io::Error::from_raw_os_error(e.raw_os_error())),
    }

    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))?;
    Ok(StartupLock { _file: file })
}

#[cfg(target_os = "linux")]
fn socket_dir() -> io::Result<PathBuf> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR not set"))?;
    let dir = PathBuf::from(runtime_dir).join("oakterm");
    ensure_dir(&dir)?;
    Ok(dir)
}

#[cfg(target_os = "macos")]
fn socket_dir() -> io::Result<PathBuf> {
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let uid = rustix::process::getuid().as_raw();
    let dir = PathBuf::from(tmpdir).join(format!("oakterm-{uid}"));
    ensure_dir(&dir)?;
    Ok(dir)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn socket_dir() -> io::Result<PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "unsupported platform for Unix sockets",
    ))
}

fn ensure_dir(dir: &std::path::Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_resolves() {
        let path = socket_path().expect("socket_path should resolve");
        assert!(path.ends_with("socket"));
        assert!(path.parent().unwrap().exists());
    }

    #[test]
    fn socket_path_is_in_oakterm_dir() {
        let path = socket_path().expect("socket_path");
        let parent = path.parent().unwrap();
        let parent_name = parent.file_name().unwrap().to_str().unwrap();
        // On macOS: oakterm-<uid>, on Linux: oakterm
        assert!(
            parent_name.starts_with("oakterm"),
            "parent dir should start with 'oakterm', got: {parent_name}"
        );
    }
}
