// PTY operations require unsafe for Command::pre_exec (child process setup)
// and libc::getpwuid for shell resolution fallback.
#![allow(unsafe_code)]

//! Platform PTY allocation, child process spawning, and I/O.
//!
//! Unix: uses `posix_openpt` + `grantpt` + `unlockpt` via rustix.
//! Windows: stub (`ConPTY` support deferred).

#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub use unix::Pty;

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

/// Terminal window size in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WinSize {
    pub cols: u16,
    pub rows: u16,
}

/// Specification for a PTY-attached child process.
///
/// `program == None` means "default login shell", resolved at spawn time via
/// `$SHELL` → `getpwuid` `pw_shell` → `/bin/sh`. The `$SHELL` and passwd
/// candidates are validated with `access(X_OK)`; failures fall through to
/// the next tier with a `WARN` log. `/bin/sh` is the unconditional terminal
/// fallback (not validated — if it's missing, `Pty::spawn` returns the
/// underlying spawn error).
///
/// `cwd == None` means "inherit parent's cwd". `cwd == Some(invalid)` falls
/// back to `$HOME` → `/` with a `WARN` log.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CommandSpec {
    /// Program to run. `None` = default login shell.
    pub program: Option<PathBuf>,
    /// Arguments. Ignored when `program == None`.
    pub args: Vec<OsString>,
    /// Working directory. `None` = inherit. `Some(invalid)` falls back to home.
    pub cwd: Option<PathBuf>,
}

impl CommandSpec {
    /// Build a spec for spawning `program` with `args` in `cwd`.
    ///
    /// `program == None` defers to default-shell resolution. `args` is ignored
    /// in that case (the default shell is invoked with no args). Caller
    /// boundaries (e.g., the daemon's wire-protocol parser) are responsible
    /// for rejecting `program: None, args: non-empty` if that combination is
    /// meaningful as user input.
    #[must_use]
    pub fn new(program: Option<PathBuf>, args: Vec<OsString>, cwd: Option<PathBuf>) -> Self {
        Self { program, args, cwd }
    }
}

/// Spawn the user's default login shell in a new PTY.
///
/// Resolves the shell via `$SHELL` → `getpwuid` → `/bin/sh`, with executable
/// validation at each tier. Sets `argv[0]` to `-{basename}` so the shell
/// behaves as a login shell (POSIX convention).
///
/// # Errors
/// Returns an error if PTY allocation or process spawning fails.
pub fn spawn_shell(size: WinSize) -> io::Result<Pty> {
    Pty::spawn(CommandSpec::default(), size)
}

/// Spawn a command in a new PTY.
///
/// `spec.program == None` defers to [`spawn_shell`] semantics.
///
/// # Errors
/// Returns an error if PTY allocation or process spawning fails.
pub fn spawn_command(spec: CommandSpec, size: WinSize) -> io::Result<Pty> {
    Pty::spawn(spec, size)
}

/// Resize a PTY via a borrowed file descriptor.
///
/// # Errors
/// Returns an error if the ioctl fails.
#[cfg(unix)]
pub fn resize_fd(
    fd: impl std::os::unix::io::AsFd,
    cols: u16,
    rows: u16,
    xpixel: u16,
    ypixel: u16,
) -> io::Result<()> {
    use rustix::termios::{Winsize, tcsetwinsize};
    let ws = Winsize {
        ws_col: cols,
        ws_row: rows,
        ws_xpixel: xpixel,
        ws_ypixel: ypixel,
    };
    tcsetwinsize(fd, ws)?;
    Ok(())
}
