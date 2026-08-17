use super::cell::Cell;
pub use oakterm_common::bidi::Direction;

/// Shell integration semantic mark from OSC 133.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum SemanticMark {
    #[default]
    None,
    PromptStart,
    InputStart,
    OutputStart,
    OutputEnd,
}

impl SemanticMark {
    /// Encode this mark as a wire byte.
    #[must_use]
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::None => 0,
            Self::PromptStart => 1,
            Self::InputStart => 2,
            Self::OutputStart => 3,
            Self::OutputEnd => 4,
        }
    }
}

/// Per-row shell-integration metadata. Independent of `semantic_mark`:
/// `ExitCode` accompanies an `OutputEnd` mark, but `WorkingDirectory` (OSC 7)
/// is recorded on its row with no semantic mark of its own.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MarkMetadata {
    ExitCode(i32),
    WorkingDirectory(String),
}

impl MarkMetadata {
    /// Encode this metadata as wire bytes (tag byte + payload).
    #[must_use]
    pub fn to_wire_bytes(&self) -> Vec<u8> {
        match self {
            Self::ExitCode(code) => {
                let mut buf = vec![0u8]; // tag 0 = exit code
                buf.extend_from_slice(&code.to_le_bytes());
                buf
            }
            Self::WorkingDirectory(dir) => {
                let mut buf = vec![1u8]; // tag 1 = working directory
                buf.extend_from_slice(dir.as_bytes());
                buf
            }
        }
    }
}

/// Optimization hint flags for a row. Set on mutation, never cleared
/// (clearing would require scanning all cells). Consumers must handle
/// false positives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct RowFlags(u8);

impl RowFlags {
    const WRAPPED: u8 = 1 << 0;
    const WRAP_CONTINUATION: u8 = 1 << 1;
    const HAS_STYLES: u8 = 1 << 2;
    const HAS_HYPERLINKS: u8 = 1 << 3;
    const HAS_GRAPHEMES: u8 = 1 << 4;

    #[must_use]
    pub const fn wrapped(self) -> bool {
        self.0 & Self::WRAPPED != 0
    }

    pub fn set_wrapped(&mut self, v: bool) {
        if v {
            self.0 |= Self::WRAPPED;
        } else {
            self.0 &= !Self::WRAPPED;
        }
    }

    #[must_use]
    pub const fn wrap_continuation(self) -> bool {
        self.0 & Self::WRAP_CONTINUATION != 0
    }

    pub fn set_wrap_continuation(&mut self, v: bool) {
        if v {
            self.0 |= Self::WRAP_CONTINUATION;
        } else {
            self.0 &= !Self::WRAP_CONTINUATION;
        }
    }

    #[must_use]
    pub const fn has_styles(self) -> bool {
        self.0 & Self::HAS_STYLES != 0
    }

    pub fn mark_has_styles(&mut self) {
        self.0 |= Self::HAS_STYLES;
    }

    #[must_use]
    pub const fn has_hyperlinks(self) -> bool {
        self.0 & Self::HAS_HYPERLINKS != 0
    }

    pub fn mark_has_hyperlinks(&mut self) {
        self.0 |= Self::HAS_HYPERLINKS;
    }

    #[must_use]
    pub const fn has_graphemes(self) -> bool {
        self.0 & Self::HAS_GRAPHEMES != 0
    }

    pub fn mark_has_graphemes(&mut self) {
        self.0 |= Self::HAS_GRAPHEMES;
    }
}

/// A horizontal sequence of cells with metadata.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Row {
    pub cells: Vec<Cell>,
    pub flags: RowFlags,
    pub direction: Direction,
    pub semantic_mark: SemanticMark,
    pub mark_metadata: Option<MarkMetadata>,
    pub seqno: u64,
}

impl Row {
    #[must_use]
    pub fn new(cols: usize) -> Self {
        Self {
            cells: vec![Cell::default(); cols],
            flags: RowFlags::default(),
            direction: Direction::Ltr,
            semantic_mark: SemanticMark::None,
            mark_metadata: None,
            seqno: 0,
        }
    }

    /// Create a new row with all cells using the given background (BCE).
    #[must_use]
    pub fn new_with_bg(cols: usize, bg: super::cell::Color) -> Self {
        let mut cell = Cell::default();
        cell.bg = bg;
        Self {
            cells: vec![cell; cols],
            flags: RowFlags::default(),
            direction: Direction::Ltr,
            semantic_mark: SemanticMark::None,
            mark_metadata: None,
            seqno: 0,
        }
    }

    pub fn reset(&mut self, seqno: u64) {
        for cell in &mut self.cells {
            cell.reset();
        }
        self.flags = RowFlags::default();
        self.direction = Direction::Ltr;
        self.semantic_mark = SemanticMark::None;
        self.mark_metadata = None;
        self.seqno = seqno;
    }

    /// Resize the row, truncating or extending with default cells.
    pub fn resize(&mut self, cols: usize) {
        self.cells.resize_with(cols, Cell::default);
    }

    /// Extract the text content of this row as a `String`.
    ///
    /// Wide character continuation cells are skipped, so byte offsets
    /// in the returned string do not correspond 1:1 to column indices.
    /// Grapheme cluster extra codepoints are appended after the base
    /// codepoint. Null codepoints (empty cells) become spaces, but
    /// trailing spaces from unwritten cells are trimmed.
    #[must_use]
    pub fn text(&self) -> String {
        self.text_range(0, usize::MAX)
    }

    /// Extract the text of the columns in `start..=end`, clamped to the
    /// row's width. Same cell handling as [`Row::text`]: a continuation
    /// cell at `start` is skipped, since its head column falls outside
    /// the range.
    #[must_use]
    pub fn text_range(&self, start: usize, end: usize) -> String {
        let Some(cells) = self
            .cells
            .get(start..=end.min(self.cells.len().saturating_sub(1)))
        else {
            return String::new();
        };
        let mut s = String::with_capacity(cells.len());
        for cell in cells {
            if cell.wide == super::cell::WideState::WideCont {
                continue;
            }
            let cp = cell.codepoint;
            if cp == '\0' {
                s.push(' ');
            } else {
                s.push(cp);
                for &g in cell.graphemes() {
                    s.push(g);
                }
            }
        }
        let trimmed = s.trim_end_matches(' ').len();
        s.truncate(trimmed);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::cell::WideState;

    fn row_from(text: &str) -> Row {
        let mut row = Row::new(text.chars().count());
        for (cell, ch) in row.cells.iter_mut().zip(text.chars()) {
            cell.codepoint = ch;
        }
        row
    }

    #[test]
    fn text_range_slices_columns_inclusively() {
        let row = row_from("hello world");
        assert_eq!(row.text_range(0, 4), "hello");
        assert_eq!(row.text_range(6, 10), "world");
        assert_eq!(row.text_range(4, 4), "o");
    }

    #[test]
    fn text_range_clamps_past_the_row_width() {
        let row = row_from("abc");
        assert_eq!(row.text_range(0, usize::MAX), "abc");
        assert_eq!(row.text_range(2, 99), "c");
        assert_eq!(row.text_range(3, 99), "");
    }

    #[test]
    fn text_range_inverted_range_is_empty() {
        let row = row_from("abc");
        assert_eq!(row.text_range(2, 1), "");
    }

    #[test]
    fn text_range_trims_trailing_blanks_but_keeps_interior() {
        let mut row = Row::new(6);
        row.cells[1].codepoint = 'a';
        row.cells[3].codepoint = 'b';
        assert_eq!(row.text_range(0, 5), " a b");
    }

    #[test]
    fn text_range_skips_a_leading_continuation_cell() {
        let mut row = Row::new(4);
        row.cells[0].codepoint = '漢';
        row.cells[0].wide = WideState::Wide;
        row.cells[1].wide = WideState::WideCont;
        row.cells[2].codepoint = 'x';

        assert_eq!(row.text_range(0, 3), "漢x");
        assert_eq!(row.text_range(1, 3), "x");
    }

    #[test]
    fn text_range_on_an_empty_row_is_empty() {
        assert_eq!(Row::new(0).text_range(0, usize::MAX), "");
    }
}
