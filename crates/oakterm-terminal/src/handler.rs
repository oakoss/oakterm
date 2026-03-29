//! Terminal handler — translates parsed VT sequences into Grid mutations.
//!
//! Implements `vte::ansi::Handler` to receive semantic callbacks from the
//! VT parser. Grid is the abstraction boundary: it uses our own types and
//! has no vte dependency. This handler is the only layer that knows about vte.

use crate::grid::Grid;
use crate::grid::cell::{self, CellFlags, WideState};

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

/// Saturating cast from usize to u16 (clamps at `u16::MAX` instead of truncating).
fn sat_u16(v: usize) -> u16 {
    u16::try_from(v).unwrap_or(u16::MAX)
}

/// Convert a vte color to our internal color representation.
fn convert_color(c: vte::ansi::Color) -> cell::Color {
    match c {
        vte::ansi::Color::Named(n) => convert_named_color(n),
        vte::ansi::Color::Spec(rgb) => cell::Color::Rgb(rgb.r, rgb.g, rgb.b),
        vte::ansi::Color::Indexed(i) => cell::Color::Indexed(i),
    }
}

/// Map vte's `NamedColor` to `cell::Color`. Standard palette entries (0-15)
/// become `Color::Named`; semantic entries (Foreground, Cursor, Dim*, etc.)
/// map to `Color::Default`.
fn convert_named_color(n: vte::ansi::NamedColor) -> cell::Color {
    use vte::ansi::NamedColor as N;
    match n {
        N::Black => cell::Color::Named(cell::NamedColor::Black),
        N::Red => cell::Color::Named(cell::NamedColor::Red),
        N::Green => cell::Color::Named(cell::NamedColor::Green),
        N::Yellow => cell::Color::Named(cell::NamedColor::Yellow),
        N::Blue => cell::Color::Named(cell::NamedColor::Blue),
        N::Magenta => cell::Color::Named(cell::NamedColor::Magenta),
        N::Cyan => cell::Color::Named(cell::NamedColor::Cyan),
        N::White => cell::Color::Named(cell::NamedColor::White),
        N::BrightBlack => cell::Color::Named(cell::NamedColor::BrightBlack),
        N::BrightRed => cell::Color::Named(cell::NamedColor::BrightRed),
        N::BrightGreen => cell::Color::Named(cell::NamedColor::BrightGreen),
        N::BrightYellow => cell::Color::Named(cell::NamedColor::BrightYellow),
        N::BrightBlue => cell::Color::Named(cell::NamedColor::BrightBlue),
        N::BrightMagenta => cell::Color::Named(cell::NamedColor::BrightMagenta),
        N::BrightCyan => cell::Color::Named(cell::NamedColor::BrightCyan),
        N::BrightWhite => cell::Color::Named(cell::NamedColor::BrightWhite),
        // Foreground, Background, BrightForeground, DimForeground, Cursor,
        // and Dim* are vte semantic values, not SGR palette indices.
        _ => cell::Color::Default,
    }
}

impl vte::ansi::Handler for Terminal<'_> {
    fn input(&mut self, c: char) {
        self.write_char(c);
    }

    fn terminal_attribute(&mut self, attr: vte::ansi::Attr) {
        use vte::ansi::Attr;
        match attr {
            Attr::Reset => {
                self.grid.current_attr = CellFlags::empty();
                self.grid.current_fg = cell::Color::Default;
                self.grid.current_bg = cell::Color::Default;
                self.grid.current_underline_style = cell::UnderlineStyle::None;
                self.grid.current_underline_color = None;
            }
            Attr::Bold => self.grid.current_attr.insert(CellFlags::BOLD),
            Attr::Dim => self.grid.current_attr.insert(CellFlags::DIM),
            Attr::Italic => self.grid.current_attr.insert(CellFlags::ITALIC),
            Attr::Underline => {
                self.grid.current_underline_style = cell::UnderlineStyle::Single;
            }
            Attr::DoubleUnderline => {
                self.grid.current_underline_style = cell::UnderlineStyle::Double;
            }
            Attr::Undercurl => {
                self.grid.current_underline_style = cell::UnderlineStyle::Curly;
            }
            Attr::DottedUnderline => {
                self.grid.current_underline_style = cell::UnderlineStyle::Dotted;
            }
            Attr::DashedUnderline => {
                self.grid.current_underline_style = cell::UnderlineStyle::Dashed;
            }
            Attr::BlinkSlow | Attr::BlinkFast => {
                self.grid.current_attr.insert(CellFlags::BLINK);
            }
            Attr::Reverse => self.grid.current_attr.insert(CellFlags::INVERSE),
            Attr::Hidden => self.grid.current_attr.insert(CellFlags::HIDDEN),
            Attr::Strike => self.grid.current_attr.insert(CellFlags::STRIKETHROUGH),
            Attr::CancelBold => self.grid.current_attr.remove(CellFlags::BOLD),
            Attr::CancelBoldDim => {
                self.grid.current_attr.remove(CellFlags::BOLD);
                self.grid.current_attr.remove(CellFlags::DIM);
            }
            Attr::CancelItalic => self.grid.current_attr.remove(CellFlags::ITALIC),
            Attr::CancelUnderline => {
                self.grid.current_underline_style = cell::UnderlineStyle::None;
            }
            Attr::CancelBlink => self.grid.current_attr.remove(CellFlags::BLINK),
            Attr::CancelReverse => self.grid.current_attr.remove(CellFlags::INVERSE),
            Attr::CancelHidden => self.grid.current_attr.remove(CellFlags::HIDDEN),
            Attr::CancelStrike => self.grid.current_attr.remove(CellFlags::STRIKETHROUGH),
            Attr::Foreground(c) => self.grid.current_fg = convert_color(c),
            Attr::Background(c) => self.grid.current_bg = convert_color(c),
            Attr::UnderlineColor(c) => {
                self.grid.current_underline_color = c.map(convert_color);
            }
        }
    }

    #[allow(clippy::cast_sign_loss)] // clamped to >= 0
    fn goto(&mut self, line: i32, col: usize) {
        let max_row = self.grid.rows.saturating_sub(1);
        let max_col = self.grid.cols.saturating_sub(1);
        let row = sat_u16(line.max(0) as usize).min(max_row);
        let col = sat_u16(col).min(max_col);
        self.grid.cursor.row = row;
        self.grid.cursor.col = col;
    }

    #[allow(clippy::cast_sign_loss)] // clamped to >= 0
    fn goto_line(&mut self, line: i32) {
        let max_row = self.grid.rows.saturating_sub(1);
        self.grid.cursor.row = sat_u16(line.max(0) as usize).min(max_row);
    }

    fn goto_col(&mut self, col: usize) {
        let max_col = self.grid.cols.saturating_sub(1);
        self.grid.cursor.col = sat_u16(col).min(max_col);
    }

    fn move_up(&mut self, count: usize) {
        self.grid.cursor.row = self.grid.cursor.row.saturating_sub(sat_u16(count));
    }

    fn move_down(&mut self, count: usize) {
        let max_row = self.grid.rows.saturating_sub(1);
        self.grid.cursor.row = self
            .grid
            .cursor
            .row
            .saturating_add(sat_u16(count))
            .min(max_row);
    }

    fn move_forward(&mut self, col: usize) {
        let max_col = self.grid.cols.saturating_sub(1);
        self.grid.cursor.col = self
            .grid
            .cursor
            .col
            .saturating_add(sat_u16(col))
            .min(max_col);
    }

    fn move_backward(&mut self, col: usize) {
        self.grid.cursor.col = self.grid.cursor.col.saturating_sub(sat_u16(col));
    }

    fn move_down_and_cr(&mut self, count: usize) {
        let max_row = self.grid.rows.saturating_sub(1);
        self.grid.cursor.row = self
            .grid
            .cursor
            .row
            .saturating_add(sat_u16(count))
            .min(max_row);
        self.grid.cursor.col = 0;
    }

    fn move_up_and_cr(&mut self, count: usize) {
        self.grid.cursor.row = self.grid.cursor.row.saturating_sub(sat_u16(count));
        self.grid.cursor.col = 0;
    }

    fn save_cursor_position(&mut self) {
        self.grid.saved_cursor = self.grid.cursor;
        self.grid.saved_attr = self.grid.current_attr;
        self.grid.saved_fg = self.grid.current_fg;
        self.grid.saved_bg = self.grid.current_bg;
        self.grid.saved_underline_style = self.grid.current_underline_style;
        self.grid.saved_underline_color = self.grid.current_underline_color;
    }

    fn restore_cursor_position(&mut self) {
        self.grid.cursor = self.grid.saved_cursor;
        self.grid.current_attr = self.grid.saved_attr;
        self.grid.current_fg = self.grid.saved_fg;
        self.grid.current_bg = self.grid.saved_bg;
        self.grid.current_underline_style = self.grid.saved_underline_style;
        self.grid.current_underline_color = self.grid.saved_underline_color;
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
    use crate::grid::cell::{CellFlags, Color, NamedColor, UnderlineStyle};
    use crate::testing::{
        assert_cell_fg, assert_cell_flags, assert_cursor_at, assert_row_text, test_grid,
    };

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

    // --- SGR attribute dispatch tests ---

    #[test]
    fn sgr_bold() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[1mX");
        assert_cell_flags(&grid, 0, 0, CellFlags::BOLD);
    }

    #[test]
    fn sgr_dim() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[2mX");
        assert_cell_flags(&grid, 0, 0, CellFlags::DIM);
    }

    #[test]
    fn sgr_italic() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[3mX");
        assert_cell_flags(&grid, 0, 0, CellFlags::ITALIC);
    }

    #[test]
    fn sgr_underline() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[4mX");
        assert_eq!(
            grid.lines[0].cells[0].underline_style,
            UnderlineStyle::Single
        );
    }

    #[test]
    fn sgr_blink() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[5mX");
        assert_cell_flags(&grid, 0, 0, CellFlags::BLINK);
    }

    #[test]
    fn sgr_inverse() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[7mX");
        assert_cell_flags(&grid, 0, 0, CellFlags::INVERSE);
    }

    #[test]
    fn sgr_hidden() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[8mX");
        assert_cell_flags(&grid, 0, 0, CellFlags::HIDDEN);
    }

    #[test]
    fn sgr_strikethrough() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[9mX");
        assert_cell_flags(&grid, 0, 0, CellFlags::STRIKETHROUGH);
    }

    #[test]
    fn sgr_named_fg_red() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[31mX");
        assert_cell_fg(&grid, 0, 0, Color::Named(NamedColor::Red));
    }

    #[test]
    fn sgr_named_fg_bright_cyan() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[96mX");
        assert_cell_fg(&grid, 0, 0, Color::Named(NamedColor::BrightCyan));
    }

    #[test]
    fn sgr_named_bg_green() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[42mX");
        assert_eq!(grid.lines[0].cells[0].bg, Color::Named(NamedColor::Green));
    }

    #[test]
    fn sgr_indexed_fg() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[38;5;208mX");
        assert_cell_fg(&grid, 0, 0, Color::Indexed(208));
    }

    #[test]
    fn sgr_indexed_bg() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[48;5;100mX");
        assert_eq!(grid.lines[0].cells[0].bg, Color::Indexed(100));
    }

    #[test]
    fn sgr_rgb_fg() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[38;2;255;128;0mX");
        assert_cell_fg(&grid, 0, 0, Color::Rgb(255, 128, 0));
    }

    #[test]
    fn sgr_rgb_bg() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[48;2;10;20;30mX");
        assert_eq!(grid.lines[0].cells[0].bg, Color::Rgb(10, 20, 30));
    }

    #[test]
    fn sgr_reset_clears_all() {
        let mut grid = test_grid(10, 1);
        // Set bold, red fg, green bg, underline, then reset.
        parse(&mut grid, b"\x1b[1;4;31;42m\x1b[0mX");
        let cell = &grid.lines[0].cells[0];
        assert_eq!(cell.flags, CellFlags::empty());
        assert_eq!(cell.fg, Color::Default);
        assert_eq!(cell.bg, Color::Default);
        assert_eq!(cell.underline_style, UnderlineStyle::None);
        assert_eq!(cell.underline_color, None);
    }

    #[test]
    fn sgr_cancel_bold_dim() {
        let mut grid = test_grid(10, 1);
        // SGR 22 cancels both bold and dim.
        parse(&mut grid, b"\x1b[1;2mA\x1b[22mB");
        assert_cell_flags(&grid, 0, 0, CellFlags::BOLD);
        assert_cell_flags(&grid, 0, 0, CellFlags::DIM);
        let cell_b = &grid.lines[0].cells[1];
        assert!(!cell_b.flags.contains(CellFlags::BOLD));
        assert!(!cell_b.flags.contains(CellFlags::DIM));
    }

    #[test]
    fn sgr_cancel_underline() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[4m\x1b[24mX");
        assert_eq!(grid.lines[0].cells[0].underline_style, UnderlineStyle::None);
    }

    #[test]
    fn sgr_double_underline() {
        let mut grid = test_grid(10, 1);
        // SGR 4:2 (colon sub-parameter, not semicolon).
        parse(&mut grid, b"\x1b[4:2mX");
        assert_eq!(
            grid.lines[0].cells[0].underline_style,
            UnderlineStyle::Double
        );
    }

    #[test]
    fn sgr_default_fg() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[31m\x1b[39mX");
        assert_cell_fg(&grid, 0, 0, Color::Default);
    }

    #[test]
    fn sgr_default_bg() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[42m\x1b[49mX");
        assert_eq!(grid.lines[0].cells[0].bg, Color::Default);
    }

    #[test]
    fn sgr_multiple_in_one_sequence() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[1;3;31mX");
        assert_cell_flags(&grid, 0, 0, CellFlags::BOLD);
        assert_cell_flags(&grid, 0, 0, CellFlags::ITALIC);
        assert_cell_fg(&grid, 0, 0, Color::Named(NamedColor::Red));
    }

    #[test]
    fn sgr_underline_color_rgb() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[58;2;255;0;128mX");
        assert_eq!(
            grid.lines[0].cells[0].underline_color,
            Some(Color::Rgb(255, 0, 128))
        );
    }

    #[test]
    fn sgr_underline_color_reset() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[58;2;255;0;128m\x1b[59mX");
        assert_eq!(grid.lines[0].cells[0].underline_color, None);
    }

    // SGR 53 (overline): vte's Attr enum lacks an Overline variant.
    // Needs custom csi_dispatch (TREK-33). Re-check when upgrading vte.

    // --- Cursor movement tests ---

    #[test]
    fn cup_goto() {
        let mut grid = test_grid(80, 24);
        // CSI 5;10 H — move to row 5, col 10 (1-based in VT, 0-based internally).
        parse(&mut grid, b"\x1b[5;10H");
        assert_cursor_at(&grid, 4, 9);
    }

    #[test]
    fn cup_goto_default_is_home() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"hello\x1b[H");
        assert_cursor_at(&grid, 0, 0);
    }

    #[test]
    fn cup_goto_clamps_to_grid() {
        let mut grid = test_grid(10, 5);
        parse(&mut grid, b"\x1b[100;100H");
        assert_cursor_at(&grid, 4, 9);
    }

    #[test]
    fn vpa_goto_line() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[5G\x1b[10d");
        // VPA moves to row 10 (1-based), col stays at 4 (from CHA).
        assert_cursor_at(&grid, 9, 4);
    }

    #[test]
    fn cha_goto_col() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[20G");
        assert_cursor_at(&grid, 0, 19);
    }

    #[test]
    fn cuu_move_up() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[5;1H\x1b[2A");
        assert_cursor_at(&grid, 2, 0);
    }

    #[test]
    fn cuu_move_up_clamps_at_top() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[2;1H\x1b[10A");
        assert_cursor_at(&grid, 0, 0);
    }

    #[test]
    fn cud_move_down() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[2B");
        assert_cursor_at(&grid, 2, 0);
    }

    #[test]
    fn cud_move_down_clamps_at_bottom() {
        let mut grid = test_grid(80, 5);
        parse(&mut grid, b"\x1b[100B");
        assert_cursor_at(&grid, 4, 0);
    }

    #[test]
    fn cuf_move_forward() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[5C");
        assert_cursor_at(&grid, 0, 5);
    }

    #[test]
    fn cub_move_backward() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[10G\x1b[3D");
        assert_cursor_at(&grid, 0, 6);
    }

    #[test]
    fn cub_move_backward_clamps_at_zero() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[3G\x1b[100D");
        assert_cursor_at(&grid, 0, 0);
    }

    #[test]
    fn cnl_move_down_and_cr() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[10G\x1b[3E");
        assert_cursor_at(&grid, 3, 0);
    }

    #[test]
    fn cpl_move_up_and_cr() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[5;10H\x1b[2F");
        assert_cursor_at(&grid, 2, 0);
    }

    #[test]
    fn cuf_move_forward_clamps_at_right() {
        let mut grid = test_grid(10, 5);
        parse(&mut grid, b"\x1b[100C");
        assert_cursor_at(&grid, 0, 9);
    }

    #[test]
    fn vpa_goto_line_clamps_to_bottom() {
        let mut grid = test_grid(80, 5);
        parse(&mut grid, b"\x1b[100d");
        assert_cursor_at(&grid, 4, 0);
    }

    #[test]
    fn cha_goto_col_clamps_to_right() {
        let mut grid = test_grid(10, 5);
        parse(&mut grid, b"\x1b[100G");
        assert_cursor_at(&grid, 0, 9);
    }

    #[test]
    fn decsc_decrc_save_restore() {
        let mut grid = test_grid(80, 24);
        // Move to (3,7), set bold+red, save cursor.
        parse(&mut grid, b"\x1b[4;8H\x1b[1;31m\x1b7");
        // Move elsewhere, change attrs.
        parse(&mut grid, b"\x1b[1;1H\x1b[0m");
        assert_cursor_at(&grid, 0, 0);
        // Restore cursor.
        parse(&mut grid, b"\x1b8");
        assert_cursor_at(&grid, 3, 7);
        // Attrs should be restored too.
        assert!(grid.current_attr.contains(CellFlags::BOLD));
        assert_eq!(grid.current_fg, Color::Named(NamedColor::Red));
    }

    #[test]
    fn cnl_clamps_at_bottom() {
        let mut grid = test_grid(80, 5);
        parse(&mut grid, b"\x1b[10G\x1b[100E");
        assert_cursor_at(&grid, 4, 0);
    }

    #[test]
    fn cpl_clamps_at_top() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[2;10H\x1b[100F");
        assert_cursor_at(&grid, 0, 0);
    }

    #[test]
    fn decrc_without_save_restores_defaults() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[5;10H\x1b[1;31m");
        parse(&mut grid, b"\x1b8");
        assert_cursor_at(&grid, 0, 0);
        assert_eq!(grid.current_attr, CellFlags::empty());
        assert_eq!(grid.current_fg, Color::Default);
    }

    #[test]
    fn decsc_decrc_restores_underline() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[4:3m\x1b7");
        parse(&mut grid, b"\x1b[0m");
        assert_eq!(grid.current_underline_style, UnderlineStyle::None);
        parse(&mut grid, b"\x1b8");
        assert_eq!(grid.current_underline_style, UnderlineStyle::Curly);
    }

    #[test]
    fn decsc_decrc_restores_bg() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[42m\x1b7");
        parse(&mut grid, b"\x1b[0m");
        assert_eq!(grid.current_bg, Color::Default);
        parse(&mut grid, b"\x1b8");
        assert_eq!(grid.current_bg, Color::Named(NamedColor::Green));
    }
}
