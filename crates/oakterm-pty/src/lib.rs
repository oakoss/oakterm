// PTY operations require unsafe for Command::pre_exec (child process setup).
#![allow(unsafe_code)]

//! Platform PTY allocation, child process spawning, and I/O.
//!
//! Unix: uses `posix_openpt` + `grantpt` + `unlockpt` via rustix.
//! Windows: stub (`ConPTY` support deferred).

#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub use unix::Pty;

use std::io;

/// Terminal window size in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WinSize {
    pub cols: u16,
    pub rows: u16,
}

/// Spawn a login shell in a new PTY with the given window size.
///
/// Uses `$SHELL` if set, otherwise falls back to `/bin/sh`.
///
/// # Errors
/// Returns an error if PTY allocation or process spawning fails.
pub fn spawn_shell(size: WinSize) -> io::Result<Pty> {
    let shell = match std::env::var("SHELL") {
        Ok(s) if !s.is_empty() => s,
        _ => "/bin/sh".to_string(),
    };
    Pty::spawn(&shell, &["-l"], size)
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
