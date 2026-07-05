//! Pane domain: PTY lifecycle state, per-pane locking, and topology tracking.

use oakterm_terminal::grid::ScreenSet;
use std::ffi::OsString;
use std::os::unix::io::RawFd;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

/// PTY lifecycle state machine.
///
/// Transitions: `NotSpawned` -> `Running` | `Failed` (terminal);
/// `Running` -> `Exited` (terminal). First client Resize triggers spawn.
pub(crate) enum PtyState {
    /// Waiting for first client Resize to determine dimensions.
    NotSpawned,
    /// Master fd for writes and resizes, plus child PID for status reporting.
    /// The `Pty` struct is owned by the read loop.
    ///
    /// `cancel` signals the read loop to exit promptly on `ClosePane`. Drop
    /// or send `()`; the loop's select sees it, breaks out, drops the `Pty`,
    /// and `Pty::Drop` kills + reaps the child. Without this, an idle shell
    /// (no output) leaves the loop blocked on `readable()` indefinitely.
    Running {
        fd: RawFd,
        pid: u32,
        cancel: tokio::sync::oneshot::Sender<()>,
    },
    /// PTY spawn failed; terminal state. The error string is returned to any
    /// client that sends a subsequent Resize.
    Failed(String),
    /// PTY read loop exited (master fd EOF or error).
    Exited { exit_code: i32 },
}

/// Per-pane state: screen buffer, PTY lifecycle, and dirty tracking.
pub(crate) struct PaneState {
    pub(crate) screens: ScreenSet,
    pub(crate) pty_state: PtyState,
    /// Sequence number of the last VT parse; clients compare to detect changes.
    pub(crate) dirty_seqno: u64,
    /// Shell command for PTY spawn. Empty = default shell.
    /// Shlex-split into program + args at spawn time (first Resize).
    pub(crate) command: String,
    /// Working directory for PTY spawn. Empty = inherit daemon's cwd.
    pub(crate) cwd: String,
    /// Tombstone set by `ClosePane` under this pane's lock. A cloned
    /// `SharedPane` can outlive removal from the map; consumers observing
    /// `closed` treat the pane as absent, so a request racing a close
    /// resolves to `UnknownPane` like it did under the single-lock design.
    pub(crate) closed: bool,
}

impl PaneState {
    /// Advance the client-visible dirty counter after a VT parse or
    /// resize. Screen switches (alt/primary) reset the active grid's
    /// seqno space, but `dirty_seqno` must keep increasing so clients
    /// know state changed.
    pub(crate) fn bump_dirty(&mut self) {
        self.dirty_seqno = self.dirty_seqno.max(self.screens.active_grid().seqno) + 1;
    }
}

/// Convert the wire-protocol `command` (single shell-style string) and `cwd`
/// into a [`oakterm_pty::CommandSpec`].
///
/// `command == ""` means default shell. Non-empty `command` is shlex-split:
/// `"htop --tree"` becomes `program=htop, args=["--tree"]`. Malformed quoting
/// returns `Err` so the daemon can surface the parse failure to the client
/// instead of silently spawning a default shell.
pub(crate) fn build_command_spec(
    command: &str,
    cwd: &str,
) -> Result<oakterm_pty::CommandSpec, String> {
    let cwd = (!cwd.is_empty()).then(|| PathBuf::from(cwd));
    if command.is_empty() {
        return Ok(oakterm_pty::CommandSpec::new(None, vec![], cwd));
    }
    let parts = shlex::split(command).ok_or_else(|| format!("shlex parse failed: {command:?}"))?;
    let mut iter = parts.into_iter();
    // Reject empty program tokens (e.g., command = "''" expands to a single
    // empty string). Without this, Command::new("") fails downstream with a
    // generic spawn error and the wrong error code.
    let program = iter
        .next()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| format!("empty program token after shlex split: {command:?}"))?;
    let args: Vec<OsString> = iter.map(OsString::from).collect();
    Ok(oakterm_pty::CommandSpec::new(
        Some(PathBuf::from(program)),
        args,
        cwd,
    ))
}

/// A pane behind its own lock: PTY bursts and client reads on one pane
/// never contend with another pane's traffic.
pub(crate) type SharedPane = Arc<Mutex<PaneState>>;

/// Tracks all panes with monotonic ID assignment. Guards topology only
/// (create/lookup/remove/focus); pane contents live behind per-pane
/// locks.
///
/// Lock order: take the manager lock, clone the `SharedPane`, release,
/// then lock the pane — never hold the manager lock across a pane lock,
/// and never hold two pane locks at once.
pub(crate) struct PaneManager {
    panes: std::collections::HashMap<u32, SharedPane>,
    next_id: u32,
    focused_pane: Option<u32>,
}

impl PaneManager {
    pub(crate) fn new() -> Self {
        Self {
            panes: std::collections::HashMap::new(),
            next_id: 0,
            focused_pane: None,
        }
    }

    /// Create a pane with the given grid dimensions. Returns the assigned ID.
    pub(crate) fn create(&mut self, cols: u16, rows: u16, command: String, cwd: String) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.panes.insert(
            id,
            Arc::new(Mutex::new(PaneState {
                screens: ScreenSet::new(cols, rows),
                pty_state: PtyState::NotSpawned,
                dirty_seqno: 0,
                command,
                cwd,
                closed: false,
            })),
        );
        // Auto-focus the first pane created.
        if self.focused_pane.is_none() {
            self.focused_pane = Some(id);
        }
        id
    }

    pub(crate) fn len(&self) -> usize {
        self.panes.len()
    }

    pub(crate) fn get(&self, id: u32) -> Option<SharedPane> {
        self.panes.get(&id).cloned()
    }

    /// If the removed pane was focused, focus falls back to any
    /// remaining pane. Callers enforce the last-pane rule (`ClosePane`
    /// refuses to remove the final pane); `remove` itself will empty
    /// the topology if asked.
    pub(crate) fn remove(&mut self, id: u32) -> Option<SharedPane> {
        let removed = self.panes.remove(&id)?;
        if self.focused_pane == Some(id) {
            self.focused_pane = self.panes.keys().next().copied();
        }
        Some(removed)
    }

    /// Gated behind `cfg(test)`: no production reader exists yet, only
    /// tests and future layout work.
    #[cfg(test)]
    pub(crate) fn focused(&self) -> Option<u32> {
        self.focused_pane
    }

    /// Focus a pane. Returns false if the pane is unknown.
    pub(crate) fn focus(&mut self, id: u32) -> bool {
        if self.panes.contains_key(&id) {
            self.focused_pane = Some(id);
            true
        } else {
            false
        }
    }

    /// Clone out `(id, SharedPane)` pairs for iterate-all paths, so
    /// callers can release the manager lock before locking each pane.
    /// Panes tombstoned after the snapshot still carry `closed`; lock
    /// each and check.
    pub(crate) fn snapshot(&self) -> Vec<(u32, SharedPane)> {
        self.panes
            .iter()
            .map(|(&id, p)| (id, Arc::clone(p)))
            .collect()
    }
}

/// Look up and lock a pane, honoring the `PaneManager` lock order (the
/// manager guard is released before the pane lock is taken). A pane
/// tombstoned by `ClosePane` reads as absent.
pub(crate) async fn lock_live_pane(
    panes: &Arc<Mutex<PaneManager>>,
    id: u32,
) -> Option<tokio::sync::OwnedMutexGuard<PaneState>> {
    let pane = panes.lock().await.get(id)?;
    let guard = pane.lock_owned().await;
    (!guard.closed).then_some(guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_command_spec_empty_command_default_shell() {
        let spec = build_command_spec("", "").expect("default spec");
        assert!(spec.program.is_none());
        assert!(spec.args.is_empty());
        assert!(spec.cwd.is_none());
    }

    #[test]
    fn build_command_spec_empty_command_with_cwd() {
        let spec = build_command_spec("", "/tmp").expect("default shell with cwd");
        assert!(spec.program.is_none());
        assert!(spec.args.is_empty());
        assert_eq!(spec.cwd, Some(PathBuf::from("/tmp")));
    }

    #[test]
    fn build_command_spec_program_only() {
        let spec = build_command_spec("htop", "").expect("htop spec");
        assert_eq!(spec.program, Some(PathBuf::from("htop")));
        assert!(spec.args.is_empty());
    }

    #[test]
    fn build_command_spec_program_with_args() {
        let spec = build_command_spec("htop --tree -d 5", "").expect("htop with args");
        assert_eq!(spec.program, Some(PathBuf::from("htop")));
        let args: Vec<String> = spec
            .args
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["--tree", "-d", "5"]);
    }

    #[test]
    fn build_command_spec_quoted_args() {
        let spec = build_command_spec("vim 'with spaces.txt'", "").expect("vim with quoted arg");
        assert_eq!(spec.program, Some(PathBuf::from("vim")));
        let args: Vec<String> = spec
            .args
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["with spaces.txt"]);
    }

    #[test]
    fn build_command_spec_malformed_quotes_errors() {
        let result = build_command_spec("echo 'unclosed", "");
        assert!(result.is_err(), "malformed shlex should return Err");
    }

    #[test]
    fn build_command_spec_quoted_empty_program_errors() {
        // shlex::split("''") returns Some([""]); the first token is an empty
        // program name, which would later fail with a generic spawn error.
        // Reject at the parse boundary so clients get MalformedPayload.
        let result = build_command_spec("''", "");
        assert!(
            result.is_err(),
            "empty quoted program should return Err, got: {result:?}"
        );
        let result = build_command_spec("\"\"", "");
        assert!(
            result.is_err(),
            "empty double-quoted program should return Err, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn pane_locks_are_independent() {
        let panes = Arc::new(Mutex::new(PaneManager::new()));
        let (a, b) = {
            let mut pm = panes.lock().await;
            let a = pm.create(80, 24, String::new(), String::new());
            let b = pm.create(80, 24, String::new(), String::new());
            (a, b)
        };
        let _held = lock_live_pane(&panes, a).await.unwrap();

        // A burst on pane A (its lock held) must not block pane B.
        let seqno = tokio::time::timeout(std::time::Duration::from_millis(100), async {
            lock_live_pane(&panes, b).await.unwrap().dirty_seqno
        })
        .await
        .expect("pane B blocked behind pane A's lock");
        assert_eq!(seqno, 0);
    }

    #[tokio::test]
    async fn tombstoned_pane_reads_as_absent() {
        let panes = Arc::new(Mutex::new(PaneManager::new()));
        let id = panes
            .lock()
            .await
            .create(80, 24, String::new(), String::new());

        lock_live_pane(&panes, id).await.unwrap().closed = true;

        // A handle that raced ClosePane resolves to absent, not to a
        // ghost pane.
        assert!(lock_live_pane(&panes, id).await.is_none());
    }

    #[test]
    fn remove_refocuses_to_a_remaining_pane() {
        let mut pm = PaneManager::new();
        let a = pm.create(80, 24, String::new(), String::new());
        let b = pm.create(80, 24, String::new(), String::new());

        assert_eq!(pm.focused(), Some(a), "first pane is auto-focused");
        pm.remove(a);
        assert_eq!(pm.focused(), Some(b));
    }

    #[test]
    fn remove_of_unfocused_pane_keeps_focus() {
        let mut pm = PaneManager::new();
        let a = pm.create(80, 24, String::new(), String::new());
        let b = pm.create(80, 24, String::new(), String::new());

        assert!(pm.focus(b));
        pm.remove(a);
        assert_eq!(pm.focused(), Some(b));
    }

    #[test]
    fn focus_of_unknown_pane_is_rejected() {
        let mut pm = PaneManager::new();
        let a = pm.create(80, 24, String::new(), String::new());

        assert!(!pm.focus(999));
        assert_eq!(pm.focused(), Some(a));
    }
}
