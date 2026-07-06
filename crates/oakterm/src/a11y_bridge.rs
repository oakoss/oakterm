//! Bridges client render state (per-pane grids, font metrics, and the shared
//! `A11yModel`) into `oakterm_a11y` tree updates, and maps between the
//! terminal's cell-based `Selection` and AccessKit's between-character
//! `TextSelection` positions.

use std::collections::HashMap;
use std::sync::Mutex;

use oakterm_a11y::SelectionRange;
use oakterm_renderer::shaper::FontMetrics;
use oakterm_terminal::grid::selection::{AnchorSide, Selection, SelectionType};

use crate::render_grid::ClientGrid;

pub(crate) struct PaneA11ySnapshot {
    pub(crate) rows: u16,
    pub(crate) cols: u16,
    pub(crate) row_texts: Vec<String>,
    pub(crate) cursor_row: u16,
    pub(crate) cursor_col: u16,
    pub(crate) title: String,
    pub(crate) scrollback_lines: u64,
    pub(crate) scroll_offset: u64,
    pub(crate) selection: Option<SelectionRange>,
}

/// Snapshot of all panes for the accessibility tree. Shared between `App`
/// and the AccessKit activation handler via `Arc<Mutex<Option<_>>>`.
pub(crate) struct A11yModel {
    pub(crate) panes: HashMap<u32, PaneA11ySnapshot>,
    pub(crate) focused: u32,
    pub(crate) cell_width: f64,
    pub(crate) cell_height: f64,
}

impl A11yModel {
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
                    // Pane pixel origins arrive with GUI split rendering (TREK-99).
                    origin: (0.0, 0.0),
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
}

/// The per-event varying inputs to an incremental update. Grid-derived fields
/// (rows/cols/cursor) and cell dimensions are supplied to `build_incremental`
/// separately, so callers only specify what actually differs between events.
pub(crate) struct A11yDelta<'a> {
    pub(crate) pane_id: u32,
    pub(crate) focused: u32,
    pub(crate) dirty_row_indices: &'a [u16],
    /// Parallel to `dirty_row_indices` (matching `IncrementalInput`).
    pub(crate) dirty_row_texts: &'a [String],
    pub(crate) cursor_changed: bool,
    pub(crate) title: &'a str,
    pub(crate) title_changed: bool,
    pub(crate) scrollback_lines: u64,
    pub(crate) scroll_offset: u64,
    pub(crate) selection: Option<SelectionRange>,
    pub(crate) selection_changed: bool,
    pub(crate) announcement: Option<&'a oakterm_a11y::Announcement>,
}

/// Resolve cell pixel dimensions, falling back to 8x16 before the font is
/// initialized.
pub(crate) fn cell_dims(metrics: Option<&FontMetrics>) -> (f64, f64) {
    metrics.map_or((8.0, 16.0), |m| {
        (f64::from(m.cell_width), f64::from(m.cell_height))
    })
}

/// Read a pane's title from the shared model, defaulting to empty when the
/// lock is poisoned or the pane is not yet tracked.
pub(crate) fn model_title(state: &Mutex<Option<A11yModel>>, pane_id: u32) -> String {
    state
        .lock()
        .ok()
        .and_then(|guard| {
            guard
                .as_ref()
                .and_then(|model| model.panes.get(&pane_id).map(|snap| snap.title.clone()))
        })
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
        pane_id: delta.pane_id,
        focused: delta.focused,
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
        scrollback_lines: delta.scrollback_lines,
        scroll_offset: delta.scroll_offset,
        selection: delta.selection.map(|s| clamp_selection_cols(s, grid)),
        selection_changed: delta.selection_changed,
        announcement: delta.announcement,
        cell_width,
        cell_height,
        origin: (0.0, 0.0),
    };
    oakterm_a11y::build_incremental_update(&input)
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
    use oakterm_a11y::row_node_id;

    fn snapshot(title: &str) -> PaneA11ySnapshot {
        PaneA11ySnapshot {
            rows: 24,
            cols: 80,
            row_texts: Vec::new(),
            cursor_row: 0,
            cursor_col: 0,
            title: title.to_string(),
            scrollback_lines: 0,
            scroll_offset: 0,
            selection: None,
        }
    }

    #[test]
    fn cell_dims_falls_back_without_font() {
        assert_eq!(cell_dims(None), (8.0, 16.0));
    }

    #[test]
    fn model_title_empty_when_unpopulated() {
        let state: Mutex<Option<A11yModel>> = Mutex::new(None);
        assert_eq!(model_title(&state, 0), "");
    }

    #[test]
    fn model_title_reads_pane_title() {
        let mut panes = HashMap::new();
        panes.insert(2, snapshot("vim"));
        let state = Mutex::new(Some(A11yModel {
            panes,
            focused: 2,
            cell_width: 8.0,
            cell_height: 16.0,
        }));
        assert_eq!(model_title(&state, 2), "vim");
        assert_eq!(model_title(&state, 7), "");
    }

    #[test]
    fn full_tree_orders_panes_by_id() {
        let mut panes = HashMap::new();
        panes.insert(5, snapshot("b"));
        panes.insert(1, snapshot("a"));
        let model = A11yModel {
            panes,
            focused: 5,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let update = model.build_full_tree();
        let window = &update
            .nodes
            .iter()
            .find(|(id, _)| *id == oakterm_a11y::WINDOW_ID)
            .expect("window")
            .1;
        assert_eq!(
            window.children(),
            &[
                oakterm_a11y::terminal_node_id(1),
                oakterm_a11y::terminal_node_id(5),
                oakterm_a11y::ANNOUNCEMENT_ID
            ]
        );
        assert_eq!(update.focus, oakterm_a11y::terminal_node_id(5));
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
    fn build_incremental_clamps_line_selection_to_row_text() {
        // The Line-selection end-of-row sentinel must resolve against the
        // trimmed row text (empty here), not the column count.
        let grid = ClientGrid::new(80, 24);
        let delta = A11yDelta {
            pane_id: 0,
            focused: 0,
            dirty_row_indices: &[],
            dirty_row_texts: &[],
            cursor_changed: false,
            title: "",
            title_changed: false,
            scrollback_lines: 0,
            scroll_offset: 0,
            selection: Some(SelectionRange {
                anchor_row: 0,
                anchor_col: 0,
                focus_row: 1,
                focus_col: usize::MAX,
            }),
            selection_changed: true,
            announcement: None,
        };
        let update = build_incremental(&grid, (8.0, 16.0), &delta);
        let terminal = update
            .nodes
            .iter()
            .find(|(id, _)| *id == oakterm_a11y::terminal_node_id(0))
            .expect("terminal");
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
                node: oakterm_a11y::terminal_node_id(0),
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
