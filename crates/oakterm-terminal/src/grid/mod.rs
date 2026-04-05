pub mod cell;
pub mod cursor;
pub mod row;
pub mod selection;

#[cfg(test)]
mod tests;

use cell::{CellFlags, Color, Rgb};
use cursor::{CharsetIndex, Cursor, ScrollRegion, StandardCharset};
use row::Row;

use crate::scroll::HotBuffer;

/// Active DEC private modes and ANSI modes as a bitfield.
/// Indexed by mode number. Supports modes 0-2047.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeFlags {
    bits: [u8; 256],
}

impl ModeFlags {
    #[must_use]
    pub fn new() -> Self {
        Self { bits: [0; 256] }
    }

    pub fn set(&mut self, mode: u16, enabled: bool) {
        let Some((byte, bit)) = Self::index(mode) else {
            return;
        };
        if enabled {
            self.bits[byte] |= 1 << bit;
        } else {
            self.bits[byte] &= !(1 << bit);
        }
    }

    #[must_use]
    pub fn get(&self, mode: u16) -> bool {
        let Some((byte, bit)) = Self::index(mode) else {
            return false;
        };
        self.bits[byte] & (1 << bit) != 0
    }

    fn index(mode: u16) -> Option<(usize, u8)> {
        if mode < 2048 {
            Some((mode as usize / 8, (mode % 8) as u8))
        } else {
            None
        }
    }
}

impl Default for ModeFlags {
    fn default() -> Self {
        Self::new()
    }
}

/// Terminal-side `BiDi` processing mode. Reserved for future support (ADR-0009).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BidiMode {
    #[default]
    Off,
    Implicit,
    Explicit,
}

/// Which screen is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ScreenId {
    #[default]
    Primary,
    Alternate,
}

/// Maximum title stack depth (matches xterm).
pub(crate) const TITLE_STACK_MAX: usize = 10;

/// The visible terminal screen.
pub struct Grid {
    pub lines: Vec<Row>,
    pub cols: u16,
    pub rows: u16,
    pub cursor: Cursor,
    pub saved_cursor: Cursor,
    pub saved_attr: CellFlags,
    pub saved_fg: Color,
    pub saved_bg: Color,
    pub saved_underline_style: cell::UnderlineStyle,
    pub saved_underline_color: Option<Color>,
    pub active_charset: CharsetIndex,
    pub charsets: [StandardCharset; 4],
    pub current_attr: CellFlags,
    pub current_fg: Color,
    pub current_bg: Color,
    pub current_underline_style: cell::UnderlineStyle,
    pub current_underline_color: Option<Color>,
    pub modes: ModeFlags,
    pub bidi_mode: BidiMode,
    pub scroll_region: Option<ScrollRegion>,
    pub tab_stops: Vec<bool>,
    pub seqno: u64,
    pub palette: [Rgb; 256],
    pub default_palette: [Rgb; 256],
    pub dynamic_fg: Option<Rgb>,
    pub dynamic_bg: Option<Rgb>,
    pub dynamic_cursor: Option<Rgb>,
    pub title: Option<String>,
    /// Title stack for CSI 22/23 t (XTWINOPS push/pop). Capped at [`TITLE_STACK_MAX`].
    pub title_stack: Vec<String>,
    /// Set by handler when BEL (0x07) is received. Cleared by daemon after sending.
    pub bell_pending: bool,
    /// Set by handler when title changes. Cleared by daemon after sending.
    pub title_dirty: bool,
    /// Set after the first VT response write failure is logged.
    /// Prevents log spam when the PTY writer is permanently broken.
    pub writer_warned: bool,
}

impl Grid {
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        let mut lines = Vec::with_capacity(rows as usize);
        for _ in 0..rows {
            lines.push(Row::new(cols as usize));
        }

        let mut tab_stops = vec![false; cols as usize];
        // Default tab stops every 8 columns.
        for i in (8..cols as usize).step_by(8) {
            tab_stops[i] = true;
        }

        Self {
            lines,
            cols,
            rows,
            cursor: Cursor::default(),
            saved_cursor: Cursor::default(),
            saved_attr: CellFlags::empty(),
            saved_fg: Color::Default,
            saved_bg: Color::Default,
            saved_underline_style: cell::UnderlineStyle::None,
            saved_underline_color: None,
            active_charset: CharsetIndex::default(),
            charsets: [StandardCharset::default(); 4],
            current_attr: CellFlags::empty(),
            current_fg: Color::Default,
            current_bg: Color::Default,
            current_underline_style: cell::UnderlineStyle::None,
            current_underline_color: None,
            modes: {
                let mut m = ModeFlags::new();
                m.set(7, true); // DECAWM: auto-wrap on by default.
                m.set(1007, true); // alternateScroll: wheel → arrow keys on alt screen.
                m
            },
            bidi_mode: BidiMode::Off,
            scroll_region: None,
            tab_stops,
            seqno: 0,
            palette: default_palette(),
            default_palette: default_palette(),
            dynamic_fg: None,
            dynamic_bg: None,
            dynamic_cursor: None,
            title: None,
            title_stack: Vec::new(),
            bell_pending: false,
            title_dirty: false,
            writer_warned: false,
        }
    }

    /// Increment the global sequence number and return the new value.
    pub fn next_seqno(&mut self) -> u64 {
        self.seqno += 1;
        self.seqno
    }

    /// Mark a row as dirty with the current global seqno.
    pub fn touch_row(&mut self, row: u16) {
        let seqno = self.next_seqno();
        if let Some(line) = self.lines.get_mut(row as usize) {
            line.seqno = seqno;
        }
    }

    /// Mark all visible rows as dirty.
    pub fn touch_all(&mut self) {
        let seqno = self.next_seqno();
        for line in &mut self.lines {
            line.seqno = seqno;
        }
    }

    /// Resize the grid to new dimensions.
    /// Returns rows removed when shrinking vertically (for scrollback capture).
    /// Rejects zero-dimension resizes (no-op, returns empty).
    pub fn resize(&mut self, cols: u16, rows: u16) -> Vec<Row> {
        if cols == 0 || rows == 0 {
            return Vec::new();
        }

        let new_rows = rows as usize;
        let captured = if new_rows < self.lines.len() {
            self.lines.split_off(new_rows)
        } else {
            Vec::new()
        };

        self.cols = cols;
        self.rows = rows;

        self.lines.resize_with(new_rows, || Row::new(cols as usize));
        for line in &mut self.lines {
            line.resize(cols as usize);
        }

        // Reset tab stops to match Grid::new behavior.
        self.tab_stops = vec![false; cols as usize];
        for i in (8..cols as usize).step_by(8) {
            self.tab_stops[i] = true;
        }

        // Clamp both cursors to new bounds.
        if self.cursor.col >= cols {
            self.cursor.col = cols - 1;
        }
        if self.cursor.row >= rows {
            self.cursor.row = rows - 1;
        }
        if self.saved_cursor.col >= cols {
            self.saved_cursor.col = cols - 1;
        }
        if self.saved_cursor.row >= rows {
            self.saved_cursor.row = rows - 1;
        }

        self.scroll_region = None;
        self.touch_all();

        captured
    }

    /// Return indices of rows changed since `since_seqno`.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)] // rows is u16, so index always fits
    pub fn dirty_rows(&self, since_seqno: u64) -> Vec<u16> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, row)| row.seqno > since_seqno)
            .map(|(i, _)| i as u16)
            .collect()
    }
}

/// The terminal maintains two grids: primary and alternate.
pub struct ScreenSet {
    active: ScreenId,
    primary: Grid,
    /// Lazily allocated on first DECSET 1049.
    alternate: Option<Grid>,
    scrollback: HotBuffer,
    /// When true (default), rows scrolling off the alternate screen top
    /// go to the primary scrollback buffer.
    save_alternate_scrollback: bool,
    /// Cold disk archive for rows pruned from the hot buffer.
    archive: Option<crate::scroll::archive_manager::ArchiveManager>,
    /// Active scrollback search, if any.
    search: Option<crate::search::SearchEngine>,
    /// VT parser state, persisted across PTY read chunks to avoid
    /// mid-sequence splits that leak escape fragments as literal text.
    processor: vte::ansi::Processor<vte::ansi::StdSyncHandler>,
}

impl ScreenSet {
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            active: ScreenId::Primary,
            primary: Grid::new(cols, rows),
            alternate: None,
            scrollback: HotBuffer::default(),
            save_alternate_scrollback: true,
            archive: None,
            processor: vte::ansi::Processor::new(),
            search: None,
        }
    }

    /// Feed PTY output through the persistent VT parser.
    ///
    /// The parser state is preserved across calls so escape sequences
    /// that span read chunk boundaries are handled correctly.
    pub fn process_bytes(&mut self, input: &[u8], writer: &mut impl std::io::Write) {
        // Temporarily extract the processor to avoid a double mutable borrow
        // (self is borrowed by Terminal::new, processor by advance).
        let mut processor = std::mem::replace(&mut self.processor, vte::ansi::Processor::new());
        {
            let mut terminal = crate::handler::Terminal::new(self, writer);
            processor.advance(&mut terminal, input);
        }
        self.processor = processor;
    }

    #[must_use]
    pub fn active_screen(&self) -> ScreenId {
        self.active
    }

    #[must_use]
    pub fn primary(&self) -> &Grid {
        &self.primary
    }

    pub fn primary_mut(&mut self) -> &mut Grid {
        &mut self.primary
    }

    #[must_use]
    pub fn has_alternate(&self) -> bool {
        self.alternate.is_some()
    }

    /// Get a reference to the active grid.
    ///
    /// # Panics
    /// Panics if the alternate screen is active but not yet allocated.
    #[must_use]
    pub fn active_grid(&self) -> &Grid {
        match self.active {
            ScreenId::Primary => &self.primary,
            ScreenId::Alternate => self
                .alternate
                .as_ref()
                .expect("alternate screen accessed before allocation"),
        }
    }

    /// Get a mutable reference to the active grid.
    ///
    /// # Panics
    /// Panics if the alternate screen is active but not yet allocated.
    pub fn active_grid_mut(&mut self) -> &mut Grid {
        match self.active {
            ScreenId::Primary => &mut self.primary,
            ScreenId::Alternate => self
                .alternate
                .as_mut()
                .expect("alternate screen accessed before allocation"),
        }
    }

    /// Switch to the alternate screen. Allocates if first use.
    pub fn enter_alternate(&mut self) {
        if self.alternate.is_none() {
            self.alternate = Some(Grid::new(self.primary.cols, self.primary.rows));
        }
        self.active = ScreenId::Alternate;
    }

    /// Switch back to the primary screen.
    pub fn exit_alternate(&mut self) {
        self.active = ScreenId::Primary;
    }

    /// Access the scrollback buffer.
    #[must_use]
    pub fn scrollback(&self) -> &HotBuffer {
        &self.scrollback
    }

    /// Access the scrollback buffer mutably.
    pub fn scrollback_mut(&mut self) -> &mut HotBuffer {
        &mut self.scrollback
    }

    /// Whether alt-screen scrollback is captured to primary.
    #[must_use]
    pub fn save_alternate_scrollback(&self) -> bool {
        self.save_alternate_scrollback
    }

    /// Set whether alt-screen scrollback is captured to primary.
    pub fn set_save_alternate_scrollback(&mut self, save: bool) {
        self.save_alternate_scrollback = save;
    }

    pub fn set_archive(&mut self, mgr: crate::scroll::archive_manager::ArchiveManager) {
        self.archive = Some(mgr);
    }

    #[must_use]
    pub fn archive(&self) -> Option<&crate::scroll::archive_manager::ArchiveManager> {
        self.archive.as_ref()
    }

    pub fn archive_mut(&mut self) -> Option<&mut crate::scroll::archive_manager::ArchiveManager> {
        self.archive.as_mut()
    }

    #[must_use]
    pub fn search(&self) -> Option<&crate::search::SearchEngine> {
        self.search.as_ref()
    }

    pub fn search_mut(&mut self) -> Option<&mut crate::search::SearchEngine> {
        self.search.as_mut()
    }

    pub fn set_search(&mut self, engine: crate::search::SearchEngine) {
        self.search = Some(engine);
    }

    pub fn clear_search(&mut self) {
        self.search = None;
    }

    /// Run the current search engine against the scrollback buffer.
    pub fn run_search(&mut self) {
        // Split borrow: take engine out, search, put it back.
        if let Some(mut engine) = self.search.take() {
            engine.search(&self.scrollback);
            self.search = Some(engine);
        }
    }

    /// Push a row to the hot buffer, archiving any pruned rows to disk.
    pub fn push_to_scrollback(&mut self, row: Row) {
        let pruned = self.scrollback.push(row);
        if !pruned.is_empty() {
            if let Some(archive) = &mut self.archive {
                if let Err(e) = archive.archive_rows(pruned) {
                    tracing::warn!(error = %e, "failed to archive pruned rows");
                }
            }
        }
    }

    /// Full terminal reset: switch to primary, drop alternate, reset primary.
    /// Scrollback is preserved (matches xterm RIS behavior).
    pub fn reset(&mut self) {
        let cols = self.primary.cols;
        let rows = self.primary.rows;
        self.active = ScreenId::Primary;
        self.alternate = None;
        self.primary = Grid::new(cols, rows);
        self.processor = vte::ansi::Processor::new();
    }

    /// Resize both primary and alternate grids (if allocated).
    /// Rows removed by shrinking are captured into scrollback.
    pub fn resize_all(&mut self, cols: u16, rows: u16) {
        let captured = self.primary.resize(cols, rows);
        for row in captured {
            self.push_to_scrollback(row);
        }
        if let Some(alt) = &mut self.alternate {
            let alt_captured = alt.resize(cols, rows);
            if self.save_alternate_scrollback {
                for row in alt_captured {
                    self.push_to_scrollback(row);
                }
            }
        }
    }
}

/// Build the default 256-color palette.
fn default_palette() -> [Rgb; 256] {
    let mut palette = [Rgb::default(); 256];

    // Standard 16 colors (approximate xterm defaults).
    let base: [(u8, u8, u8); 16] = [
        (0, 0, 0),       // Black
        (205, 0, 0),     // Red
        (0, 205, 0),     // Green
        (205, 205, 0),   // Yellow
        (0, 0, 238),     // Blue
        (205, 0, 205),   // Magenta
        (0, 205, 205),   // Cyan
        (229, 229, 229), // White
        (127, 127, 127), // Bright Black
        (255, 0, 0),     // Bright Red
        (0, 255, 0),     // Bright Green
        (255, 255, 0),   // Bright Yellow
        (92, 92, 255),   // Bright Blue
        (255, 0, 255),   // Bright Magenta
        (0, 255, 255),   // Bright Cyan
        (255, 255, 255), // Bright White
    ];
    for (i, (r, g, b)) in base.iter().enumerate() {
        palette[i] = Rgb {
            r: *r,
            g: *g,
            b: *b,
        };
    }

    // 216-color cube (indices 16-231): 6x6x6 RGB.
    for r in 0..6u8 {
        for g in 0..6u8 {
            for b in 0..6u8 {
                let idx = 16 + (r * 36 + g * 6 + b) as usize;
                let to_val = |c: u8| if c == 0 { 0 } else { 55 + 40 * c };
                palette[idx] = Rgb {
                    r: to_val(r),
                    g: to_val(g),
                    b: to_val(b),
                };
            }
        }
    }

    // 24-step grayscale (indices 232-255).
    for i in 0..24u8 {
        let v = 8 + 10 * i;
        palette[232 + i as usize] = Rgb { r: v, g: v, b: v };
    }

    palette
}
