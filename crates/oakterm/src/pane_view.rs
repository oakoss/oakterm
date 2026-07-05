//! Per-pane render-side state: the client grid plus the view state that
//! multiplies with it when split rendering (TREK-99) lands — scrollback
//! viewport, selection, and mode flags all follow a pane, not the window.

use crate::render_grid::ClientGrid;
use oakterm_terminal::grid::selection::Selection;

pub(crate) struct PaneView {
    pub(crate) grid: ClientGrid,
    /// Lines scrolled up from bottom. 0 = live view (at bottom).
    pub(crate) viewport_offset: u32,
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

    /// Scroll up into host scrollback, entering scrollback mode on the
    /// first scroll so the grid snapshots the live view.
    pub(crate) fn scroll_up(&mut self, lines: u32) {
        if !self.grid.is_scrolled() {
            self.grid.enter_scrollback();
        }
        self.viewport_offset = self.viewport_offset.saturating_add(lines);
    }

    /// Scroll toward live view. Returns true when the viewport reaches
    /// live (offset 0), signalling the caller to exit scrollback.
    pub(crate) fn scroll_down(&mut self, lines: u32) -> bool {
        self.viewport_offset = self.viewport_offset.saturating_sub(lines);
        self.viewport_offset == 0
    }
}

/// Outcome of clamping the host scrollback viewport offset against the
/// daemon's reported buffer length.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ScrollbackClampOutcome {
    /// Clamped to a non-zero offset; stay in scrollback.
    Clamp(u32),
    /// Buffer is empty (or smaller than 1); leave scrollback mode entirely.
    ReturnToLive,
}

/// Clamp `current` viewport offset to the daemon's actual scrollback length.
/// Returns `ReturnToLive` when the clamp lands at zero, signalling the caller
/// should call `return_to_live()` instead of painting a "[0 lines]" indicator.
pub(crate) fn clamp_viewport(current: u32, total: u32) -> ScrollbackClampOutcome {
    let clamped = current.min(total);
    if clamped == 0 {
        ScrollbackClampOutcome::ReturnToLive
    } else {
        ScrollbackClampOutcome::Clamp(clamped)
    }
}

#[cfg(test)]
mod tests {
    use super::{PaneView, ScrollbackClampOutcome, clamp_viewport};
    use crate::render_grid::ClientGrid;

    #[test]
    fn scroll_up_enters_scrollback_and_accumulates() {
        let mut view = PaneView::new(ClientGrid::new(80, 24));
        view.scroll_up(5);
        assert!(view.grid.is_scrolled());
        assert_eq!(view.viewport_offset, 5);
        view.scroll_up(3);
        assert_eq!(view.viewport_offset, 8);
    }

    #[test]
    fn scroll_down_reports_reaching_live() {
        let mut view = PaneView::new(ClientGrid::new(80, 24));
        view.scroll_up(5);
        assert!(!view.scroll_down(3), "still scrolled at offset 2");
        assert!(view.scroll_down(10), "saturates to live at offset 0");
        assert_eq!(view.viewport_offset, 0);
    }

    #[test]
    fn clamp_below_total_keeps_offset() {
        assert_eq!(clamp_viewport(10, 100), ScrollbackClampOutcome::Clamp(10));
    }

    #[test]
    fn clamp_above_total_clamps_to_total() {
        assert_eq!(clamp_viewport(2412, 50), ScrollbackClampOutcome::Clamp(50));
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
