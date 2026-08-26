//! Render daemon `DirtyRow` cells back into plain text lines.
//!
//! Mirrors the GUI's `ClientGrid::row_texts`: a `0` codepoint is a blank cell,
//! invalid codepoints become U+FFFD, and trailing spaces are trimmed.

use oakterm_protocol::message::ScrollbackData;
use oakterm_protocol::render::{DirtyRow, RenderUpdate};

/// One row's cells rendered to a string, trailing spaces trimmed.
fn row_to_string(row: &DirtyRow) -> String {
    let mut s = String::with_capacity(row.cells.len());
    for cell in &row.cells {
        if cell.codepoint == 0 {
            s.push(' ');
        } else {
            s.push(char::from_u32(cell.codepoint).unwrap_or('\u{FFFD}'));
        }
    }
    s.truncate(s.trim_end_matches(' ').len());
    s
}

/// The current visible screen as text. `RenderUpdate` rows are keyed by
/// `row_index`, so sort by it; gaps (rows never written) are skipped.
#[must_use]
pub fn visible_screen(update: &RenderUpdate) -> String {
    let mut rows: Vec<&DirtyRow> = update.dirty_rows.iter().collect();
    rows.sort_by_key(|r| r.row_index);
    lines_to_block(rows.into_iter().map(row_to_string))
}

/// Scrollback rows as text, in the order the daemon returned them (oldest to
/// newest across the requested window).
#[must_use]
pub fn scrollback(data: &ScrollbackData) -> String {
    lines_to_block(data.rows.iter().map(row_to_string))
}

/// Join lines, then trim wholly-blank leading/trailing lines so a mostly-empty
/// screen doesn't print as a wall of newlines.
fn lines_to_block(lines: impl Iterator<Item = String>) -> String {
    let lines: Vec<String> = lines.collect();
    let first = lines.iter().position(|l| !l.is_empty());
    let last = lines.iter().rposition(|l| !l.is_empty());
    match (first, last) {
        (Some(a), Some(b)) => lines[a..=b].join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oakterm_protocol::render::WireCell;

    fn cell(ch: char) -> WireCell {
        WireCell {
            codepoint: ch as u32,
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

    fn row(index: u16, text: &str) -> DirtyRow {
        DirtyRow {
            row_index: index,
            cells: text.chars().map(cell).collect(),
            semantic_mark: 0,
            mark_metadata: vec![],
        }
    }

    #[test]
    fn null_codepoint_becomes_space_and_trailing_trimmed() {
        let mut r = row(0, "hi");
        r.cells.push(WireCell {
            codepoint: 0,
            ..cell(' ')
        });
        r.cells.push(cell(' '));
        assert_eq!(row_to_string(&r), "hi");
    }

    #[test]
    fn invalid_codepoint_becomes_replacement() {
        let mut r = row(0, "");
        r.cells.push(WireCell {
            codepoint: 0xD800, // lone surrogate: not a valid char
            ..cell('x')
        });
        assert_eq!(row_to_string(&r), "\u{FFFD}");
    }

    #[test]
    fn visible_screen_sorts_by_row_index() {
        let update = RenderUpdate {
            pane_id: 0,
            seqno: 1,
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
            dirty_rows: vec![row(2, "third"), row(0, "first"), row(1, "second")],
        };
        assert_eq!(visible_screen(&update), "first\nsecond\nthird");
    }

    #[test]
    fn blank_leading_and_trailing_rows_are_trimmed() {
        let update = RenderUpdate {
            pane_id: 0,
            seqno: 1,
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
            dirty_rows: vec![row(0, ""), row(1, "content"), row(2, "")],
        };
        assert_eq!(visible_screen(&update), "content");
    }

    #[test]
    fn scrollback_preserves_order() {
        let data = ScrollbackData {
            pane_id: 0,
            start_row: -2,
            has_more: false,
            total_rows: 2,
            base: 0,
            rows: vec![row(0, "older"), row(0, "newer")],
        };
        assert_eq!(scrollback(&data), "older\nnewer");
    }
}
