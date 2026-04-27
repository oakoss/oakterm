//! Unix PTY implementation using rustix.

use crate::{CommandSpec, WinSize};
use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
use rustix::fs::{Access, OFlags};
use rustix::pty::{OpenptFlags, openpt};
use rustix::termios::{Winsize, tcsetwinsize};
use std::ffi::CStr;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use tracing::warn;

/// A pseudoterminal with a master fd and child process.
pub struct Pty {
    master: OwnedFd,
    child: Child,
    reaped: bool,
}

impl Pty {
    /// Allocate a PTY, set the window size, and spawn a command per `spec`.
    ///
    /// # Errors
    /// Returns an error if PTY allocation, window size setting, or process
    /// spawning fails.
    pub fn spawn(spec: CommandSpec, size: WinSize) -> io::Result<Self> {
        let master = open_master()?;
        let slave = open_slave(&master)?;

        set_winsize(&slave, size)?;

        let child = spawn_child(spec, slave)?;

        Ok(Self {
            master,
            child,
            reaped: false,
        })
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

    /// Get the child's PID.
    ///
    /// After the child exits, this PID may be recycled by the OS.
    #[must_use]
    pub fn child_pid(&self) -> u32 {
        self.child.id()
    }

    /// Wait for the child process to exit and return its status.
    ///
    /// # Errors
    /// Returns the underlying I/O error from `waitpid(2)`; typically only
    /// fails if the child was already reaped out-of-band.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }

    /// Poll the child process without blocking.
    ///
    /// # Errors
    /// Returns the underlying I/O error from `waitpid(2)`.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.reaped = true;
        }
        Ok(status)
    }

    /// Send SIGKILL to the child. Idempotent: errors when the child is
    /// already gone are intentionally swallowed.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }

    /// True if the child has been reaped via `wait()` / `try_wait()`.
    #[must_use]
    pub fn is_reaped(&self) -> bool {
        self.reaped
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Already reaped via wait()/try_wait(): nothing left for Drop to do.
        if self.reaped {
            return;
        }

        // Closing the master fd (which happens after this Drop runs)
        // sends SIGHUP to the child. Kill + wait ensures no zombies even if
        // SIGHUP is ignored.
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

fn spawn_child(spec: CommandSpec, slave: OwnedFd) -> io::Result<Child> {
    // Dup before transferring ownership — no unsafe fd aliasing.
    let stdout_fd = rustix::io::dup(&slave)?;
    let stderr_fd = rustix::io::dup(&slave)?;

    let stdin: Stdio = slave.into();
    let stdout: Stdio = stdout_fd.into();
    let stderr: Stdio = stderr_fd.into();

    let CommandSpec { program, args, cwd } = spec;

    let (program, login_mode) = match program {
        Some(p) => (p, false),
        None => (resolve_default_shell(), true),
    };

    let mut cmd = Command::new(&program);
    cmd.args(&args).stdin(stdin).stdout(stdout).stderr(stderr);

    if login_mode {
        // Universal login-shell convention: argv[0] starts with '-'. Works
        // for sh/dash/bash/zsh and csh/tcsh, unlike `-l` which is bash/zsh
        // only. See std::os::unix::process::CommandExt::arg0.
        let basename = program.file_name().and_then(|n| n.to_str()).unwrap_or("sh");
        cmd.arg0(format!("-{basename}"));
    }

    apply_cwd(&mut cmd, cwd.as_deref());

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

/// Resolve the user's default shell via three-tier fallback.
///
/// Order: `$SHELL` → `getpwuid(getuid()).pw_shell` → `/bin/sh`. Each candidate
/// is checked with `access(X_OK)`; non-executable candidates fall through with
/// a `WARN` log so users can diagnose without strace.
fn resolve_default_shell() -> PathBuf {
    if let Ok(env_shell) = std::env::var("SHELL") {
        if !env_shell.is_empty() {
            let env_path = Path::new(&env_shell);
            if is_executable(env_path) {
                return PathBuf::from(env_shell);
            }
            warn!(
                shell = %env_shell,
                "$SHELL is not executable, falling back to passwd entry"
            );
        }
    }

    if let Some(passwd_shell) = passwd_shell() {
        if is_executable(&passwd_shell) {
            return passwd_shell;
        }
        warn!(
            shell = %passwd_shell.display(),
            "passwd shell is not executable, falling back to /bin/sh"
        );
    }

    PathBuf::from("/bin/sh")
}

fn is_executable(path: &Path) -> bool {
    rustix::fs::access(path, Access::EXEC_OK).is_ok()
}

/// Look up the current user's shell via `getpwuid(getuid())`.
fn passwd_shell() -> Option<PathBuf> {
    // SAFETY: getpwuid returns a pointer into a process-wide static buffer
    // shared by the entire passwd family (getpwnam, getpwent, getlogin on
    // some platforms). We read pw_shell into an owned PathBuf before
    // returning, so a subsequent passwd call can't dangle our pointer.
    // The remaining hazard is concurrent passwd calls from any thread
    // racing on that shared buffer; the daemon serializes spawns under the
    // PaneManager mutex today, but this is an implicit contract that
    // external library callers don't share. Migrating to getpwuid_r is
    // tracked separately so the contract becomes structural.
    unsafe {
        let entry = libc::getpwuid(libc::getuid());
        if entry.is_null() {
            return None;
        }
        let shell_ptr = (*entry).pw_shell;
        if shell_ptr.is_null() {
            return None;
        }
        let shell = CStr::from_ptr(shell_ptr).to_str().ok()?;
        if shell.is_empty() {
            return None;
        }
        Some(PathBuf::from(shell))
    }
}

/// Apply a working directory to `cmd`, validating and falling back when needed.
///
/// `None` is a no-op (child inherits parent's cwd). `Some(invalid)` logs a
/// `WARN` and falls back to `$HOME` → `/`.
fn apply_cwd(cmd: &mut Command, cwd: Option<&Path>) {
    let Some(requested) = cwd else {
        return;
    };
    if requested.is_dir() {
        cmd.current_dir(requested);
        return;
    }
    let fallback = home_dir()
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| PathBuf::from("/"));
    warn!(
        requested = %requested.display(),
        fallback = %fallback.display(),
        "requested cwd is not a directory, falling back"
    );
    cmd.current_dir(fallback);
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::process::{Pid, test_kill_process};
    use std::io::Read;

    fn spawn_sh(size: WinSize) -> Pty {
        // No login mode: avoid login shell RC files that produce output and
        // fill the PTY buffer (nobody drains the master in these sync tests).
        let spec = CommandSpec {
            program: Some(PathBuf::from("/bin/sh")),
            args: vec![],
            cwd: None,
        };
        Pty::spawn(spec, size).expect("spawn shell")
    }

    /// Drain the PTY master until EOF (i.e., the child has exited and the
    /// slave side closed) or `max` bytes accumulate. Used by tests that need
    /// to inspect child output. Caller is responsible for ensuring the child
    /// will exit (a short-lived command, or send `exit\n` for a shell).
    fn drain_master(pty: &Pty, max: usize) -> Vec<u8> {
        // Dup so closing our File doesn't affect the Pty's master fd.
        // Mirror spawn_child's pattern (rustix::io::dup of a borrowed fd)
        // so we don't introduce libc::dup's -1-on-failure footgun.
        let dup_fd = rustix::io::dup(pty.master_fd()).expect("dup master fd");
        let mut file: std::fs::File = dup_fd.into();
        let mut buf = vec![0u8; max];
        let mut total = 0;
        while total < max {
            match file.read(&mut buf[total..]) {
                // Both EOF (0 bytes) and Err mean we're done draining.
                Ok(0) | Err(_) => break,
                Ok(n) => total += n,
            }
        }
        buf.truncate(total);
        buf
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

    #[test]
    fn spawn_command_with_explicit_program() {
        // /bin/echo exits after printing its args, so master EOFs naturally.
        // Verify args were actually passed by checking the output.
        let spec = CommandSpec {
            program: Some(PathBuf::from("/bin/echo")),
            args: vec!["oakterm-spawn-test".into()],
            cwd: None,
        };
        let pty = Pty::spawn(spec, WinSize { cols: 80, rows: 24 }).expect("spawn echo");
        let out = drain_master(&pty, 256);
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("oakterm-spawn-test"),
            "expected echo to output its arg, got: {text:?}"
        );
    }

    #[test]
    fn spawn_command_explicit_program_no_login_argv0() {
        // Explicit program must NOT be invoked in login mode (no '-' argv[0]).
        // Use sh -c so $0 is set by the shell and we can inspect it.
        let spec = CommandSpec {
            program: Some(PathBuf::from("/bin/sh")),
            args: vec!["-c".into(), "echo argv0=$0".into()],
            cwd: None,
        };
        let pty = Pty::spawn(spec, WinSize { cols: 80, rows: 24 }).expect("spawn sh -c");
        let out = drain_master(&pty, 256);
        let text = String::from_utf8_lossy(&out);
        // sh -c sets $0 to "sh" (or the script name if -s), never starting with '-'.
        assert!(
            !text.contains("argv0=-"),
            "explicit program must not be launched in login mode, got: {text:?}"
        );
    }

    #[test]
    fn spawn_command_invalid_cwd_falls_back() {
        // Bad cwd should NOT cause spawn to fail; we fall back to $HOME → /.
        // Verify the fallback actually happened by running pwd and checking
        // we landed somewhere reasonable (not the bogus path, not nothing).
        let spec = CommandSpec {
            program: Some(PathBuf::from("/bin/sh")),
            args: vec!["-c".into(), "pwd".into()],
            cwd: Some(PathBuf::from(
                "/this/path/definitely/does/not/exist/oakterm",
            )),
        };
        let pty = Pty::spawn(spec, WinSize { cols: 80, rows: 24 })
            .expect("spawn should succeed despite invalid cwd");
        let out = drain_master(&pty, 256);
        let text = String::from_utf8_lossy(&out);
        assert!(
            !text.contains("/this/path/definitely/does/not/exist"),
            "child should have fallen back away from the bogus cwd, got: {text:?}"
        );
        // Must report *some* directory (HOME, /, or the inherited daemon cwd).
        assert!(
            text.trim_start().starts_with('/'),
            "expected pwd output to start with '/', got: {text:?}"
        );
    }

    #[test]
    fn spawn_command_with_valid_cwd() {
        // sh -c 'pwd' exits after printing, so master will EOF naturally.
        let spec = CommandSpec {
            program: Some(PathBuf::from("/bin/sh")),
            args: vec!["-c".into(), "pwd".into()],
            cwd: Some(PathBuf::from("/tmp")),
        };
        let pty = Pty::spawn(spec, WinSize { cols: 80, rows: 24 }).expect("spawn sh -c pwd");
        let out = drain_master(&pty, 256);
        let text = String::from_utf8_lossy(&out);
        // /tmp may be a symlink (e.g., to /private/tmp on macOS), so accept either.
        assert!(
            text.contains("/tmp") || text.contains("/private/tmp"),
            "expected pwd output to contain /tmp, got: {text:?}"
        );
    }

    #[test]
    fn spawn_default_shell_uses_login_argv0() {
        // Default shell with `echo argv0=$0; exit` reports a leading dash on argv[0].
        // The `exit` ensures the shell terminates so our drain hits EOF.
        let spec = CommandSpec {
            program: None,
            args: vec![],
            cwd: None,
        };
        let pty = Pty::spawn(spec, WinSize { cols: 80, rows: 24 }).expect("spawn default shell");
        pty.write(b"echo argv0=$0\nexit\n")
            .expect("write to master");
        let out = drain_master(&pty, 8192);
        let text = String::from_utf8_lossy(&out);
        assert!(
            text.contains("argv0=-"),
            "expected argv[0] to start with '-' (login shell), got: {text:?}"
        );
    }

    #[test]
    fn resolve_default_shell_returns_executable() {
        let shell = resolve_default_shell();
        assert!(
            is_executable(&shell),
            "resolved shell {} is not executable",
            shell.display()
        );
    }
}
