//! Terminal handler — translates parsed VT sequences into Grid mutations.
//!
//! Implements `vte::Perform` to receive callbacks from the VT parser state
//! machine. Phase 0.1: ground state (print) and C0 controls (BS, CR, LF, HT).
//! CSI/OSC/DCS/APC dispatch to no-ops.

use crate::grid::Grid;
use crate::grid::cell::WideState;

/// C0 control codes.
const BS: u8 = 0x08;
const HT: u8 = 0x09;
const LF: u8 = 0x0A;
const VT: u8 = 0x0B;
const FF: u8 = 0x0C;
const CR: u8 = 0x0D;

/// Terminal handler that mutates a Grid based on parsed VT sequences.
pub struct Handler<'a> {
    grid: &'a mut Grid,
}

impl<'a> Handler<'a> {
    pub fn new(grid: &'a mut Grid) -> Self {
        Self { grid }
    }

    /// Write a character at the cursor position and advance.
    fn write_char(&mut self, c: char) {
        let cols = self.grid.cols;

        // Auto-wrap: cursor past last column wraps to next line.
        if self.grid.cursor.col >= cols {
            let row = self.grid.cursor.row as usize;
            if let Some(line) = self.grid.lines.get_mut(row) {
                line.flags.set_wrapped(true);
            }
            self.grid.cursor.col = 0;
            self.linefeed();

            // Mark the continuation line.
            let new_row = self.grid.cursor.row as usize;
            if let Some(line) = self.grid.lines.get_mut(new_row) {
                line.flags.set_wrap_continuation(true);
            }
        }

        let row = self.grid.cursor.row as usize;
        let col = self.grid.cursor.col as usize;

        if let Some(line) = self.grid.lines.get_mut(row) {
            if let Some(cell) = line.cells.get_mut(col) {
                cell.codepoint = c;
                cell.fg = self.grid.current_fg;
                cell.bg = self.grid.current_bg;
                cell.flags = self.grid.current_attr;
                cell.underline_style = self.grid.current_underline_style;
                cell.underline_color = self.grid.current_underline_color;
                cell.wide = WideState::Narrow;
                cell.extra_codepoints.clear();
                cell.hyperlink = None;

                if cell.has_style() {
                    line.flags.mark_has_styles();
                }
            }

            let seqno = self.grid.next_seqno();
            self.grid.lines[row].seqno = seqno;
        }

        self.grid.cursor.col += 1;
    }

    /// Move cursor down one line, scrolling if at the bottom of the scroll region.
    fn linefeed(&mut self) {
        let bottom = self
            .grid
            .scroll_region
            .map_or(self.grid.rows - 1, |r| r.bottom);

        if self.grid.cursor.row >= bottom {
            self.scroll_up(1);
        } else {
            self.grid.cursor.row += 1;
        }
    }

    /// Scroll the scroll region up by `count` lines.
    fn scroll_up(&mut self, count: u16) {
        let top = self.grid.scroll_region.map_or(0, |r| r.top) as usize;
        let bottom = self
            .grid
            .scroll_region
            .map_or(self.grid.rows - 1, |r| r.bottom) as usize;

        let count = (count as usize).min(bottom - top + 1);

        // Rotate the scroll region and reinitialize the vacated rows.
        self.grid.lines[top..=bottom].rotate_left(count);
        let cols = self.grid.cols as usize;
        for row in &mut self.grid.lines[(bottom + 1 - count)..=bottom] {
            *row = crate::grid::row::Row::new(cols);
        }

        let seqno = self.grid.next_seqno();
        for row in &mut self.grid.lines[top..=bottom] {
            row.seqno = seqno;
        }
    }

    /// Move to the next tab stop (every 8 columns by default).
    #[allow(clippy::cast_possible_truncation)] // cols is u16, so indices fit
    fn horizontal_tab(&mut self) {
        let col = self.grid.cursor.col as usize;
        let cols = self.grid.cols as usize;

        for i in (col + 1)..cols {
            if self.grid.tab_stops[i] {
                self.grid.cursor.col = i as u16;
                return;
            }
        }
        self.grid.cursor.col = (cols - 1) as u16;
    }
}

impl vte::Perform for Handler<'_> {
    fn print(&mut self, c: char) {
        self.write_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            BS => {
                if self.grid.cursor.col > 0 {
                    self.grid.cursor.col -= 1;
                }
            }
            HT => self.horizontal_tab(),
            LF | VT | FF => self.linefeed(),
            CR => self.grid.cursor.col = 0,
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        _params: &vte::Params,
        _intermediates: &[u8],
        _ignore: bool,
        _action: char,
    ) {
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {}

    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {}

    fn hook(&mut self, _params: &vte::Params, _intermediates: &[u8], _ignore: bool, _action: char) {
    }

    fn put(&mut self, _byte: u8) {}

    fn unhook(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Grid;
    use crate::grid::cell::{CellFlags, Color, NamedColor};
    use crate::testing::{assert_cursor_at, assert_row_text, test_grid};

    fn parse(grid: &mut Grid, input: &[u8]) {
        let mut parser = vte::Parser::new();
        let mut handler = Handler::new(grid);
        parser.advance(&mut handler, input);
    }

    #[test]
    fn print_ascii() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"hello");
        assert_row_text(&grid, 0, "hello");
        assert_cursor_at(&grid, 0, 5);
    }

    #[test]
    fn print_marks_dirty() {
        let mut grid = test_grid(80, 24);
        let before = grid.seqno;
        parse(&mut grid, b"x");
        assert_eq!(grid.dirty_rows(before), vec![0]);
    }

    #[test]
    fn cr_returns_to_column_zero() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"hello\rworld");
        assert_row_text(&grid, 0, "world");
        assert_cursor_at(&grid, 0, 5);
    }

    #[test]
    fn lf_moves_down() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"line1\r\nline2");
        assert_row_text(&grid, 0, "line1");
        assert_row_text(&grid, 1, "line2");
    }

    #[test]
    fn bare_lf_preserves_column() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"abc\ndef");
        assert_row_text(&grid, 0, "abc");
        assert_row_text(&grid, 1, "   def");
        assert_cursor_at(&grid, 1, 6);
    }

    #[test]
    fn crlf_moves_to_start_of_next_line() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"aaa\r\nbbb");
        assert_row_text(&grid, 0, "aaa");
        assert_row_text(&grid, 1, "bbb");
        assert_cursor_at(&grid, 1, 3);
    }

    #[test]
    fn bs_moves_back() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"abc\x08x");
        assert_row_text(&grid, 0, "abx");
    }

    #[test]
    fn bs_at_column_zero_stays() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x08x");
        assert_row_text(&grid, 0, "x");
        assert_cursor_at(&grid, 0, 1);
    }

    #[test]
    fn ht_advances_to_tab_stop() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"a\tb");
        assert_cursor_at(&grid, 0, 9);
        assert_row_text(&grid, 0, "a       b");
    }

    #[test]
    fn auto_wrap_at_right_margin() {
        let mut grid = test_grid(5, 3);
        parse(&mut grid, b"12345x");
        assert_row_text(&grid, 0, "12345");
        assert_row_text(&grid, 1, "x");
        assert_cursor_at(&grid, 1, 1);
        assert!(grid.lines[0].flags.wrapped());
        assert!(grid.lines[1].flags.wrap_continuation());
    }

    #[test]
    fn scroll_at_bottom() {
        let mut grid = test_grid(10, 3);
        parse(&mut grid, b"line1\r\nline2\r\nline3\r\nline4");
        assert_row_text(&grid, 0, "line2");
        assert_row_text(&grid, 1, "line3");
        assert_row_text(&grid, 2, "line4");
    }

    #[test]
    fn applies_current_attributes() {
        let mut grid = test_grid(10, 1);
        grid.current_fg = Color::Named(NamedColor::Red);
        grid.current_attr = CellFlags::BOLD;
        parse(&mut grid, b"hi");
        assert_eq!(grid.lines[0].cells[0].fg, Color::Named(NamedColor::Red));
        assert!(grid.lines[0].cells[0].flags.contains(CellFlags::BOLD));
    }
}
