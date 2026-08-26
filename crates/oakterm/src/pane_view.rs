//! Per-pane render-side state: the client grid plus the view state that
//! multiplies with it when split rendering (TREK-99) lands — scrollback
//! viewport, selection, and mode flags all follow a pane, not the window.

use crate::copy_keys::{Motion, PendingPrefix};
use crate::copy_mode::{
    CopyModeState, CopySelectionType, FillFailure, FillRequest, SelectionEffect, YankPlan,
};
use crate::copy_motion::resolve;
use crate::render_grid::ClientGrid;
use oakterm_protocol::message::ScrollbackData;
use oakterm_protocol::render::{DirtyRow, RenderUpdate};
use oakterm_terminal::grid::selection::Selection;
use std::num::NonZeroU32;

/// Owns the pane's scroll state: the viewport offset and the grid's
/// live-snapshot are only ever changed together, through the methods
/// below. Nothing else may call `ClientGrid::enter_scrollback` /
/// `exit_scrollback` or move the offset — that split ownership is the
/// TREK-139 scrollback-corruption class. Copy-mode row indices join that
/// invariant: they are resolved against a daemon pin taken when the grid
/// froze, so they mean nothing once it thaws.
pub(crate) struct PaneView {
    grid: ClientGrid,
    /// Lines scrolled up from bottom. 0 = live view (at bottom).
    viewport_offset: u32,
    /// Copy-mode state while this pane is in it. Per-pane rather than
    /// per-window so it survives focus moving to another pane and back.
    copy_mode: Option<CopyModeState>,
    /// Offset the visible cells were last composed at. `viewport_offset`
    /// moves when a scroll is REQUESTED; this moves when its page
    /// arrives, so between the two only this one names rows on screen.
    painted_offset: u32,
    /// Serial of the newest host-scrollback request. Older replies carry
    /// rows for an offset the viewport has since left, and painting one
    /// puts the wrong page on screen until the next request answers.
    scrollback_serial: Option<u32>,
    pub(crate) selection: Option<Selection>,
    /// `history_len` of the last applied `RenderUpdate` — the base bound
    /// to the live-painted content (ADR-0025). `None` until the first
    /// update arrives.
    render_base: Option<u64>,
    /// Serve-time base of the scrollback page currently painted, when
    /// the viewport shows one (ADR-0025).
    scroll_base: Option<u64>,
    /// Whether the terminal has DECSET 2004 (bracketed paste) active.
    pub(crate) bracketed_paste: bool,
    /// Last `(cols, rows)` sent to the daemon; suppresses redundant resizes.
    pub(crate) last_sent_dims: (u16, u16),
    /// Last OSC title pushed by the daemon; the status bar displays the
    /// focused pane's.
    pub(crate) title: String,
}

impl PaneView {
    pub(crate) fn new(grid: ClientGrid) -> Self {
        Self {
            grid,
            viewport_offset: 0,
            copy_mode: None,
            painted_offset: 0,
            scrollback_serial: None,
            selection: None,
            render_base: None,
            scroll_base: None,
            bracketed_paste: false,
            last_sent_dims: (0, 0),
            title: String::new(),
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

    /// Discard copy-mode state because the viewport moved or thawed,
    /// reporting whether there was any. Copy-mode rows are indexed off a
    /// pin taken against the view as it stood, so any move invalidates
    /// them — every viewport primitive routes through here so no present
    /// or future caller can move the view and leave them behind.
    fn invalidate_copy_mode(&mut self) -> bool {
        self.copy_mode.take().is_some()
    }

    /// Put the live snapshot back on screen. That page is painted by
    /// definition, so the painted offset returns to 0 with it.
    fn restore_live_page(&mut self) {
        self.grid.exit_scrollback();
        self.painted_offset = 0;
        self.scroll_base = None;
    }

    /// The base bound to the painted page (ADR-0025 clause 3): the last
    /// `RenderUpdate`'s at offset 0, the serving `ScrollbackData`'s when
    /// fully scrolled. `None` before anything painted, or when a partial
    /// page spans two instants — the caller refetches the scrollback
    /// page and retries entry once it is single-instant.
    pub(crate) fn copy_mode_entry_base(&self) -> Option<u64> {
        if self.painted_offset == 0 {
            return self.render_base;
        }
        if self.painted_offset >= u32::from(self.grid.rows) {
            return self.scroll_base;
        }
        match (self.render_base, self.scroll_base) {
            (Some(render), Some(scroll)) if render == scroll => Some(render),
            _ => None,
        }
    }

    /// Move the viewport to live, leaving copy mode alone. Only for
    /// callers that have already settled it.
    fn snap_to_live(&mut self) {
        self.viewport_offset = 0;
        self.restore_live_page();
    }

    /// Snapshot the live view, leaving copy mode alone.
    fn freeze(&mut self) {
        self.grid.enter_scrollback();
    }

    /// Scroll up into host scrollback, entering scrollback mode on the
    /// first scroll so the grid snapshots the live view.
    #[must_use = "a discarded copy mode leaves a daemon pin to release"]
    pub(crate) fn scroll_up(&mut self, lines: u32) -> bool {
        self.freeze();
        self.viewport_offset = self.viewport_offset.saturating_add(lines);
        self.invalidate_copy_mode()
    }

    /// Scroll toward live view, exiting scrollback when the viewport
    /// reaches live.
    pub(crate) fn scroll_down(&mut self, lines: u32) -> ScrollOutcome {
        self.viewport_offset = self.viewport_offset.saturating_sub(lines);
        let reached_live = self.viewport_offset == 0;
        if reached_live {
            self.restore_live_page();
        }
        ScrollOutcome {
            reached_live,
            copy_mode_exited: self.invalidate_copy_mode(),
        }
    }

    /// Return to live view: offset 0, snapshot restored.
    #[must_use = "a discarded copy mode leaves a daemon pin to release"]
    pub(crate) fn return_to_live(&mut self) -> bool {
        self.snap_to_live();
        self.invalidate_copy_mode()
    }

    /// Jump straight to a scrollback offset, entering scrollback on the
    /// first jump. Offset 0 returns to live.
    #[must_use = "a discarded copy mode leaves a daemon pin to release"]
    pub(crate) fn set_scroll_offset(&mut self, offset: u32) -> bool {
        if offset == 0 {
            self.snap_to_live();
        } else {
            self.freeze();
            self.viewport_offset = offset;
        }
        self.invalidate_copy_mode()
    }

    /// Snapshot the live view without moving the viewport, so the view a
    /// prompt search was issued against stays frozen until the daemon
    /// answers with a target offset (or `return_to_live` cancels).
    #[must_use = "a discarded copy mode leaves a daemon pin to release"]
    pub(crate) fn freeze_live(&mut self) -> bool {
        self.freeze();
        self.invalidate_copy_mode()
    }

    /// Clamp the offset to the daemon's reported scrollback length,
    /// returning to live when the clamp lands at zero.
    ///
    /// Copy mode is untouched because it cannot be active here: every
    /// scrollback reply for a pane in copy mode is claimed by serial or
    /// dropped before the clamp.
    pub(crate) fn clamp_scrollback(&mut self, total: u32) -> ScrollbackClampOutcome {
        let outcome = clamp_viewport(self.viewport_offset, total);
        match outcome {
            ScrollbackClampOutcome::Clamp(clamped) => self.viewport_offset = clamped.get(),
            ScrollbackClampOutcome::ReturnToLive => self.snap_to_live(),
        }
        outcome
    }

    /// Route a daemon render update into the visible cells (live) or the
    /// saved snapshot (scrolled), so a scrollback page is never
    /// overwritten by live output.
    pub(crate) fn apply_update(&mut self, update: &RenderUpdate) {
        self.render_base = Some(update.history_len);
        if self.grid.is_scrolled() {
            self.grid.apply_update_while_scrolled(update);
        } else {
            self.grid.apply_update(update);
        }
    }

    /// Compose the viewport from daemon scrollback rows at the current
    /// offset, optionally painting the scroll position indicator.
    /// `base` is the serve-time anchor the rows were resolved against
    /// (ADR-0025), remembered as the painted page's.
    pub(crate) fn apply_scrollback(&mut self, rows: &[DirtyRow], base: u64, show_indicator: bool) {
        #[allow(clippy::cast_possible_truncation)]
        let offset = self.viewport_offset.min(u32::from(u16::MAX)) as u16;
        self.grid.apply_scrollback(rows, offset);
        self.painted_offset = self.viewport_offset;
        self.scroll_base = Some(base);
        if show_indicator {
            self.grid.set_scroll_indicator(self.viewport_offset);
        }
    }

    /// Resize the grid and return to live view; a scrollback page has the
    /// wrong dimensions to keep showing. Returns whether copy mode was
    /// torn down, which obliges the caller to send `ExitCopyMode`: a
    /// resize that moves no rows leaves the daemon's pin in place, and it
    /// tells the client nothing either way (Spec-0008).
    #[must_use = "a discarded copy mode leaves a daemon pin to release"]
    pub(crate) fn resize(&mut self, cols: u16, rows: u16) -> bool {
        let exited = self.invalidate_copy_mode();
        self.snap_to_live();
        self.grid.resize(cols, rows);
        exited
    }

    /// Enter copy mode: freeze the live view and seed the cursor at the
    /// bottom-left of what is on screen (Spec-0008). Returns the initial
    /// cache fill for the caller to send once `EnterCopyMode` has gone
    /// out, so the daemon pins before it resolves the window.
    ///
    /// Re-entering re-seeds, matching the daemon's treatment of a second
    /// `EnterCopyMode` as an implicit exit plus enter (ADR-0012).
    pub(crate) fn enter_copy_mode(&mut self) -> FillRequest {
        // `freeze`, not `freeze_live`: a re-entry replaces the state
        // rather than owing an `ExitCopyMode`, since the daemon treats a
        // second enter as a re-pin.
        self.freeze();
        // Seed from the painted page: a scroll still awaiting its page
        // leaves `viewport_offset` naming undrawn rows, and copy mode
        // drops that reply, so the skew would never resolve. Adopting it
        // abandons the pending scroll the user cannot see yet.
        self.viewport_offset = self.painted_offset;
        // The dedicated entry snapshot (ADR-0025 clause 6): the painted
        // cells as they stand, not the saved live snapshot, which keeps
        // absorbing updates while scrolled.
        let snapshot: Vec<Vec<char>> = (0..self.grid.rows)
            .map(|visible| self.grid.row_text(visible).chars().collect())
            .collect();
        let state = CopyModeState::new(
            self.grid.cols,
            self.grid.rows,
            self.painted_offset,
            snapshot,
        );
        let fill = state.initial_fill();
        self.copy_mode = Some(state);
        fill
    }

    /// Leave copy mode and thaw the view. Returns whether there was any
    /// state to discard, so a caller only sends `ExitCopyMode` for a pin
    /// the daemon actually holds.
    pub(crate) fn exit_copy_mode(&mut self) -> bool {
        let exited = self.invalidate_copy_mode();
        if exited {
            self.snap_to_live();
        }
        exited
    }

    #[must_use]
    pub(crate) fn is_copy_mode(&self) -> bool {
        self.copy_mode.is_some()
    }

    /// Note the host-scrollback request now outstanding for this pane.
    pub(crate) fn record_scrollback_request(&mut self, serial: u32) {
        self.scrollback_serial = Some(serial);
    }

    /// Whether a `ScrollbackData` answers this pane's newest host-
    /// scrollback request. An older reply describes an offset the
    /// viewport has left, so painting it shows the wrong page.
    #[must_use]
    pub(crate) fn claims_scrollback(&self, serial: u32) -> bool {
        self.scrollback_serial == Some(serial)
    }

    #[must_use]
    #[allow(dead_code, reason = "read by the copy-mode renderer (TREK-114)")]
    pub(crate) fn copy_mode(&self) -> Option<&CopyModeState> {
        self.copy_mode.as_ref()
    }

    /// Move the copy-mode cursor, clamped to the rows copy mode can
    /// address. A no-op outside copy mode.
    pub(crate) fn set_copy_mode_cursor(&mut self, row: i64, col: u16) {
        if let Some(state) = &mut self.copy_mode {
            state.set_cursor(row, col);
        }
    }

    /// Apply a copy-mode motion, resolved against the cached scrollback
    /// rows and the frozen page. The selection effect applies first, so
    /// `Extend` anchors at the pre-move cursor — taking both here keeps
    /// that ordering out of the caller's hands. A no-op outside copy
    /// mode.
    pub(crate) fn move_copy_mode_cursor(&mut self, motion: Motion, effect: SelectionEffect) {
        let Some(state) = &mut self.copy_mode else {
            return;
        };
        state.apply_selection_effect(effect);
        let target = resolve(motion, state.cursor(), state.motion_bounds(), &*state);
        self.set_copy_mode_cursor(target.0, target.1);
    }

    /// The prefix key a multi-key sequence is waiting on.
    pub(crate) fn copy_mode_pending_prefix(&self) -> Option<PendingPrefix> {
        self.copy_mode.as_ref()?.pending_prefix()
    }

    pub(crate) fn set_copy_mode_pending_prefix(&mut self, pending: Option<PendingPrefix>) {
        if let Some(state) = &mut self.copy_mode {
            state.set_pending_prefix(pending);
        }
    }

    /// Start, switch, or cancel the copy-mode selection.
    pub(crate) fn toggle_copy_mode_selection(&mut self, ty: CopySelectionType) {
        if let Some(state) = &mut self.copy_mode {
            state.toggle_selection(ty);
        }
    }

    /// Where the selection's text comes from under the ADR-0025 split.
    pub(crate) fn copy_mode_yank_plan(&self) -> Option<YankPlan> {
        self.copy_mode.as_ref()?.yank_plan()
    }

    /// Drop the copy-mode selection, reporting whether there was one.
    pub(crate) fn clear_copy_mode_selection(&mut self) -> bool {
        self.copy_mode
            .as_mut()
            .is_some_and(CopyModeState::clear_selection)
    }

    /// The selection's ordered endpoints, or `None` with nothing selected.
    #[cfg(test)]
    pub(crate) fn copy_mode_yank_range(&self) -> Option<crate::copy_mode::YankRange> {
        self.copy_mode.as_ref()?.yank_range()
    }

    pub(crate) fn set_copy_mode_pinned_base(&mut self, base: u64) {
        if let Some(state) = &mut self.copy_mode {
            state.set_pinned_base(base);
        }
    }

    /// Note a `GetScrollback` sent for the copy-mode cache, so its reply
    /// can be matched back to the window it asked for.
    pub(crate) fn record_copy_mode_fill(&mut self, serial: u32, request: FillRequest) {
        if let Some(state) = &mut self.copy_mode {
            state.record_fill(serial, request);
        }
    }

    /// File a `ScrollbackData` into the copy-mode cache. False means the
    /// reply belongs to the ordinary scrollback path instead.
    pub(crate) fn apply_copy_mode_scrollback(
        &mut self,
        serial: u32,
        data: &ScrollbackData,
    ) -> bool {
        self.copy_mode
            .as_mut()
            .is_some_and(|state| state.apply_fill(serial, data))
    }

    /// The next background fill the cursor's position calls for, if any.
    #[must_use]
    pub(crate) fn plan_copy_mode_prefetch(&self) -> Option<FillRequest> {
        self.copy_mode.as_ref()?.plan_prefetch()
    }

    /// Retire a copy-mode fill the daemon answered with an error.
    pub(crate) fn fail_copy_mode_fill(&mut self, serial: u32, retryable: bool) -> FillFailure {
        self.copy_mode
            .as_mut()
            .map_or(FillFailure::Unclaimed, |state| {
                state.fail_fill(serial, retryable)
            })
    }

    /// Note the re-issue of a fill that already failed once.
    pub(crate) fn record_copy_mode_retry(&mut self, serial: u32, request: FillRequest) {
        if let Some(state) = &mut self.copy_mode {
            state.record_retry(serial, request);
        }
    }
}

/// Outcome of scrolling toward live view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "a discarded copy mode leaves a daemon pin to release"]
pub(crate) struct ScrollOutcome {
    /// The viewport reached live (offset 0); the caller should request a
    /// full refresh.
    pub(crate) reached_live: bool,
    /// Copy mode was discarded; the caller owes an `ExitCopyMode`.
    pub(crate) copy_mode_exited: bool,
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
    use crate::copy_keys::{Motion, PendingPrefix};
    use crate::copy_mode::{CopyModeState, CopySelectionType, FillRequest, SelectionEffect};
    use crate::render_grid::ClientGrid;
    use oakterm_protocol::message::ScrollbackData;
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
            input_flags: 0,
            kitty_kbd_flags: 0,
            history_len: 0,
            dirty_rows: vec![make_dirty_row(row, text)],
        }
    }

    /// Offset and snapshot must agree except in the deliberate
    /// `freeze_live` handshake (scrolled at offset 0).
    ///
    /// The copy-mode half is the converse, and the half that matters
    /// there: copy mode's defining state is frozen-at-offset-0, so the
    /// first check alone would assert nothing about it. An active copy
    /// mode must hold a frozen grid whose `viewport_top` names the very
    /// offset the viewport is at — that pair is what makes the cursor's
    /// rows the rows on screen.
    fn assert_lockstep(view: &PaneView) {
        if view.viewport_offset() > 0 {
            assert!(
                view.is_scrolled(),
                "offset {} but grid not scrolled",
                view.viewport_offset()
            );
        }
        if let Some(state) = view.copy_mode() {
            assert!(view.is_scrolled(), "copy mode on a thawed grid");
            assert_eq!(
                state.viewport_top(),
                -i64::from(view.viewport_offset()),
                "copy mode indexes a different page than the viewport shows"
            );
        }
    }

    #[test]
    fn scroll_up_enters_scrollback_and_accumulates() {
        let mut view = PaneView::new(ClientGrid::new(80, 24));
        assert!(!view.scroll_up(5), "no copy mode to release");
        assert!(view.is_scrolled());
        assert_eq!(view.viewport_offset(), 5);
        assert!(!view.scroll_up(3), "no copy mode to release");
        assert_eq!(view.viewport_offset(), 8);
        assert_lockstep(&view);
    }

    #[test]
    fn scroll_down_reports_reaching_live_and_exits_scrollback() {
        let mut view = PaneView::new(ClientGrid::new(80, 24));
        assert!(!view.scroll_up(5), "no copy mode to release");
        assert!(
            !view.scroll_down(3).reached_live,
            "still scrolled at offset 2"
        );
        assert!(view.is_scrolled());
        assert!(
            view.scroll_down(10).reached_live,
            "saturates to live at offset 0"
        );
        assert_eq!(view.viewport_offset(), 0);
        assert!(!view.is_scrolled(), "reaching live must exit scrollback");
    }

    #[test]
    fn update_routing_follows_scroll_state() {
        let mut view = PaneView::new(ClientGrid::new(4, 2));
        view.apply_update(&make_update(1, 0, b"live"));
        assert_eq!(view.grid().row_text(0), "live");

        // Scrolled: updates land in the snapshot, visible cells freeze.
        assert!(!view.scroll_up(5), "no copy mode to release");
        view.apply_update(&make_update(2, 0, b"new!"));
        assert_eq!(view.grid().row_text(0), "live", "visible cells frozen");
        assert_eq!(view.grid().seqno, 2, "snapshot still tracks the daemon");

        // Back to live: the snapshot (with the buffered update) is visible
        // and updates land in the visible cells again.
        assert!(!view.return_to_live(), "no copy mode to release");
        assert_eq!(view.grid().row_text(0), "new!");
        view.apply_update(&make_update(3, 0, b"more"));
        assert_eq!(view.grid().row_text(0), "more");
        assert_lockstep(&view);
    }

    #[test]
    fn return_to_live_resets_offset_and_snapshot_together() {
        let mut view = PaneView::new(ClientGrid::new(4, 2));
        assert!(!view.scroll_up(7), "no copy mode to release");
        assert!(!view.return_to_live(), "no copy mode to release");
        assert_eq!(view.viewport_offset(), 0);
        assert!(!view.is_scrolled());
    }

    #[test]
    fn set_scroll_offset_enters_scrollback_and_zero_returns_to_live() {
        let mut view = PaneView::new(ClientGrid::new(4, 2));
        assert!(!view.set_scroll_offset(12), "no copy mode to release");
        assert_eq!(view.viewport_offset(), 12);
        assert!(view.is_scrolled());
        assert_lockstep(&view);

        assert!(!view.set_scroll_offset(0), "no copy mode to release");
        assert_eq!(view.viewport_offset(), 0);
        assert!(!view.is_scrolled());
    }

    #[test]
    fn set_scroll_offset_keeps_existing_snapshot() {
        let mut view = PaneView::new(ClientGrid::new(4, 2));
        view.apply_update(&make_update(1, 0, b"orig"));
        assert!(!view.freeze_live(), "no copy mode to release");
        view.apply_update(&make_update(2, 0, b"new!"));
        // Jumping to an offset must not re-snapshot the frozen cells.
        assert!(!view.set_scroll_offset(3), "no copy mode to release");
        assert!(!view.return_to_live(), "no copy mode to release");
        assert_eq!(view.grid().row_text(0), "new!");
    }

    #[test]
    fn freeze_live_holds_view_at_offset_zero() {
        let mut view = PaneView::new(ClientGrid::new(4, 2));
        view.apply_update(&make_update(1, 0, b"live"));
        assert!(!view.freeze_live(), "no copy mode to release");
        assert_eq!(view.viewport_offset(), 0);
        assert!(view.is_scrolled());
        view.apply_update(&make_update(2, 0, b"new!"));
        assert_eq!(view.grid().row_text(0), "live", "frozen view holds");
        assert!(!view.return_to_live(), "no copy mode to release");
        assert_eq!(view.grid().row_text(0), "new!");
    }

    #[test]
    fn freeze_live_while_scrolled_preserves_offset_and_snapshot() {
        let mut view = PaneView::new(ClientGrid::new(4, 2));
        view.apply_update(&make_update(1, 0, b"aaaa"));
        assert!(!view.scroll_up(50), "no copy mode to release");
        view.apply_scrollback(&[make_dirty_row(0, b"page")], 0, false);
        assert_eq!(view.viewport_offset(), 50);

        // PromptSearch(Older) can fire while already scrolled: freeze_live
        // must not move the offset or re-snapshot the visible scrollback page.
        assert!(!view.freeze_live(), "no copy mode to release");
        assert_eq!(view.viewport_offset(), 50);
        assert!(view.is_scrolled());
        view.apply_update(&make_update(2, 0, b"live"));
        assert_eq!(view.grid().row_text(0), "page", "frozen page preserved");
        assert_lockstep(&view);
    }

    #[test]
    fn clamp_scrollback_applies_clamp_to_offset() {
        let mut view = PaneView::new(ClientGrid::new(4, 2));
        assert!(!view.scroll_up(2412), "no copy mode to release");
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
        assert!(!view.scroll_up(10), "no copy mode to release");
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
        assert!(!view.scroll_up(1), "no copy mode to release");
        view.apply_scrollback(&[make_dirty_row(0, b"sbk")], 0, false);
        // Top row from scrollback, live snapshot fills below.
        assert_eq!(view.grid().row_text(0), "sbk");
        assert_eq!(view.grid().row_text(1), "abc");
        assert!(!view.return_to_live(), "no copy mode to release");
        assert_eq!(view.grid().row_text(0), "abc");
    }

    #[test]
    fn resize_returns_to_live() {
        let mut view = PaneView::new(ClientGrid::new(4, 2));
        assert!(!view.scroll_up(9), "no copy mode to release");
        assert!(!view.resize(10, 5), "no copy mode to tear down");
        assert_eq!(view.viewport_offset(), 0);
        assert!(!view.is_scrolled());
        assert_eq!((view.grid().cols, view.grid().rows), (10, 5));
    }

    // --- Copy mode (Spec-0008) ---

    fn scrollback_data(start_row: i64, rows: usize) -> ScrollbackData {
        ScrollbackData {
            pane_id: 0,
            start_row,
            has_more: true,
            total_rows: 500,
            base: 0,
            rows: (0..rows).map(|_| make_dirty_row(0, b"sbk")).collect(),
        }
    }

    /// Entry freezes the view in place rather than scrolling it: the
    /// cursor starts on the live grid, and the daemon's pin is only
    /// meaningful against the rows that were on screen when it was taken.
    #[test]
    fn entering_copy_mode_freezes_the_view_at_offset_zero() {
        let mut view = PaneView::new(ClientGrid::new(4, 8));
        view.apply_update(&make_update(1, 0, b"live"));

        let fill = view.enter_copy_mode();

        view.set_copy_mode_pinned_base(0);

        assert_eq!(view.viewport_offset(), 0);
        assert!(view.is_scrolled(), "the live view is frozen");
        assert_eq!(view.copy_mode().map(CopyModeState::cursor), Some((7, 0)));
        assert_eq!(fill.start_row, -8, "one screen above the viewport");
        assert_lockstep(&view);

        // Output arriving during copy mode must not move what is shown.
        view.apply_update(&make_update(2, 0, b"new!"));
        assert_eq!(view.grid().row_text(0), "live");
    }

    /// The race: `viewport_offset` moves when a scroll is requested, but
    /// the grid keeps showing the previous page until that reply lands.
    /// Seeding from the requested offset would index rows nothing has
    /// drawn — and permanently, since copy mode drops the reply that
    /// would have caught the view up.
    #[test]
    fn entering_copy_mode_mid_scroll_seeds_from_the_painted_page() {
        let mut view = PaneView::new(ClientGrid::new(4, 8));
        assert!(!view.scroll_up(5), "no copy mode to release");
        view.apply_scrollback(&[make_dirty_row(0, b"page")], 0, false);

        // A second scroll: the offset moves, its page has not arrived.
        assert!(!view.scroll_up(30), "no copy mode to release");

        let fill = view.enter_copy_mode();

        view.set_copy_mode_pinned_base(0);

        assert_eq!(
            view.copy_mode().map(CopyModeState::cursor),
            Some((2, 0)),
            "seeded from the painted page at offset 5, not the pending 35"
        );
        assert_eq!(fill.start_row, -13, "one screen above the painted page");
        assert_eq!(
            view.viewport_offset(),
            5,
            "the offset adopts the page actually on screen"
        );
        assert_lockstep(&view);
    }

    /// With no scroll outstanding the two offsets agree, so entry is
    /// unchanged — the fix must not perturb the settled case. This also
    /// carries the wiring guard: every other `viewport_top` test builds
    /// the state directly, so `new(rows, 0)` here would go unnoticed.
    #[test]
    fn entering_copy_mode_with_no_pending_page_uses_the_current_offset() {
        let mut view = PaneView::new(ClientGrid::new(4, 8));
        assert!(!view.scroll_up(5), "no copy mode to release");
        view.apply_scrollback(&[make_dirty_row(0, b"page")], 0, false);

        let fill = view.enter_copy_mode();

        view.set_copy_mode_pinned_base(0);

        assert_eq!(view.copy_mode().map(CopyModeState::cursor), Some((2, 0)));
        assert_eq!(fill.start_row, -13);
        assert_eq!(view.viewport_offset(), 5);
    }

    /// Returning to live restores a page that is painted by definition,
    /// so a later entry must not seed from a stale scrollback offset.
    #[test]
    fn reaching_live_resets_the_painted_page() {
        let mut view = PaneView::new(ClientGrid::new(4, 8));
        assert!(!view.scroll_up(5), "no copy mode to release");
        view.apply_scrollback(&[make_dirty_row(0, b"page")], 0, false);
        assert!(view.scroll_down(5).reached_live);

        view.enter_copy_mode();

        view.set_copy_mode_pinned_base(0);

        assert_eq!(
            view.copy_mode().map(CopyModeState::cursor),
            Some((7, 0)),
            "live view seeds at the grid bottom"
        );
        assert_lockstep(&view);
    }

    #[test]
    fn exiting_copy_mode_discards_the_state_and_thaws_the_view() {
        let mut view = PaneView::new(ClientGrid::new(4, 8));
        view.apply_update(&make_update(1, 0, b"live"));
        view.enter_copy_mode();
        view.set_copy_mode_pinned_base(0);
        view.apply_update(&make_update(2, 0, b"new!"));

        assert!(view.exit_copy_mode());

        assert!(view.copy_mode().is_none());
        assert!(!view.is_scrolled(), "the view follows live output again");
        assert_eq!(view.grid().row_text(0), "new!");
        assert!(
            !view.exit_copy_mode(),
            "a second exit must not claim a pin to release"
        );
    }

    /// A resize invalidates the pin's row indices, and the daemon says
    /// nothing when it drops one, so the client tears down on its own.
    #[test]
    fn resize_tears_down_copy_mode_and_reports_it() {
        let mut view = PaneView::new(ClientGrid::new(4, 8));
        view.enter_copy_mode();

        view.set_copy_mode_pinned_base(0);

        assert!(view.resize(10, 5), "caller owes the daemon an ExitCopyMode");

        assert!(view.copy_mode().is_none());
        assert!(!view.is_scrolled());
        assert_lockstep(&view);
    }

    /// Copy-mode fills are matched by serial, so a reply that belongs to
    /// the ordinary scrollback path is refused rather than swallowed.
    #[test]
    fn only_a_matching_fill_serial_lands_in_the_copy_mode_cache() {
        let mut view = PaneView::new(ClientGrid::new(4, 8));
        view.enter_copy_mode();
        view.set_copy_mode_pinned_base(0);
        view.record_copy_mode_fill(
            42,
            FillRequest {
                start_row: -8,
                count: 8,
            },
        );

        assert!(!view.apply_copy_mode_scrollback(9, &scrollback_data(-8, 8)));
        assert!(view.apply_copy_mode_scrollback(42, &scrollback_data(-8, 8)));
        assert_eq!(view.copy_mode().map(|s| s.cache().len()), Some(8));
    }

    /// Motions read the frozen grid for on-screen rows. Entering live,
    /// copy-mode row N is grid row N, so a word motion has to see the
    /// text the pane is displaying.
    #[test]
    fn motions_read_the_frozen_grid_for_rows_on_screen() {
        let mut view = PaneView::new(ClientGrid::new(16, 8));
        view.apply_update(&make_update(1, 3, b"alpha beta"));
        view.enter_copy_mode();
        view.set_copy_mode_pinned_base(0);
        view.set_copy_mode_cursor(3, 0);

        view.move_copy_mode_cursor(Motion::WordForward, SelectionEffect::Keep);
        assert_eq!(view.copy_mode().map(CopyModeState::cursor), Some((3, 6)));

        view.move_copy_mode_cursor(Motion::LineEnd, SelectionEffect::Keep);
        assert_eq!(view.copy_mode().map(CopyModeState::cursor), Some((3, 9)));
    }

    /// And the cache for scrollback rows, which the grid never holds.
    /// Reading only one source would make half the buffer unnavigable.
    #[test]
    fn motions_read_the_cache_for_rows_below_the_pin() {
        let mut view = PaneView::new(ClientGrid::new(16, 8));
        view.enter_copy_mode();
        view.set_copy_mode_pinned_base(0);
        view.record_copy_mode_fill(
            1,
            FillRequest {
                start_row: -8,
                count: 8,
            },
        );
        let mut data = scrollback_data(-8, 8);
        data.rows[0] = make_dirty_row(0, b"one two");
        assert!(view.apply_copy_mode_scrollback(1, &data));

        view.set_copy_mode_cursor(-8, 0);
        view.move_copy_mode_cursor(Motion::WordForward, SelectionEffect::Keep);
        assert_eq!(view.copy_mode().map(CopyModeState::cursor), Some((-8, 4)));
    }

    /// Entering scrolled, a copy-mode row maps to grid row
    /// `row - viewport_top` — and the cache wins wherever it holds the
    /// row, since the grid shows the same screen line through a different
    /// page. Getting either wrong yanks text the user never pointed at.
    #[test]
    fn a_scrolled_entry_resolves_motions_against_the_cached_text() {
        let mut view = PaneView::new(ClientGrid::new(16, 8));
        assert!(!view.scroll_up(1), "no copy mode to release");
        view.apply_scrollback(&[make_dirty_row(0, b"zzzz")], 0, false);
        view.enter_copy_mode();

        view.set_copy_mode_pinned_base(0);

        // Copy-mode row -1 is grid row 0 on this page, and the cache
        // holds different text for it than the grid shows.
        assert!(
            view.grid().row_text(0).starts_with("zzzz"),
            "the grid text must differ from the cached text"
        );
        view.record_copy_mode_fill(
            1,
            FillRequest {
                start_row: -1,
                count: 1,
            },
        );
        let mut data = scrollback_data(-1, 1);
        data.rows[0] = make_dirty_row(0, b"one two");
        assert!(view.apply_copy_mode_scrollback(1, &data));

        view.set_copy_mode_cursor(-1, 0);
        view.move_copy_mode_cursor(Motion::WordForward, SelectionEffect::Keep);

        assert_eq!(
            view.copy_mode().map(CopyModeState::cursor),
            Some((-1, 4)),
            "the motion read the grid's `zzzz`, not the cache's `one two`"
        );
    }

    /// Prefetch fires on the cursor entering the edge quarter under
    /// motions, not only under a direct `set_cursor` — the path a user
    /// actually walks into scrollback on.
    #[test]
    fn repeated_half_page_motions_reach_the_prefetch_boundary() {
        let mut view = PaneView::new(ClientGrid::new(16, 8));
        view.enter_copy_mode();
        view.set_copy_mode_pinned_base(0);
        view.record_copy_mode_fill(
            1,
            FillRequest {
                start_row: -24,
                count: 24,
            },
        );
        assert!(view.apply_copy_mode_scrollback(1, &scrollback_data(-24, 24)));
        assert!(
            view.plan_copy_mode_prefetch().is_none(),
            "the cursor starts mid-window"
        );

        for _ in 0..7 {
            view.move_copy_mode_cursor(Motion::HalfPageUp, SelectionEffect::Keep);
        }

        assert_eq!(
            view.copy_mode().map(CopyModeState::cursor),
            Some((-21, 0)),
            "seven half pages up from row 7"
        );
        assert_eq!(
            view.plan_copy_mode_prefetch(),
            Some(FillRequest {
                start_row: -32,
                count: 8
            })
        );
    }

    /// The clamp is the state's, so a motion cannot walk off the ends
    /// even though the resolver was handed the same bounds.
    #[test]
    fn motions_stay_within_the_addressable_rows() {
        let mut view = PaneView::new(ClientGrid::new(16, 8));
        view.enter_copy_mode();

        view.set_copy_mode_pinned_base(0);

        view.set_copy_mode_cursor(0, 0);
        view.move_copy_mode_cursor(Motion::PageUp, SelectionEffect::Keep);
        assert_eq!(view.copy_mode().map(CopyModeState::cursor), Some((0, 0)));

        view.move_copy_mode_cursor(Motion::Bottom, SelectionEffect::Keep);
        view.move_copy_mode_cursor(Motion::PageDown, SelectionEffect::Keep);
        assert_eq!(view.copy_mode().map(CopyModeState::cursor), Some((7, 0)));
    }

    /// Motions and the selection are per-pane state, so nothing here
    /// reaches a pane that is not in copy mode.
    #[test]
    fn copy_mode_commands_are_inert_outside_copy_mode() {
        let mut view = PaneView::new(ClientGrid::new(16, 8));

        view.move_copy_mode_cursor(Motion::Down, SelectionEffect::Extend);
        view.toggle_copy_mode_selection(CopySelectionType::Character);
        view.set_copy_mode_pending_prefix(Some(PendingPrefix::G));

        assert!(view.copy_mode().is_none());
        assert_eq!(view.copy_mode_yank_range(), None);
        assert_eq!(view.copy_mode_pending_prefix(), None);
        assert!(!view.clear_copy_mode_selection());
    }

    /// `Extend` anchors before the motion resolves: the selection starts
    /// at the pre-move cursor, not wherever the move lands.
    #[test]
    fn an_extending_move_anchors_at_the_pre_move_cursor() {
        let mut view = PaneView::new(ClientGrid::new(16, 8));
        view.enter_copy_mode();
        view.set_copy_mode_pinned_base(0);
        view.set_copy_mode_cursor(3, 2);

        view.move_copy_mode_cursor(Motion::Right, SelectionEffect::Extend);

        let range = view.copy_mode_yank_range().expect("selecting");
        assert_eq!((range.start_row, range.start_col), (3, 2));
        assert_eq!((range.end_row, range.end_col), (3, 3));
    }

    /// Copy-mode state is per-pane, so a `g` armed on one pane cannot
    /// complete a `gg` on another after focus moves.
    #[test]
    fn a_pending_prefix_belongs_to_its_own_pane() {
        let mut first = PaneView::new(ClientGrid::new(16, 8));
        let mut second = PaneView::new(ClientGrid::new(16, 8));
        first.enter_copy_mode();
        second.enter_copy_mode();

        first.set_copy_mode_pending_prefix(Some(PendingPrefix::G));

        assert_eq!(first.copy_mode_pending_prefix(), Some(PendingPrefix::G));
        assert_eq!(second.copy_mode_pending_prefix(), None);
    }

    /// A pane not in copy mode leaves every scrollback reply to the
    /// ordinary path, which is what keeps the host-scrollback viewport
    /// working while another pane is in copy mode.
    #[test]
    fn a_pane_outside_copy_mode_claims_no_scrollback_reply() {
        let mut view = PaneView::new(ClientGrid::new(4, 8));
        assert!(!view.apply_copy_mode_scrollback(42, &scrollback_data(-8, 8)));
        assert!(view.plan_copy_mode_prefetch().is_none());
    }

    /// Every viewport primitive tears copy mode down and says so. The
    /// pin is indexed against the view as it stood at entry, so any move
    /// leaves the cursor naming rows that are no longer on screen — and
    /// a caller that is not told cannot release the daemon's pin.
    #[test]
    fn every_viewport_move_reports_tearing_copy_mode_down() {
        fn assert_tears_down(name: &str, apply: impl Fn(&mut PaneView) -> bool) {
            let mut view = PaneView::new(ClientGrid::new(4, 8));
            view.enter_copy_mode();

            view.set_copy_mode_pinned_base(0);

            assert!(apply(&mut view), "{name} did not report the teardown");
            assert!(
                view.copy_mode().is_none(),
                "{name} left copy mode populated"
            );
        }

        assert_tears_down("scroll_up", |v| v.scroll_up(5));
        assert_tears_down("scroll_down", |v| v.scroll_down(3).copy_mode_exited);
        assert_tears_down("return_to_live", PaneView::return_to_live);
        assert_tears_down("set_scroll_offset", |v| v.set_scroll_offset(7));
        assert_tears_down("set_scroll_offset(0)", |v| v.set_scroll_offset(0));
        assert_tears_down("freeze_live", PaneView::freeze_live);
        assert_tears_down("resize", |v| v.resize(10, 5));
    }

    /// The relayout path: a move that changes nothing about the
    /// dimensions still thaws the grid, so it must still tear copy mode
    /// down — otherwise live output scrolls the rows the cursor names.
    #[test]
    fn returning_to_live_at_offset_zero_still_tears_copy_mode_down() {
        let mut view = PaneView::new(ClientGrid::new(4, 8));
        view.enter_copy_mode();
        view.set_copy_mode_pinned_base(0);
        assert_eq!(view.viewport_offset(), 0, "entered live, not scrolled");

        assert!(view.return_to_live(), "the thaw invalidates copy mode");
        assert!(view.copy_mode().is_none());
    }

    /// A reply to a superseded request describes an offset the viewport
    /// has left; painting it would show the wrong page.
    #[test]
    fn only_the_newest_scrollback_request_is_claimed() {
        let mut view = PaneView::new(ClientGrid::new(4, 8));
        view.record_scrollback_request(11);
        assert!(view.claims_scrollback(11));

        view.record_scrollback_request(12);
        assert!(!view.claims_scrollback(11), "superseded");
        assert!(view.claims_scrollback(12));
    }

    #[test]
    fn a_pane_that_asked_for_no_scrollback_claims_nothing() {
        let view = PaneView::new(ClientGrid::new(4, 8));
        assert!(!view.claims_scrollback(0));
        assert!(!view.claims_scrollback(11));
    }

    /// Copy mode belongs to the pane, not the window: two panes hold
    /// independent state, which is what lets it survive focus moving
    /// away and back.
    #[test]
    fn two_panes_hold_copy_mode_state_independently() {
        let mut a = PaneView::new(ClientGrid::new(4, 8));
        let mut b = PaneView::new(ClientGrid::new(4, 4));
        a.enter_copy_mode();
        b.enter_copy_mode();

        a.set_copy_mode_cursor(0, 0);

        assert_eq!(a.copy_mode().map(CopyModeState::cursor), Some((0, 0)));
        assert_eq!(
            b.copy_mode().map(CopyModeState::cursor),
            Some((3, 0)),
            "the other pane keeps its own cursor and height"
        );
        assert!(b.exit_copy_mode());
        assert!(a.copy_mode().is_some(), "exiting one must not exit both");
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

    // --- ADR-0025 entry base and snapshot ---

    fn update_with_history(seqno: u64, history_len: u64) -> RenderUpdate {
        RenderUpdate {
            history_len,
            ..make_update(seqno, 0, b"")
        }
    }

    /// The entry base follows the painted source (ADR-0025 clause 3):
    /// the last update's history length at offset 0, the scrollback
    /// page's serve-time base when fully scrolled.
    #[test]
    fn entry_base_follows_the_painted_source() {
        let mut view = PaneView::new(ClientGrid::new(16, 4));
        assert_eq!(view.copy_mode_entry_base(), None, "nothing painted yet");

        view.apply_update(&update_with_history(1, 7));
        assert_eq!(view.copy_mode_entry_base(), Some(7));

        // Fully scrolled: the page is entirely scrollback, so its own
        // serve-time base wins even though updates kept arriving.
        assert!(!view.scroll_up(4), "no copy mode to release");
        view.apply_scrollback(&[make_dirty_row(0, b"old")], 5, false);
        view.apply_update(&update_with_history(2, 9));
        assert_eq!(view.copy_mode_entry_base(), Some(5));
    }

    /// A partially scrolled page mixes two sources; entry needs them to
    /// name the same instant, else there is no base to echo.
    #[test]
    fn a_two_instant_partial_page_has_no_entry_base() {
        let mut view = PaneView::new(ClientGrid::new(16, 4));
        view.apply_update(&update_with_history(1, 7));
        assert!(!view.scroll_up(2), "no copy mode to release");
        view.apply_scrollback(&[make_dirty_row(0, b"old")], 5, false);
        assert_eq!(view.copy_mode_entry_base(), None, "7 != 5");

        view.apply_scrollback(&[make_dirty_row(0, b"old")], 7, false);
        assert_eq!(view.copy_mode_entry_base(), Some(7), "instants agree");
    }

    /// Entry snapshots the painted cells (ADR-0025 clause 6): output
    /// arriving after entry mutates the grid but not what copy mode
    /// reads for painted-page rows.
    #[test]
    fn entry_snapshots_the_painted_cells() {
        let mut view = PaneView::new(ClientGrid::new(16, 4));
        view.apply_update(&make_update(1, 0, b"before"));
        view.enter_copy_mode();
        view.set_copy_mode_pinned_base(0);
        view.apply_update(&make_update(2, 0, b"after!"));

        let state = view.copy_mode().expect("in copy mode");
        let row: String = state.snapshot_row(0).expect("page row").iter().collect();
        assert_eq!(row.trim_end(), "before");
    }
}
