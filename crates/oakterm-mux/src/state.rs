//! Tabs, workspaces, and the top-level multiplexer state (Spec-0007).
//!
//! A [`Tab`] owns one tiled layout tree plus floating panes; a [`Workspace`]
//! owns an ordered list of tabs; [`MultiplexerState`] owns the workspaces and
//! all ID generation. Mutation goes through methods so the focus and
//! non-empty invariants hold at every step.

use crate::layout::{LayoutNode, PaneId, SplitDirection};
use crate::ops::{CloseOutcome, LayoutError};

/// Unique tab identifier, assigned by [`MultiplexerState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(pub u32);

/// Unique workspace identifier, assigned by [`MultiplexerState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkspaceId(pub u32);

/// A pane rendered above the tiled layout, positioned in pixels relative to
/// the tab's content area (Spec-0007 Floating Panes).
#[derive(Debug, Clone, PartialEq)]
pub struct FloatingPane {
    pub pane_id: PaneId,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Hidden floating panes stay alive but are not rendered.
    pub visible: bool,
}

/// Result of [`MultiplexerState::close_pane`]. Discarding it loses the
/// tab-closed signal, stranding daemon-side pane state for a tab that no
/// longer exists.
#[must_use]
#[derive(Debug, Clone, PartialEq)]
pub enum PaneCloseOutcome {
    /// The pane was removed from its tab; focus within that tab moved to
    /// `focus_hint` if the removed pane was focused.
    Removed { focus_hint: PaneId },
    /// The pane was its tab's last: the tab closed with it, and the
    /// workspace too when the tab was the workspace's last.
    TabClosed {
        tab: TabId,
        workspace_closed: Option<WorkspaceId>,
    },
}

/// One tiled layout tree plus floating panes and a focused pane (Spec-0007
/// Tab).
///
/// Invariants: every `PaneId` in the layout tree and floating list is unique
/// within the tab, and `focused_pane` references one of them. A tab always
/// contains at least one pane; [`Tab::close_pane`] returns
/// [`CloseOutcome::LastPane`] instead of emptying itself, and the caller
/// closes the tab.
#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    id: TabId,
    /// User-visible name. Empty until pane titles land; renameable later
    /// (TREK-209).
    name: String,
    layout: LayoutNode,
    /// Ordered by z-index (last = topmost). Population arrives with the
    /// floating-pane wire messages (TREK-210).
    floating: Vec<FloatingPane>,
    focused_pane: PaneId,
}

impl Tab {
    fn new(id: TabId, pane: PaneId) -> Self {
        Self {
            id,
            name: String::new(),
            layout: LayoutNode::leaf(pane),
            floating: Vec::new(),
            focused_pane: pane,
        }
    }

    #[must_use]
    pub fn id(&self) -> TabId {
        self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The name-vs-title fallback (Spec-0007) is applied downstream when
    /// the display name is resolved.
    pub(crate) fn set_name(&mut self, name: String) {
        self.name = name;
    }

    #[must_use]
    pub fn layout(&self) -> &LayoutNode {
        &self.layout
    }

    #[must_use]
    pub fn floating(&self) -> &[FloatingPane] {
        &self.floating
    }

    #[must_use]
    pub fn focused_pane(&self) -> PaneId {
        self.focused_pane
    }

    /// Whether the pane is in this tab's layout tree or floating list.
    #[must_use]
    pub fn contains(&self, pane: PaneId) -> bool {
        self.layout.contains(pane) || self.floating.iter().any(|f| f.pane_id == pane)
    }

    /// Every pane in this tab: tiled leaves first (left-to-right), then
    /// floating panes in z-order.
    #[must_use]
    pub fn pane_ids(&self) -> Vec<PaneId> {
        let mut ids = self.layout.pane_ids();
        ids.extend(self.floating.iter().map(|f| f.pane_id));
        ids
    }

    /// Insert `new` beside `target` in the layout tree (Spec-0007 Split).
    /// Focus does not move; callers implementing the `SplitPane` behavior
    /// follow with [`Tab::focus_pane`].
    ///
    /// # Errors
    /// [`LayoutError::PaneNotFound`] if `target` is not in the tree;
    /// [`LayoutError::PaneAlreadyPresent`] if `new` is already in the tab
    /// (tiled or floating).
    pub(crate) fn split(
        &mut self,
        target: PaneId,
        new: PaneId,
        direction: SplitDirection,
    ) -> Result<(), LayoutError> {
        // LayoutNode::split only checks the tree; the tab's uniqueness
        // invariant also covers the floating list.
        if self.contains(new) {
            return Err(LayoutError::PaneAlreadyPresent(new));
        }
        self.layout.split(target, new, direction)
    }

    /// Close a pane out of the layout tree. When the focused pane is
    /// removed, focus moves to the close hint (Spec-0007 Close). Returns
    /// [`CloseOutcome::LastPane`] with the tab unchanged when `target` is
    /// the only pane — the caller closes the tab instead.
    ///
    /// Closes tiled panes only; floating-pane close semantics land with
    /// their wire messages (TREK-210).
    ///
    /// # Errors
    /// [`LayoutError::PaneNotFound`] if `target` is not in the tree.
    pub(crate) fn close_pane(&mut self, target: PaneId) -> Result<CloseOutcome, LayoutError> {
        let outcome = self.layout.close(target)?;
        if let CloseOutcome::Removed { focus_hint } = outcome
            && self.focused_pane == target
        {
            self.focused_pane = focus_hint;
        }
        Ok(outcome)
    }

    /// Focus a pane in this tab. Returns false if the pane is not here.
    #[must_use = "false means the pane was not focused"]
    pub(crate) fn focus_pane(&mut self, pane: PaneId) -> bool {
        let known = self.contains(pane);
        if known {
            self.focused_pane = pane;
        }
        known
    }

    /// Move the border between two sibling panes (Spec-0007 Resize).
    ///
    /// # Errors
    /// See [`LayoutNode::resize`].
    pub(crate) fn resize(
        &mut self,
        pane: PaneId,
        neighbor: PaneId,
        delta_weight: f32,
        min_weight: f32,
    ) -> Result<(), LayoutError> {
        self.layout.resize(pane, neighbor, delta_weight, min_weight)
    }

    /// Exchange two panes' positions in the layout tree. Focus follows pane
    /// identity, not position, so `focused_pane` is untouched.
    ///
    /// # Errors
    /// See [`LayoutNode::swap`].
    pub(crate) fn swap(&mut self, a: PaneId, b: PaneId) -> Result<(), LayoutError> {
        self.layout.swap(a, b)
    }
}

/// Re-point an active index after removing `removed` from a list now
/// `new_len` long: the surviving active element stays active; when the
/// active element itself was removed, the next one takes over, or the
/// previous when the removed element was last.
fn clamp_active_after_remove(active: &mut usize, removed: usize, new_len: usize) {
    if *active > removed || *active >= new_len {
        *active = active.saturating_sub(1);
    }
}

/// An independent context of tabs (Spec-0007 Workspace). `tabs` is never
/// empty and `active_tab` always indexes into it.
///
/// Switching arrives with the workspace wire messages (TREK-105);
/// rename and direct close with TREK-209.
#[derive(Debug, Clone, PartialEq)]
pub struct Workspace {
    id: WorkspaceId,
    name: String,
    tabs: Vec<Tab>,
    active_tab: usize,
}

impl Workspace {
    fn new(id: WorkspaceId, name: String, tab: Tab) -> Self {
        Self {
            id,
            name,
            tabs: vec![tab],
            active_tab: 0,
        }
    }

    #[must_use]
    pub fn id(&self) -> WorkspaceId {
        self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn set_name(&mut self, name: String) {
        self.name = name;
    }

    #[must_use]
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    #[must_use]
    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    #[must_use]
    pub fn active_tab_index(&self) -> usize {
        self.active_tab
    }

    /// Remove a tab, keeping `active_tab` on the same tab when it survives.
    /// When the active tab itself is removed, the tab after it takes over,
    /// or the one before when the removed tab was last. Returns true when
    /// the workspace is left empty — the caller closes the workspace.
    /// An unknown `tab` is a no-op that also returns false.
    fn close_tab(&mut self, tab: TabId) -> bool {
        let Some(index) = self.tabs.iter().position(|t| t.id == tab) else {
            return false;
        };
        self.tabs.remove(index);
        clamp_active_after_remove(&mut self.active_tab, index, self.tabs.len());
        self.tabs.is_empty()
    }
}

/// Top-level multiplexer state owned by the daemon (Spec-0007).
///
/// `workspaces` is non-empty from the first pane on; the empty state models
/// a daemon with no panes yet. Closing the last pane of the last tab of the
/// last workspace returns to the empty state — the daemon-exit rule
/// (Spec-0007 Workspace invariant) is the caller's policy.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MultiplexerState {
    workspaces: Vec<Workspace>,
    active_workspace: usize,
    next_pane_id: u32,
    next_tab_id: u32,
    next_workspace_id: u32,
}

impl MultiplexerState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn has_panes(&self) -> bool {
        !self.workspaces.is_empty()
    }

    /// Reserve the next pane ID. The caller creates the pane and threads
    /// the ID into [`MultiplexerState::seed`],
    /// [`MultiplexerState::split_pane`], or a floating insert.
    ///
    /// # Panics
    /// After `u32::MAX` allocations — wrapping would reuse a live ID and
    /// silently corrupt the pane map, so exhaustion fails loudly.
    pub fn allocate_pane_id(&mut self) -> PaneId {
        let id = PaneId(self.next_pane_id);
        self.next_pane_id = self
            .next_pane_id
            .checked_add(1)
            .expect("pane ID space exhausted");
        id
    }

    /// Keep the pane-ID allocator ahead of an externally supplied ID so a
    /// later allocation can never collide (Spec-0007: IDs are never reused
    /// within a daemon session).
    fn note_external_pane_id(&mut self, pane: PaneId) {
        self.next_pane_id = self.next_pane_id.max(pane.0.saturating_add(1));
    }

    fn allocate_tab_id(&mut self) -> TabId {
        let id = TabId(self.next_tab_id);
        self.next_tab_id = self
            .next_tab_id
            .checked_add(1)
            .expect("tab ID space exhausted");
        id
    }

    fn allocate_workspace_id(&mut self) -> WorkspaceId {
        let id = WorkspaceId(self.next_workspace_id);
        self.next_workspace_id = self
            .next_workspace_id
            .checked_add(1)
            .expect("workspace ID space exhausted");
        id
    }

    #[must_use]
    pub fn workspaces(&self) -> &[Workspace] {
        &self.workspaces
    }

    #[must_use]
    pub fn active_workspace(&self) -> Option<&Workspace> {
        self.workspaces.get(self.active_workspace)
    }

    #[must_use]
    pub fn active_workspace_index(&self) -> usize {
        self.active_workspace
    }

    #[must_use]
    pub fn active_tab(&self) -> Option<&Tab> {
        self.active_workspace().map(Workspace::active_tab)
    }

    /// Workspace and tab indices of the tab holding `pane`.
    fn locate(&self, pane: PaneId) -> Option<(usize, usize)> {
        self.workspaces.iter().enumerate().find_map(|(wi, ws)| {
            ws.tabs
                .iter()
                .position(|t| t.contains(pane))
                .map(|ti| (wi, ti))
        })
    }

    /// Workspace and tab indices of the tab with id `tab`.
    fn locate_tab(&self, tab: TabId) -> Option<(usize, usize)> {
        self.workspaces
            .iter()
            .enumerate()
            .find_map(|(wi, ws)| ws.tabs.iter().position(|t| t.id == tab).map(|ti| (wi, ti)))
    }

    /// The tab holding `pane`, searched across all workspaces.
    #[must_use]
    pub fn tab_containing(&self, pane: PaneId) -> Option<&Tab> {
        self.locate(pane)
            .map(|(wi, ti)| &self.workspaces[wi].tabs[ti])
    }

    /// Mutable variant of [`MultiplexerState::tab_containing`],
    /// crate-internal so external mutation goes through the
    /// pane-addressed operations that maintain the active indices —
    /// focusing directly through this handle could point a background
    /// tab's focus at a pane that never renders.
    pub(crate) fn tab_containing_mut(&mut self, pane: PaneId) -> Option<&mut Tab> {
        let (wi, ti) = self.locate(pane)?;
        Some(&mut self.workspaces[wi].tabs[ti])
    }

    /// Insert `new` beside `target` in its tab's layout tree (Spec-0007
    /// Split). Focus does not move; callers implementing the `SplitPane`
    /// behavior follow with [`MultiplexerState::focus_pane`].
    ///
    /// # Errors
    /// [`LayoutError::PaneNotFound`] if no tab contains `target`;
    /// [`LayoutError::PaneAlreadyPresent`] if a tab already tracks `new`.
    pub fn split_pane(
        &mut self,
        target: PaneId,
        new: PaneId,
        direction: SplitDirection,
    ) -> Result<(), LayoutError> {
        if self.tab_containing(new).is_some() {
            return Err(LayoutError::PaneAlreadyPresent(new));
        }
        self.tab_containing_mut(target)
            .ok_or(LayoutError::PaneNotFound(target))?
            .split(target, new, direction)?;
        self.note_external_pane_id(new);
        Ok(())
    }

    /// Move the border between two sibling panes (Spec-0007 Resize;
    /// positive `delta_weight` grows `pane`).
    ///
    /// # Errors
    /// [`LayoutError::PaneNotFound`] if no tab contains `pane`; otherwise
    /// see [`LayoutNode::resize`].
    pub fn resize_pane(
        &mut self,
        pane: PaneId,
        neighbor: PaneId,
        delta_weight: f32,
        min_weight: f32,
    ) -> Result<(), LayoutError> {
        self.tab_containing_mut(pane)
            .ok_or(LayoutError::PaneNotFound(pane))?
            .resize(pane, neighbor, delta_weight, min_weight)
    }

    /// Exchange two panes' positions in their shared tab.
    ///
    /// # Errors
    /// [`LayoutError::PaneNotFound`] if no tab contains `a`, or if `b` is
    /// not in the same tab; otherwise see [`LayoutNode::swap`].
    pub fn swap_panes(&mut self, a: PaneId, b: PaneId) -> Result<(), LayoutError> {
        self.tab_containing_mut(a)
            .ok_or(LayoutError::PaneNotFound(a))?
            .swap(a, b)
    }

    /// Create the first workspace ("default") and tab around the first
    /// pane. Refuses without changes when state already exists, keeping
    /// the existing topology authoritative — the caller decides whether
    /// that is a bug (the daemon treats it as a model desync).
    #[must_use = "false means the multiplexer was already seeded and the pane is untracked"]
    pub fn seed(&mut self, pane: PaneId) -> bool {
        if !self.workspaces.is_empty() {
            return false;
        }
        self.note_external_pane_id(pane);
        let tab_id = self.allocate_tab_id();
        let ws_id = self.allocate_workspace_id();
        let tab = Tab::new(tab_id, pane);
        self.workspaces
            .push(Workspace::new(ws_id, "default".to_string(), tab));
        self.active_workspace = 0;
        true
    }

    /// Create a tab around `pane` in the active workspace and make it
    /// active (Spec-0001 `NewTab`). Returns `None` — creating nothing —
    /// when no workspace exists (the first pane goes through
    /// [`MultiplexerState::seed`]) or when `pane` is already tracked by a
    /// tab (pane IDs are globally unique; a duplicate is a caller bug and
    /// fails debug builds).
    #[must_use = "None means the tab was not created and the pane is untracked"]
    pub fn new_tab(&mut self, pane: PaneId) -> Option<TabId> {
        self.workspaces.get(self.active_workspace)?;
        if self.tab_containing(pane).is_some() {
            debug_assert!(false, "pane {} is already tracked by a tab", pane.0);
            return None;
        }
        self.note_external_pane_id(pane);
        let tab_id = self.allocate_tab_id();
        let ws = &mut self.workspaces[self.active_workspace];
        ws.tabs.push(Tab::new(tab_id, pane));
        ws.active_tab = ws.tabs.len() - 1;
        Some(tab_id)
    }

    /// Create a workspace with one tab around `pane` and make it active
    /// (Spec-0001 `NewWorkspace`). Returns `None` — creating nothing —
    /// when `pane` is already tracked by a tab (a caller bug; fails debug
    /// builds).
    #[must_use = "None means the workspace was not created and the pane is untracked"]
    pub fn new_workspace(&mut self, name: String, pane: PaneId) -> Option<WorkspaceId> {
        if self.tab_containing(pane).is_some() {
            debug_assert!(false, "pane {} is already tracked by a tab", pane.0);
            return None;
        }
        self.note_external_pane_id(pane);
        let tab_id = self.allocate_tab_id();
        let ws_id = self.allocate_workspace_id();
        let tab = Tab::new(tab_id, pane);
        self.workspaces.push(Workspace::new(ws_id, name, tab));
        self.active_workspace = self.workspaces.len() - 1;
        Some(ws_id)
    }

    /// Focus a pane, activating its workspace and tab so the focused pane
    /// is always visible. Returns false if no tab contains the pane.
    #[must_use = "false means the pane was not focused"]
    pub fn focus_pane(&mut self, pane: PaneId) -> bool {
        let Some((ws_index, tab_index)) = self.locate(pane) else {
            return false;
        };
        let ws = &mut self.workspaces[ws_index];
        if !ws.tabs[tab_index].focus_pane(pane) {
            debug_assert!(false, "located tab must contain pane {}", pane.0);
            return false;
        }
        ws.active_tab = tab_index;
        self.active_workspace = ws_index;
        true
    }

    /// Remove the workspace at `ws_index`, re-pointing the active index.
    fn close_workspace_at(&mut self, ws_index: usize) -> WorkspaceId {
        let closed = self.workspaces.remove(ws_index).id;
        clamp_active_after_remove(&mut self.active_workspace, ws_index, self.workspaces.len());
        closed
    }

    /// Rename a tab (Spec-0001 `RenameTab`). An empty name reverts to the
    /// pane-title fallback. Returns false if no tab has id `tab`.
    #[must_use = "false means the tab was not found and nothing was renamed"]
    pub fn rename_tab(&mut self, tab: TabId, name: String) -> bool {
        let Some((wi, ti)) = self.locate_tab(tab) else {
            return false;
        };
        self.workspaces[wi].tabs[ti].set_name(name);
        true
    }

    /// Rename a workspace (Spec-0001 `RenameWorkspace`). Returns false if
    /// no workspace has id `ws`.
    #[must_use = "false means the workspace was not found and nothing was renamed"]
    pub fn rename_workspace(&mut self, ws: WorkspaceId, name: String) -> bool {
        let Some(workspace) = self.workspaces.iter_mut().find(|w| w.id == ws) else {
            return false;
        };
        workspace.set_name(name);
        true
    }

    /// Reorder a tab within its workspace to `new_index` (Spec-0001
    /// `MoveTab`), clamped to the tab count. The active tab stays on the
    /// same `TabId` regardless of the shuffle. Returns false if no tab has
    /// id `tab`.
    #[must_use = "false means the tab was not found and nothing moved"]
    pub fn move_tab(&mut self, tab: TabId, new_index: usize) -> bool {
        let Some((wi, from)) = self.locate_tab(tab) else {
            return false;
        };
        let ws = &mut self.workspaces[wi];
        let active_id = ws.tabs[ws.active_tab].id;
        let to = new_index.min(ws.tabs.len() - 1);
        if to != from {
            let moved = ws.tabs.remove(from);
            ws.tabs.insert(to, moved);
            // The active tab may have shifted position; follow its id. The
            // reorder only permutes tabs, so active_id is always present —
            // a miss is an invariant break, loud in debug.
            ws.active_tab = ws
                .tabs
                .iter()
                .position(|t| t.id == active_id)
                .unwrap_or_else(|| {
                    debug_assert!(false, "active tab id vanished during move_tab");
                    ws.active_tab
                });
        }
        true
    }

    /// Close an entire workspace and return every pane it held, so the
    /// caller can shut down their PTYs (Spec-0001 `CloseWorkspace`). The
    /// active workspace is re-clamped. Returns `None` — removing nothing —
    /// if no workspace has id `ws`. Closing the last workspace empties the
    /// state; the daemon-exit rule is the caller's policy.
    #[must_use = "the returned panes must be shut down; None means nothing was closed"]
    pub fn close_workspace(&mut self, ws: WorkspaceId) -> Option<Vec<PaneId>> {
        let ws_index = self.workspaces.iter().position(|w| w.id == ws)?;
        let panes: Vec<PaneId> = self.workspaces[ws_index]
            .tabs
            .iter()
            .flat_map(Tab::pane_ids)
            .collect();
        self.close_workspace_at(ws_index);
        Some(panes)
    }

    /// Close a pane, cascading per Spec-0007: the last pane closes its tab,
    /// the last tab closes its workspace. Active indices are re-clamped.
    ///
    /// # Errors
    /// [`LayoutError::PaneNotFound`] if no tab contains the pane.
    pub fn close_pane(&mut self, pane: PaneId) -> Result<PaneCloseOutcome, LayoutError> {
        let Some((ws_index, tab_index)) = self.locate(pane) else {
            return Err(LayoutError::PaneNotFound(pane));
        };

        let ws = &mut self.workspaces[ws_index];
        let tab = &mut ws.tabs[tab_index];
        match tab.close_pane(pane)? {
            CloseOutcome::Removed { focus_hint } => Ok(PaneCloseOutcome::Removed { focus_hint }),
            CloseOutcome::LastPane => {
                let tab_id = tab.id;
                let workspace_closed = ws
                    .close_tab(tab_id)
                    .then(|| self.close_workspace_at(ws_index));
                Ok(PaneCloseOutcome::TabClosed {
                    tab: tab_id,
                    workspace_closed,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> (MultiplexerState, PaneId) {
        let mut mux = MultiplexerState::new();
        let pane = mux.allocate_pane_id();
        assert!(mux.seed(pane));
        (mux, pane)
    }

    #[test]
    fn new_state_has_no_panes() {
        let mux = MultiplexerState::new();
        assert!(!mux.has_panes());
        assert!(mux.active_tab().is_none());
        assert!(mux.active_workspace().is_none());
    }

    #[test]
    fn pane_ids_are_monotonic() {
        let mut mux = MultiplexerState::new();
        assert_eq!(mux.allocate_pane_id(), PaneId(0));
        assert_eq!(mux.allocate_pane_id(), PaneId(1));
        assert_eq!(mux.allocate_pane_id(), PaneId(2));
    }

    #[test]
    fn seed_creates_default_workspace_and_tab() {
        let (mux, pane) = seeded();
        assert!(mux.has_panes());
        let ws = mux.active_workspace().expect("workspace");
        assert_eq!(ws.id(), WorkspaceId(0));
        assert_eq!(ws.name(), "default");
        assert_eq!(ws.tabs().len(), 1);
        let tab = mux.active_tab().expect("tab");
        assert_eq!(tab.id(), TabId(0));
        assert_eq!(tab.focused_pane(), pane);
        assert!(tab.layout().is_leaf());
        assert!(tab.floating().is_empty());
    }

    #[test]
    fn split_does_not_move_focus() {
        let (mut mux, first) = seeded();
        let second = mux.allocate_pane_id();
        mux.tab_containing_mut(first)
            .expect("tab")
            .split(first, second, SplitDirection::Horizontal)
            .expect("split");
        let tab = mux.active_tab().expect("tab");
        assert_eq!(tab.focused_pane(), first);
        assert!(tab.contains(second));
    }

    #[test]
    fn focus_pane_focuses_and_rejects_unknown() {
        let (mut mux, first) = seeded();
        let second = mux.allocate_pane_id();
        mux.tab_containing_mut(first)
            .expect("tab")
            .split(first, second, SplitDirection::Vertical)
            .expect("split");
        assert!(mux.focus_pane(second));
        assert_eq!(mux.active_tab().expect("tab").focused_pane(), second);
        assert!(!mux.focus_pane(PaneId(99)));
        assert_eq!(mux.active_tab().expect("tab").focused_pane(), second);
    }

    #[test]
    fn closing_focused_pane_moves_focus_to_hint() {
        let (mut mux, first) = seeded();
        let second = mux.allocate_pane_id();
        mux.tab_containing_mut(first)
            .expect("tab")
            .split(first, second, SplitDirection::Horizontal)
            .expect("split");
        assert!(mux.focus_pane(second));
        let outcome = mux.close_pane(second).expect("close");
        assert_eq!(outcome, PaneCloseOutcome::Removed { focus_hint: first });
        assert_eq!(mux.active_tab().expect("tab").focused_pane(), first);
    }

    #[test]
    fn closing_unfocused_pane_keeps_focus() {
        let (mut mux, first) = seeded();
        let second = mux.allocate_pane_id();
        mux.tab_containing_mut(first)
            .expect("tab")
            .split(first, second, SplitDirection::Horizontal)
            .expect("split");
        let _ = mux.close_pane(second).expect("close");
        assert_eq!(mux.active_tab().expect("tab").focused_pane(), first);
    }

    #[test]
    fn closing_last_pane_cascades_to_workspace() {
        let (mut mux, pane) = seeded();
        let outcome = mux.close_pane(pane).expect("close");
        assert_eq!(
            outcome,
            PaneCloseOutcome::TabClosed {
                tab: TabId(0),
                workspace_closed: Some(WorkspaceId(0)),
            }
        );
        assert!(!mux.has_panes());
    }

    #[test]
    fn closing_last_pane_of_tab_keeps_sibling_tab() {
        let (mut mux, first) = seeded();
        let second = mux.allocate_pane_id();
        let tab2 = mux.new_tab(second).expect("second tab");
        let outcome = mux.close_pane(second).expect("close");
        assert_eq!(
            outcome,
            PaneCloseOutcome::TabClosed {
                tab: tab2,
                workspace_closed: None,
            }
        );
        let ws = mux.active_workspace().expect("workspace");
        assert_eq!(ws.tabs().len(), 1);
        assert_eq!(ws.active_tab().focused_pane(), first);
    }

    #[test]
    fn new_tab_becomes_active_with_fresh_id() {
        let (mut mux, _) = seeded();
        let pane = mux.allocate_pane_id();
        let tab_id = mux.new_tab(pane).expect("new tab");
        assert_eq!(tab_id, TabId(1));
        let tab = mux.active_tab().expect("tab");
        assert_eq!(tab.id(), tab_id);
        assert_eq!(tab.focused_pane(), pane);
        assert_eq!(mux.active_workspace().expect("ws").active_tab_index(), 1);
    }

    #[test]
    fn new_tab_without_workspace_is_refused() {
        let mut mux = MultiplexerState::new();
        let pane = mux.allocate_pane_id();
        assert!(mux.new_tab(pane).is_none());
        // A refused call must not burn a tab ID: the seeded default tab
        // is still TabId(0).
        assert!(mux.seed(pane));
        assert_eq!(mux.active_tab().expect("tab").id(), TabId(0));
    }

    #[test]
    fn new_workspace_becomes_active_with_fresh_ids() {
        let (mut mux, first) = seeded();
        let pane = mux.allocate_pane_id();
        let ws_id = mux
            .new_workspace("work".to_string(), pane)
            .expect("workspace");
        assert_eq!(ws_id, WorkspaceId(1));
        let ws = mux.active_workspace().expect("workspace");
        assert_eq!(ws.id(), ws_id);
        assert_eq!(ws.name(), "work");
        assert_eq!(ws.active_tab().focused_pane(), pane);
        // The seeded workspace is intact behind it.
        assert!(mux.focus_pane(first));
        assert_eq!(mux.active_workspace_index(), 0);
    }

    #[test]
    fn focus_pane_activates_its_workspace_and_tab() {
        let (mut mux, first) = seeded();
        let second = mux.allocate_pane_id();
        assert!(mux.new_workspace("work".to_string(), second).is_some());
        assert_eq!(mux.active_workspace_index(), 1);
        assert!(mux.focus_pane(first));
        assert_eq!(mux.active_workspace_index(), 0);
        assert_eq!(mux.active_tab().expect("tab").focused_pane(), first);
    }

    #[test]
    fn closing_active_workspace_clamps_active_index() {
        let (mut mux, first) = seeded();
        let second = mux.allocate_pane_id();
        assert!(mux.new_workspace("work".to_string(), second).is_some());
        let outcome = mux.close_pane(second).expect("close");
        assert!(matches!(
            outcome,
            PaneCloseOutcome::TabClosed {
                workspace_closed: Some(_),
                ..
            }
        ));
        assert_eq!(mux.active_workspace_index(), 0);
        assert_eq!(mux.active_tab().expect("tab").focused_pane(), first);
    }

    #[test]
    fn closing_earlier_workspace_keeps_active_workspace_stable() {
        let (mut mux, first) = seeded();
        let second = mux.allocate_pane_id();
        assert!(mux.new_workspace("work".to_string(), second).is_some());
        // Active is workspace 1; close workspace 0's only pane.
        let outcome = mux.close_pane(first).expect("close");
        assert!(matches!(
            outcome,
            PaneCloseOutcome::TabClosed {
                workspace_closed: Some(WorkspaceId(0)),
                ..
            }
        ));
        assert_eq!(mux.active_tab().expect("tab").focused_pane(), second);
    }

    #[test]
    fn closing_earlier_tab_keeps_active_tab_stable() {
        let (mut mux, first) = seeded();
        let second = mux.allocate_pane_id();
        let _ = mux.new_tab(second).expect("second tab");
        // Active is tab index 1; close tab 0's only pane.
        let outcome = mux.close_pane(first).expect("close");
        assert!(matches!(outcome, PaneCloseOutcome::TabClosed { .. }));
        let ws = mux.active_workspace().expect("workspace");
        assert_eq!(ws.active_tab_index(), 0);
        assert_eq!(ws.active_tab().focused_pane(), second);
    }

    #[test]
    fn close_unknown_pane_is_an_error() {
        let (mut mux, _) = seeded();
        assert_eq!(
            mux.close_pane(PaneId(42)),
            Err(LayoutError::PaneNotFound(PaneId(42)))
        );
    }

    #[test]
    fn tab_containing_finds_panes_across_workspaces() {
        let (mut mux, first) = seeded();
        let second = mux.allocate_pane_id();
        assert!(mux.new_workspace("work".to_string(), second).is_some());
        assert_eq!(mux.tab_containing(first).map(Tab::id), Some(TabId(0)));
        assert_eq!(mux.tab_containing(second).map(Tab::id), Some(TabId(1)));
        assert!(mux.tab_containing(PaneId(99)).is_none());
    }

    #[test]
    fn seed_refuses_when_already_seeded() {
        let (mut mux, pane) = seeded();
        let before = mux.clone();
        assert!(!mux.seed(pane));
        assert_eq!(mux, before);
    }

    #[test]
    fn reseed_after_full_close_uses_fresh_ids() {
        let (mut mux, pane) = seeded();
        let _ = mux.close_pane(pane).expect("close");
        assert!(!mux.has_panes());
        let second = mux.allocate_pane_id();
        assert_ne!(second, pane);
        assert!(mux.seed(second));
        let ws = mux.active_workspace().expect("workspace");
        assert_eq!(ws.id(), WorkspaceId(1));
        assert_eq!(ws.active_tab().id(), TabId(1));
    }

    #[test]
    fn seed_advances_the_pane_allocator_past_external_ids() {
        let mut mux = MultiplexerState::new();
        assert!(mux.seed(PaneId(7)));
        assert_eq!(mux.allocate_pane_id(), PaneId(8));
    }

    #[test]
    #[cfg_attr(debug_assertions, should_panic(expected = "already tracked"))]
    fn new_tab_rejects_a_pane_tracked_elsewhere() {
        let (mut mux, first) = seeded();
        assert!(mux.new_tab(first).is_none());
    }

    #[test]
    #[cfg_attr(debug_assertions, should_panic(expected = "already tracked"))]
    fn new_workspace_rejects_a_pane_tracked_elsewhere() {
        let (mut mux, first) = seeded();
        assert!(mux.new_workspace("dup".to_string(), first).is_none());
    }

    #[test]
    fn split_pane_rejects_a_pane_tracked_in_another_tab() {
        let (mut mux, first) = seeded();
        let second = mux.allocate_pane_id();
        assert!(mux.new_tab(second).is_some());
        // `first` lives in tab 0; splitting it into tab 1's tree is refused.
        assert_eq!(
            mux.split_pane(second, first, SplitDirection::Horizontal),
            Err(LayoutError::PaneAlreadyPresent(first))
        );
    }

    #[test]
    fn tab_split_rejects_a_pane_already_in_the_tab() {
        let (mut mux, first) = seeded();
        let second = mux.allocate_pane_id();
        let tab = mux.tab_containing_mut(first).expect("tab");
        tab.split(first, second, SplitDirection::Horizontal)
            .expect("split");
        assert_eq!(
            tab.split(first, second, SplitDirection::Vertical),
            Err(LayoutError::PaneAlreadyPresent(second))
        );
    }

    #[test]
    fn closing_a_pane_in_a_background_workspace_keeps_the_view() {
        let (mut mux, first) = seeded();
        let second = mux.allocate_pane_id();
        mux.tab_containing_mut(first)
            .expect("tab")
            .split(first, second, SplitDirection::Horizontal)
            .expect("split");
        assert!(mux.focus_pane(second));
        let third = mux.allocate_pane_id();
        assert!(mux.new_workspace("work".to_string(), third).is_some());
        assert_eq!(mux.active_workspace_index(), 1);
        let outcome = mux.close_pane(second).expect("close");
        assert_eq!(outcome, PaneCloseOutcome::Removed { focus_hint: first });
        // The active view is untouched; the background tab's focus moved.
        assert_eq!(mux.active_workspace_index(), 1);
        assert_eq!(
            mux.tab_containing(first).expect("tab").focused_pane(),
            first
        );
    }

    #[test]
    fn background_tab_keeps_its_own_focused_pane() {
        let (mut mux, a1) = seeded();
        let a2 = mux.allocate_pane_id();
        mux.tab_containing_mut(a1)
            .expect("tab")
            .split(a1, a2, SplitDirection::Horizontal)
            .expect("split");
        assert!(mux.focus_pane(a2));
        let b1 = mux.allocate_pane_id();
        assert!(mux.new_tab(b1).is_some());
        assert_eq!(mux.workspaces()[0].tabs()[0].focused_pane(), a2);
    }

    #[test]
    fn closing_a_middle_tab_while_the_last_is_active_follows_the_active_tab() {
        let (mut mux, _a) = seeded();
        let b = mux.allocate_pane_id();
        assert!(mux.new_tab(b).is_some());
        let c = mux.allocate_pane_id();
        let tc = mux.new_tab(c).expect("tab c");
        // Active is c (index 2); closing b's tab (index 1) must follow it.
        let _ = mux.close_pane(b).expect("close");
        let ws = mux.active_workspace().expect("workspace");
        assert_eq!(ws.active_tab().id(), tc);
    }

    #[test]
    fn closing_the_active_middle_tab_activates_the_next_tab() {
        let (mut mux, _a) = seeded();
        let b = mux.allocate_pane_id();
        let tb = mux.new_tab(b).expect("tab b");
        let c = mux.allocate_pane_id();
        let tc = mux.new_tab(c).expect("tab c");
        assert!(mux.focus_pane(b));
        assert_eq!(mux.active_tab().expect("tab").id(), tb);
        let _ = mux.close_pane(b).expect("close");
        assert_eq!(mux.active_tab().expect("tab").id(), tc);
    }

    #[test]
    fn closing_a_middle_workspace_while_the_last_is_active_follows_the_active_workspace() {
        let (mut mux, _a) = seeded();
        let b = mux.allocate_pane_id();
        assert!(mux.new_workspace("b".to_string(), b).is_some());
        let c = mux.allocate_pane_id();
        let wc = mux.new_workspace("c".to_string(), c).expect("ws c");
        let _ = mux.close_pane(b).expect("close");
        assert_eq!(mux.active_workspace().expect("workspace").id(), wc);
    }

    #[test]
    fn rename_tab_pins_a_name_and_empty_clears_it() {
        let (mut mux, _) = seeded();
        assert!(mux.rename_tab(TabId(0), "build".to_string()));
        assert_eq!(mux.active_tab().expect("tab").name(), "build");
        // Empty name reverts to the pane-title fallback.
        assert!(mux.rename_tab(TabId(0), String::new()));
        assert_eq!(mux.active_tab().expect("tab").name(), "");
        // Unknown tab is a no-op.
        assert!(!mux.rename_tab(TabId(99), "nope".to_string()));
    }

    #[test]
    fn rename_workspace_sets_name_and_rejects_unknown() {
        let (mut mux, _) = seeded();
        assert!(mux.rename_workspace(WorkspaceId(0), "work".to_string()));
        assert_eq!(mux.active_workspace().expect("ws").name(), "work");
        assert!(!mux.rename_workspace(WorkspaceId(99), "nope".to_string()));
    }

    #[test]
    fn move_tab_reorders_and_keeps_active_on_its_id() {
        let (mut mux, _a) = seeded();
        let b = mux.allocate_pane_id();
        let tb = mux.new_tab(b).expect("tab b");
        let c = mux.allocate_pane_id();
        let tc = mux.new_tab(c).expect("tab c");
        // Order is [0, tb, tc], active is tc (index 2).
        assert!(mux.move_tab(tc, 0));
        let ws = mux.active_workspace().expect("ws");
        let ids: Vec<TabId> = ws.tabs().iter().map(Tab::id).collect();
        assert_eq!(ids, vec![tc, TabId(0), tb]);
        // Active tab still follows tc, now at index 0.
        assert_eq!(ws.active_tab().id(), tc);
        assert_eq!(ws.active_tab_index(), 0);
    }

    #[test]
    fn move_tab_of_a_background_tab_keeps_the_active_tab() {
        let (mut mux, _a) = seeded();
        let b = mux.allocate_pane_id();
        let tb = mux.new_tab(b).expect("tab b");
        let c = mux.allocate_pane_id();
        let tc = mux.new_tab(c).expect("tab c");
        // Order [0, tb, tc]; make the middle tab (tb) active.
        assert!(mux.focus_pane(b));
        assert_eq!(mux.active_workspace().expect("ws").active_tab().id(), tb);
        // Move a non-active tab (tc) across the active slot to the front.
        assert!(mux.move_tab(tc, 0));
        let ws = mux.active_workspace().expect("ws");
        let ids: Vec<TabId> = ws.tabs().iter().map(Tab::id).collect();
        assert_eq!(ids, vec![tc, TabId(0), tb]);
        // The active tab is still tb, now shifted to index 2.
        assert_eq!(ws.active_tab().id(), tb);
        assert_eq!(ws.active_tab_index(), 2);
    }

    #[test]
    fn move_tab_clamps_index_and_is_noop_for_same_slot() {
        let (mut mux, _a) = seeded();
        let b = mux.allocate_pane_id();
        let tb = mux.new_tab(b).expect("tab b");
        // Past-the-end index clamps to the last slot.
        assert!(mux.move_tab(TabId(0), 99));
        let ids: Vec<TabId> = mux
            .active_workspace()
            .expect("ws")
            .tabs()
            .iter()
            .map(Tab::id)
            .collect();
        assert_eq!(ids, vec![tb, TabId(0)]);
        // Moving to the slot it already occupies changes nothing.
        assert!(mux.move_tab(tb, 0));
        let ids: Vec<TabId> = mux
            .active_workspace()
            .expect("ws")
            .tabs()
            .iter()
            .map(Tab::id)
            .collect();
        assert_eq!(ids, vec![tb, TabId(0)]);
        // Unknown tab is a no-op.
        assert!(!mux.move_tab(TabId(99), 0));
    }

    #[test]
    fn close_workspace_returns_every_pane_and_reclamps() {
        let (mut mux, first) = seeded();
        // Give workspace 0 a second tab with two panes.
        let b = mux.allocate_pane_id();
        let _ = mux.new_tab(b).expect("tab b");
        let c = mux.allocate_pane_id();
        mux.tab_containing_mut(b)
            .expect("tab")
            .split(b, c, SplitDirection::Horizontal)
            .expect("split");
        // A second workspace, now active.
        let d = mux.allocate_pane_id();
        let wd = mux.new_workspace("d".to_string(), d).expect("ws d");
        assert_eq!(mux.active_workspace().expect("ws").id(), wd);

        let mut panes = mux.close_workspace(WorkspaceId(0)).expect("closed");
        panes.sort();
        assert_eq!(panes, vec![first, b, c]);
        // Active workspace (wd) is preserved; only wd remains.
        assert_eq!(mux.workspaces().len(), 1);
        assert_eq!(mux.active_workspace().expect("ws").id(), wd);
        assert!(mux.tab_containing(first).is_none());
    }

    #[test]
    fn close_workspace_rejects_unknown_and_can_empty_the_state() {
        let (mut mux, pane) = seeded();
        assert!(mux.close_workspace(WorkspaceId(99)).is_none());
        // Closing the only workspace empties the state (daemon-exit policy
        // is the caller's).
        let panes = mux.close_workspace(WorkspaceId(0)).expect("closed");
        assert_eq!(panes, vec![pane]);
        assert!(!mux.has_panes());
    }
}
