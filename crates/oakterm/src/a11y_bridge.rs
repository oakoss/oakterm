//! Bridges client render state (the grid, font metrics, and the shared
//! `A11ySnapshot`) into `oakterm_a11y` tree updates. The winit user-event
//! handlers built near-identical `IncrementalInput` values at four sites
//! (render, scrollback, title, bell); this module centralizes that.

use std::sync::Mutex;

use oakterm_renderer::shaper::FontMetrics;

use crate::render_grid::ClientGrid;

/// Snapshot of state needed to build the accessibility tree. Shared between
/// `App` and the AccessKit activation handler via `Arc<Mutex<Option<_>>>`.
pub(crate) struct A11ySnapshot {
    pub(crate) rows: u16,
    pub(crate) cols: u16,
    pub(crate) row_texts: Vec<String>,
    pub(crate) cursor_row: u16,
    pub(crate) cursor_col: u16,
    pub(crate) title: String,
    pub(crate) scrollback_lines: u64,
    pub(crate) cell_width: f64,
    pub(crate) cell_height: f64,
    /// Set when title changes; cleared after the next incremental update.
    pub(crate) title_changed: bool,
}

/// The per-event varying inputs to an incremental update. Grid-derived fields
/// (rows/cols/cursor) and cell dimensions are supplied to `build_incremental`
/// separately, so callers only specify what actually differs between events.
pub(crate) struct A11yDelta<'a> {
    pub(crate) dirty_row_indices: &'a [u16],
    /// Parallel to `dirty_row_indices` (matching `IncrementalInput`).
    pub(crate) dirty_row_texts: &'a [String],
    pub(crate) cursor_changed: bool,
    pub(crate) title: &'a str,
    pub(crate) title_changed: bool,
    pub(crate) announcement: Option<&'a oakterm_a11y::Announcement>,
}

/// Resolve cell pixel dimensions, falling back to 8x16 before the font is
/// initialized.
pub(crate) fn cell_dims(metrics: Option<&FontMetrics>) -> (f64, f64) {
    metrics.map_or((8.0, 16.0), |m| {
        (f64::from(m.cell_width), f64::from(m.cell_height))
    })
}

/// Read the current terminal title from the shared snapshot, defaulting to
/// empty when the lock is poisoned or the snapshot is not yet populated.
pub(crate) fn snapshot_title(state: &Mutex<Option<A11ySnapshot>>) -> String {
    state
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|snap| snap.title.clone()))
        .unwrap_or_default()
}

/// The cursor row's text is read from `grid` for clamping.
pub(crate) fn build_incremental(
    grid: &ClientGrid,
    (cell_width, cell_height): (f64, f64),
    delta: &A11yDelta<'_>,
) -> accesskit::TreeUpdate {
    let cursor_row_text = grid.row_text(grid.cursor_y);
    let input = oakterm_a11y::IncrementalInput {
        rows: grid.rows,
        cols: grid.cols,
        dirty_row_indices: delta.dirty_row_indices,
        dirty_row_texts: delta.dirty_row_texts,
        cursor_row: grid.cursor_y,
        cursor_col: grid.cursor_x,
        cursor_changed: delta.cursor_changed,
        cursor_row_text: &cursor_row_text,
        title: delta.title,
        title_changed: delta.title_changed,
        announcement: delta.announcement,
        cell_width,
        cell_height,
    };
    oakterm_a11y::build_incremental_update(&input)
}

#[cfg(test)]
mod tests {
    use super::{A11ySnapshot, cell_dims, snapshot_title};
    use std::sync::Mutex;

    #[test]
    fn cell_dims_falls_back_without_font() {
        assert_eq!(cell_dims(None), (8.0, 16.0));
    }

    #[test]
    fn snapshot_title_empty_when_unpopulated() {
        let state: Mutex<Option<A11ySnapshot>> = Mutex::new(None);
        assert_eq!(snapshot_title(&state), "");
    }

    #[test]
    fn snapshot_title_reads_populated_title() {
        let state = Mutex::new(Some(A11ySnapshot {
            rows: 24,
            cols: 80,
            row_texts: Vec::new(),
            cursor_row: 0,
            cursor_col: 0,
            title: "vim".to_string(),
            scrollback_lines: 0,
            cell_width: 8.0,
            cell_height: 16.0,
            title_changed: false,
        }));
        assert_eq!(snapshot_title(&state), "vim");
    }
}
