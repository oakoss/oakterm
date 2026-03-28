//! Unix PTY implementation using rustix.

use crate::WinSize;
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use rustix::fs::OFlags;
use rustix::pty::{OpenptFlags, openpt};
use rustix::termios::{Winsize, tcsetwinsize};
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

/// A pseudoterminal with a master fd and child process.
pub struct Pty {
    master: OwnedFd,
    child: Child,
}

impl Pty {
    /// Allocate a PTY, set the window size, and spawn a command.
    ///
    /// # Errors
    /// Returns an error if PTY allocation, window size setting, or process
    /// spawning fails.
    pub fn spawn(program: &str, args: &[&str], size: WinSize) -> io::Result<Self> {
        let master = open_master()?;
        let slave = open_slave(&master)?;

        set_winsize(&slave, size)?;

        let child = spawn_child(program, args, slave)?;

        Ok(Self { master, child })
    }

    /// Borrow the master fd for async I/O wrapping.
    #[must_use]
    pub fn master_fd(&self) -> BorrowedFd<'_> {
        self.master.as_fd()
    }

    /// Get a raw fd for the master side (for tokio `AsyncFd`).
    #[must_use]
    pub fn master_raw_fd(&self) -> std::os::unix::io::RawFd {
        use std::os::unix::io::AsRawFd;
        self.master.as_raw_fd()
    }

    /// Write data to the PTY master (sends input to the child process).
    ///
    /// # Errors
    /// Returns an error if the write fails.
    pub fn write(&self, data: &[u8]) -> io::Result<usize> {
        let borrowed = self.master.as_fd();
        rustix::io::write(borrowed, data)
            .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
    }

    /// Update the PTY window size.
    ///
    /// # Errors
    /// Returns an error if the ioctl fails.
    pub fn resize(&self, size: WinSize) -> io::Result<()> {
        set_winsize(&self.master, size)
    }

    /// Access the child process.
    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Get the child's PID.
    ///
    /// After the child exits, this PID may be recycled by the OS.
    #[must_use]
    pub fn child_pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Closing the master fd (which happens after this Drop runs)
        // sends SIGHUP to the child. Kill + wait ensures no zombies.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn open_master() -> io::Result<OwnedFd> {
    let master = openpt(OpenptFlags::RDWR)?;
    rustix::pty::grantpt(&master)?;
    rustix::pty::unlockpt(&master)?;
    Ok(master)
}

fn open_slave(master: &OwnedFd) -> io::Result<OwnedFd> {
    let name = rustix::pty::ptsname(master, Vec::new())?;
    let slave = rustix::fs::open(name.as_c_str(), OFlags::RDWR, rustix::fs::Mode::empty())?;
    Ok(slave)
}

fn set_winsize(fd: impl AsFd, size: WinSize) -> io::Result<()> {
    let ws = Winsize {
        ws_col: size.cols,
        ws_row: size.rows,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    tcsetwinsize(fd, ws)?;
    Ok(())
}

fn spawn_child(program: &str, args: &[&str], slave: OwnedFd) -> io::Result<Child> {
    // Dup before transferring ownership — no unsafe fd aliasing.
    let stdout_fd = rustix::io::dup(&slave)?;
    let stderr_fd = rustix::io::dup(&slave)?;

    let stdin: Stdio = slave.into();
    let stdout: Stdio = stdout_fd.into();
    let stderr: Stdio = stderr_fd.into();

    let mut cmd = Command::new(program);
    cmd.args(args).stdin(stdin).stdout(stdout).stderr(stderr);

    // SAFETY: pre_exec runs between fork and exec in the child process.
    // setsid() is async-signal-safe. ioctl(TIOCSCTTY) on fd 0 (stdin,
    // which is the slave PTY) acquires it as the controlling terminal.
    unsafe {
        cmd.pre_exec(|| {
            rustix::process::setsid()
                .map_err(|e| io::Error::new(e.kind(), format!("setsid failed: {e}")))?;

            // Fd 0 is stdin (the slave PTY). Make it the controlling terminal.
            rustix::process::ioctl_tiocsctty(BorrowedFd::borrow_raw(0))?;

            Ok(())
        });
    }

    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::process::{Pid, test_kill_process};

    fn spawn_sh(size: WinSize) -> Pty {
        // No -l: avoid login shell RC files that produce output and fill
        // the PTY buffer (nobody drains the master in these sync tests).
        Pty::spawn("/bin/sh", &[], size).expect("spawn shell")
    }

    #[test]
    fn spawn_shell_has_pid() {
        let size = WinSize { cols: 80, rows: 24 };
        let pty = spawn_sh(size);
        assert!(pty.child_pid() > 0);
        // Drop handles cleanup.
    }

    #[test]
    fn resize_after_spawn() {
        let size = WinSize { cols: 80, rows: 24 };
        let pty = spawn_sh(size);
        pty.resize(WinSize {
            cols: 120,
            rows: 40,
        })
        .expect("resize");
        // Drop handles cleanup (kill + wait).
    }

    #[test]
    fn drop_cleans_up_child() {
        let size = WinSize { cols: 80, rows: 24 };
        let pty = spawn_sh(size);
        let pid = pty.child_pid();
        drop(pty); // Should kill + wait, no zombie.

        #[allow(clippy::cast_possible_wrap)] // PID fits in i32
        let result = test_kill_process(unsafe { Pid::from_raw_unchecked(pid as i32) });
        assert!(result.is_err(), "child should be gone after drop");
    }
}
