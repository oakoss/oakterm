//! Per-pane render-side state: the client grid plus the view state that
//! multiplies with it when split rendering (TREK-99) lands — scrollback
//! viewport, selection, and mode flags all follow a pane, not the window.

use crate::render_grid::ClientGrid;
use oakterm_protocol::render::{DirtyRow, RenderUpdate};
use oakterm_terminal::grid::selection::Selection;
use std::num::NonZeroU32;

/// Owns the pane's scroll state: the viewport offset and the grid's
/// live-snapshot are only ever changed together, through the methods
/// below. Nothing else may call `ClientGrid::enter_scrollback` /
/// `exit_scrollback` or move the offset — that split ownership is the
/// TREK-139 scrollback-corruption class.
pub(crate) struct PaneView {
    grid: ClientGrid,
    /// Lines scrolled up from bottom. 0 = live view (at bottom).
    viewport_offset: u32,
    pub(crate) selection: Option<Selection>,
    /// Whether the terminal has DECSET 2004 (bracketed paste) active.
    pub(crate) bracketed_paste: bool,
    /// Last `(cols, rows)` sent to the daemon; suppresses redundant resizes.
    pub(crate) last_sent_dims: (u16, u16),
}

impl PaneView {
    pub(crate) fn new(grid: ClientGrid) -> Self {
        Self {
            grid,
            viewport_offset: 0,
            selection: None,
            bracketed_paste: false,
            last_sent_dims: (0, 0),
        }
    }

    #[must_use]
    pub(crate) fn grid(&self) -> &ClientGrid {
        &self.grid
    }

    #[cfg(test)]
    pub(crate) fn grid_mut(&mut self) -> &mut ClientGrid {
        &mut self.grid
    }

    /// Lines scrolled up from bottom. 0 = live view (at bottom).
    #[must_use]
    pub(crate) fn viewport_offset(&self) -> u32 {
        self.viewport_offset
    }

    /// Whether the grid has snapshotted the live view — either the viewport
    /// shows scrollback, or the live view is frozen by `freeze_live`.
    #[must_use]
    pub(crate) fn is_scrolled(&self) -> bool {
        self.grid.is_scrolled()
    }

    /// Scroll up into host scrollback, entering scrollback mode on the
    /// first scroll so the grid snapshots the live view.
    pub(crate) fn scroll_up(&mut self, lines: u32) {
        self.grid.enter_scrollback();
        self.viewport_offset = self.viewport_offset.saturating_add(lines);
    }

    /// Scroll toward live view, exiting scrollback when the viewport
    /// reaches live. Returns true on reaching live (offset 0), signalling
    /// the caller to request a full refresh.
    pub(crate) fn scroll_down(&mut self, lines: u32) -> bool {
        self.viewport_offset = self.viewport_offset.saturating_sub(lines);
        if self.viewport_offset == 0 {
            self.grid.exit_scrollback();
            true
        } else {
            false
        }
    }

    /// Return to live view: offset 0, snapshot restored.
    pub(crate) fn return_to_live(&mut self) {
        self.viewport_offset = 0;
        self.grid.exit_scrollback();
    }

    /// Jump straight to a scrollback offset, entering scrollback on the
    /// first jump. Offset 0 returns to live.
    pub(crate) fn set_scroll_offset(&mut self, offset: u32) {
        if offset == 0 {
            self.return_to_live();
        } else {
            self.grid.enter_scrollback();
            self.viewport_offset = offset;
        }
    }

    /// Snapshot the live view without moving the viewport, so the view a
    /// prompt search was issued against stays frozen until the daemon
    /// answers with a target offset (or `return_to_live` cancels).
    pub(crate) fn freeze_live(&mut self) {
        self.grid.enter_scrollback();
    }

    /// Clamp the offset to the daemon's reported scrollback length,
    /// returning to live when the clamp lands at zero.
    pub(crate) fn clamp_scrollback(&mut self, total: u32) -> ScrollbackClampOutcome {
        let outcome = clamp_viewport(self.viewport_offset, total);
        match outcome {
            ScrollbackClampOutcome::Clamp(clamped) => self.viewport_offset = clamped.get(),
            ScrollbackClampOutcome::ReturnToLive => self.return_to_live(),
        }
        outcome
    }

    /// Route a daemon render update into the visible cells (live) or the
    /// saved snapshot (scrolled), so a scrollback page is never
    /// overwritten by live output.
    pub(crate) fn apply_update(&mut self, update: &RenderUpdate) {
        if self.grid.is_scrolled() {
            self.grid.apply_update_while_scrolled(update);
        } else {
            self.grid.apply_update(update);
        }
    }

    /// Compose the viewport from daemon scrollback rows at the current
    /// offset, optionally painting the scroll position indicator.
    pub(crate) fn apply_scrollback(&mut self, rows: &[DirtyRow], show_indicator: bool) {
        #[allow(clippy::cast_possible_truncation)]
        let offset = self.viewport_offset.min(u32::from(u16::MAX)) as u16;
        self.grid.apply_scrollback(rows, offset);
        if show_indicator {
            self.grid.set_scroll_indicator(self.viewport_offset);
        }
    }

    /// Resize the grid and return to live view; a scrollback page has the
    /// wrong dimensions to keep showing.
    pub(crate) fn resize(&mut self, cols: u16, rows: u16) {
        self.return_to_live();
        self.grid.resize(cols, rows);
    }
}

/// Outcome of clamping the host scrollback viewport offset against the
/// daemon's reported buffer length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScrollbackClampOutcome {
    /// Clamped to a non-zero offset; stay in scrollback.
    Clamp(NonZeroU32),
    /// Buffer is empty (or smaller than 1); leave scrollback mode entirely.
    ReturnToLive,
}

/// Clamp `current` viewport offset to the daemon's actual scrollback length.
/// Returns `ReturnToLive` when the clamp lands at zero, signalling the caller
/// should return to live instead of painting a "[0 lines]" indicator.
fn clamp_viewport(current: u32, total: u32) -> ScrollbackClampOutcome {
    NonZeroU32::new(current.min(total)).map_or(
        ScrollbackClampOutcome::ReturnToLive,
        ScrollbackClampOutcome::Clamp,
    )
}

#[cfg(test)]
mod tests {
    use super::{PaneView, ScrollbackClampOutcome, clamp_viewport};
    use crate::render_grid::ClientGrid;
    use oakterm_protocol::render::{DirtyRow, RenderUpdate, WireCell};
    use std::num::NonZeroU32;

    fn nz(n: u32) -> NonZeroU32 {
        NonZeroU32::new(n).expect("test offset is non-zero")
    }

    fn make_wire_cell(ch: u8) -> WireCell {
        WireCell {
            codepoint: u32::from(ch),
            fg_r: 255,
            fg_g: 255,
            fg_b: 255,
            fg_type: 0,
            bg_r: 0,
            bg_g: 0,
            bg_b: 0,
            bg_type: 0,
            flags: 0,
            extra: vec![],
        }
    }

    fn make_dirty_row(index: u16, text: &[u8]) -> DirtyRow {
        DirtyRow {
            row_index: index,
            cells: text.iter().map(|&ch| make_wire_cell(ch)).collect(),
            semantic_mark: 0,
            mark_metadata: vec![],
        }
    }

    fn make_update(seqno: u64, row: u16, text: &[u8]) -> RenderUpdate {
        RenderUpdate {
            pane_id: 0,
            seqno,
            cursor_x: 0,
            cursor_y: 0,
            cursor_style: 0,
            cursor_visible: true,
            bg_r: 0,
            bg_g: 0,
            bg_b: 0,
            bracketed_paste: false,
            alt_screen: false,
            dirty_rows: vec![make_dirty_row(row, text)],
        }
    }

    /// Offset and snapshot must agree except in the deliberate
    /// `freeze_live` handshake (scrolled at offset 0).
    fn assert_lockstep(view: &PaneView) {
        if view.viewport_offset() > 0 {
            assert!(
                view.is_scrolled(),
                "offset {} but grid not scrolled",
                view.viewport_offset()
            );
        }
    }

    #[test]
    fn scroll_up_enters_scrollback_and_accumulates() {
        let mut view = PaneView::new(ClientGrid::new(80, 24));
        view.scroll_up(5);
        assert!(view.is_scrolled());
        assert_eq!(view.viewport_offset(), 5);
        view.scroll_up(3);
        assert_eq!(view.viewport_offset(), 8);
        assert_lockstep(&view);
    }

    #[test]
    fn scroll_down_reports_reaching_live_and_exits_scrollback() {
        let mut view = PaneView::new(ClientGrid::new(80, 24));
        view.scroll_up(5);
        assert!(!view.scroll_down(3), "still scrolled at offset 2");
        assert!(view.is_scrolled());
        assert!(view.scroll_down(10), "saturates to live at offset 0");
        assert_eq!(view.viewport_offset(), 0);
        assert!(!view.is_scrolled(), "reaching live must exit scrollback");
    }

    #[test]
    fn update_routing_follows_scroll_state() {
        let mut view = PaneView::new(ClientGrid::new(4, 2));
        view.apply_update(&make_update(1, 0, b"live"));
        assert_eq!(view.grid().row_text(0), "live");

        // Scrolled: updates land in the snapshot, visible cells freeze.
        view.scroll_up(5);
        view.apply_update(&make_update(2, 0, b"new!"));
        assert_eq!(view.grid().row_text(0), "live", "visible cells frozen");
        assert_eq!(view.grid().seqno, 2, "snapshot still tracks the daemon");

        // Back to live: the snapshot (with the buffered update) is visible
        // and updates land in the visible cells again.
        view.return_to_live();
        assert_eq!(view.grid().row_text(0), "new!");
        view.apply_update(&make_update(3, 0, b"more"));
        assert_eq!(view.grid().row_text(0), "more");
        assert_lockstep(&view);
    }

    #[test]
    fn return_to_live_resets_offset_and_snapshot_together() {
        let mut view = PaneView::new(ClientGrid::new(4, 2));
        view.scroll_up(7);
        view.return_to_live();
        assert_eq!(view.viewport_offset(), 0);
        assert!(!view.is_scrolled());
    }

    #[test]
    fn set_scroll_offset_enters_scrollback_and_zero_returns_to_live() {
        let mut view = PaneView::new(ClientGrid::new(4, 2));
        view.set_scroll_offset(12);
        assert_eq!(view.viewport_offset(), 12);
        assert!(view.is_scrolled());
        assert_lockstep(&view);

        view.set_scroll_offset(0);
        assert_eq!(view.viewport_offset(), 0);
        assert!(!view.is_scrolled());
    }

    #[test]
    fn set_scroll_offset_keeps_existing_snapshot() {
        let mut view = PaneView::new(ClientGrid::new(4, 2));
        view.apply_update(&make_update(1, 0, b"orig"));
        view.freeze_live();
        view.apply_update(&make_update(2, 0, b"new!"));
        // Jumping to an offset must not re-snapshot the frozen cells.
        view.set_scroll_offset(3);
        view.return_to_live();
        assert_eq!(view.grid().row_text(0), "new!");
    }

    #[test]
    fn freeze_live_holds_view_at_offset_zero() {
        let mut view = PaneView::new(ClientGrid::new(4, 2));
        view.apply_update(&make_update(1, 0, b"live"));
        view.freeze_live();
        assert_eq!(view.viewport_offset(), 0);
        assert!(view.is_scrolled());
        view.apply_update(&make_update(2, 0, b"new!"));
        assert_eq!(view.grid().row_text(0), "live", "frozen view holds");
        view.return_to_live();
        assert_eq!(view.grid().row_text(0), "new!");
    }

    #[test]
    fn freeze_live_while_scrolled_preserves_offset_and_snapshot() {
        let mut view = PaneView::new(ClientGrid::new(4, 2));
        view.apply_update(&make_update(1, 0, b"aaaa"));
        view.scroll_up(50);
        view.apply_scrollback(&[make_dirty_row(0, b"page")], false);
        assert_eq!(view.viewport_offset(), 50);

        // PromptSearch(Older) can fire while already scrolled: freeze_live
        // must not move the offset or re-snapshot the visible scrollback page.
        view.freeze_live();
        assert_eq!(view.viewport_offset(), 50);
        assert!(view.is_scrolled());
        view.apply_update(&make_update(2, 0, b"live"));
        assert_eq!(view.grid().row_text(0), "page", "frozen page preserved");
        assert_lockstep(&view);
    }

    #[test]
    fn clamp_scrollback_applies_clamp_to_offset() {
        let mut view = PaneView::new(ClientGrid::new(4, 2));
        view.scroll_up(2412);
        assert_eq!(
            view.clamp_scrollback(50),
            ScrollbackClampOutcome::Clamp(nz(50))
        );
        assert_eq!(view.viewport_offset(), 50);
        assert!(view.is_scrolled());
        assert_lockstep(&view);
    }

    #[test]
    fn clamp_scrollback_empty_buffer_returns_to_live() {
        let mut view = PaneView::new(ClientGrid::new(4, 2));
        view.scroll_up(10);
        assert_eq!(
            view.clamp_scrollback(0),
            ScrollbackClampOutcome::ReturnToLive
        );
        assert_eq!(view.viewport_offset(), 0);
        assert!(!view.is_scrolled());
    }

    #[test]
    fn apply_scrollback_composes_at_own_offset() {
        let mut view = PaneView::new(ClientGrid::new(3, 3));
        view.apply_update(&make_update(1, 0, b"abc"));
        view.scroll_up(1);
        view.apply_scrollback(&[make_dirty_row(0, b"sbk")], false);
        // Top row from scrollback, live snapshot fills below.
        assert_eq!(view.grid().row_text(0), "sbk");
        assert_eq!(view.grid().row_text(1), "abc");
        view.return_to_live();
        assert_eq!(view.grid().row_text(0), "abc");
    }

    #[test]
    fn resize_returns_to_live() {
        let mut view = PaneView::new(ClientGrid::new(4, 2));
        view.scroll_up(9);
        view.resize(10, 5);
        assert_eq!(view.viewport_offset(), 0);
        assert!(!view.is_scrolled());
        assert_eq!((view.grid().cols, view.grid().rows), (10, 5));
    }

    #[test]
    fn clamp_below_total_keeps_offset() {
        assert_eq!(
            clamp_viewport(10, 100),
            ScrollbackClampOutcome::Clamp(nz(10))
        );
    }

    #[test]
    fn clamp_above_total_clamps_to_total() {
        assert_eq!(
            clamp_viewport(2412, 50),
            ScrollbackClampOutcome::Clamp(nz(50))
        );
    }

    #[test]
    fn clamp_total_zero_returns_to_live() {
        assert_eq!(clamp_viewport(10, 0), ScrollbackClampOutcome::ReturnToLive);
    }

    #[test]
    fn clamp_current_zero_returns_to_live() {
        assert_eq!(clamp_viewport(0, 100), ScrollbackClampOutcome::ReturnToLive);
    }
}
