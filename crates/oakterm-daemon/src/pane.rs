//! Pane domain: PTY lifecycle state, per-pane locking, and topology tracking.

use oakterm_mux::{
    BorderExtents, LayoutError, LayoutNode, MultiplexerState, PaneCloseOutcome, PaneId,
    SplitDirection, SplitPreview, Tab, TabId, Workspace, WorkspaceId,
};
use oakterm_terminal::grid::ScreenSet;
use std::ffi::OsString;
use std::os::unix::io::RawFd;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error};

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

/// Atomic view of the manager topology; see `PaneManager::topology_snapshot`.
pub(crate) struct TopologySnapshot {
    pub(crate) layout: LayoutNode,
    pub(crate) focused: Option<u32>,
    pub(crate) panes: Vec<(u32, SharedPane)>,
}

/// Tracks all panes. Guards topology only (create/lookup/remove/focus);
/// the Spec-0007 workspace/tab/layout model lives in [`MultiplexerState`]
/// and pane contents live behind per-pane locks.
///
/// Invariant: the mux tabs' pane IDs are exactly the pane map's keys.
/// `create`/`split_create` insert into both; `remove` closes the pane out
/// of its tab.
///
/// Lock order: take the manager lock, clone the `SharedPane`, release,
/// then lock the pane — never hold the manager lock across a pane lock,
/// and never hold two pane locks at once.
pub(crate) struct PaneManager {
    panes: std::collections::HashMap<u32, SharedPane>,
    mux: MultiplexerState,
}

impl PaneManager {
    pub(crate) fn new() -> Self {
        Self {
            panes: std::collections::HashMap::new(),
            mux: MultiplexerState::new(),
        }
    }

    fn insert_pane_state(&mut self, id: u32, cols: u16, rows: u16, command: String, cwd: String) {
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
    }

    /// Create a pane with the given grid dimensions. Returns the assigned ID.
    ///
    /// The pane also enters the mux model: the first pane seeds the
    /// default workspace and tab; later ones split the active tab's
    /// focused pane horizontally without moving focus. `SplitPane`
    /// requests pick their own target and direction via
    /// [`PaneManager::split_create`].
    pub(crate) fn create(&mut self, cols: u16, rows: u16, command: String, cwd: String) -> u32 {
        let id = self.mux.allocate_pane_id();
        if self.mux.has_panes() {
            // A seeded mux always has an active tab with a focused pane in
            // its tree; a failure here means the model is out of sync —
            // log, never panic. The debug_asserts make a future invariant
            // regression fail tests instead of degrading quietly in
            // production.
            if let Some(target) = self.mux.active_tab().map(Tab::focused_pane) {
                if let Err(e) = self.mux.split_pane(target, id, SplitDirection::Horizontal) {
                    error!(pane_id = id.0, target = target.0, error = %e, "layout insert failed; mux out of sync, pane will exist outside any layout");
                    debug_assert!(false, "layout insert failed: {e}");
                }
            } else {
                error!(
                    pane_id = id.0,
                    "no active tab while mux has panes; pane will exist outside any layout"
                );
                debug_assert!(false, "no active tab while mux has panes");
            }
        } else if !self.mux.seed(id) {
            error!(
                pane_id = id.0,
                "seed refused on a non-empty mux; pane will exist outside any layout"
            );
            debug_assert!(false, "seed refused on a non-empty mux");
        }
        self.insert_pane_state(id.0, cols, rows, command, cwd);
        id.0
    }

    /// Create a pane and insert it beside `target` in its tab's layout
    /// (Spec-0007 Split). Focus moves to the new pane. The caller owns
    /// the minimum-size pre-check via [`PaneManager::split_preview`].
    ///
    /// # Errors
    /// [`LayoutError::PaneNotFound`] if no tab contains `target`; the
    /// pane map and mux are unchanged on error.
    pub(crate) fn split_create(
        &mut self,
        target: u32,
        direction: SplitDirection,
        cols: u16,
        rows: u16,
        command: String,
        cwd: String,
    ) -> Result<u32, LayoutError> {
        let target = PaneId(target);
        // Split the tree first with a freshly allocated id; the map insert
        // is then infallible relative to the split, so no rollback path
        // exists to get wrong. An error path burns the allocated id, which
        // is harmless for a monotonic u32.
        let id = self.mux.allocate_pane_id();
        self.mux.split_pane(target, id, direction)?;
        if !self.mux.focus_pane(id) {
            error!(
                pane_id = id.0,
                "freshly split pane not focusable; focus left unchanged"
            );
            debug_assert!(false, "freshly split pane must be focusable");
        }
        self.insert_pane_state(id.0, cols, rows, command, cwd);
        Ok(id.0)
    }

    /// Create a pane in a fresh tab and make that tab active (Spec-0001
    /// `NewTab`). An empty mux seeds the default workspace instead, which
    /// yields the same shape: one new tab holding one new pane. Returns
    /// `(tab_id, pane_id)`, or `None` when the mux refuses the insert — a
    /// model desync, since the pane ID is freshly allocated.
    pub(crate) fn new_tab_create(
        &mut self,
        cols: u16,
        rows: u16,
        command: String,
        cwd: String,
    ) -> Option<(u32, u32)> {
        // An error path burns the allocated pane ID, which is harmless
        // for a monotonic u32.
        let id = self.mux.allocate_pane_id();
        let tab_id = if self.mux.has_panes() {
            self.mux.new_tab(id)?
        } else {
            if !self.mux.seed(id) {
                error!(pane_id = id.0, "seed refused on a non-empty mux");
                debug_assert!(false, "seed refused on a non-empty mux");
                return None;
            }
            let Some(tab) = self.mux.active_tab() else {
                error!(pane_id = id.0, "seeded mux has no active tab");
                debug_assert!(false, "seeded mux must have an active tab");
                return None;
            };
            tab.id()
        };
        self.insert_pane_state(id.0, cols, rows, command, cwd);
        Some((tab_id.0, id.0))
    }

    /// Create a pane in a fresh workspace (one tab, one pane) and make
    /// that workspace active (Spec-0001 `NewWorkspace`). The pane runs the
    /// default shell in the daemon's cwd — the wire message carries no
    /// command. Returns `(workspace_id, tab_id, pane_id)`, or `None` when
    /// the mux refuses the insert — a model desync, since the pane ID is
    /// freshly allocated.
    pub(crate) fn new_workspace_create(
        &mut self,
        name: String,
        cols: u16,
        rows: u16,
    ) -> Option<(u32, u32, u32)> {
        // An error path burns the allocated pane ID, which is harmless
        // for a monotonic u32.
        let id = self.mux.allocate_pane_id();
        let ws_id = self.mux.new_workspace(name, id)?;
        // new_workspace made the fresh workspace active, so the active tab
        // is its single tab.
        let Some(tab) = self.mux.active_tab() else {
            error!(
                workspace_id = ws_id.0,
                pane_id = id.0,
                "fresh workspace has no active tab; mux out of sync"
            );
            debug_assert!(false, "fresh workspace must have an active tab");
            return None;
        };
        let tab_id = tab.id();
        self.insert_pane_state(id.0, cols, rows, String::new(), String::new());
        Some((ws_id.0, tab_id.0, id.0))
    }

    /// Activate a workspace, keeping its own active tab and focused pane
    /// (Spec-0001 `SwitchWorkspace`). Returns false when the workspace is
    /// unknown.
    pub(crate) fn switch_workspace(&mut self, workspace: u32) -> bool {
        let Some(pane) = self
            .mux
            .workspaces()
            .iter()
            .find(|w| w.id() == WorkspaceId(workspace))
            .map(|w| w.active_tab().focused_pane())
        else {
            return false;
        };
        let focused = self.mux.focus_pane(pane);
        if !focused {
            error!(
                workspace_id = workspace,
                pane_id = pane.0,
                "workspace's focused pane not focusable; mux out of sync"
            );
            debug_assert!(false, "workspace's focused pane must be focusable");
        }
        focused
    }

    fn tab_by_id(&self, tab: TabId) -> Option<&Tab> {
        self.mux
            .workspaces()
            .iter()
            .flat_map(Workspace::tabs)
            .find(|t| t.id() == tab)
    }

    /// Every pane in a tab: layout leaves plus floating panes. `None`
    /// when no workspace holds the tab.
    pub(crate) fn tab_pane_ids(&self, tab: u32) -> Option<Vec<u32>> {
        let tab = self.tab_by_id(TabId(tab))?;
        let mut ids: Vec<u32> = tab.layout().pane_ids().iter().map(|p| p.0).collect();
        ids.extend(tab.floating().iter().map(|f| f.pane_id.0));
        Some(ids)
    }

    /// Activate a tab, keeping the tab's own focused pane (Spec-0001
    /// `SwitchTab`). Returns false when the tab is unknown.
    pub(crate) fn switch_tab(&mut self, tab: u32) -> bool {
        let Some(pane) = self.tab_by_id(TabId(tab)).map(Tab::focused_pane) else {
            return false;
        };
        let focused = self.mux.focus_pane(pane);
        if !focused {
            error!(
                tab_id = tab,
                pane_id = pane.0,
                "tab's focused pane not focusable; mux out of sync"
            );
            debug_assert!(false, "tab's focused pane must be focusable");
        }
        focused
    }

    /// Predicted extents for a split of `target`, for the Spec-0007
    /// minimum-size pre-check.
    pub(crate) fn split_preview(
        &self,
        target: u32,
        direction: SplitDirection,
    ) -> Result<SplitPreview, LayoutError> {
        let target = PaneId(target);
        self.mux
            .tab_containing(target)
            .ok_or(LayoutError::PaneNotFound(target))?
            .layout()
            .split_preview(target, direction)
    }

    /// Geometry of the border shared by two panes, for converting a
    /// cell-space resize delta into weight space.
    pub(crate) fn border_extents(
        &self,
        pane: u32,
        neighbor: u32,
    ) -> Result<BorderExtents, LayoutError> {
        let pane = PaneId(pane);
        self.mux
            .tab_containing(pane)
            .ok_or(LayoutError::PaneNotFound(pane))?
            .layout()
            .border_extents(pane, PaneId(neighbor))
    }

    /// Move the border between two panes by `delta_weight` (Spec-0007
    /// Resize; positive grows `pane`).
    pub(crate) fn resize_layout(
        &mut self,
        pane: u32,
        neighbor: u32,
        delta_weight: f32,
        min_weight: f32,
    ) -> Result<(), LayoutError> {
        self.mux
            .resize_pane(PaneId(pane), PaneId(neighbor), delta_weight, min_weight)
    }

    pub(crate) fn swap_layout(&mut self, a: u32, b: u32) -> Result<(), LayoutError> {
        self.mux.swap_panes(PaneId(a), PaneId(b))
    }

    pub(crate) fn len(&self) -> usize {
        self.panes.len()
    }

    pub(crate) fn get(&self, id: u32) -> Option<SharedPane> {
        self.panes.get(&id).cloned()
    }

    /// The pane is closed out of its tab; if the removed pane was focused,
    /// focus moves to the tab's nearest-sibling hint (Spec-0007 Close).
    /// The last pane closes its tab, cascading to the workspace. Callers
    /// enforce the last-pane rule (`ClosePane` refuses to remove the
    /// final pane); `remove` itself will empty the topology if asked.
    pub(crate) fn remove(&mut self, id: u32) -> Option<SharedPane> {
        let removed = self.panes.remove(&id)?;
        match self.mux.close_pane(PaneId(id)) {
            Ok(PaneCloseOutcome::Removed { .. }) => {}
            Ok(PaneCloseOutcome::TabClosed {
                tab,
                workspace_closed,
            }) => {
                // No tab-lifecycle push to clients exists yet; a client
                // learns of a cascaded tab close only by re-reading the layout.
                debug!(
                    pane_id = id,
                    tab = tab.0,
                    workspace = workspace_closed.map(|w| w.0),
                    "pane close cascaded to its tab"
                );
            }
            Err(e) => {
                error!(pane_id = id, error = %e, "removed pane missing from mux model");
                debug_assert!(false, "removed pane missing from mux model: {e}");
            }
        }
        Some(removed)
    }

    pub(crate) fn focused(&self) -> Option<u32> {
        self.mux.active_tab().map(|t| t.focused_pane().0)
    }

    /// The active tab's layout tree. Mutations go through the
    /// split/resize/swap methods; this read-only view serves session
    /// saving (Spec-0010) and `GetLayoutTree`.
    pub(crate) fn layout(&self) -> Option<&LayoutNode> {
        self.mux.active_tab().map(Tab::layout)
    }

    /// Snapshot the topology under one lock: the cloned layout tree, the
    /// focused pane, and every `(id, SharedPane)` pair. `None` when there
    /// is no layout (no panes). The three reads must stay atomic — a split
    /// landing between them would desync the tree from the focus/pane set;
    /// session saving (Spec-0010) relies on that atomicity. Callers release
    /// the manager lock before locking panes (manager->pane lock order).
    ///
    /// Single-tab limitation, now live: `layout` is the active tab's tree
    /// while `panes` spans every tab. Multi-tab session save must aggregate
    /// all tabs (deferred to the Spec-0010 session work); until then a
    /// background tab's panes are saved without a layout placement.
    pub(crate) fn topology_snapshot(&self) -> Option<TopologySnapshot> {
        let layout = self.layout()?.clone();
        Some(TopologySnapshot {
            layout,
            focused: self.focused(),
            panes: self.snapshot(),
        })
    }

    /// Focus a pane, activating its workspace and tab. Returns false if
    /// the pane is unknown.
    pub(crate) fn focus(&mut self, id: u32) -> bool {
        let focused = self.mux.focus_pane(PaneId(id));
        if focused != self.panes.contains_key(&id) {
            error!(
                pane_id = id,
                in_mux = focused,
                "mux model and pane map disagree on pane"
            );
            debug_assert!(false, "mux model and pane map disagree on pane {id}");
        }
        focused
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

    #[test]
    fn create_seeds_layout_and_splits_focused() {
        let mut pm = PaneManager::new();
        let a = pm.create(80, 24, String::new(), String::new());
        let tree = pm.layout().expect("first pane seeds the tree");
        assert!(tree.contains(PaneId(a)));
        assert!(tree.is_leaf());

        let b = pm.create(80, 24, String::new(), String::new());
        let tree = pm.layout().unwrap();
        assert!(tree.contains(PaneId(a)));
        assert!(tree.contains(PaneId(b)));
        assert!(tree.is_canonical());
        // CreatePane does not move focus (only SplitPane does).
        assert_eq!(pm.focused(), Some(a));
    }

    #[test]
    fn split_create_inserts_and_focuses_new_pane() {
        let mut pm = PaneManager::new();
        let a = pm.create(80, 24, String::new(), String::new());
        let b = pm
            .split_create(
                a,
                SplitDirection::Vertical,
                80,
                24,
                String::new(),
                String::new(),
            )
            .expect("split of the root pane");

        assert_eq!(pm.focused(), Some(b), "focus moves to the new pane");
        assert_eq!(pm.len(), 2);
        let tree = pm.layout().unwrap();
        assert!(tree.contains(PaneId(b)));
        assert!(tree.is_canonical());
    }

    #[test]
    fn split_create_unknown_target_leaves_map_unchanged() {
        let mut pm = PaneManager::new();
        let a = pm.create(80, 24, String::new(), String::new());
        let result = pm.split_create(
            999,
            SplitDirection::Horizontal,
            80,
            24,
            String::new(),
            String::new(),
        );
        assert_eq!(result, Err(LayoutError::PaneNotFound(PaneId(999))));
        assert_eq!(pm.len(), 1, "no pane leaked on rejected split");
        assert_eq!(pm.focused(), Some(a));
    }

    #[test]
    fn remove_moves_focus_to_layout_hint() {
        let mut pm = PaneManager::new();
        let a = pm.create(80, 24, String::new(), String::new());
        let b = pm
            .split_create(
                a,
                SplitDirection::Horizontal,
                80,
                24,
                String::new(),
                String::new(),
            )
            .unwrap();
        let c = pm
            .split_create(
                b,
                SplitDirection::Horizontal,
                80,
                24,
                String::new(),
                String::new(),
            )
            .unwrap();

        // H[a, b, c] with c focused; removing c focuses its left sibling.
        pm.remove(c);
        assert_eq!(pm.focused(), Some(b));
        assert!(!pm.layout().unwrap().contains(PaneId(c)));
    }

    #[test]
    fn remove_last_pane_clears_layout() {
        let mut pm = PaneManager::new();
        let a = pm.create(80, 24, String::new(), String::new());
        pm.remove(a);
        assert!(pm.layout().is_none());
        assert_eq!(pm.focused(), None);
    }

    #[test]
    fn create_after_removing_last_pane_reseeds() {
        let mut pm = PaneManager::new();
        let a = pm.create(80, 24, String::new(), String::new());
        pm.remove(a);
        let b = pm.create(80, 24, String::new(), String::new());
        assert_ne!(a, b, "pane IDs are never reused");
        assert_eq!(pm.focused(), Some(b));
        assert!(pm.layout().expect("layout").is_leaf());
    }

    #[test]
    fn swap_layout_exchanges_leaves() {
        let mut pm = PaneManager::new();
        let a = pm.create(80, 24, String::new(), String::new());
        let b = pm
            .split_create(
                a,
                SplitDirection::Horizontal,
                80,
                24,
                String::new(),
                String::new(),
            )
            .unwrap();

        pm.swap_layout(a, b).unwrap();
        assert_eq!(
            pm.layout().unwrap().pane_ids(),
            vec![PaneId(b), PaneId(a)],
            "leaf order reflects the swap"
        );
        assert_eq!(
            pm.swap_layout(a, 999),
            Err(LayoutError::PaneNotFound(PaneId(999)))
        );
    }

    #[test]
    fn resize_layout_delegates_with_border_validation() {
        let mut pm = PaneManager::new();
        let a = pm.create(80, 24, String::new(), String::new());
        let b = pm
            .split_create(
                a,
                SplitDirection::Horizontal,
                80,
                24,
                String::new(),
                String::new(),
            )
            .unwrap();

        let ext = pm.border_extents(a, b).unwrap();
        assert_eq!(ext.axis, SplitDirection::Horizontal);
        pm.resize_layout(a, b, 0.1, 0.01).unwrap();

        // H[V[a,c], V[b,d]]: a (top-left) and d (bottom-right) only meet
        // at a corner — no resizable border.
        let c = pm
            .split_create(
                a,
                SplitDirection::Vertical,
                80,
                24,
                String::new(),
                String::new(),
            )
            .unwrap();
        let d = pm
            .split_create(
                b,
                SplitDirection::Vertical,
                80,
                24,
                String::new(),
                String::new(),
            )
            .unwrap();
        let _ = c;
        assert_eq!(
            pm.resize_layout(a, d, 0.1, 0.01),
            Err(LayoutError::NotAdjacentSiblings {
                pane: PaneId(a),
                neighbor: PaneId(d)
            })
        );
    }

    #[test]
    fn create_splits_the_focused_pane_not_the_first() {
        let mut pm = PaneManager::new();
        let a = pm.create(80, 24, String::new(), String::new());
        let b = pm
            .split_create(
                a,
                SplitDirection::Horizontal,
                80,
                24,
                String::new(),
                String::new(),
            )
            .unwrap();

        // Focus is on b; the unanchored create must insert after b, not a.
        let c = pm.create(80, 24, String::new(), String::new());
        assert_eq!(
            pm.layout().unwrap().pane_ids(),
            vec![PaneId(a), PaneId(b), PaneId(c)]
        );
    }

    #[test]
    fn create_remove_create_preserves_tree_map_sync() {
        let mut pm = PaneManager::new();
        let a = pm.create(80, 24, String::new(), String::new());
        let b = pm.create(80, 24, String::new(), String::new());
        pm.remove(a);
        let c = pm.create(80, 24, String::new(), String::new());

        let mut leaves = pm.layout().unwrap().pane_ids();
        leaves.sort_by_key(|p| p.0);
        assert_eq!(leaves, vec![PaneId(b), PaneId(c)]);
        assert_eq!(pm.len(), 2);
    }

    #[test]
    fn resize_layout_moves_border_by_converted_cell_delta() {
        let mut pm = PaneManager::new();
        let a = pm.create(80, 24, String::new(), String::new());
        let b = pm
            .split_create(
                a,
                SplitDirection::Horizontal,
                80,
                24,
                String::new(),
                String::new(),
            )
            .unwrap();

        // An 80-col pane at half the tree width implies a 160-cell
        // container: +16 cells is +0.1 weight (the handler's conversion).
        let min_weight = 2.0 / 160.0;
        pm.resize_layout(a, b, 16.0 / 160.0, min_weight).unwrap();
        let ext = pm.border_extents(a, b).unwrap();
        assert!(
            (ext.pane_extent - 0.6).abs() < 1e-5,
            "grew to {}",
            ext.pane_extent
        );

        // An oversized shrink clamps at the 2-column minimum weight.
        pm.resize_layout(a, b, -10.0, min_weight).unwrap();
        let ext = pm.border_extents(a, b).unwrap();
        assert!(
            (ext.pane_extent - min_weight).abs() < 1e-5,
            "clamped to {}",
            ext.pane_extent
        );
    }

    #[test]
    fn layout_ops_on_empty_manager_error() {
        let mut pm = PaneManager::new();
        assert!(pm.split_preview(0, SplitDirection::Horizontal).is_err());
        assert!(pm.border_extents(0, 1).is_err());
        assert!(pm.resize_layout(0, 1, 0.1, 0.01).is_err());
        assert!(pm.swap_layout(0, 1).is_err());
        assert!(
            pm.split_create(
                0,
                SplitDirection::Horizontal,
                80,
                24,
                String::new(),
                String::new()
            )
            .is_err()
        );
    }
}
