//! Bridges client render state (per-pane grids, font metrics, and the shared
//! `A11yModel`) into `oakterm_a11y` tree updates, and maps between the
//! terminal's cell-based `Selection` and AccessKit's between-character
//! `TextSelection` positions.
//!
//! Three entry points, each mutating the model and building the matching
//! tree update under one lock so snapshot state can never diverge from
//! what assistive technology was told: [`apply`] for per-pane content
//! events, [`sync_layout`] for split-topology and geometry changes, and
//! [`set_focus`] for focus moves between panes.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use oakterm_a11y::{Announcement, SelectionRange};
use oakterm_renderer::shaper::FontMetrics;
use oakterm_terminal::grid::selection::{AnchorSide, Selection, SelectionType};

use crate::pane_view::PaneView;
use crate::render_grid::ClientGrid;
use tracing::{debug, warn};

struct PaneA11ySnapshot {
    rows: u16,
    cols: u16,
    row_texts: Vec<String>,
    cursor_row: u16,
    cursor_col: u16,
    title: String,
    scrollback_lines: u64,
    scroll_offset: u64,
    selection: Option<SelectionRange>,
    /// Pane pixel origin; row bounds are positioned relative to it.
    /// Updated by [`sync_layout`] whenever the split geometry changes.
    origin: (f64, f64),
}

impl PaneA11ySnapshot {
    fn from_view(view: &PaneView) -> Self {
        let grid = view.grid();
        Self {
            rows: grid.rows,
            cols: grid.cols,
            row_texts: grid.row_texts(),
            cursor_row: grid.cursor_y,
            cursor_col: grid.cursor_x,
            title: String::new(),
            scrollback_lines: 0,
            scroll_offset: u64::from(view.viewport_offset()),
            selection: view_selection(view, view.viewport_offset()),
            origin: (0.0, 0.0),
        }
    }

    /// Refresh the grid-derived fields at the given viewport offset.
    fn refresh_from(&mut self, view: &PaneView, offset: u32) {
        let grid = view.grid();
        self.rows = grid.rows;
        self.cols = grid.cols;
        self.row_texts = grid.row_texts();
        self.cursor_row = grid.cursor_y;
        self.cursor_col = grid.cursor_x;
        self.scroll_offset = u64::from(offset);
        self.selection = view_selection(view, offset);
    }
}

fn view_selection(view: &PaneView, offset: u32) -> Option<SelectionRange> {
    view.selection
        .as_ref()
        .and_then(|s| selection_range(s, offset, view.grid().rows))
}

/// What changed this frame; drives which snapshot fields [`apply`] mutates
/// and what the resulting tree update carries.
#[derive(Clone, Copy)]
pub(crate) enum A11yEvent<'a> {
    /// Live-view grid update. `dirty_rows` is `(row index, text)` from the
    /// daemon's `RenderUpdate`; output announcements derive from it.
    Render { dirty_rows: &'a [(u16, String)] },
    /// Viewport scrolled: every visible row changed. `total_rows` is the
    /// daemon's scrollback length (`scroll_y_max`).
    Scrollback { total_rows: u64 },
    /// Pane title changed. The OS window title is the caller's concern.
    Title(&'a str),
    /// The tracked selection may have changed; no-op when it hasn't.
    SelectionChanged,
    /// Grid dimensions changed: refresh the snapshot and rebuild the full
    /// tree (row node IDs are only stable while dimensions hold).
    Resize,
    /// Push a live-region announcement (bell); no snapshot mutation.
    Announce(&'a Announcement),
    /// Clear the live region so the next identical text re-announces.
    ClearAnnouncement,
}

impl A11yEvent<'_> {
    /// Log label naming which user-visible behavior an event carries.
    fn kind(&self) -> &'static str {
        match self {
            Self::Render { .. } => "render",
            Self::Scrollback { .. } => "scrollback",
            Self::Title(_) => "title",
            Self::SelectionChanged => "selection",
            Self::Resize => "resize",
            Self::Announce(_) => "announce",
            Self::ClearAnnouncement => "clear-announcement",
        }
    }
}

/// Snapshot of all panes for the accessibility tree. Shared between `App`
/// and the AccessKit activation handler via `Arc<Mutex<Option<_>>>`; all
/// mutation goes through the module's entry points ([`apply`],
/// [`sync_layout`], [`set_focus`]) so the snapshot and the updates sent
/// to AT stay consistent.
pub(crate) struct A11yModel {
    panes: HashMap<u32, PaneA11ySnapshot>,
    focused: u32,
    cell_width: f64,
    cell_height: f64,
    /// Debounce for output announcements: at most one per 100ms.
    last_announcement: Option<Instant>,
}

impl A11yModel {
    pub(crate) fn new(focused: u32, (cell_width, cell_height): (f64, f64)) -> Self {
        Self {
            panes: HashMap::new(),
            focused,
            cell_width,
            cell_height,
            last_announcement: None,
        }
    }

    /// Split panes register through [`sync_layout`]; this direct path
    /// seeds pane 0 at startup.
    pub(crate) fn register_pane(&mut self, pane_id: u32, view: &PaneView) {
        self.panes
            .insert(pane_id, PaneA11ySnapshot::from_view(view));
    }

    /// Update cell pixel dimensions after a runtime font change. Row bounds
    /// derive from these, so the caller should follow with an
    /// [`A11yEvent::Resize`] to rebuild the tree.
    pub(crate) fn set_cell_dims(&mut self, (cell_width, cell_height): (f64, f64)) {
        self.cell_width = cell_width;
        self.cell_height = cell_height;
    }

    /// Build the full tree from every pane snapshot, in stable pane-id
    /// order so repeated builds produce identical child lists.
    pub(crate) fn build_full_tree(&self) -> accesskit::TreeUpdate {
        let mut ids: Vec<u32> = self.panes.keys().copied().collect();
        ids.sort_unstable();
        let panes: Vec<oakterm_a11y::PaneInput<'_>> = ids
            .iter()
            .map(|id| {
                let snap = &self.panes[id];
                oakterm_a11y::PaneInput {
                    pane_id: *id,
                    rows: snap.rows,
                    cols: snap.cols,
                    row_texts: &snap.row_texts,
                    cursor_row: snap.cursor_row,
                    cursor_col: snap.cursor_col,
                    title: &snap.title,
                    scrollback_lines: snap.scrollback_lines,
                    scroll_offset: snap.scroll_offset,
                    selection: snap.selection,
                    origin: snap.origin,
                }
            })
            .collect();
        oakterm_a11y::build_initial_tree(&oakterm_a11y::TreeInput {
            panes: &panes,
            focused: self.focused,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
        })
    }

    /// Announce output that reached the bottom rows, debounced to one per
    /// 100ms. The timestamp is consumed only when an announcement is
    /// produced, so a suppressed or empty candidate never eats the window.
    fn output_announcement(
        &mut self,
        dirty_rows: &[(u16, String)],
        grid_rows: u16,
    ) -> Option<Announcement> {
        if dirty_rows.is_empty() || grid_rows == 0 {
            return None;
        }
        let bottom = grid_rows - 1;
        let has_bottom = dirty_rows.iter().any(|(i, _)| *i == bottom);
        let debounce_ok = self
            .last_announcement
            .is_none_or(|t| t.elapsed().as_millis() >= 100);
        if !(has_bottom && debounce_ok) {
            return None;
        }
        let text: String = dirty_rows
            .iter()
            .filter(|(i, _)| *i >= bottom.saturating_sub(2))
            .map(|(_, t)| t.as_str())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            return None;
        }
        self.last_announcement = Some(Instant::now());
        Some(Announcement {
            text,
            level: accesskit::Live::Polite,
        })
    }
}

/// What an event contributes to the incremental update, decided while the
/// snapshot is mutated.
#[derive(Default)]
struct EventOutcome<'a> {
    dirty_rows: std::borrow::Cow<'a, [(u16, String)]>,
    cursor_changed: bool,
    title_changed: bool,
    selection_changed: bool,
}

/// Mutate the snapshot for one event. Returns `None` when the event changed
/// nothing worth pushing. `Resize` is handled by [`apply`] (it needs the
/// whole model for the full-tree rebuild); `Announce`/`ClearAnnouncement`
/// never reach here (they touch no pane state).
fn apply_to_snapshot<'a>(
    snap: &mut PaneA11ySnapshot,
    view: &PaneView,
    event: &A11yEvent<'a>,
) -> Option<EventOutcome<'a>> {
    let grid = view.grid();
    match event {
        A11yEvent::Render { dirty_rows } => {
            let cursor_changed =
                snap.cursor_row != grid.cursor_y || snap.cursor_col != grid.cursor_x;
            // The render path only runs on the live view, so the offset is
            // 0 whenever that contract holds — and stays truthful if not.
            snap.refresh_from(view, view.viewport_offset());
            if dirty_rows.is_empty() && !cursor_changed {
                return None;
            }
            Some(EventOutcome {
                dirty_rows: std::borrow::Cow::Borrowed(dirty_rows),
                cursor_changed,
                ..Default::default()
            })
        }
        A11yEvent::Scrollback { total_rows } => {
            snap.refresh_from(view, view.viewport_offset());
            snap.scrollback_lines = *total_rows;
            // Every visible row changed; cursor_changed forces the terminal
            // rebuild that carries the new scroll position.
            Some(EventOutcome {
                dirty_rows: (0..grid.rows)
                    .map(|i| (i, grid.row_text(i)))
                    .collect::<Vec<_>>()
                    .into(),
                cursor_changed: true,
                ..Default::default()
            })
        }
        A11yEvent::Title(title) => {
            snap.title = (*title).to_string();
            // The rebuilt terminal node carries scroll state; keep it
            // current even when no scroll event refreshed the snapshot yet.
            snap.scroll_offset = u64::from(view.viewport_offset());
            Some(EventOutcome {
                title_changed: true,
                ..Default::default()
            })
        }
        A11yEvent::SelectionChanged => {
            let sel = view_selection(view, view.viewport_offset());
            if snap.selection == sel {
                return None;
            }
            snap.selection = sel;
            snap.scroll_offset = u64::from(view.viewport_offset());
            Some(EventOutcome {
                selection_changed: true,
                ..Default::default()
            })
        }
        A11yEvent::Announce(_) | A11yEvent::ClearAnnouncement | A11yEvent::Resize => {
            unreachable!("handled by apply before the snapshot step")
        }
    }
}

/// Apply an event to a pane's snapshot and build the tree update to push,
/// all under one model lock. Returns `None` when there is nothing to push:
/// the model is unpopulated or poisoned, the pane is untracked (the tree
/// never parented its nodes), or the event changed nothing.
pub(crate) fn apply(
    state: &Mutex<Option<A11yModel>>,
    pane_id: u32,
    view: &PaneView,
    event: A11yEvent<'_>,
) -> Option<accesskit::TreeUpdate> {
    let mut guard = match state.lock() {
        Ok(g) => g,
        Err(e) => {
            warn!(error = %e, event = event.kind(), "a11y: mutex poisoned");
            return None;
        }
    };
    let Some(model) = guard.as_mut() else {
        debug!(
            pane_id,
            event = event.kind(),
            "a11y: event before model init"
        );
        return None;
    };
    let grid = view.grid();

    // Announcements touch only the shared window-level live region, so
    // they need no pane snapshot and skip the pane-presence gate.
    match event {
        A11yEvent::Announce(ann) => {
            return Some(build_update(
                model,
                pane_id,
                grid,
                &EventOutcome::default(),
                Some(ann),
            ));
        }
        A11yEvent::ClearAnnouncement => {
            return Some(build_update(
                model,
                pane_id,
                grid,
                &EventOutcome::default(),
                None,
            ));
        }
        _ => {}
    }

    if !model.panes.contains_key(&pane_id) {
        debug!(
            pane_id,
            event = event.kind(),
            "a11y: event for pane not in a11y model"
        );
        return None;
    }

    if let A11yEvent::Resize = event {
        // Row node IDs are recalculated when dimensions change; the
        // viewport was reset to 0 by the caller.
        let snap = model.panes.get_mut(&pane_id).expect("presence checked");
        snap.refresh_from(view, view.viewport_offset());
        return Some(model.build_full_tree());
    }

    let snap = model.panes.get_mut(&pane_id).expect("presence checked");
    let outcome = apply_to_snapshot(snap, view, &event)?;

    let announcement = if let A11yEvent::Render { dirty_rows } = event {
        model.output_announcement(dirty_rows, grid.rows)
    } else {
        None
    };
    Some(build_update(
        model,
        pane_id,
        grid,
        &outcome,
        announcement.as_ref(),
    ))
}

/// Reconcile the model with the visible split layout: register panes new
/// to `origins`, drop panes that left it, and adopt per-pane pixel
/// origins. Returns the full tree to push when anything changed — row
/// bounds derive from origins, so any origin or dimension shift rebuilds
/// the tree. Callers keep `origins` equal to the set of panes on screen.
pub(crate) fn sync_layout(
    state: &Mutex<Option<A11yModel>>,
    panes: &HashMap<u32, PaneView>,
    origins: &[(u32, (f64, f64))],
) -> Option<accesskit::TreeUpdate> {
    let mut guard = match state.lock() {
        Ok(g) => g,
        Err(e) => {
            warn!(error = %e, "a11y: mutex poisoned in layout sync");
            return None;
        }
    };
    let Some(model) = guard.as_mut() else {
        debug!("a11y: layout sync before model init");
        return None;
    };

    let mut changed = false;
    for &(pane_id, origin) in origins {
        let Some(view) = panes.get(&pane_id) else {
            // A visible pane absent from the a11y tree is a caller-
            // contract breach, not routine degradation.
            warn!(pane_id, "a11y: layout pane without a view; not tracked");
            continue;
        };
        if let Some(snap) = model.panes.get_mut(&pane_id) {
            let dims_changed = snap.rows != view.grid().rows || snap.cols != view.grid().cols;
            if dims_changed {
                snap.refresh_from(view, view.viewport_offset());
                snap.origin = origin;
                changed = true;
            } else if snap.origin != origin {
                // Origin-only shift (live divider drag): row bounds
                // derive from the origin at build time, so skip the
                // full row-text refresh; carry the scroll offset the
                // resize path may have reset.
                snap.origin = origin;
                snap.scroll_offset = u64::from(view.viewport_offset());
                changed = true;
            }
        } else {
            let mut snap = PaneA11ySnapshot::from_view(view);
            snap.origin = origin;
            model.panes.insert(pane_id, snap);
            changed = true;
        }
    }
    let before = model.panes.len();
    model
        .panes
        .retain(|id, _| origins.iter().any(|&(o, _)| o == *id));
    if model.panes.len() != before {
        changed = true;
        if !model.panes.contains_key(&model.focused) {
            debug!(
                focused = model.focused,
                "a11y: focused pane left the layout; focus falls back to the window"
            );
        }
    }

    changed.then(|| model.build_full_tree())
}

/// Move accessibility focus to `pane_id`'s terminal node. Returns the
/// focus-only update to push, or `None` when focus is already there or
/// the pane isn't in the tree yet — the intent is still recorded then,
/// so the next full tree carries this focus.
pub(crate) fn set_focus(
    state: &Mutex<Option<A11yModel>>,
    pane_id: u32,
) -> Option<accesskit::TreeUpdate> {
    let mut guard = match state.lock() {
        Ok(g) => g,
        Err(e) => {
            warn!(error = %e, "a11y: mutex poisoned in focus change");
            return None;
        }
    };
    let Some(model) = guard.as_mut() else {
        debug!(pane_id, "a11y: focus change before model init");
        return None;
    };
    if model.focused == pane_id {
        return None;
    }
    model.focused = pane_id;
    if !model.panes.contains_key(&pane_id) {
        debug!(pane_id, "a11y: focus target not yet in a11y model");
        return None;
    }
    Some(oakterm_a11y::build_focus_update(pane_id))
}

/// Build the incremental update from the (already mutated) snapshot. For
/// announcement-only pushes the pane may be untracked — the terminal node
/// is not rebuilt then, so the snapshot-derived fields are unused defaults.
fn build_update(
    model: &A11yModel,
    pane_id: u32,
    grid: &ClientGrid,
    outcome: &EventOutcome<'_>,
    announcement: Option<&Announcement>,
) -> accesskit::TreeUpdate {
    let empty = PaneA11ySnapshot {
        rows: 0,
        cols: 0,
        row_texts: Vec::new(),
        cursor_row: 0,
        cursor_col: 0,
        title: String::new(),
        scrollback_lines: 0,
        scroll_offset: 0,
        selection: None,
        origin: (0.0, 0.0),
    };
    let snap = model.panes.get(&pane_id).unwrap_or(&empty);
    let cursor_row_text = grid.row_text(grid.cursor_y);
    // The update's focus must name a live node. `model.focused` can
    // transiently name an untracked pane (focus intent recorded before
    // its pane's layout sync, or the focused pane pruned); fall back to
    // the updating pane, then to the window (announce-only pushes may
    // come from an untracked pane too).
    let focused = if model.panes.contains_key(&model.focused) {
        Some(model.focused)
    } else if model.panes.contains_key(&pane_id) {
        Some(pane_id)
    } else {
        None
    };
    let input = oakterm_a11y::IncrementalInput {
        pane_id,
        focused,
        rows: grid.rows,
        cols: grid.cols,
        dirty_rows: &outcome.dirty_rows,
        cursor_row: grid.cursor_y,
        cursor_col: grid.cursor_x,
        cursor_changed: outcome.cursor_changed,
        cursor_row_text: &cursor_row_text,
        title: &snap.title,
        title_changed: outcome.title_changed,
        scrollback_lines: snap.scrollback_lines,
        scroll_offset: snap.scroll_offset,
        selection: snap.selection.map(|s| clamp_selection_cols(s, grid)),
        selection_changed: outcome.selection_changed,
        announcement,
        cell_width: model.cell_width,
        cell_height: model.cell_height,
        origin: snap.origin,
    };
    oakterm_a11y::build_incremental_update(&input)
}

/// Resolve cell pixel dimensions, falling back to 8x16 before the font is
/// initialized.
pub(crate) fn cell_dims(metrics: Option<&FontMetrics>) -> (f64, f64) {
    metrics.map_or((8.0, 16.0), |m| {
        (f64::from(m.cell_width), f64::from(m.cell_height))
    })
}

/// Clamp selection character positions to each row's trimmed text length.
/// Incremental tree updates carry no row texts, so this is the only place
/// the exact clamp can happen (the a11y crate bounds to `cols` as a
/// backstop). Also resolves the `Line`-selection end-of-row sentinel.
fn clamp_selection_cols(mut sel: SelectionRange, grid: &ClientGrid) -> SelectionRange {
    let text_len = |row: u16| {
        let row = row.min(grid.rows.saturating_sub(1));
        grid.row_text(row).chars().count()
    };
    sel.anchor_col = sel.anchor_col.min(text_len(sel.anchor_row));
    sel.focus_col = sel.focus_col.min(text_len(sel.focus_row));
    sel
}

/// Map the terminal's cell-inclusive `Selection` to visible-viewport
/// between-character positions. Selection rows are viewport-relative
/// (`visible_row = sel_row + viewport_offset`); rows partially inside the
/// `rows`-tall viewport clamp to its edge per Spec-0006, and a selection
/// entirely outside it (above or below) yields `None`. `Block` selections
/// map to the stream range between their corners — AccessKit's
/// `TextSelection` cannot express a rectangle, so the intermediate rows
/// read as fully selected.
pub(crate) fn selection_range(
    sel: &Selection,
    viewport_offset: u32,
    rows: u16,
) -> Option<SelectionRange> {
    let (start, end) = sel.normalized();
    let to_visible = |row: i64| row + i64::from(viewport_offset);
    let (start_vis, end_vis) = (to_visible(start.row), to_visible(end.row));
    if end_vis < 0 || start_vis >= i64::from(rows) {
        return None;
    }
    let clamp_row = |row: i64| {
        u16::try_from(row.max(0))
            .unwrap_or(u16::MAX)
            .min(rows.saturating_sub(1))
    };

    // Between-character position after a cell is its column + 1.
    let boundary = |anchor: &oakterm_terminal::grid::selection::SelectionAnchor| {
        usize::from(anchor.col) + usize::from(anchor.side == AnchorSide::Right)
    };
    let (anchor_col, focus_col) = match sel.ty {
        // Full rows; callers clamp to each row's text length.
        SelectionType::Line => (0, usize::MAX),
        _ => (boundary(&start), boundary(&end)),
    };
    Some(SelectionRange {
        anchor_row: clamp_row(start_vis),
        anchor_col,
        focus_row: clamp_row(end_vis),
        focus_col,
    })
}

/// Map an AT-requested `TextSelection` back onto a pane and a terminal
/// `Selection`. Returns the target pane and `None` selection for a
/// collapsed (caret) request, which callers treat as clear-selection.
/// Fails when either endpoint is not a row node, the endpoints span
/// panes, or a row is outside the `rows`-tall viewport (otherwise the AT
/// request would "succeed" while selecting nothing visible).
pub(crate) fn selection_from_a11y(
    sel: &accesskit::TextSelection,
    viewport_offset: u32,
    rows: u16,
) -> Option<(u32, Option<Selection>)> {
    let (anchor_pane, Some(anchor_row)) = oakterm_a11y::decode_node_id(sel.anchor.node)? else {
        return None;
    };
    let (focus_pane, Some(focus_row)) = oakterm_a11y::decode_node_id(sel.focus.node)? else {
        return None;
    };
    if anchor_pane != focus_pane || anchor_row >= rows || focus_row >= rows {
        return None;
    }

    let a = (anchor_row, sel.anchor.character_index);
    let f = (focus_row, sel.focus.character_index);
    if a == f {
        return Some((anchor_pane, None));
    }
    let (first, last) = if a <= f { (a, f) } else { (f, a) };

    let to_sel_row = |visible: u16| i64::from(visible) - i64::from(viewport_offset);
    let col = |c: usize| u16::try_from(c).unwrap_or(u16::MAX);
    let mut selection = Selection::new(
        SelectionType::Normal,
        to_sel_row(first.0),
        col(first.1),
        AnchorSide::Left,
    );
    // End side Left excludes the boundary cell, matching the exclusive
    // between-character focus position. A focus at column 0 of a later row
    // selects nothing on that row (`contains_normal` would still include
    // cell 0), so the selection ends at the previous row's end instead.
    if last.1 == 0 {
        selection.update(to_sel_row(last.0 - 1), u16::MAX, AnchorSide::Right);
    } else {
        selection.update(to_sel_row(last.0), col(last.1), AnchorSide::Left);
    }
    Some((anchor_pane, Some(selection)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use oakterm_a11y::{ANNOUNCEMENT_ID, row_node_id, terminal_node_id};

    fn tracked_model(pane_id: u32, view: &PaneView) -> Mutex<Option<A11yModel>> {
        let mut model = A11yModel::new(pane_id, (8.0, 16.0));
        model.register_pane(pane_id, view);
        Mutex::new(Some(model))
    }

    fn announcement_value(update: &accesskit::TreeUpdate) -> Option<String> {
        update
            .nodes
            .iter()
            .find(|(id, _)| *id == ANNOUNCEMENT_ID)
            .and_then(|(_, n)| n.value().map(str::to_string))
    }

    #[test]
    fn cell_dims_falls_back_without_font() {
        assert_eq!(cell_dims(None), (8.0, 16.0));
    }

    #[test]
    fn sync_layout_registers_new_panes_and_dedups_unchanged() {
        let view0 = PaneView::new(ClientGrid::new(10, 4));
        let view1 = PaneView::new(ClientGrid::new(10, 4));
        let state = tracked_model(0, &view0);
        let mut panes = HashMap::new();
        panes.insert(0, view0);
        panes.insert(1, view1);
        let origins = [(0, (0.0, 0.0)), (1, (200.0, 0.0))];

        let update = sync_layout(&state, &panes, &origins).expect("new pane changes the tree");
        assert!(
            update
                .nodes
                .iter()
                .any(|(id, _)| *id == terminal_node_id(1)),
            "full tree parents the new pane"
        );

        assert!(
            sync_layout(&state, &panes, &origins).is_none(),
            "unchanged layout pushes nothing"
        );
    }

    #[test]
    fn sync_layout_origin_shift_rebuilds_the_tree() {
        let view0 = PaneView::new(ClientGrid::new(10, 4));
        let state = tracked_model(0, &view0);
        let mut panes = HashMap::new();
        panes.insert(0, view0);
        assert!(sync_layout(&state, &panes, &[(0, (0.0, 0.0))]).is_none());
        assert!(
            sync_layout(&state, &panes, &[(0, (12.0, 0.0))]).is_some(),
            "row bounds derive from origins"
        );
    }

    #[test]
    fn sync_layout_drops_panes_that_left_the_layout() {
        let view0 = PaneView::new(ClientGrid::new(10, 4));
        let view1 = PaneView::new(ClientGrid::new(10, 4));
        let state = tracked_model(0, &view0);
        let mut panes = HashMap::new();
        panes.insert(0, view0);
        panes.insert(1, view1);
        let both = [(0, (0.0, 0.0)), (1, (200.0, 0.0))];
        sync_layout(&state, &panes, &both).expect("registers pane 1");

        let update =
            sync_layout(&state, &panes, &[(0, (0.0, 0.0))]).expect("removal changes the tree");
        assert!(
            !update
                .nodes
                .iter()
                .any(|(id, _)| *id == terminal_node_id(1)),
            "departed pane's subtree is gone"
        );
        let view = &panes[&1];
        assert!(
            apply(&state, 1, view, A11yEvent::SelectionChanged).is_none(),
            "departed pane is untracked again"
        );
    }

    #[test]
    fn set_focus_moves_focus_once_and_dedups() {
        let view0 = PaneView::new(ClientGrid::new(10, 4));
        let view1 = PaneView::new(ClientGrid::new(10, 4));
        let state = tracked_model(0, &view0);
        let mut panes = HashMap::new();
        panes.insert(0, view0);
        panes.insert(1, view1);
        sync_layout(&state, &panes, &[(0, (0.0, 0.0)), (1, (200.0, 0.0))])
            .expect("registers pane 1");

        let update = set_focus(&state, 1).expect("focus moved");
        assert_eq!(update.focus, terminal_node_id(1));
        assert!(update.nodes.is_empty(), "focus-only update");
        assert!(set_focus(&state, 1).is_none(), "already focused");
    }

    #[test]
    fn set_focus_untracked_pane_records_intent_for_next_full_tree() {
        let view0 = PaneView::new(ClientGrid::new(10, 4));
        let view9 = PaneView::new(ClientGrid::new(10, 4));
        let state = tracked_model(0, &view0);
        assert!(
            set_focus(&state, 9).is_none(),
            "no update while the pane isn't in the tree"
        );

        let mut panes = HashMap::new();
        panes.insert(0, view0);
        panes.insert(9, view9);
        let update = sync_layout(&state, &panes, &[(0, (0.0, 0.0)), (9, (200.0, 0.0))])
            .expect("registers pane 9");
        assert_eq!(
            update.focus,
            terminal_node_id(9),
            "the recorded intent rides the next full tree"
        );
    }

    #[test]
    fn incremental_updates_clamp_focus_to_a_tracked_pane() {
        let view = PaneView::new(ClientGrid::new(10, 4));
        let state = tracked_model(0, &view);
        assert!(
            set_focus(&state, 9).is_none(),
            "intent recorded for untracked pane"
        );

        let dirty = vec![(0u16, "hi".to_string())];
        let update = apply(&state, 0, &view, A11yEvent::Render { dirty_rows: &dirty })
            .expect("dirty row pushes an update");
        assert_eq!(
            update.focus,
            terminal_node_id(0),
            "incremental focus must name a live node, not the recorded intent"
        );
    }

    #[test]
    fn sync_layout_positions_row_bounds_at_pane_origins() {
        let view0 = PaneView::new(ClientGrid::new(10, 4));
        let view1 = PaneView::new(ClientGrid::new(10, 4));
        let state = tracked_model(0, &view0);
        let mut panes = HashMap::new();
        panes.insert(0, view0);
        panes.insert(1, view1);

        let update = sync_layout(&state, &panes, &[(0, (0.0, 0.0)), (1, (200.0, 48.0))])
            .expect("new pane changes the tree");
        let bounds = |node_id| {
            update
                .nodes
                .iter()
                .find(|(id, _)| *id == node_id)
                .and_then(|(_, n)| n.bounds())
                .expect("row node has bounds")
        };
        assert!((bounds(row_node_id(0, 0)).x0 - 0.0).abs() < f64::EPSILON);
        assert!((bounds(row_node_id(1, 0)).x0 - 200.0).abs() < f64::EPSILON);
        assert!((bounds(row_node_id(1, 0)).y0 - 48.0).abs() < f64::EPSILON);
    }

    #[test]
    fn incremental_updates_carry_the_pane_origin() {
        let view0 = PaneView::new(ClientGrid::new(10, 4));
        let view1 = PaneView::new(ClientGrid::new(10, 4));
        let state = tracked_model(0, &view0);
        let mut panes = HashMap::new();
        panes.insert(0, view0);
        panes.insert(1, view1);
        sync_layout(&state, &panes, &[(0, (0.0, 0.0)), (1, (200.0, 0.0))])
            .expect("registers pane 1");

        let dirty = vec![(0u16, "hi".to_string())];
        let view = &panes[&1];
        let update = apply(&state, 1, view, A11yEvent::Render { dirty_rows: &dirty })
            .expect("dirty row pushes an update");
        let row = update
            .nodes
            .iter()
            .find(|(id, _)| *id == row_node_id(1, 0))
            .and_then(|(_, n)| n.bounds())
            .expect("dirty row carries bounds");
        assert!(
            (row.x0 - 200.0).abs() < f64::EPSILON,
            "incremental rows position at the pane origin, not (0,0)"
        );
    }

    #[test]
    fn sync_layout_dimension_change_refreshes_and_rebuilds() {
        let view0 = PaneView::new(ClientGrid::new(10, 4));
        let state = tracked_model(0, &view0);
        let mut panes = HashMap::new();
        panes.insert(0, view0);
        let origins = [(0, (0.0, 0.0))];
        assert!(sync_layout(&state, &panes, &origins).is_none());

        panes.insert(0, PaneView::new(ClientGrid::new(10, 6)));
        let update = sync_layout(&state, &panes, &origins)
            .expect("dimension change rebuilds even at the same origin");
        assert!(
            update.nodes.iter().any(|(id, _)| *id == row_node_id(0, 5)),
            "tree carries the new row count"
        );
    }

    #[test]
    fn sync_layout_dropping_focused_pane_falls_back_to_window_focus() {
        let view0 = PaneView::new(ClientGrid::new(10, 4));
        let view1 = PaneView::new(ClientGrid::new(10, 4));
        let state = tracked_model(0, &view0);
        let mut panes = HashMap::new();
        panes.insert(0, view0);
        panes.insert(1, view1);
        sync_layout(&state, &panes, &[(0, (0.0, 0.0)), (1, (200.0, 0.0))])
            .expect("registers pane 1");

        let update =
            sync_layout(&state, &panes, &[(1, (0.0, 0.0))]).expect("removal changes the tree");
        assert_eq!(update.focus, oakterm_a11y::WINDOW_ID);

        // The dangling focus must not leak into later incremental
        // updates either — they clamp to the updating pane.
        let dirty = vec![(0u16, "hi".to_string())];
        let view = &panes[&1];
        let update = apply(&state, 1, view, A11yEvent::Render { dirty_rows: &dirty })
            .expect("surviving pane still updates");
        assert_eq!(update.focus, terminal_node_id(1));
    }

    #[test]
    fn announce_from_untracked_pane_focuses_the_window() {
        let view = PaneView::new(ClientGrid::new(10, 4));
        let state = tracked_model(0, &view);
        assert!(
            set_focus(&state, 9).is_none(),
            "intent recorded for a pane that never joins the tree"
        );

        let ann = Announcement {
            text: "Bell".into(),
            level: accesskit::Live::Assertive,
        };
        let update = apply(&state, 9, &view, A11yEvent::Announce(&ann))
            .expect("announcements bypass pane tracking");
        assert_eq!(update.focus, oakterm_a11y::WINDOW_ID);
    }

    #[test]
    fn apply_untracked_pane_returns_none() {
        let view = PaneView::new(ClientGrid::new(80, 24));
        let state = tracked_model(0, &view);
        assert!(apply(&state, 9, &view, A11yEvent::SelectionChanged).is_none());
    }

    #[test]
    fn apply_unpopulated_model_returns_none() {
        let view = PaneView::new(ClientGrid::new(80, 24));
        let state: Mutex<Option<A11yModel>> = Mutex::new(None);
        assert!(apply(&state, 0, &view, A11yEvent::Resize).is_none());
    }

    #[test]
    fn apply_render_pushes_dirty_rows_and_announces() {
        let view = PaneView::new(ClientGrid::new(80, 24));
        let state = tracked_model(0, &view);
        let dirty = vec![(23u16, "hello".to_string())];
        let update = apply(&state, 0, &view, A11yEvent::Render { dirty_rows: &dirty })
            .expect("dirty rows push");
        assert!(update.nodes.iter().any(|(id, _)| *id == row_node_id(0, 23)));
        assert_eq!(announcement_value(&update).as_deref(), Some("hello"));
    }

    #[test]
    fn apply_render_nothing_changed_returns_none() {
        let view = PaneView::new(ClientGrid::new(80, 24));
        let state = tracked_model(0, &view);
        // No dirty rows and the cursor still matches the snapshot.
        assert!(apply(&state, 0, &view, A11yEvent::Render { dirty_rows: &[] }).is_none());
    }

    #[test]
    fn apply_render_announcement_debounced() {
        let view = PaneView::new(ClientGrid::new(80, 24));
        let state = tracked_model(0, &view);
        let dirty = vec![(23u16, "first".to_string())];
        let first =
            apply(&state, 0, &view, A11yEvent::Render { dirty_rows: &dirty }).expect("push");
        assert_eq!(announcement_value(&first).as_deref(), Some("first"));
        // Immediately after, the debounce window suppresses the next one.
        let dirty2 = vec![(23u16, "second".to_string())];
        let second = apply(
            &state,
            0,
            &view,
            A11yEvent::Render {
                dirty_rows: &dirty2,
            },
        )
        .expect("push");
        assert_eq!(announcement_value(&second).as_deref(), Some(""));
    }

    #[test]
    fn apply_selection_change_dedups() {
        let mut view = PaneView::new(ClientGrid::new(80, 24));
        let mut sel = Selection::new(SelectionType::Normal, 1, 2, AnchorSide::Left);
        sel.update(1, 5, AnchorSide::Right);
        view.selection = Some(sel);
        let state = tracked_model(0, &view);
        // register_pane snapshotted the selection, so the first event is a no-op...
        assert!(apply(&state, 0, &view, A11yEvent::SelectionChanged).is_none());
        // ...clearing it is a change...
        view.selection = None;
        let update = apply(&state, 0, &view, A11yEvent::SelectionChanged).expect("change");
        assert!(
            update
                .nodes
                .iter()
                .any(|(id, _)| *id == terminal_node_id(0))
        );
        // ...and repeating the cleared state is a no-op again.
        assert!(apply(&state, 0, &view, A11yEvent::SelectionChanged).is_none());
    }

    #[test]
    fn apply_title_updates_snapshot_and_pushes() {
        let view = PaneView::new(ClientGrid::new(80, 24));
        let state = tracked_model(0, &view);
        let update = apply(&state, 0, &view, A11yEvent::Title("vim")).expect("title push");
        let terminal = update
            .nodes
            .iter()
            .find(|(id, _)| *id == terminal_node_id(0))
            .expect("terminal node");
        assert_eq!(terminal.1.label(), Some("vim"));
        // The stored title survives into the next full tree.
        let full = state.lock().unwrap().as_ref().unwrap().build_full_tree();
        let terminal = full
            .nodes
            .iter()
            .find(|(id, _)| *id == terminal_node_id(0))
            .expect("terminal node");
        assert_eq!(terminal.1.label(), Some("vim"));
    }

    #[test]
    fn apply_scrollback_carries_scroll_state() {
        let mut view = PaneView::new(ClientGrid::new(80, 24));
        view.scroll_up(5);
        let state = tracked_model(0, &view);
        let update = apply(&state, 0, &view, A11yEvent::Scrollback { total_rows: 100 })
            .expect("scrollback push");
        let terminal = update
            .nodes
            .iter()
            .find(|(id, _)| *id == terminal_node_id(0))
            .expect("terminal node");
        assert_eq!(terminal.1.scroll_y(), Some(5.0));
        assert_eq!(terminal.1.scroll_y_max(), Some(100.0));
        // All visible rows are pushed.
        assert!(update.nodes.iter().any(|(id, _)| *id == row_node_id(0, 0)));
        assert!(update.nodes.iter().any(|(id, _)| *id == row_node_id(0, 23)));
    }

    #[test]
    fn apply_resize_returns_full_tree_from_refreshed_snapshot() {
        // Scroll first so a stale snapshot would carry scroll_y = 5; resize
        // (viewport reset to 0 by the caller) must rebuild from fresh state.
        let mut view = PaneView::new(ClientGrid::new(80, 24));
        view.scroll_up(5);
        let state = tracked_model(0, &view);
        apply(&state, 0, &view, A11yEvent::Scrollback { total_rows: 100 }).expect("scrolled");
        view.scroll_down(5);
        let update = apply(&state, 0, &view, A11yEvent::Resize).expect("full rebuild");
        assert!(update.tree.is_some(), "resize must rebuild the full tree");
        let terminal = update
            .nodes
            .iter()
            .find(|(id, _)| *id == terminal_node_id(0))
            .expect("terminal node");
        assert_eq!(terminal.1.scroll_y(), Some(0.0));
    }

    #[test]
    fn apply_announce_and_clear() {
        let view = PaneView::new(ClientGrid::new(80, 24));
        let state = tracked_model(0, &view);
        let ann = Announcement {
            text: "Bell".into(),
            level: accesskit::Live::Assertive,
        };
        let bell = apply(&state, 0, &view, A11yEvent::Announce(&ann)).expect("announce");
        assert_eq!(announcement_value(&bell).as_deref(), Some("Bell"));
        let clear = apply(&state, 0, &view, A11yEvent::ClearAnnouncement).expect("clear");
        assert_eq!(announcement_value(&clear).as_deref(), Some(""));
    }

    #[test]
    fn apply_render_empty_candidate_does_not_stamp_debounce() {
        // An empty-text candidate must not consume the debounce window: the
        // next real announcement still fires.
        let view = PaneView::new(ClientGrid::new(80, 24));
        let state = tracked_model(0, &view);
        let empty = vec![(23u16, String::new())];
        let first =
            apply(&state, 0, &view, A11yEvent::Render { dirty_rows: &empty }).expect("push");
        assert_eq!(announcement_value(&first).as_deref(), Some(""));
        let dirty = vec![(23u16, "hello".to_string())];
        let second =
            apply(&state, 0, &view, A11yEvent::Render { dirty_rows: &dirty }).expect("push");
        assert_eq!(announcement_value(&second).as_deref(), Some("hello"));
    }

    #[test]
    fn apply_render_announcement_window_expires_and_suppression_does_not_stamp() {
        use std::time::Duration;
        let view = PaneView::new(ClientGrid::new(80, 24));
        let state = tracked_model(0, &view);
        let dirty = vec![(23u16, "first".to_string())];
        apply(&state, 0, &view, A11yEvent::Render { dirty_rows: &dirty }).expect("push");

        // A suppressed candidate inside the window must not re-stamp it.
        let backdated = Instant::now()
            .checked_sub(Duration::from_millis(60))
            .expect("clock predates test");
        state.lock().unwrap().as_mut().unwrap().last_announcement = Some(backdated);
        let dirty2 = vec![(23u16, "second".to_string())];
        let suppressed = apply(
            &state,
            0,
            &view,
            A11yEvent::Render {
                dirty_rows: &dirty2,
            },
        )
        .expect("push");
        assert_eq!(announcement_value(&suppressed).as_deref(), Some(""));
        assert_eq!(
            state.lock().unwrap().as_ref().unwrap().last_announcement,
            Some(backdated),
            "suppressed candidate must not consume the window"
        );

        // Once the window expires, the next candidate announces.
        state.lock().unwrap().as_mut().unwrap().last_announcement =
            Instant::now().checked_sub(Duration::from_millis(150));
        let dirty3 = vec![(23u16, "third".to_string())];
        let third = apply(
            &state,
            0,
            &view,
            A11yEvent::Render {
                dirty_rows: &dirty3,
            },
        )
        .expect("push");
        assert_eq!(announcement_value(&third).as_deref(), Some("third"));
    }

    #[test]
    fn apply_render_cursor_move_pushes_terminal_then_settles() {
        let mut view = PaneView::new(ClientGrid::new(80, 24));
        let state = tracked_model(0, &view);
        view.grid_mut().cursor_x = 5;
        // Cursor moved with no dirty rows: the terminal node (which carries
        // the cursor as a text selection) must be rebuilt...
        let update =
            apply(&state, 0, &view, A11yEvent::Render { dirty_rows: &[] }).expect("cursor push");
        assert!(
            update
                .nodes
                .iter()
                .any(|(id, _)| *id == terminal_node_id(0))
        );
        // ...and the comparison uses the refreshed snapshot: repeating the
        // same state is a no-op.
        assert!(apply(&state, 0, &view, A11yEvent::Render { dirty_rows: &[] }).is_none());
    }

    #[test]
    fn apply_render_none_still_refreshes_snapshot() {
        // A no-push frame still refreshes the snapshot, so an AT connecting
        // later sees the current scroll position, not a stale one.
        let mut view = PaneView::new(ClientGrid::new(80, 24));
        view.scroll_up(5);
        let state = tracked_model(0, &view);
        apply(&state, 0, &view, A11yEvent::Scrollback { total_rows: 100 }).expect("scrolled");
        view.scroll_down(5);
        assert!(apply(&state, 0, &view, A11yEvent::Render { dirty_rows: &[] }).is_none());
        let full = state.lock().unwrap().as_ref().unwrap().build_full_tree();
        let terminal = full
            .nodes
            .iter()
            .find(|(id, _)| *id == terminal_node_id(0))
            .expect("terminal node");
        assert_eq!(terminal.1.scroll_y(), Some(0.0));
    }

    #[test]
    fn apply_announce_bypasses_pane_tracking() {
        // The announcement node is window-level; a bell must reach AT even
        // for a pane the model does not track.
        let view = PaneView::new(ClientGrid::new(80, 24));
        let state = tracked_model(0, &view);
        let ann = Announcement {
            text: "Bell".into(),
            level: accesskit::Live::Assertive,
        };
        let bell = apply(&state, 9, &view, A11yEvent::Announce(&ann)).expect("announce");
        assert_eq!(announcement_value(&bell).as_deref(), Some("Bell"));
    }

    #[test]
    fn selection_range_maps_cells_to_positions() {
        // Cells (1,2)..=(1,5) selected: anchor position 2, focus position 6.
        let mut sel = Selection::new(SelectionType::Normal, 1, 2, AnchorSide::Left);
        sel.update(1, 5, AnchorSide::Right);
        let range = selection_range(&sel, 0, 24).expect("visible");
        assert_eq!(
            range,
            SelectionRange {
                anchor_row: 1,
                anchor_col: 2,
                focus_row: 1,
                focus_col: 6,
            }
        );
    }

    #[test]
    fn selection_range_applies_viewport_offset() {
        // Rows -3..-2 with offset 5 → visible rows 2..3.
        let mut sel = Selection::new(SelectionType::Normal, -3, 0, AnchorSide::Left);
        sel.update(-2, 4, AnchorSide::Right);
        let range = selection_range(&sel, 5, 24).expect("visible");
        assert_eq!(range.anchor_row, 2);
        assert_eq!(range.focus_row, 3);
    }

    #[test]
    fn selection_range_reversed_drag_normalizes() {
        // Right-to-left drag: anchor at (2,9), dragged back to (1,3). The
        // normalized mapping must match the forward equivalent.
        let mut sel = Selection::new(SelectionType::Normal, 2, 9, AnchorSide::Right);
        sel.update(1, 3, AnchorSide::Left);
        let range = selection_range(&sel, 0, 24).expect("visible");
        assert_eq!(range.anchor_row, 1);
        assert_eq!(range.anchor_col, 3);
        assert_eq!(range.focus_row, 2);
        assert_eq!(range.focus_col, 10);
    }

    #[test]
    fn selection_range_clamps_partial_overlap() {
        // Start above the viewport clamps to row 0.
        let mut sel = Selection::new(SelectionType::Normal, -4, 3, AnchorSide::Left);
        sel.update(2, 4, AnchorSide::Right);
        let range = selection_range(&sel, 1, 24).expect("partially visible");
        assert_eq!(range.anchor_row, 0);
        assert_eq!(range.focus_row, 3);
    }

    #[test]
    fn selection_range_none_when_entirely_above_viewport() {
        let mut sel = Selection::new(SelectionType::Normal, -10, 0, AnchorSide::Left);
        sel.update(-8, 4, AnchorSide::Right);
        assert_eq!(selection_range(&sel, 2, 24), None);
    }

    #[test]
    fn selection_range_none_when_entirely_below_viewport() {
        // Live-view selection at rows 20..22, then the user scrolls up 30
        // lines: visible rows 50..52 are past the 24-row viewport.
        let mut sel = Selection::new(SelectionType::Normal, 20, 0, AnchorSide::Left);
        sel.update(22, 4, AnchorSide::Right);
        assert_eq!(selection_range(&sel, 30, 24), None);
    }

    #[test]
    fn selection_range_clamps_below_viewport_end() {
        // End below the viewport clamps to the last visible row.
        let mut sel = Selection::new(SelectionType::Normal, 1, 0, AnchorSide::Left);
        sel.update(30, 4, AnchorSide::Right);
        let range = selection_range(&sel, 0, 24).expect("partially visible");
        assert_eq!(range.anchor_row, 1);
        assert_eq!(range.focus_row, 23);
    }

    #[test]
    fn selection_range_line_spans_full_rows() {
        let sel = Selection::new(SelectionType::Line, 3, 0, AnchorSide::Left);
        let range = selection_range(&sel, 0, 24).expect("visible");
        assert_eq!(range.anchor_col, 0);
        assert_eq!(range.focus_col, usize::MAX);
    }

    #[test]
    fn selection_range_block_maps_to_stream_range() {
        // A down-left block drag maps to the stream range between its
        // corners (TextSelection cannot express a rectangle); pin the
        // approximation so a change to it is deliberate.
        let mut sel = Selection::new(SelectionType::Block, 1, 10, AnchorSide::Left);
        sel.update(3, 2, AnchorSide::Right);
        let range = selection_range(&sel, 0, 24).expect("visible");
        assert_eq!(
            range,
            SelectionRange {
                anchor_row: 1,
                anchor_col: 10,
                focus_row: 3,
                focus_col: 3,
            }
        );
    }

    #[test]
    fn apply_render_clamps_line_selection_to_row_text() {
        // The Line-selection end-of-row sentinel must resolve against the
        // trimmed row text (empty here), not the column count.
        let mut view = PaneView::new(ClientGrid::new(80, 24));
        let state = tracked_model(0, &view);
        view.selection = Some(Selection::new(SelectionType::Line, 1, 0, AnchorSide::Left));
        let update = apply(&state, 0, &view, A11yEvent::SelectionChanged).expect("line selection");
        let terminal = update
            .nodes
            .iter()
            .find(|(id, _)| *id == terminal_node_id(0))
            .expect("terminal node");
        let sel = terminal.1.text_selection().expect("selection");
        assert_eq!(sel.focus.character_index, 0);
    }

    #[test]
    fn selection_from_a11y_round_trips() {
        let at_sel = accesskit::TextSelection {
            anchor: accesskit::TextPosition {
                node: row_node_id(3, 1),
                character_index: 2,
            },
            focus: accesskit::TextPosition {
                node: row_node_id(3, 4),
                character_index: 7,
            },
        };
        let (pane, sel) = selection_from_a11y(&at_sel, 0, 24).expect("decodes");
        let sel = sel.expect("non-collapsed");
        assert_eq!(pane, 3);
        assert_eq!(sel.start.row, 1);
        assert_eq!(sel.start.col, 2);
        assert_eq!(sel.end.row, 4);
        assert_eq!(sel.end.col, 7);
        assert_eq!(sel.end.side, AnchorSide::Left);
        // The exclusive focus position 7 excludes cell 7 via the Left side.
        assert!(sel.contains(4, 6));
        assert!(!sel.contains(4, 7));
    }

    #[test]
    fn selection_from_a11y_reversed_endpoints_normalize() {
        // A screen reader may send anchor after focus (backwards selection);
        // the mapping must match the forward-order equivalent.
        let at_sel = accesskit::TextSelection {
            anchor: accesskit::TextPosition {
                node: row_node_id(0, 4),
                character_index: 7,
            },
            focus: accesskit::TextPosition {
                node: row_node_id(0, 1),
                character_index: 2,
            },
        };
        let (_, sel) = selection_from_a11y(&at_sel, 0, 24).expect("decodes");
        let sel = sel.expect("non-collapsed");
        assert_eq!(sel.start.row, 1);
        assert_eq!(sel.start.col, 2);
        assert_eq!(sel.end.row, 4);
        assert_eq!(sel.end.col, 7);
        assert!(sel.contains(4, 6));
        assert!(!sel.contains(4, 7));
    }

    #[test]
    fn selection_from_a11y_focus_at_col_zero_ends_previous_row() {
        // An exclusive focus at column 0 selects nothing on that row; the
        // mapped selection must end at the previous row's end instead of
        // gaining cell 0 (contains_normal includes it for end.col == 0).
        let at_sel = accesskit::TextSelection {
            anchor: accesskit::TextPosition {
                node: row_node_id(0, 0),
                character_index: 5,
            },
            focus: accesskit::TextPosition {
                node: row_node_id(0, 1),
                character_index: 0,
            },
        };
        let (_, sel) = selection_from_a11y(&at_sel, 0, 24).expect("decodes");
        let sel = sel.expect("non-collapsed");
        assert!(sel.contains(0, 5), "row 0 selected from the anchor");
        assert!(sel.contains(0, 79), "row 0 selected to its end");
        assert!(!sel.contains(1, 0), "nothing selected on the focus row");
    }

    #[test]
    fn selection_from_a11y_collapsed_clears() {
        let pos = accesskit::TextPosition {
            node: row_node_id(0, 2),
            character_index: 5,
        };
        let at_sel = accesskit::TextSelection {
            anchor: pos,
            focus: pos,
        };
        assert_eq!(selection_from_a11y(&at_sel, 0, 24), Some((0, None)));
    }

    #[test]
    fn selection_from_a11y_rejects_rows_outside_viewport() {
        // A row node beyond the pane's visible rows selects nothing on
        // screen; reject rather than let the AT request "succeed".
        let at_sel = accesskit::TextSelection {
            anchor: accesskit::TextPosition {
                node: row_node_id(0, 1),
                character_index: 0,
            },
            focus: accesskit::TextPosition {
                node: row_node_id(0, 30),
                character_index: 2,
            },
        };
        assert_eq!(selection_from_a11y(&at_sel, 0, 24), None);
    }

    #[test]
    fn selection_from_a11y_rejects_cross_pane() {
        let at_sel = accesskit::TextSelection {
            anchor: accesskit::TextPosition {
                node: row_node_id(0, 1),
                character_index: 0,
            },
            focus: accesskit::TextPosition {
                node: row_node_id(1, 1),
                character_index: 0,
            },
        };
        assert_eq!(selection_from_a11y(&at_sel, 0, 24), None);
    }

    #[test]
    fn selection_from_a11y_rejects_non_row_nodes() {
        let at_sel = accesskit::TextSelection {
            anchor: accesskit::TextPosition {
                node: terminal_node_id(0),
                character_index: 0,
            },
            focus: accesskit::TextPosition {
                node: row_node_id(0, 1),
                character_index: 0,
            },
        };
        assert_eq!(selection_from_a11y(&at_sel, 0, 24), None);
    }

    #[test]
    fn selection_from_a11y_maps_offset_into_scrollback() {
        let at_sel = accesskit::TextSelection {
            anchor: accesskit::TextPosition {
                node: row_node_id(0, 0),
                character_index: 0,
            },
            focus: accesskit::TextPosition {
                node: row_node_id(0, 2),
                character_index: 3,
            },
        };
        let (_, sel) = selection_from_a11y(&at_sel, 5, 24).expect("decodes");
        let sel = sel.expect("non-collapsed");
        assert_eq!(sel.start.row, -5);
        assert_eq!(sel.end.row, -3);
    }
}
