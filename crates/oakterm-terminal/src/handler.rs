//! Terminal handler — translates parsed VT sequences into Grid mutations.
//!
//! Implements `vte::ansi::Handler` to receive semantic callbacks from the
//! VT parser. Grid is the abstraction boundary: it uses our own types and
//! has no vte dependency. This handler is the only layer that knows about vte.
//!
//! The handler is generic over `TermTarget`: tests pass a bare `Grid`,
//! while the daemon passes a `ScreenSet` for alternate screen support.

use crate::grid::cell::{self, CellFlags, WideState};
use crate::grid::row::Row;
use crate::grid::{Grid, ScreenId, ScreenSet};

/// Abstraction over `Grid` and `ScreenSet` for the handler.
///
/// `enter_alternate` / `exit_alternate` may change which grid
/// `active_grid_mut()` returns. Implementors supporting multiple screens
/// must override both switch methods. Default no-ops exist for
/// single-screen contexts (tests).
pub trait TermTarget {
    fn active_grid_mut(&mut self) -> &mut Grid;
    /// Mode flags from the primary grid. Alt-screen modes (47/1047/1049)
    /// are stored on the primary grid before switching, so DECRPM queries
    /// must read from primary to report them correctly.
    fn primary_mode_flags(&self) -> &crate::grid::ModeFlags;
    /// Session-scoped write-failure flag. Always reads from the primary
    /// grid so that switching between primary/alternate screens does not
    /// re-trigger the warning.
    fn writer_warned_mut(&mut self) -> &mut bool;
    fn enter_alternate(&mut self) {}
    fn exit_alternate(&mut self) {}
    /// Push rows that scrolled off the visible area into scrollback.
    /// Default no-op for single-grid test contexts.
    fn push_scrollback(&mut self, _rows: Vec<Row>) {}
    /// Full terminal reset (RIS). Resets to primary screen, drops alternate.
    fn reset(&mut self) {
        let g = self.active_grid_mut();
        let cols = g.cols;
        let rows = g.rows;
        *g = Grid::new(cols, rows);
    }
}

/// No-op screen switching. Alt-screen sequences (47/1047/1049) save, clear,
/// and restore all hit the same grid. Intentional for unit testing
/// non-screen-switching VT sequences.
impl TermTarget for Grid {
    fn active_grid_mut(&mut self) -> &mut Grid {
        self
    }
    fn primary_mode_flags(&self) -> &crate::grid::ModeFlags {
        &self.modes
    }
    fn writer_warned_mut(&mut self) -> &mut bool {
        &mut self.writer_warned
    }
}

impl TermTarget for ScreenSet {
    fn active_grid_mut(&mut self) -> &mut Grid {
        ScreenSet::active_grid_mut(self)
    }
    fn primary_mode_flags(&self) -> &crate::grid::ModeFlags {
        &self.primary().modes
    }
    fn writer_warned_mut(&mut self) -> &mut bool {
        &mut self.primary_mut().writer_warned
    }
    fn enter_alternate(&mut self) {
        ScreenSet::enter_alternate(self);
    }
    fn exit_alternate(&mut self) {
        ScreenSet::exit_alternate(self);
    }
    fn push_scrollback(&mut self, rows: Vec<Row>) {
        if self.active_screen() == ScreenId::Alternate && !self.save_alternate_scrollback() {
            return;
        }
        for row in rows {
            self.push_to_scrollback(row);
        }
    }
    fn reset(&mut self) {
        ScreenSet::reset(self);
    }
}

/// Terminal state wrapper that implements `vte::ansi::Handler`.
/// Generic over `TermTarget` so tests use bare `Grid` and the daemon uses `ScreenSet`.
/// Holds a writer for sending responses back to the PTY (DA1, DSR, etc.).
pub struct Terminal<'a, T: TermTarget, W: std::io::Write> {
    target: &'a mut T,
    writer: &'a mut W,
}

impl<'a, T: TermTarget, W: std::io::Write> Terminal<'a, T, W> {
    pub fn new(target: &'a mut T, writer: &'a mut W) -> Self {
        Self { target, writer }
    }

    /// Log a VT response write failure once at warn level per session.
    /// Subsequent failures drop to trace to avoid spam.
    fn warn_writer(&mut self, e: &std::io::Error, response: &str) {
        let warned = self.target.writer_warned_mut();
        if *warned {
            tracing::trace!(error = %e, response, "repeated VT response write failure");
        } else {
            *warned = true;
            tracing::warn!(error = %e, response, "failed to write VT response");
        }
    }
}

// --- Free functions for grid manipulation (avoids borrow checker conflicts) ---

fn write_char(g: &mut Grid, c: char) -> Vec<Row> {
    let c = map_charset(g, c);
    let cols = g.cols;
    let mut captured = Vec::new();

    if g.cursor.col >= cols {
        if g.modes.get(7) {
            // DECAWM: auto-wrap to next line.
            let row = g.cursor.row as usize;
            if let Some(line) = g.lines.get_mut(row) {
                line.flags.set_wrapped(true);
            }
            g.cursor.col = 0;
            captured = do_linefeed(g);

            let new_row = g.cursor.row as usize;
            if let Some(line) = g.lines.get_mut(new_row) {
                line.flags.set_wrap_continuation(true);
            }
        } else {
            // No wrap: overwrite the last column.
            g.cursor.col = cols - 1;
        }
    }

    // IRM (insert mode): shift existing cells right before writing.
    if g.modes.get(4) {
        do_insert_blank(g, 1);
    }

    let row = g.cursor.row as usize;
    let col = g.cursor.col as usize;

    if let Some(line) = g.lines.get_mut(row) {
        if let Some(cell) = line.cells.get_mut(col) {
            cell.codepoint = c;
            cell.fg = g.current_fg;
            cell.bg = g.current_bg;
            cell.flags = g.current_attr;
            cell.underline_style = g.current_underline_style;
            cell.set_underline_color(g.current_underline_color);
            cell.wide = WideState::Narrow;
            cell.clear_graphemes();
            cell.set_hyperlink(None);

            if cell.has_style() {
                line.flags.mark_has_styles();
            }
        }

        let seqno = g.next_seqno();
        g.lines[row].seqno = seqno;
    }

    g.cursor.col += 1;
    captured
}

fn do_linefeed(g: &mut Grid) -> Vec<Row> {
    let bottom = g.scroll_region.map_or(g.rows - 1, |r| r.bottom);
    let captured = if g.cursor.row >= bottom {
        do_scroll_up(g, 1)
    } else {
        g.cursor.row += 1;
        Vec::new()
    };
    // LNM (mode 20): LF implies CR.
    if g.modes.get(20) {
        g.cursor.col = 0;
    }
    captured
}

/// Scroll the grid up by `count` rows within the scroll region.
/// Returns the rows that scrolled off the top (only when scroll region
/// starts at row 0, i.e. full-screen scroll). Sub-region scrolls are
/// internal rearrangement and produce no scrollback.
fn do_scroll_up(g: &mut Grid, count: usize) -> Vec<Row> {
    let top = g.scroll_region.map_or(0, |r| r.top) as usize;
    let bottom = g.scroll_region.map_or(g.rows - 1, |r| r.bottom) as usize;
    let count = count.min(bottom - top + 1);

    g.lines[top..=bottom].rotate_left(count);
    let cols = g.cols as usize;
    let bg = g.current_bg;

    // After rotate_left, the old top rows sit at [bottom+1-count..=bottom].
    // Capture them before overwriting (only for full-screen scrolls).
    let mut captured = Vec::new();
    if top == 0 {
        captured.reserve(count);
        for row in &mut g.lines[(bottom + 1 - count)..=bottom] {
            captured.push(std::mem::replace(row, Row::new_with_bg(cols, bg)));
        }
    } else {
        for row in &mut g.lines[(bottom + 1 - count)..=bottom] {
            *row = Row::new_with_bg(cols, bg);
        }
    }

    let seqno = g.next_seqno();
    for row in &mut g.lines[top..=bottom] {
        row.seqno = seqno;
    }

    captured
}

fn do_scroll_down(g: &mut Grid, count: usize) {
    let top = g.scroll_region.map_or(0, |r| r.top) as usize;
    let bottom = g.scroll_region.map_or(g.rows - 1, |r| r.bottom) as usize;
    let count = count.min(bottom - top + 1);
    let cols = g.cols as usize;
    let bg = g.current_bg;

    g.lines[top..=bottom].rotate_right(count);
    for row in &mut g.lines[top..top + count] {
        *row = Row::new_with_bg(cols, bg);
    }

    let seqno = g.next_seqno();
    for row in &mut g.lines[top..=bottom] {
        row.seqno = seqno;
    }
}

fn do_insert_blank(g: &mut Grid, count: usize) {
    let row = g.cursor.row as usize;
    let col = g.cursor.col as usize;
    let cols = g.cols as usize;
    let bg = g.current_bg;
    if col >= cols {
        return;
    }
    let count = count.min(cols - col);
    if let Some(line) = g.lines.get_mut(row) {
        line.cells[col..].rotate_right(count);
        for cell in &mut line.cells[col..col + count] {
            cell.erase_with_bg(bg);
        }
    }
}

#[allow(clippy::cast_possible_truncation)] // cols is u16, indices fit
fn do_tab(g: &mut Grid) {
    let col = g.cursor.col as usize;
    let cols = g.cols as usize;
    for i in (col + 1)..cols {
        if g.tab_stops[i] {
            g.cursor.col = i as u16;
            return;
        }
    }
    g.cursor.col = (cols - 1) as u16;
}

/// Scroll region top and bottom rows. Returns (0, rows-1) when no region is set.
fn scroll_bounds(g: &Grid) -> (u16, u16) {
    let top = g.scroll_region.map_or(0, |r| r.top);
    let bottom = g
        .scroll_region
        .map_or(g.rows.saturating_sub(1), |r| r.bottom);
    (top, bottom)
}

fn save_cursor(g: &mut Grid) {
    g.saved_cursor = g.cursor;
    g.saved_attr = g.current_attr;
    g.saved_fg = g.current_fg;
    g.saved_bg = g.current_bg;
    g.saved_underline_style = g.current_underline_style;
    g.saved_underline_color = g.current_underline_color;
    g.saved_origin_mode = g.modes.get(6);
}

fn restore_cursor(g: &mut Grid) {
    g.cursor = g.saved_cursor;
    g.current_attr = g.saved_attr;
    g.current_fg = g.saved_fg;
    g.current_bg = g.saved_bg;
    g.current_underline_style = g.saved_underline_style;
    g.current_underline_color = g.saved_underline_color;
    g.modes.set(6, g.saved_origin_mode);

    // Clamp restored cursor to grid dimensions (resize may have shrunk).
    g.cursor.row = g.cursor.row.min(g.rows.saturating_sub(1));
    g.cursor.col = g.cursor.col.min(g.cols.saturating_sub(1));
    // Under DECOM, further clamp to the current scroll region.
    if g.saved_origin_mode {
        let (top, bottom) = scroll_bounds(g);
        g.cursor.row = g.cursor.row.clamp(top, bottom);
    }
}

fn clear_grid(g: &mut Grid) {
    let cols = g.cols as usize;
    let bg = g.current_bg;
    for line in &mut g.lines {
        *line = Row::new_with_bg(cols, bg);
    }
    g.touch_all();
}

// --- Utility functions ---

/// Map a character through the active charset, delegating to vte's table.
/// Extension point: when Lua config lands (Spec-0005), check user
/// overrides before falling back to vte.
fn map_charset(g: &Grid, c: char) -> char {
    use crate::grid::cursor::StandardCharset;
    let vte_cs = match g.charsets[g.active_charset as usize] {
        StandardCharset::Ascii => vte::ansi::StandardCharset::Ascii,
        StandardCharset::SpecialGraphics => {
            vte::ansi::StandardCharset::SpecialCharacterAndLineDrawing
        }
    };
    vte_cs.map(c)
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

// --- vte Handler implementation ---

impl<T: TermTarget, W: std::io::Write> vte::ansi::Handler for Terminal<'_, T, W> {
    fn input(&mut self, c: char) {
        let captured = write_char(self.target.active_grid_mut(), c);
        if !captured.is_empty() {
            self.target.push_scrollback(captured);
        }
    }

    fn terminal_attribute(&mut self, attr: vte::ansi::Attr) {
        use vte::ansi::Attr;
        let g = self.target.active_grid_mut();
        match attr {
            Attr::Reset => {
                g.current_attr = CellFlags::empty();
                g.current_fg = cell::Color::Default;
                g.current_bg = cell::Color::Default;
                g.current_underline_style = cell::UnderlineStyle::None;
                g.current_underline_color = None;
            }
            Attr::Bold => g.current_attr.insert(CellFlags::BOLD),
            Attr::Dim => g.current_attr.insert(CellFlags::DIM),
            Attr::Italic => g.current_attr.insert(CellFlags::ITALIC),
            Attr::Underline => g.current_underline_style = cell::UnderlineStyle::Single,
            Attr::DoubleUnderline => g.current_underline_style = cell::UnderlineStyle::Double,
            Attr::Undercurl => g.current_underline_style = cell::UnderlineStyle::Curly,
            Attr::DottedUnderline => g.current_underline_style = cell::UnderlineStyle::Dotted,
            Attr::DashedUnderline => g.current_underline_style = cell::UnderlineStyle::Dashed,
            Attr::BlinkSlow | Attr::BlinkFast => g.current_attr.insert(CellFlags::BLINK),
            Attr::Reverse => g.current_attr.insert(CellFlags::INVERSE),
            Attr::Hidden => g.current_attr.insert(CellFlags::HIDDEN),
            Attr::Strike => g.current_attr.insert(CellFlags::STRIKETHROUGH),
            Attr::CancelBold => g.current_attr.remove(CellFlags::BOLD),
            Attr::CancelBoldDim => {
                g.current_attr.remove(CellFlags::BOLD);
                g.current_attr.remove(CellFlags::DIM);
            }
            Attr::CancelItalic => g.current_attr.remove(CellFlags::ITALIC),
            Attr::CancelUnderline => g.current_underline_style = cell::UnderlineStyle::None,
            Attr::CancelBlink => g.current_attr.remove(CellFlags::BLINK),
            Attr::CancelReverse => g.current_attr.remove(CellFlags::INVERSE),
            Attr::CancelHidden => g.current_attr.remove(CellFlags::HIDDEN),
            Attr::CancelStrike => g.current_attr.remove(CellFlags::STRIKETHROUGH),
            Attr::Foreground(c) => g.current_fg = convert_color(c),
            Attr::Background(c) => g.current_bg = convert_color(c),
            Attr::UnderlineColor(c) => g.current_underline_color = c.map(convert_color),
        }
    }

    #[allow(clippy::cast_sign_loss)] // clamped to >= 0
    fn goto(&mut self, line: i32, col: usize) {
        let g = self.target.active_grid_mut();
        let max_col = g.cols.saturating_sub(1);
        if g.modes.get(6) {
            let (top, bottom) = scroll_bounds(g);
            let row = sat_u16(line.max(0) as usize).saturating_add(top);
            g.cursor.row = row.clamp(top, bottom);
        } else {
            let max_row = g.rows.saturating_sub(1);
            g.cursor.row = sat_u16(line.max(0) as usize).min(max_row);
        }
        g.cursor.col = sat_u16(col).min(max_col);
    }

    #[allow(clippy::cast_sign_loss)] // clamped to >= 0
    fn goto_line(&mut self, line: i32) {
        let g = self.target.active_grid_mut();
        if g.modes.get(6) {
            let (top, bottom) = scroll_bounds(g);
            let row = sat_u16(line.max(0) as usize).saturating_add(top);
            g.cursor.row = row.clamp(top, bottom);
        } else {
            let max_row = g.rows.saturating_sub(1);
            g.cursor.row = sat_u16(line.max(0) as usize).min(max_row);
        }
    }

    fn goto_col(&mut self, col: usize) {
        let g = self.target.active_grid_mut();
        let max_col = g.cols.saturating_sub(1);
        g.cursor.col = sat_u16(col).min(max_col);
    }

    fn move_up(&mut self, count: usize) {
        let g = self.target.active_grid_mut();
        let floor = if g.modes.get(6) {
            scroll_bounds(g).0
        } else {
            0
        };
        g.cursor.row = g.cursor.row.saturating_sub(sat_u16(count)).max(floor);
    }

    fn move_down(&mut self, count: usize) {
        let g = self.target.active_grid_mut();
        let ceiling = if g.modes.get(6) {
            scroll_bounds(g).1
        } else {
            g.rows.saturating_sub(1)
        };
        g.cursor.row = g.cursor.row.saturating_add(sat_u16(count)).min(ceiling);
    }

    fn move_forward(&mut self, col: usize) {
        let g = self.target.active_grid_mut();
        let max_col = g.cols.saturating_sub(1);
        g.cursor.col = g.cursor.col.saturating_add(sat_u16(col)).min(max_col);
    }

    fn move_backward(&mut self, col: usize) {
        let g = self.target.active_grid_mut();
        g.cursor.col = g.cursor.col.saturating_sub(sat_u16(col));
    }

    fn move_down_and_cr(&mut self, count: usize) {
        let g = self.target.active_grid_mut();
        let ceiling = if g.modes.get(6) {
            scroll_bounds(g).1
        } else {
            g.rows.saturating_sub(1)
        };
        g.cursor.row = g.cursor.row.saturating_add(sat_u16(count)).min(ceiling);
        g.cursor.col = 0;
    }

    fn move_up_and_cr(&mut self, count: usize) {
        let g = self.target.active_grid_mut();
        let floor = if g.modes.get(6) {
            scroll_bounds(g).0
        } else {
            0
        };
        g.cursor.row = g.cursor.row.saturating_sub(sat_u16(count)).max(floor);
        g.cursor.col = 0;
    }

    fn save_cursor_position(&mut self) {
        save_cursor(self.target.active_grid_mut());
    }

    fn restore_cursor_position(&mut self) {
        restore_cursor(self.target.active_grid_mut());
    }

    fn backspace(&mut self) {
        let g = self.target.active_grid_mut();
        if g.cursor.col > 0 {
            g.cursor.col -= 1;
        }
    }

    fn carriage_return(&mut self) {
        self.target.active_grid_mut().cursor.col = 0;
    }

    fn linefeed(&mut self) {
        let captured = do_linefeed(self.target.active_grid_mut());
        if !captured.is_empty() {
            self.target.push_scrollback(captured);
        }
    }

    fn put_tab(&mut self, count: u16) {
        for _ in 0..count {
            do_tab(self.target.active_grid_mut());
        }
    }

    fn scroll_up(&mut self, count: usize) {
        let captured = do_scroll_up(self.target.active_grid_mut(), count);
        if !captured.is_empty() {
            self.target.push_scrollback(captured);
        }
    }

    fn scroll_down(&mut self, count: usize) {
        do_scroll_down(self.target.active_grid_mut(), count);
    }

    fn clear_screen(&mut self, mode: vte::ansi::ClearMode) {
        let g = self.target.active_grid_mut();
        let row = g.cursor.row as usize;
        let col = g.cursor.col as usize;
        let cols = g.cols as usize;
        let rows = g.rows as usize;

        let bg = g.current_bg; // BCE: erased cells inherit current bg.
        match mode {
            vte::ansi::ClearMode::Below => {
                if let Some(line) = g.lines.get_mut(row) {
                    for cell in &mut line.cells[col..] {
                        cell.erase_with_bg(bg);
                    }
                }
                for line in &mut g.lines[row + 1..rows] {
                    *line = Row::new_with_bg(cols, bg);
                }
            }
            vte::ansi::ClearMode::Above => {
                for line in &mut g.lines[..row] {
                    *line = Row::new_with_bg(cols, bg);
                }
                if let Some(line) = g.lines.get_mut(row) {
                    for cell in &mut line.cells[..=col.min(cols - 1)] {
                        cell.erase_with_bg(bg);
                    }
                }
            }
            vte::ansi::ClearMode::All => {
                for line in &mut g.lines {
                    *line = Row::new_with_bg(cols, bg);
                }
            }
            vte::ansi::ClearMode::Saved => {}
        }
        g.touch_all();
    }

    fn clear_line(&mut self, mode: vte::ansi::LineClearMode) {
        let g = self.target.active_grid_mut();
        let row = g.cursor.row as usize;
        let col = g.cursor.col as usize;
        let cols = g.cols as usize;
        let bg = g.current_bg;

        let Some(line) = g.lines.get_mut(row) else {
            return;
        };
        match mode {
            vte::ansi::LineClearMode::Right => {
                for cell in &mut line.cells[col..] {
                    cell.erase_with_bg(bg);
                }
            }
            vte::ansi::LineClearMode::Left => {
                for cell in &mut line.cells[..=col.min(cols - 1)] {
                    cell.erase_with_bg(bg);
                }
            }
            vte::ansi::LineClearMode::All => {
                for cell in &mut line.cells {
                    cell.erase_with_bg(bg);
                }
            }
        }
        g.touch_row(g.cursor.row);
    }

    fn erase_chars(&mut self, count: usize) {
        let g = self.target.active_grid_mut();
        let row = g.cursor.row as usize;
        let col = g.cursor.col as usize;
        let cols = g.cols as usize;
        let bg = g.current_bg;
        let end = (col + count).min(cols);

        if let Some(line) = g.lines.get_mut(row) {
            for cell in &mut line.cells[col..end] {
                cell.erase_with_bg(bg);
            }
        }
        g.touch_row(g.cursor.row);
    }

    fn insert_blank(&mut self, count: usize) {
        let g = self.target.active_grid_mut();
        do_insert_blank(g, count);
        g.touch_row(g.cursor.row);
    }

    fn delete_chars(&mut self, count: usize) {
        let g = self.target.active_grid_mut();
        let row = g.cursor.row as usize;
        let col = g.cursor.col as usize;
        let cols = g.cols as usize;
        let bg = g.current_bg;
        let count = count.min(cols - col);

        if let Some(line) = g.lines.get_mut(row) {
            line.cells[col..].rotate_left(count);
            for cell in &mut line.cells[cols - count..] {
                cell.erase_with_bg(bg);
            }
        }
        g.touch_row(g.cursor.row);
    }

    fn insert_blank_lines(&mut self, count: usize) {
        let g = self.target.active_grid_mut();
        let region_top = g.scroll_region.map_or(0, |r| r.top) as usize;
        let top = g.cursor.row as usize;
        let bottom = g.scroll_region.map_or(g.rows - 1, |r| r.bottom) as usize;

        if top < region_top || top > bottom {
            return;
        }
        let count = count.min(bottom - top + 1);
        let cols = g.cols as usize;
        let bg = g.current_bg;

        g.lines[top..=bottom].rotate_right(count);
        for line in &mut g.lines[top..top + count] {
            *line = Row::new_with_bg(cols, bg);
        }

        let seqno = g.next_seqno();
        for line in &mut g.lines[top..=bottom] {
            line.seqno = seqno;
        }
    }

    /// DL deletes lines at cursor within the scroll region. Deleted rows
    /// are NOT captured to scrollback (matches xterm: only full-screen
    /// scroll-region scrolls via LF / `scroll_up` produce scrollback).
    fn delete_lines(&mut self, count: usize) {
        let g = self.target.active_grid_mut();
        let region_top = g.scroll_region.map_or(0, |r| r.top) as usize;
        let top = g.cursor.row as usize;
        let bottom = g.scroll_region.map_or(g.rows - 1, |r| r.bottom) as usize;

        if top < region_top || top > bottom {
            return;
        }
        let count = count.min(bottom - top + 1);
        let cols = g.cols as usize;
        let bg = g.current_bg;

        g.lines[top..=bottom].rotate_left(count);
        for line in &mut g.lines[(bottom + 1 - count)..=bottom] {
            *line = Row::new_with_bg(cols, bg);
        }

        let seqno = g.next_seqno();
        for line in &mut g.lines[top..=bottom] {
            line.seqno = seqno;
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        let g = self.target.active_grid_mut();
        // vte passes 1-based params; convert to 0-based.
        let max_row = g.rows.saturating_sub(1) as usize;
        let top = top.saturating_sub(1).min(max_row);
        let bottom = bottom.map_or(max_row, |b| b.saturating_sub(1).min(max_row));

        if top < bottom && (top > 0 || bottom < max_row) {
            g.scroll_region = Some(crate::grid::cursor::ScrollRegion {
                top: top as u16,
                bottom: bottom as u16,
            });
        } else {
            g.scroll_region = None;
        }
        // DECSTBM homes the cursor. Under DECOM, home is scroll_region.top.
        g.cursor.row = if g.modes.get(6) {
            g.scroll_region.map_or(0, |r| r.top)
        } else {
            0
        };
        g.cursor.col = 0;
    }

    fn reverse_index(&mut self) {
        let g = self.target.active_grid_mut();
        let top = g.scroll_region.map_or(0, |r| r.top);
        if g.cursor.row <= top {
            do_scroll_down(g, 1);
        } else {
            g.cursor.row -= 1;
        }
    }

    fn set_private_mode(&mut self, mode: vte::ansi::PrivateMode) {
        let num = mode.raw();
        match num {
            // Mode 47: switch to alternate (no cursor save, no clear).
            // No-op if already on alternate.
            47 if !self.target.active_grid_mut().modes.get(47) => {
                self.target.active_grid_mut().modes.set(47, true);
                self.target.enter_alternate();
                self.target.active_grid_mut().touch_all();
            }
            // Mode 1047: switch to alternate, clear it.
            // No-op if already on alternate.
            1047 if !self.target.active_grid_mut().modes.get(1047) => {
                self.target.active_grid_mut().modes.set(1047, true);
                self.target.enter_alternate();
                clear_grid(self.target.active_grid_mut());
            }
            // Mode 1049: save cursor on primary, switch, clear alternate.
            // Unconditionally saves cursor and clears even if already on alternate.
            1049 => {
                save_cursor(self.target.active_grid_mut());
                self.target.active_grid_mut().modes.set(1049, true);
                self.target.enter_alternate();
                clear_grid(self.target.active_grid_mut());
            }
            // DECOM: setting origin mode homes cursor to scroll region top.
            6 => {
                let g = self.target.active_grid_mut();
                g.modes.set(6, true);
                g.cursor.row = g.scroll_region.map_or(0, |r| r.top);
                g.cursor.col = 0;
            }
            25 => {
                let g = self.target.active_grid_mut();
                g.modes.set(num, true);
                g.cursor.visible = true;
            }
            _ => {
                self.target.active_grid_mut().modes.set(num, true);
            }
        }
    }

    fn unset_private_mode(&mut self, mode: vte::ansi::PrivateMode) {
        let num = mode.raw();
        match num {
            // Mode 47: switch back to primary.
            47 => {
                self.target.exit_alternate();
                // Clear flag on primary (where it was set).
                self.target.active_grid_mut().modes.set(47, false);
                self.target.active_grid_mut().touch_all();
            }
            // Mode 1047: clear alternate, switch back.
            1047 => {
                clear_grid(self.target.active_grid_mut());
                self.target.exit_alternate();
                self.target.active_grid_mut().modes.set(1047, false);
            }
            // Mode 1049: switch back, restore cursor.
            1049 => {
                self.target.exit_alternate();
                // Clear flag and restore cursor on primary (where they were saved).
                self.target.active_grid_mut().modes.set(1049, false);
                restore_cursor(self.target.active_grid_mut());
                self.target.active_grid_mut().touch_all();
            }
            // DECOM: resetting origin mode homes cursor to (0,0).
            6 => {
                let g = self.target.active_grid_mut();
                g.modes.set(6, false);
                g.cursor.row = 0;
                g.cursor.col = 0;
            }
            25 => {
                let g = self.target.active_grid_mut();
                g.modes.set(num, false);
                g.cursor.visible = false;
            }
            _ => {
                self.target.active_grid_mut().modes.set(num, false);
            }
        }
    }

    fn set_mode(&mut self, mode: vte::ansi::Mode) {
        self.target.active_grid_mut().modes.set(mode.raw(), true);
    }

    fn unset_mode(&mut self, mode: vte::ansi::Mode) {
        self.target.active_grid_mut().modes.set(mode.raw(), false);
    }

    fn set_active_charset(&mut self, index: vte::ansi::CharsetIndex) {
        let g = self.target.active_grid_mut();
        g.active_charset = match index {
            vte::ansi::CharsetIndex::G0 => crate::grid::cursor::CharsetIndex::G0,
            vte::ansi::CharsetIndex::G1 => crate::grid::cursor::CharsetIndex::G1,
            vte::ansi::CharsetIndex::G2 => crate::grid::cursor::CharsetIndex::G2,
            vte::ansi::CharsetIndex::G3 => crate::grid::cursor::CharsetIndex::G3,
        };
    }

    fn configure_charset(
        &mut self,
        index: vte::ansi::CharsetIndex,
        charset: vte::ansi::StandardCharset,
    ) {
        let idx = match index {
            vte::ansi::CharsetIndex::G0 => 0,
            vte::ansi::CharsetIndex::G1 => 1,
            vte::ansi::CharsetIndex::G2 => 2,
            vte::ansi::CharsetIndex::G3 => 3,
        };
        let cs = match charset {
            vte::ansi::StandardCharset::Ascii => crate::grid::cursor::StandardCharset::Ascii,
            vte::ansi::StandardCharset::SpecialCharacterAndLineDrawing => {
                crate::grid::cursor::StandardCharset::SpecialGraphics
            }
        };
        self.target.active_grid_mut().charsets[idx] = cs;
    }

    #[allow(clippy::cast_possible_truncation)] // tab stop index fits in u16
    fn move_backward_tabs(&mut self, count: u16) {
        let g = self.target.active_grid_mut();
        for _ in 0..count {
            let col = g.cursor.col as usize;
            if col == 0 {
                break;
            }
            for i in (0..col).rev() {
                if g.tab_stops[i] {
                    g.cursor.col = i as u16;
                    break;
                }
                if i == 0 {
                    g.cursor.col = 0;
                }
            }
        }
    }

    fn set_horizontal_tabstop(&mut self) {
        let g = self.target.active_grid_mut();
        let col = g.cursor.col as usize;
        if col < g.tab_stops.len() {
            g.tab_stops[col] = true;
        }
    }

    fn clear_tabs(&mut self, mode: vte::ansi::TabulationClearMode) {
        let g = self.target.active_grid_mut();
        match mode {
            vte::ansi::TabulationClearMode::Current => {
                let col = g.cursor.col as usize;
                if col < g.tab_stops.len() {
                    g.tab_stops[col] = false;
                }
            }
            vte::ansi::TabulationClearMode::All => {
                for stop in &mut g.tab_stops {
                    *stop = false;
                }
            }
        }
    }

    // newline() not overridden: vte dispatches ESC E (NEL) as
    // linefeed() + carriage_return(), never as newline().

    fn substitute(&mut self) {
        let captured = write_char(self.target.active_grid_mut(), '\u{FFFD}');
        if !captured.is_empty() {
            self.target.push_scrollback(captured);
        }
    }

    fn decaln(&mut self) {
        let g = self.target.active_grid_mut();
        let cols = g.cols as usize;
        for line in &mut g.lines {
            for cell in &mut line.cells[..cols] {
                cell.reset();
                cell.codepoint = 'E';
            }
        }
        g.cursor.row = 0;
        g.cursor.col = 0;
        g.scroll_region = None;
        g.touch_all();
    }

    fn push_title(&mut self) {
        let g = self.target.active_grid_mut();
        if g.title_stack.len() < crate::grid::TITLE_STACK_MAX {
            let current = g.title.clone().unwrap_or_default();
            g.title_stack.push(current);
        } else {
            tracing::debug!(
                max = crate::grid::TITLE_STACK_MAX,
                "title stack full, push ignored"
            );
        }
    }

    fn pop_title(&mut self) {
        let g = self.target.active_grid_mut();
        if let Some(title) = g.title_stack.pop() {
            g.title = if title.is_empty() { None } else { Some(title) };
            g.title_dirty = true;
        } else {
            tracing::trace!("pop_title on empty stack, no-op");
        }
    }

    fn set_color(&mut self, index: usize, color: vte::ansi::Rgb) {
        let g = self.target.active_grid_mut();
        let rgb = crate::grid::cell::Rgb {
            r: color.r,
            g: color.g,
            b: color.b,
        };
        match index {
            0..=255 => {
                g.palette[index] = rgb;
                g.touch_all();
            }
            256 => {
                g.dynamic_fg = Some(rgb);
                g.touch_all();
            }
            257 => {
                g.dynamic_bg = Some(rgb);
                g.touch_all();
            }
            258 => g.dynamic_cursor = Some(rgb),
            _ => {}
        }
    }

    fn reset_color(&mut self, index: usize) {
        let g = self.target.active_grid_mut();
        match index {
            0..=255 => {
                g.palette[index] = g.default_palette[index];
                g.touch_all();
            }
            256 => {
                g.dynamic_fg = None;
                g.touch_all();
            }
            257 => {
                g.dynamic_bg = None;
                g.touch_all();
            }
            258 => g.dynamic_cursor = None,
            _ => {}
        }
    }

    fn dynamic_color_sequence(&mut self, prefix: String, index: usize, terminator: &str) {
        let g = self.target.active_grid_mut();
        let color = match index {
            0..=255 => g.palette[index],
            256 => g.dynamic_fg.unwrap_or(crate::grid::cell::Rgb {
                r: 255,
                g: 255,
                b: 255,
            }),
            257 => g
                .dynamic_bg
                .unwrap_or(crate::grid::cell::Rgb { r: 0, g: 0, b: 0 }),
            258 => g.dynamic_cursor.unwrap_or(crate::grid::cell::Rgb {
                r: 255,
                g: 255,
                b: 255,
            }),
            _ => return,
        };
        // X11 rgb: format uses 16-bit per channel (8-bit value doubled).
        let r = u16::from(color.r) * 257;
        let green = u16::from(color.g) * 257;
        let b = u16::from(color.b) * 257;
        if let Err(e) = write!(
            self.writer,
            "\x1b]{prefix};rgb:{r:04x}/{green:04x}/{b:04x}{terminator}"
        ) {
            self.warn_writer(&e, "OSC color report");
        }
    }

    fn set_title(&mut self, title: Option<String>) {
        let g = self.target.active_grid_mut();
        g.title = title;
        g.title_dirty = true;
    }

    fn bell(&mut self) {
        self.target.active_grid_mut().bell_pending = true;
    }

    fn identify_terminal(&mut self, intermediate: Option<char>) {
        match intermediate {
            // DA1 (CSI c): report VT220 with ANSI color.
            None => {
                if let Err(e) = self.writer.write_all(b"\x1b[?62;22c") {
                    self.warn_writer(&e, "DA1");
                }
            }
            // DA2 (CSI > c): report version.
            Some('>') => {
                if let Err(e) = self.writer.write_all(b"\x1b[>0;0;0c") {
                    self.warn_writer(&e, "DA2");
                }
            }
            _ => {}
        }
    }

    fn device_status(&mut self, param: usize) {
        match param {
            5 => {
                // DSR: device status — report "OK".
                if let Err(e) = self.writer.write_all(b"\x1b[0n") {
                    self.warn_writer(&e, "DSR status");
                }
            }
            6 => {
                // DSR: cursor position report (1-based).
                // Under DECOM, row is relative to scroll region top.
                let g = self.target.active_grid_mut();
                let origin = if g.modes.get(6) {
                    g.scroll_region.map_or(0, |r| r.top)
                } else {
                    0
                };
                let row = g.cursor.row.saturating_sub(origin) + 1;
                let col = g.cursor.col + 1;
                if let Err(e) = write!(self.writer, "\x1b[{row};{col}R") {
                    self.warn_writer(&e, "DSR/CPR");
                }
            }
            _ => {}
        }
    }

    fn set_cursor_style(&mut self, style: Option<vte::ansi::CursorStyle>) {
        use crate::grid::cursor::CursorStyle as CS;
        let g = self.target.active_grid_mut();
        match style {
            Some(s) => {
                g.cursor.style = match (s.shape, s.blinking) {
                    (vte::ansi::CursorShape::Block, true) => CS::BlinkingBlock,
                    (vte::ansi::CursorShape::Underline, true) => CS::BlinkingUnderline,
                    (vte::ansi::CursorShape::Underline, false) => CS::SteadyUnderline,
                    (vte::ansi::CursorShape::Beam, true) => CS::BlinkingBar,
                    (vte::ansi::CursorShape::Beam, false) => CS::SteadyBar,
                    _ => CS::SteadyBlock,
                };
            }
            // DECSCUSR 0: reset to default.
            None => g.cursor.style = CS::BlinkingBlock,
        }
    }

    fn reset_state(&mut self) {
        self.target.reset();
    }

    fn set_keypad_application_mode(&mut self) {
        self.target.active_grid_mut().modes.set(66, true);
    }

    fn unset_keypad_application_mode(&mut self) {
        self.target.active_grid_mut().modes.set(66, false);
    }

    fn move_forward_tabs(&mut self, count: u16) {
        let g = self.target.active_grid_mut();
        let cols = g.cols as usize;
        for _ in 0..count {
            let start = g.cursor.col as usize + 1;
            if start >= cols {
                break;
            }
            if let Some(pos) = g.tab_stops[start..].iter().position(|&t| t) {
                g.cursor.col = u16::try_from(start + pos).unwrap_or(g.cols - 1);
            } else {
                g.cursor.col = g.cols - 1;
                break;
            }
        }
    }

    fn report_mode(&mut self, mode: vte::ansi::Mode) {
        // Only report modes we semantically handle. set_mode/unset_mode
        // store all mode numbers via catch-all, but storing a bit does not
        // mean the mode is implemented. Reporting 0 (not recognized) is
        // more accurate than reporting a stored-but-unimplemented flag.
        let num = mode.raw();
        let setting = match num {
            4 | 20 => {
                if self.target.active_grid_mut().modes.get(num) {
                    1
                } else {
                    2
                }
            }
            _ => 0, // not recognized
        };
        if let Err(e) = write!(self.writer, "\x1b[{num};{setting}$y") {
            self.warn_writer(&e, "DECRPM");
        }
    }

    fn report_private_mode(&mut self, mode: vte::ansi::PrivateMode) {
        let num = mode.raw();
        let setting = match num {
            // Alt-screen modes are stored on the primary grid.
            47 | 1047 | 1049 => {
                if self.target.primary_mode_flags().get(num) {
                    1
                } else {
                    2
                }
            }
            1 | 6 | 7 | 12 | 25 | 66 | 1000 | 1002 | 1003 | 1004 | 1005 | 1006 | 1007 | 2004
            | 2026 => {
                if self.target.active_grid_mut().modes.get(num) {
                    1
                } else {
                    2
                }
            }
            _ => 0, // not recognized
        };
        if let Err(e) = write!(self.writer, "\x1b[?{num};{setting}$y") {
            self.warn_writer(&e, "DECRPM private");
        }
    }

    fn text_area_size_chars(&mut self) {
        let g = self.target.active_grid_mut();
        if let Err(e) = write!(self.writer, "\x1b[8;{};{}t", g.rows, g.cols) {
            self.warn_writer(&e, "text area size");
        }
    }
}

/// Feed bytes through the vte parser into a Terminal handler.
/// `writer` receives responses to device queries (DA1, DSR, etc.).
pub fn process_bytes(target: &mut impl TermTarget, input: &[u8], writer: &mut impl std::io::Write) {
    let mut processor = vte::ansi::Processor::<vte::ansi::StdSyncHandler>::new();
    let mut terminal = Terminal::new(target, writer);
    processor.advance(&mut terminal, input);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Grid;
    use crate::grid::cell::{CellFlags, Color, NamedColor, UnderlineStyle};
    use crate::testing::{
        assert_cell_fg, assert_cell_flags, assert_cursor_at, assert_row_text, test_grid,
        test_screen,
    };

    fn parse(grid: &mut Grid, input: &[u8]) {
        process_bytes(grid, input, &mut std::io::sink());
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
        assert_eq!(cell.underline_color(), None);
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
            grid.lines[0].cells[0].underline_color(),
            Some(Color::Rgb(255, 0, 128))
        );
    }

    #[test]
    fn sgr_underline_color_reset() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[58;2;255;0;128m\x1b[59mX");
        assert_eq!(grid.lines[0].cells[0].underline_color(), None);
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

    // --- Grid editing tests ---

    #[test]
    fn ed_clear_below() {
        let mut grid = test_grid(10, 3);
        parse(&mut grid, b"aaaaaaaaaa\r\nbbbbbbbbbb\r\ncccccccccc");
        parse(&mut grid, b"\x1b[2;1H\x1b[0J");
        assert_row_text(&grid, 0, "aaaaaaaaaa");
        assert_row_text(&grid, 1, "");
        assert_row_text(&grid, 2, "");
    }

    #[test]
    fn ed_clear_above() {
        let mut grid = test_grid(10, 3);
        parse(&mut grid, b"aaaaaaaaaa\r\nbbbbbbbbbb\r\ncccccccccc");
        // Cursor at row 2, col 4 (0-indexed). Clears rows 0-1 and row 2 cols 0-4.
        parse(&mut grid, b"\x1b[3;5H\x1b[1J");
        assert_row_text(&grid, 0, "");
        assert_row_text(&grid, 1, "");
        assert_row_text(&grid, 2, "     ccccc");
    }

    #[test]
    fn ed_clear_all() {
        let mut grid = test_grid(10, 3);
        parse(&mut grid, b"aaaaaaaaaa\r\nbbbbbbbbbb\r\ncccccccccc");
        parse(&mut grid, b"\x1b[2J");
        assert_row_text(&grid, 0, "");
        assert_row_text(&grid, 1, "");
        assert_row_text(&grid, 2, "");
    }

    #[test]
    fn el_clear_right() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"abcdefghij");
        parse(&mut grid, b"\x1b[5G\x1b[0K");
        assert_row_text(&grid, 0, "abcd");
    }

    #[test]
    fn el_clear_left() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"abcdefghij");
        parse(&mut grid, b"\x1b[5G\x1b[1K");
        assert_row_text(&grid, 0, "     fghij");
    }

    #[test]
    fn el_clear_all() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"abcdefghij");
        parse(&mut grid, b"\x1b[5G\x1b[2K");
        assert_row_text(&grid, 0, "");
    }

    #[test]
    fn ech_erase_chars() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"abcdefghij");
        parse(&mut grid, b"\x1b[3G\x1b[4X");
        assert_row_text(&grid, 0, "ab    ghij");
    }

    #[test]
    fn ich_insert_blank_chars() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"abcdefghij");
        parse(&mut grid, b"\x1b[3G\x1b[2@");
        assert_row_text(&grid, 0, "ab  cdefgh");
    }

    #[test]
    fn dch_delete_chars() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"abcdefghij");
        parse(&mut grid, b"\x1b[3G\x1b[2P");
        assert_row_text(&grid, 0, "abefghij");
    }

    #[test]
    fn il_insert_blank_lines() {
        let mut grid = test_grid(10, 3);
        parse(&mut grid, b"aaa\r\nbbb\r\nccc");
        parse(&mut grid, b"\x1b[2;1H\x1b[1L");
        assert_row_text(&grid, 0, "aaa");
        assert_row_text(&grid, 1, "");
        assert_row_text(&grid, 2, "bbb");
    }

    #[test]
    fn dl_delete_lines() {
        let mut grid = test_grid(10, 3);
        parse(&mut grid, b"aaa\r\nbbb\r\nccc");
        parse(&mut grid, b"\x1b[2;1H\x1b[1M");
        assert_row_text(&grid, 0, "aaa");
        assert_row_text(&grid, 1, "ccc");
        assert_row_text(&grid, 2, "");
    }

    #[test]
    fn decstbm_set_scrolling_region() {
        let mut grid = test_grid(10, 5);
        parse(&mut grid, b"11111\r\n22222\r\n33333\r\n44444\r\n55555");
        // Set scroll region to rows 2-4 (1-based).
        parse(&mut grid, b"\x1b[2;4r");
        // Move to bottom of region and linefeed to scroll within region.
        parse(&mut grid, b"\x1b[4;1H\r\n");
        assert_row_text(&grid, 0, "11111");
        assert_row_text(&grid, 1, "33333");
        assert_row_text(&grid, 2, "44444");
        assert_row_text(&grid, 3, "");
        assert_row_text(&grid, 4, "55555");
    }

    #[test]
    fn scroll_down_inserts_at_top() {
        let mut grid = test_grid(10, 3);
        parse(&mut grid, b"aaa\r\nbbb\r\nccc");
        parse(&mut grid, b"\x1b[1T");
        assert_row_text(&grid, 0, "");
        assert_row_text(&grid, 1, "aaa");
        assert_row_text(&grid, 2, "bbb");
    }

    #[test]
    fn reverse_index_scrolls_down_at_top() {
        let mut grid = test_grid(10, 3);
        parse(&mut grid, b"aaa\r\nbbb\r\nccc");
        parse(&mut grid, b"\x1b[1;1H\x1bM");
        assert_row_text(&grid, 0, "");
        assert_row_text(&grid, 1, "aaa");
        assert_row_text(&grid, 2, "bbb");
        assert_cursor_at(&grid, 0, 0);
    }

    #[test]
    fn reverse_index_moves_up_without_scroll() {
        let mut grid = test_grid(10, 3);
        parse(&mut grid, b"aaa\r\nbbb\r\nccc");
        parse(&mut grid, b"\x1b[2;1H\x1bM");
        assert_cursor_at(&grid, 0, 0);
        assert_row_text(&grid, 0, "aaa");
    }

    #[test]
    fn il_within_scroll_region() {
        let mut grid = test_grid(10, 5);
        parse(&mut grid, b"11111\r\n22222\r\n33333\r\n44444\r\n55555");
        parse(&mut grid, b"\x1b[2;4r\x1b[2;1H\x1b[1L");
        assert_row_text(&grid, 0, "11111");
        assert_row_text(&grid, 1, "");
        assert_row_text(&grid, 2, "22222");
        assert_row_text(&grid, 3, "33333");
        assert_row_text(&grid, 4, "55555");
    }

    #[test]
    fn dl_within_scroll_region() {
        let mut grid = test_grid(10, 5);
        parse(&mut grid, b"11111\r\n22222\r\n33333\r\n44444\r\n55555");
        parse(&mut grid, b"\x1b[2;4r\x1b[2;1H\x1b[1M");
        assert_row_text(&grid, 0, "11111");
        assert_row_text(&grid, 1, "33333");
        assert_row_text(&grid, 2, "44444");
        assert_row_text(&grid, 3, "");
        assert_row_text(&grid, 4, "55555");
    }

    #[test]
    fn scroll_down_within_scroll_region() {
        let mut grid = test_grid(10, 5);
        parse(&mut grid, b"11111\r\n22222\r\n33333\r\n44444\r\n55555");
        parse(&mut grid, b"\x1b[2;4r\x1b[1T");
        assert_row_text(&grid, 0, "11111");
        assert_row_text(&grid, 1, "");
        assert_row_text(&grid, 2, "22222");
        assert_row_text(&grid, 3, "33333");
        assert_row_text(&grid, 4, "55555");
    }

    #[test]
    fn reverse_index_within_scroll_region() {
        let mut grid = test_grid(10, 5);
        parse(&mut grid, b"11111\r\n22222\r\n33333\r\n44444\r\n55555");
        parse(&mut grid, b"\x1b[2;4r\x1b[2;1H\x1bM");
        assert_row_text(&grid, 0, "11111");
        assert_row_text(&grid, 1, "");
        assert_row_text(&grid, 2, "22222");
        assert_row_text(&grid, 3, "33333");
        assert_row_text(&grid, 4, "55555");
    }

    #[test]
    fn scroll_up_explicit() {
        let mut grid = test_grid(10, 3);
        parse(&mut grid, b"aaa\r\nbbb\r\nccc");
        parse(&mut grid, b"\x1b[1S");
        assert_row_text(&grid, 0, "bbb");
        assert_row_text(&grid, 1, "ccc");
        assert_row_text(&grid, 2, "");
    }

    #[test]
    fn ech_clamps_at_end_of_line() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"abcdefghij");
        parse(&mut grid, b"\x1b[9G\x1b[100X");
        assert_row_text(&grid, 0, "abcdefgh");
    }

    #[test]
    fn decstbm_reset_clears_region() {
        let mut grid = test_grid(10, 5);
        parse(&mut grid, b"\x1b[2;4r");
        assert!(grid.scroll_region.is_some());
        parse(&mut grid, b"\x1b[r");
        assert!(grid.scroll_region.is_none());
        assert_cursor_at(&grid, 0, 0);
    }

    // --- Mode management tests ---

    #[test]
    fn decset_show_cursor() {
        let mut grid = test_grid(10, 1);
        // DECTCEM off then on.
        parse(&mut grid, b"\x1b[?25l");
        assert!(!grid.cursor.visible);
        parse(&mut grid, b"\x1b[?25h");
        assert!(grid.cursor.visible);
    }

    #[test]
    fn decset_autowrap_default_on() {
        let grid = test_grid(10, 1);
        assert!(grid.modes.get(7));
    }

    #[test]
    fn decset_autowrap_off_prevents_wrap() {
        let mut grid = test_grid(5, 2);
        parse(&mut grid, b"\x1b[?7l");
        parse(&mut grid, b"abcdefgh");
        // Without wrap, characters overwrite at the last column.
        assert_row_text(&grid, 0, "abcdh");
        assert_row_text(&grid, 1, "");
    }

    #[test]
    fn decset_autowrap_on_wraps() {
        let mut grid = test_grid(5, 2);
        // DECAWM is on by default.
        parse(&mut grid, b"abcdefgh");
        assert_row_text(&grid, 0, "abcde");
        assert_row_text(&grid, 1, "fgh");
    }

    #[test]
    fn decset_autowrap_toggle() {
        let mut grid = test_grid(5, 2);
        parse(&mut grid, b"\x1b[?7l");
        parse(&mut grid, b"abcdefg");
        assert_row_text(&grid, 0, "abcdg");
        // Re-enable wrap.
        parse(&mut grid, b"\x1b[?7h\r");
        parse(&mut grid, b"12345XY");
        assert_row_text(&grid, 0, "12345");
        assert_row_text(&grid, 1, "XY");
    }

    #[test]
    fn mode_irm_insert() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"abcdef");
        // Enable insert mode, move to col 2, type "XY".
        parse(&mut grid, b"\x1b[4h\x1b[3GXY");
        assert_row_text(&grid, 0, "abXYcdef");
    }

    #[test]
    fn mode_irm_toggle() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"abcdef");
        parse(&mut grid, b"\x1b[4h\x1b[3GXY");
        assert_row_text(&grid, 0, "abXYcdef");
        // Disable insert mode, overwrite.
        parse(&mut grid, b"\x1b[4l\x1b[3GZZ");
        assert_row_text(&grid, 0, "abZZcdef");
    }

    #[test]
    fn mode_lnm_auto_newline() {
        let mut grid = test_grid(10, 2);
        parse(&mut grid, b"\x1b[20h"); // LNM on: LF implies CR.
        parse(&mut grid, b"\x1b[5Gabc\ndef");
        assert_row_text(&grid, 0, "    abc");
        // With LNM, the LF should also CR, so "def" starts at col 0.
        assert_row_text(&grid, 1, "def");
    }

    #[test]
    fn mode_lnm_toggle() {
        let mut grid = test_grid(10, 3);
        parse(&mut grid, b"\x1b[20h\x1b[5Gabc\ndef");
        assert_row_text(&grid, 1, "def");
        // Disable LNM: bare LF preserves column (col 7 after "ghi").
        parse(&mut grid, b"\x1b[20l\x1b[5Gghi\njkl");
        assert_row_text(&grid, 2, "       jkl");
    }

    #[test]
    fn decset_stores_mode_flag() {
        let mut grid = test_grid(10, 1);
        assert!(!grid.modes.get(2004));
        parse(&mut grid, b"\x1b[?2004h");
        assert!(grid.modes.get(2004));
        parse(&mut grid, b"\x1b[?2004l");
        assert!(!grid.modes.get(2004));
    }

    #[test]
    fn decset_1049_stores_flag() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[?1049h");
        assert!(grid.modes.get(1049));
        parse(&mut grid, b"\x1b[?1049l");
        assert!(!grid.modes.get(1049));
    }

    // --- DECOM (origin mode) tests ---

    #[test]
    fn decom_goto_relative_to_region() {
        let mut grid = test_grid(80, 24);
        // Set scroll region rows 5-15 (1-based: 6-16), enable DECOM.
        parse(&mut grid, b"\x1b[6;16r\x1b[?6h");
        // CUP row 1 (1-based) should land at screen row 5 (0-based).
        parse(&mut grid, b"\x1b[1;1H");
        assert_cursor_at(&grid, 5, 0);
        // CUP row 3 should land at screen row 7.
        parse(&mut grid, b"\x1b[3;1H");
        assert_cursor_at(&grid, 7, 0);
    }

    #[test]
    fn decom_goto_clamps_to_region() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[6;16r\x1b[?6h");
        // CUP row 99 should clamp to region bottom (row 15).
        parse(&mut grid, b"\x1b[99;1H");
        assert_cursor_at(&grid, 15, 0);
    }

    #[test]
    fn decom_move_up_clamps_at_region_top() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[6;16r\x1b[?6h");
        // Position at row 5 (region top), move up 10 — should stay at 5.
        parse(&mut grid, b"\x1b[1;1H\x1b[10A");
        assert_cursor_at(&grid, 5, 0);
    }

    #[test]
    fn decom_move_down_clamps_at_region_bottom() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[6;16r\x1b[?6h");
        // Position at region top, move down 99 — should clamp to row 15.
        parse(&mut grid, b"\x1b[1;1H\x1b[99B");
        assert_cursor_at(&grid, 15, 0);
    }

    #[test]
    fn decom_set_homes_cursor() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[6;16r");
        grid.cursor.row = 10;
        grid.cursor.col = 5;
        // Enabling DECOM homes cursor to region top.
        parse(&mut grid, b"\x1b[?6h");
        assert_cursor_at(&grid, 5, 0);
    }

    #[test]
    fn decom_reset_homes_cursor() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[6;16r\x1b[?6h");
        grid.cursor.row = 10;
        grid.cursor.col = 5;
        // Disabling DECOM homes cursor to (0,0).
        parse(&mut grid, b"\x1b[?6l");
        assert_cursor_at(&grid, 0, 0);
    }

    #[test]
    fn decom_decstbm_homes_to_region() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[?6h");
        // Setting scroll region while DECOM is active homes to region top.
        parse(&mut grid, b"\x1b[11;20r");
        assert_cursor_at(&grid, 10, 0);
    }

    #[test]
    fn decom_cpr_reports_relative() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[6;16r\x1b[?6h");
        // CUP to row 3 in region (absolute row 7).
        parse(&mut grid, b"\x1b[3;5H");
        let mut out = Vec::new();
        process_bytes(&mut grid, b"\x1b[6n", &mut out);
        // CPR should report row 3, col 5 (region-relative, 1-based).
        assert_eq!(out, b"\x1b[3;5R");
    }

    #[test]
    fn decom_off_goto_is_absolute() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[6;16r");
        // DECOM off (default). CUP row 1 should be screen row 0.
        parse(&mut grid, b"\x1b[1;1H");
        assert_cursor_at(&grid, 0, 0);
    }

    #[test]
    fn decom_no_region_is_full_screen() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[?6h");
        // No scroll region = full screen. CUP row 1 = screen row 0.
        parse(&mut grid, b"\x1b[1;1H");
        assert_cursor_at(&grid, 0, 0);
        // CUP row 24 = screen row 23.
        parse(&mut grid, b"\x1b[24;1H");
        assert_cursor_at(&grid, 23, 0);
    }

    #[test]
    fn decom_save_restore_preserves_origin_mode() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[6;16r\x1b[?6h");
        assert!(grid.modes.get(6));
        // DECSC saves origin mode.
        parse(&mut grid, b"\x1b7");
        // Disable DECOM.
        parse(&mut grid, b"\x1b[?6l");
        assert!(!grid.modes.get(6));
        // DECRC restores origin mode.
        parse(&mut grid, b"\x1b8");
        assert!(grid.modes.get(6));
    }

    #[test]
    fn decom_goto_line_relative() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[6;16r\x1b[?6h");
        // VPA row 3 (1-based) should land at screen row 7.
        parse(&mut grid, b"\x1b[3d");
        assert_cursor_at(&grid, 7, 0);
    }

    #[test]
    fn decom_cnl_clamps_at_region_bottom() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[6;16r\x1b[?6h");
        parse(&mut grid, b"\x1b[1;5H"); // row 5(region), col 5
        // CNL 99 — clamp at region bottom, col to 0.
        parse(&mut grid, b"\x1b[99E");
        assert_cursor_at(&grid, 15, 0);
    }

    #[test]
    fn decom_cpl_clamps_at_region_top() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[6;16r\x1b[?6h");
        parse(&mut grid, b"\x1b[5;5H"); // row 5 in region
        // CPL 99 — clamp at region top, col to 0.
        parse(&mut grid, b"\x1b[99F");
        assert_cursor_at(&grid, 5, 0);
    }

    #[test]
    fn decom_restore_clamps_to_changed_region() {
        let mut grid = test_grid(80, 24);
        // Set region 5-15, enable DECOM, move to row 5, save.
        parse(&mut grid, b"\x1b[6;16r\x1b[?6h\x1b[1;1H\x1b7");
        assert_cursor_at(&grid, 5, 0);
        // Change region to 10-20, restore — cursor should clamp to new top.
        parse(&mut grid, b"\x1b[11;21r\x1b8");
        assert!(grid.cursor.row >= 10);
        assert!(grid.cursor.row <= 20);
    }

    // --- Alternate screen tests ---

    #[test]
    fn alt_screen_1049_preserves_primary() {
        let mut screen = test_screen(10, 3);
        process_bytes(&mut screen, b"primary", &mut std::io::sink());
        process_bytes(&mut screen, b"\x1b[?1049h", &mut std::io::sink());
        process_bytes(&mut screen, b"alternate", &mut std::io::sink());
        assert_row_text(screen.active_grid(), 0, "alternate");
        process_bytes(&mut screen, b"\x1b[?1049l", &mut std::io::sink());
        assert_row_text(screen.active_grid(), 0, "primary");
    }

    #[test]
    fn alt_screen_1049_saves_restores_cursor() {
        let mut screen = test_screen(10, 3);
        process_bytes(&mut screen, b"\x1b[2;5H", &mut std::io::sink());
        assert_eq!(screen.active_grid().cursor.row, 1);
        assert_eq!(screen.active_grid().cursor.col, 4);
        process_bytes(&mut screen, b"\x1b[?1049h", &mut std::io::sink());
        // Cursor on alternate starts at home after clear.
        assert_eq!(screen.active_grid().cursor.row, 0);
        assert_eq!(screen.active_grid().cursor.col, 0);
        process_bytes(&mut screen, b"\x1b[?1049l", &mut std::io::sink());
        // Cursor restored to primary position.
        assert_eq!(screen.active_grid().cursor.row, 1);
        assert_eq!(screen.active_grid().cursor.col, 4);
    }

    #[test]
    fn alt_screen_47_no_clear() {
        let mut screen = test_screen(10, 3);
        process_bytes(&mut screen, b"primary", &mut std::io::sink());
        process_bytes(&mut screen, b"\x1b[?47h", &mut std::io::sink());
        // Mode 47: no clear on enter (alternate was lazily allocated empty).
        process_bytes(&mut screen, b"alt", &mut std::io::sink());
        assert_row_text(screen.active_grid(), 0, "alt");
        process_bytes(&mut screen, b"\x1b[?47l", &mut std::io::sink());
        assert_row_text(screen.active_grid(), 0, "primary");
    }

    #[test]
    fn alt_screen_1047_clears_on_enter() {
        let mut screen = test_screen(10, 3);
        process_bytes(&mut screen, b"primary", &mut std::io::sink());
        process_bytes(&mut screen, b"\x1b[?1047h", &mut std::io::sink());
        // Mode 1047: cleared on enter.
        assert_row_text(screen.active_grid(), 0, "");
        process_bytes(&mut screen, b"\x1b[?1047l", &mut std::io::sink());
        assert_row_text(screen.active_grid(), 0, "primary");
    }

    // --- Device query and terminal state tests ---

    #[test]
    fn da1_response() {
        let mut grid = test_grid(10, 3);
        let mut response = Vec::new();
        process_bytes(&mut grid, b"\x1b[c", &mut response);
        assert_eq!(response, b"\x1b[?62;22c");
    }

    #[test]
    fn da2_response() {
        let mut grid = test_grid(10, 3);
        let mut response = Vec::new();
        process_bytes(&mut grid, b"\x1b[>c", &mut response);
        assert_eq!(response, b"\x1b[>0;0;0c");
    }

    #[test]
    fn dsr_cursor_position_report() {
        let mut grid = test_grid(80, 24);
        parse(&mut grid, b"\x1b[5;10H");
        let mut response = Vec::new();
        process_bytes(&mut grid, b"\x1b[6n", &mut response);
        // Cursor at (4,9) 0-based → (5,10) 1-based.
        assert_eq!(response, b"\x1b[5;10R");
    }

    #[test]
    fn set_cursor_style_bar() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[5 q");
        assert_eq!(
            grid.cursor.style,
            crate::grid::cursor::CursorStyle::BlinkingBar
        );
    }

    #[test]
    fn set_cursor_style_steady_block() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[2 q");
        assert_eq!(
            grid.cursor.style,
            crate::grid::cursor::CursorStyle::SteadyBlock
        );
    }

    #[test]
    fn set_cursor_style_reset() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b[5 q\x1b[0 q");
        assert_eq!(
            grid.cursor.style,
            crate::grid::cursor::CursorStyle::BlinkingBlock
        );
    }

    #[test]
    fn dsr_at_home_position() {
        let mut grid = test_grid(10, 3);
        let mut response = Vec::new();
        process_bytes(&mut grid, b"\x1b[6n", &mut response);
        assert_eq!(response, b"\x1b[1;1R");
    }

    #[test]
    fn reset_state_clears_grid() {
        let mut grid = test_grid(10, 3);
        parse(&mut grid, b"hello\x1b[1;31m\x1b[?2004h");
        parse(&mut grid, b"\x1bc");
        assert_row_text(&grid, 0, "");
        assert_eq!(grid.current_fg, Color::Default);
        assert_eq!(grid.current_attr, CellFlags::empty());
        assert_cursor_at(&grid, 0, 0);
        // Modes reset (2004 cleared, DECAWM restored to default on).
        assert!(!grid.modes.get(2004));
        assert!(grid.modes.get(7));
        assert_eq!(grid.cols, 10);
        assert_eq!(grid.rows, 3);
    }

    #[test]
    fn keypad_application_mode() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b=");
        assert!(grid.modes.get(66));
        parse(&mut grid, b"\x1b>");
        assert!(!grid.modes.get(66));
    }

    // --- Charset, tab, and misc tests ---

    #[test]
    fn line_drawing_charset() {
        let mut grid = test_grid(10, 1);
        // ESC ( 0 = configure G0 as line drawing, then print box chars.
        parse(&mut grid, b"\x1b(0lqqk");
        assert_eq!(grid.lines[0].cells[0].codepoint, '┌');
        assert_eq!(grid.lines[0].cells[1].codepoint, '─');
        assert_eq!(grid.lines[0].cells[2].codepoint, '─');
        assert_eq!(grid.lines[0].cells[3].codepoint, '┐');
    }

    #[test]
    fn charset_switch_back_to_ascii() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b(0q\x1b(Bq");
        assert_eq!(grid.lines[0].cells[0].codepoint, '─');
        assert_eq!(grid.lines[0].cells[1].codepoint, 'q');
    }

    #[test]
    fn backward_tab() {
        let mut grid = test_grid(80, 1);
        // Move to col 16 (past two tab stops), backward tab once.
        parse(&mut grid, b"\x1b[17G\x1b[Z");
        assert_cursor_at(&grid, 0, 8);
    }

    #[test]
    fn set_and_clear_tabstop() {
        let mut grid = test_grid(80, 1);
        // Move to col 5, set a tab stop, move to col 0, tab to it.
        parse(&mut grid, b"\x1b[6G\x1bH\x1b[1G\t");
        assert_cursor_at(&grid, 0, 5);
        // Clear the tab stop at col 5.
        parse(&mut grid, b"\x1b[6G\x1b[0g\x1b[1G\t");
        // Should skip to next default stop at col 8.
        assert_cursor_at(&grid, 0, 8);
    }

    #[test]
    fn clear_all_tabstops() {
        let mut grid = test_grid(80, 1);
        parse(&mut grid, b"\x1b[3g\t");
        // With all stops cleared, tab goes to last column.
        assert_cursor_at(&grid, 0, 79);
    }

    #[test]
    fn nel_cr_plus_lf() {
        let mut grid = test_grid(10, 2);
        parse(&mut grid, b"\x1b[5Gabc\x1bEdef");
        assert_row_text(&grid, 0, "    abc");
        // NEL = CR + LF, so "def" starts at col 0.
        assert_row_text(&grid, 1, "def");
    }

    #[test]
    fn substitute_prints_replacement() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"ab\x1acd");
        assert_eq!(grid.lines[0].cells[2].codepoint, '\u{FFFD}');
    }

    #[test]
    fn decaln_fills_with_e() {
        let mut grid = test_grid(5, 2);
        parse(&mut grid, b"\x1b#8");
        assert_row_text(&grid, 0, "EEEEE");
        assert_row_text(&grid, 1, "EEEEE");
    }

    // --- Palette and dynamic color tests ---

    #[test]
    fn set_palette_color() {
        let mut grid = test_grid(10, 1);
        // OSC 4;1;rgb:ff/00/00 ST — set palette index 1 to red.
        parse(&mut grid, b"\x1b]4;1;rgb:ff/00/00\x1b\\");
        assert_eq!(grid.palette[1].r, 255);
        assert_eq!(grid.palette[1].g, 0);
        assert_eq!(grid.palette[1].b, 0);
    }

    #[test]
    fn reset_palette_color() {
        let mut grid = test_grid(10, 1);
        let original = grid.palette[1];
        parse(&mut grid, b"\x1b]4;1;rgb:ff/00/00\x1b\\");
        assert_ne!(grid.palette[1], original);
        parse(&mut grid, b"\x1b]104;1\x1b\\");
        assert_eq!(grid.palette[1], original);
    }

    #[test]
    fn set_title_osc_2() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b]2;hello world\x1b\\");
        assert_eq!(grid.title.as_deref(), Some("hello world"));
    }

    // --- Title stack tests ---

    #[test]
    fn push_pop_title_round_trip() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b]2;original\x1b\\");
        grid.title_dirty = false;
        parse(&mut grid, b"\x1b[22;0t"); // push
        parse(&mut grid, b"\x1b]2;temporary\x1b\\");
        assert_eq!(grid.title.as_deref(), Some("temporary"));
        grid.title_dirty = false;
        parse(&mut grid, b"\x1b[23;0t"); // pop
        assert_eq!(grid.title.as_deref(), Some("original"));
        assert!(grid.title_dirty);
    }

    #[test]
    fn pop_empty_stack_is_noop() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b]2;keep me\x1b\\");
        grid.title_dirty = false;
        parse(&mut grid, b"\x1b[23;0t"); // pop on empty stack
        assert_eq!(grid.title.as_deref(), Some("keep me"));
        assert!(!grid.title_dirty);
    }

    #[test]
    fn push_pop_multiple_lifo() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b]2;first\x1b\\");
        parse(&mut grid, b"\x1b[22;0t"); // push "first"
        parse(&mut grid, b"\x1b]2;second\x1b\\");
        parse(&mut grid, b"\x1b[22;0t"); // push "second"
        parse(&mut grid, b"\x1b]2;third\x1b\\");
        parse(&mut grid, b"\x1b[22;0t"); // push "third"

        parse(&mut grid, b"\x1b[23;0t"); // pop → "third"
        assert_eq!(grid.title.as_deref(), Some("third"));
        parse(&mut grid, b"\x1b[23;0t"); // pop → "second"
        assert_eq!(grid.title.as_deref(), Some("second"));
        parse(&mut grid, b"\x1b[23;0t"); // pop → "first"
        assert_eq!(grid.title.as_deref(), Some("first"));
    }

    #[test]
    fn push_respects_depth_limit() {
        let mut grid = test_grid(10, 1);
        for i in 0..11 {
            parse(&mut grid, format!("\x1b]2;title{i}\x1b\\").as_bytes());
            parse(&mut grid, b"\x1b[22;0t"); // push
        }
        // Only 10 entries should be stored (11th push ignored).
        assert_eq!(grid.title_stack.len(), 10);

        // Pop all 10.
        for _ in 0..10 {
            parse(&mut grid, b"\x1b[23;0t");
        }
        assert!(grid.title_stack.is_empty());

        // 11th pop is a no-op.
        grid.title_dirty = false;
        parse(&mut grid, b"\x1b[23;0t");
        assert!(!grid.title_dirty);
    }

    #[test]
    fn reset_clears_title_stack() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b]2;saved\x1b\\");
        parse(&mut grid, b"\x1b[22;0t"); // push
        assert_eq!(grid.title_stack.len(), 1);

        parse(&mut grid, b"\x1bc"); // RIS
        assert!(grid.title_stack.is_empty());

        // Pop should be no-op.
        grid.title_dirty = false;
        parse(&mut grid, b"\x1b[23;0t");
        assert!(!grid.title_dirty);
    }

    #[test]
    fn push_none_title() {
        let mut grid = test_grid(10, 1);
        assert!(grid.title.is_none());
        parse(&mut grid, b"\x1b[22;0t"); // push None (stored as "")
        parse(&mut grid, b"\x1b]2;something\x1b\\");
        assert_eq!(grid.title.as_deref(), Some("something"));
        parse(&mut grid, b"\x1b[23;0t"); // pop → None
        assert!(grid.title.is_none());
    }

    // --- Forward tab, mode reporting, text area size tests ---

    #[test]
    fn move_forward_tabs() {
        let mut grid = test_grid(80, 1);
        // CSI 2 I — move forward 2 tab stops.
        parse(&mut grid, b"\x1b[2I");
        assert_cursor_at(&grid, 0, 16);
    }

    #[test]
    fn move_forward_tabs_at_last_col() {
        let mut grid = test_grid(80, 1);
        grid.cursor.col = 79;
        parse(&mut grid, b"\x1b[1I");
        assert_cursor_at(&grid, 0, 79); // no-op at last column
    }

    #[test]
    fn move_forward_tabs_past_last_stop() {
        let mut grid = test_grid(80, 1);
        grid.cursor.col = 72;
        parse(&mut grid, b"\x1b[1I"); // no tab stop between 72 and 79
        assert_cursor_at(&grid, 0, 79); // clamp to last column
    }

    #[test]
    fn report_mode_set() {
        let mut grid = test_grid(10, 1);
        let mut out = Vec::new();
        grid.modes.set(4, true); // IRM (insert mode)
        process_bytes(&mut grid, b"\x1b[4$p", &mut out);
        assert_eq!(out, b"\x1b[4;1$y");
    }

    #[test]
    fn report_mode_reset() {
        let mut grid = test_grid(10, 1);
        let mut out = Vec::new();
        // IRM (mode 4) is off by default.
        process_bytes(&mut grid, b"\x1b[4$p", &mut out);
        assert_eq!(out, b"\x1b[4;2$y");
    }

    #[test]
    fn report_private_mode_set() {
        let mut grid = test_grid(10, 1);
        let mut out = Vec::new();
        // DECAWM (mode 7) is on by default.
        process_bytes(&mut grid, b"\x1b[?7$p", &mut out);
        assert_eq!(out, b"\x1b[?7;1$y");
    }

    #[test]
    fn report_private_mode_reset() {
        let mut grid = test_grid(10, 1);
        let mut out = Vec::new();
        // Mode 25 (DECTCEM) is off by default.
        process_bytes(&mut grid, b"\x1b[?25$p", &mut out);
        assert_eq!(out, b"\x1b[?25;2$y");
    }

    #[test]
    fn report_private_mode_unrecognized() {
        let mut grid = test_grid(10, 1);
        let mut out = Vec::new();
        // Mode 9999 is not recognized — should report 0.
        process_bytes(&mut grid, b"\x1b[?9999$p", &mut out);
        assert_eq!(out, b"\x1b[?9999;0$y");
    }

    #[test]
    fn report_mode_unrecognized() {
        let mut grid = test_grid(10, 1);
        let mut out = Vec::new();
        // Mode 99 is not recognized — should report 0.
        process_bytes(&mut grid, b"\x1b[99$p", &mut out);
        assert_eq!(out, b"\x1b[99;0$y");
    }

    #[test]
    fn report_alt_screen_mode_from_alternate() {
        let mut screen = test_screen(10, 3);
        let mut out = Vec::new();
        // Enter alt screen via mode 1049.
        process_bytes(&mut screen, b"\x1b[?1049h", &mut out);
        out.clear();
        // Query mode 1049 while on alternate — should report 1 (set).
        process_bytes(&mut screen, b"\x1b[?1049$p", &mut out);
        assert_eq!(out, b"\x1b[?1049;1$y");
    }

    #[test]
    fn report_alt_screen_mode_47_from_alternate() {
        let mut screen = test_screen(10, 3);
        let mut out = Vec::new();
        process_bytes(&mut screen, b"\x1b[?47h", &mut out);
        out.clear();
        process_bytes(&mut screen, b"\x1b[?47$p", &mut out);
        assert_eq!(out, b"\x1b[?47;1$y");
    }

    #[test]
    fn report_alt_screen_mode_reset_after_exit() {
        let mut screen = test_screen(10, 3);
        let mut out = Vec::new();
        process_bytes(&mut screen, b"\x1b[?1049h", &mut out);
        process_bytes(&mut screen, b"\x1b[?1049l", &mut out);
        out.clear();
        process_bytes(&mut screen, b"\x1b[?1049$p", &mut out);
        assert_eq!(out, b"\x1b[?1049;2$y");
    }

    #[test]
    fn text_area_size_chars_report() {
        let mut grid = test_grid(80, 24);
        let mut out = Vec::new();
        process_bytes(&mut grid, b"\x1b[18t", &mut out);
        assert_eq!(out, b"\x1b[8;24;80t");
    }

    #[test]
    fn dynamic_bg_color() {
        let mut grid = test_grid(10, 1);
        // OSC 11;rgb:20/30/40 — set dynamic background.
        parse(&mut grid, b"\x1b]11;rgb:20/30/40\x1b\\");
        assert_eq!(
            grid.dynamic_bg,
            Some(crate::grid::cell::Rgb {
                r: 0x20,
                g: 0x30,
                b: 0x40,
            })
        );
    }

    #[test]
    fn reset_dynamic_bg() {
        let mut grid = test_grid(10, 1);
        parse(&mut grid, b"\x1b]11;rgb:20/30/40\x1b\\");
        assert!(grid.dynamic_bg.is_some());
        parse(&mut grid, b"\x1b]111\x1b\\");
        assert!(grid.dynamic_bg.is_none());
    }

    // --- Mode 1007 (alternateScroll) tests ---

    #[test]
    fn mode_1007_default_on() {
        let grid = test_grid(10, 3);
        assert!(grid.modes.get(1007), "alternateScroll should default ON");
    }

    #[test]
    fn mode_1007_toggle() {
        let mut grid = test_grid(10, 3);
        assert!(grid.modes.get(1007));
        // Disable: CSI ? 1007 l
        parse(&mut grid, b"\x1b[?1007l");
        assert!(!grid.modes.get(1007));
        // Re-enable: CSI ? 1007 h
        parse(&mut grid, b"\x1b[?1007h");
        assert!(grid.modes.get(1007));
    }

    // --- Scrollback capture tests (use ScreenSet, not bare Grid) ---

    fn parse_screen(screen: &mut ScreenSet, input: &[u8]) {
        process_bytes(screen, input, &mut std::io::sink());
    }

    #[test]
    fn scroll_captures_row_to_scrollback() {
        let mut screen = test_screen(10, 3);
        // Fill 3 rows and scroll one off.
        parse_screen(&mut screen, b"aaa\r\nbbb\r\nccc\r\nddd");
        assert_eq!(screen.scrollback().len(), 1);
        let row = screen.scrollback().get(0).unwrap();
        // The first row "aaa" scrolled off.
        let text: String = row.cells.iter().take(3).map(|c| c.codepoint).collect();
        assert_eq!(text, "aaa");
    }

    #[test]
    fn scroll_captures_multiple_rows() {
        let mut screen = test_screen(10, 3);
        // Fill 3 rows, then scroll 3 more off.
        parse_screen(&mut screen, b"r1\r\nr2\r\nr3\r\nr4\r\nr5\r\nr6");
        assert_eq!(screen.scrollback().len(), 3);
    }

    #[test]
    fn sub_region_scroll_no_scrollback() {
        let mut screen = test_screen(10, 5);
        // Set scroll region to rows 2-4 (1-based: CSI 2;4 r).
        parse_screen(&mut screen, b"\x1b[2;4r");
        // Move cursor to row 4 and scroll within the region.
        parse_screen(&mut screen, b"\x1b[4;1H");
        parse_screen(&mut screen, b"\r\nline");
        assert_eq!(
            screen.scrollback().len(),
            0,
            "sub-region scroll should not produce scrollback"
        );
    }

    #[test]
    fn linefeed_at_bottom_captures_scrollback() {
        let mut screen = test_screen(5, 2);
        parse_screen(&mut screen, b"ab\r\ncd\r\nef");
        // Two rows visible, "ab" should have scrolled off.
        assert_eq!(screen.scrollback().len(), 1);
        let row = screen.scrollback().get(0).unwrap();
        let text: String = row.cells.iter().take(2).map(|c| c.codepoint).collect();
        assert_eq!(text, "ab");
    }

    #[test]
    fn explicit_scroll_up_captures_scrollback() {
        let mut screen = test_screen(10, 3);
        parse_screen(&mut screen, b"aaa\r\nbbb\r\nccc");
        // Explicit scroll-up by 2: CSI 2 S
        parse_screen(&mut screen, b"\x1b[2S");
        assert_eq!(screen.scrollback().len(), 2);
    }

    #[test]
    fn auto_wrap_captures_scrollback() {
        // 3-col, 2-row grid. Fill both rows via wrapping, then wrap again.
        let mut screen = test_screen(3, 2);
        // "abcdef" fills row 0 (abc) and wraps to row 1 (def).
        // "ghi" wraps past row 1, scrolling row 0 ("abc") off.
        parse_screen(&mut screen, b"abcdefghi");
        assert_eq!(screen.scrollback().len(), 1);
        let row = screen.scrollback().get(0).unwrap();
        let text: String = row.cells.iter().take(3).map(|c| c.codepoint).collect();
        assert_eq!(text, "abc");
    }

    #[test]
    fn scroll_count_exceeding_grid_clamped() {
        let mut screen = test_screen(10, 3);
        parse_screen(&mut screen, b"aaa\r\nbbb\r\nccc");
        // Scroll up by 100 (clamped to 3).
        parse_screen(&mut screen, b"\x1b[100S");
        assert_eq!(screen.scrollback().len(), 3);
    }

    #[test]
    fn alt_screen_scrollback_discarded_by_default() {
        let mut screen = test_screen(10, 3);
        // Enter alt screen (mode 1049), fill and scroll.
        parse_screen(&mut screen, b"\x1b[?1049h");
        parse_screen(&mut screen, b"aaa\r\nbbb\r\nccc\r\nddd");
        // Default: alt screen rows are discarded (matches xterm/alacritty/kitty).
        assert_eq!(screen.scrollback().len(), 0);
        // Exiting the alt screen must not flush alt rows into primary scrollback either.
        parse_screen(&mut screen, b"\x1b[?1049l");
        assert_eq!(screen.scrollback().len(), 0);
    }

    #[test]
    fn alt_screen_scrollback_opt_in() {
        let mut screen = test_screen(10, 3);
        screen.set_save_alternate_scrollback(true);
        parse_screen(&mut screen, b"\x1b[?1049h");
        parse_screen(&mut screen, b"aaa\r\nbbb\r\nccc\r\nddd");
        assert_eq!(
            screen.scrollback().len(),
            1,
            "opt-in flag should capture alt screen scroll-off"
        );
    }

    #[test]
    fn primary_screen_scrollback_unaffected_by_flag() {
        let mut screen = test_screen(10, 3);
        screen.set_save_alternate_scrollback(false);
        // Stay on primary — flag only affects alternate.
        parse_screen(&mut screen, b"aaa\r\nbbb\r\nccc\r\nddd");
        assert_eq!(screen.scrollback().len(), 1);
    }
}
