//! Terminal handler — translates parsed VT sequences into Grid mutations.
//!
//! Implements `vte::ansi::Handler` to receive semantic callbacks from the
//! VT parser. Grid is the abstraction boundary: it uses our own types and
//! has no vte dependency. This handler is the only layer that knows about vte.

use crate::grid::Grid;
use crate::grid::cell::WideState;

/// Terminal state wrapper that implements `vte::ansi::Handler`.
/// Grid is the vte-free contract; Terminal bridges vte's types to Grid's API.
pub struct Terminal<'a> {
    grid: &'a mut Grid,
}

impl<'a> Terminal<'a> {
    pub fn new(grid: &'a mut Grid) -> Self {
        Self { grid }
    }

    fn write_char(&mut self, c: char) {
        let cols = self.grid.cols;

        if self.grid.cursor.col >= cols {
            let row = self.grid.cursor.row as usize;
            if let Some(line) = self.grid.lines.get_mut(row) {
                line.flags.set_wrapped(true);
            }
            self.grid.cursor.col = 0;
            self.do_linefeed();

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

    fn do_linefeed(&mut self) {
        let bottom = self
            .grid
            .scroll_region
            .map_or(self.grid.rows - 1, |r| r.bottom);

        if self.grid.cursor.row >= bottom {
            self.do_scroll_up(1);
        } else {
            self.grid.cursor.row += 1;
        }
    }

    fn do_scroll_up(&mut self, count: usize) {
        let top = self.grid.scroll_region.map_or(0, |r| r.top) as usize;
        let bottom = self
            .grid
            .scroll_region
            .map_or(self.grid.rows - 1, |r| r.bottom) as usize;

        let count = count.min(bottom - top + 1);

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

    #[allow(clippy::cast_possible_truncation)] // cols is u16, indices fit
    fn do_tab(&mut self) {
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

impl vte::ansi::Handler for Terminal<'_> {
    fn input(&mut self, c: char) {
        self.write_char(c);
    }

    fn backspace(&mut self) {
        if self.grid.cursor.col > 0 {
            self.grid.cursor.col -= 1;
        }
    }

    fn carriage_return(&mut self) {
        self.grid.cursor.col = 0;
    }

    fn linefeed(&mut self) {
        self.do_linefeed();
    }

    fn put_tab(&mut self, count: u16) {
        for _ in 0..count {
            self.do_tab();
        }
    }

    fn scroll_up(&mut self, count: usize) {
        self.do_scroll_up(count);
    }
}

/// Feed bytes through the vte parser into a Terminal handler.
pub fn process_bytes(grid: &mut Grid, input: &[u8]) {
    let mut processor = vte::ansi::Processor::<vte::ansi::StdSyncHandler>::new();
    let mut terminal = Terminal::new(grid);
    processor.advance(&mut terminal, input);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Grid;
    use crate::grid::cell::{CellFlags, Color, NamedColor};
    use crate::testing::{assert_cursor_at, assert_row_text, test_grid};

    fn parse(grid: &mut Grid, input: &[u8]) {
        process_bytes(grid, input);
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
