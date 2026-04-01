/// Text selection type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionType {
    Normal,
    Block,
    Semantic,
    Line,
}

/// Half-cell precision for selection boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorSide {
    Left,
    Right,
}

/// Selection anchor point. Row is signed: negative values reference scrollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionAnchor {
    pub row: i64,
    pub col: u16,
    pub side: AnchorSide,
}

/// Text selection state, tracked separately from the grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub ty: SelectionType,
    pub start: SelectionAnchor,
    pub end: SelectionAnchor,
}

impl Selection {
    /// Create a new selection starting at a point. Start and end are the same
    /// initially; call `update` as the user drags.
    #[must_use]
    pub fn new(ty: SelectionType, row: i64, col: u16, side: AnchorSide) -> Self {
        let anchor = SelectionAnchor { row, col, side };
        Self {
            ty,
            start: anchor,
            end: anchor,
        }
    }

    /// Update the end anchor (during drag).
    pub fn update(&mut self, row: i64, col: u16, side: AnchorSide) {
        self.end = SelectionAnchor { row, col, side };
    }

    /// Return (earlier, later) anchors regardless of drag direction.
    #[must_use]
    pub fn normalized(&self) -> (SelectionAnchor, SelectionAnchor) {
        let a = self.start;
        let b = self.end;
        if a.row < b.row || (a.row == b.row && a.col <= b.col) {
            (a, b)
        } else {
            (b, a)
        }
    }

    /// Check if a cell is within the selection.
    #[must_use]
    pub fn contains(&self, row: i64, col: u16) -> bool {
        match self.ty {
            SelectionType::Line => self.contains_line(row),
            SelectionType::Block => self.contains_block(row, col),
            _ => self.contains_normal(row, col),
        }
    }

    fn contains_normal(&self, row: i64, col: u16) -> bool {
        let (start, end) = self.normalized();
        if row < start.row || row > end.row {
            return false;
        }
        if start.row == end.row {
            // Single-line selection.
            let left = if start.side == AnchorSide::Right {
                start.col + 1
            } else {
                start.col
            };
            let right = if end.side == AnchorSide::Left && end.col > 0 {
                end.col - 1
            } else {
                end.col
            };
            return col >= left && col <= right;
        }
        // Multi-line: first row from start to end, middle rows full, last row from 0 to end.
        if row == start.row {
            let left = if start.side == AnchorSide::Right {
                start.col + 1
            } else {
                start.col
            };
            col >= left
        } else if row == end.row {
            let right = if end.side == AnchorSide::Left && end.col > 0 {
                end.col - 1
            } else {
                end.col
            };
            col <= right
        } else {
            true // Middle rows are fully selected.
        }
    }

    fn contains_line(&self, row: i64) -> bool {
        let (start, end) = self.normalized();
        row >= start.row && row <= end.row
    }

    fn contains_block(&self, row: i64, col: u16) -> bool {
        let (start, end) = self.normalized();
        if row < start.row || row > end.row {
            return false;
        }
        let left = start.col.min(end.col);
        let right = start.col.max(end.col);
        col >= left && col <= right
    }

    /// Whether the selection is empty (zero-width, same point).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// Find word boundaries around a column position in a row of text.
/// Words are sequences of alphanumeric characters and underscores.
/// Returns (`start_col`, `end_col`) inclusive.
#[must_use]
pub fn word_boundaries(row_text: &[char], col: u16) -> (u16, u16) {
    let col = col as usize;
    if col >= row_text.len() {
        let end = row_text.len().saturating_sub(1);
        #[allow(clippy::cast_possible_truncation)]
        return (end as u16, end as u16);
    }

    let is_word_char = |c: char| c.is_alphanumeric() || c == '_';
    let at_word = is_word_char(row_text[col]);

    // Scan left.
    let mut start = col;
    while start > 0 {
        let prev = row_text[start - 1];
        if at_word != is_word_char(prev) {
            break;
        }
        start -= 1;
    }

    // Scan right.
    let mut end = col;
    while end + 1 < row_text.len() {
        let next = row_text[end + 1];
        if at_word != is_word_char(next) {
            break;
        }
        end += 1;
    }

    #[allow(clippy::cast_possible_truncation)]
    (start as u16, end as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_selection() {
        let sel = Selection::new(SelectionType::Normal, 5, 10, AnchorSide::Left);
        assert_eq!(sel.start.row, 5);
        assert_eq!(sel.start.col, 10);
        assert_eq!(sel.end, sel.start);
        assert!(sel.is_empty());
    }

    #[test]
    fn update_moves_end() {
        let mut sel = Selection::new(SelectionType::Normal, 0, 0, AnchorSide::Left);
        sel.update(2, 15, AnchorSide::Right);
        assert_eq!(sel.end.row, 2);
        assert_eq!(sel.end.col, 15);
        assert!(!sel.is_empty());
    }

    #[test]
    fn normalized_reverses_backwards() {
        let mut sel = Selection::new(SelectionType::Normal, 5, 10, AnchorSide::Left);
        sel.update(2, 5, AnchorSide::Right);
        let (start, end) = sel.normalized();
        assert_eq!(start.row, 2);
        assert_eq!(end.row, 5);
    }

    #[test]
    fn contains_single_line() {
        let mut sel = Selection::new(SelectionType::Normal, 3, 5, AnchorSide::Left);
        sel.update(3, 10, AnchorSide::Right);
        assert!(sel.contains(3, 5));
        assert!(sel.contains(3, 7));
        assert!(sel.contains(3, 10));
        assert!(!sel.contains(3, 4));
        assert!(!sel.contains(3, 11));
        assert!(!sel.contains(2, 7));
    }

    #[test]
    fn contains_multi_line() {
        let mut sel = Selection::new(SelectionType::Normal, 1, 5, AnchorSide::Left);
        sel.update(3, 10, AnchorSide::Right);
        // First row: from col 5 onwards
        assert!(!sel.contains(1, 4));
        assert!(sel.contains(1, 5));
        assert!(sel.contains(1, 80));
        // Middle row: everything
        assert!(sel.contains(2, 0));
        assert!(sel.contains(2, 80));
        // Last row: up to col 10
        assert!(sel.contains(3, 0));
        assert!(sel.contains(3, 10));
        assert!(!sel.contains(3, 11));
    }

    #[test]
    fn contains_line_selection() {
        let mut sel = Selection::new(SelectionType::Line, 2, 0, AnchorSide::Left);
        sel.update(4, 0, AnchorSide::Left);
        assert!(!sel.contains(1, 0));
        assert!(sel.contains(2, 0));
        assert!(sel.contains(2, 80));
        assert!(sel.contains(3, 50));
        assert!(sel.contains(4, 0));
        assert!(!sel.contains(5, 0));
    }

    #[test]
    fn contains_block_selection() {
        let mut sel = Selection::new(SelectionType::Block, 1, 5, AnchorSide::Left);
        sel.update(3, 10, AnchorSide::Right);
        assert!(sel.contains(2, 5));
        assert!(sel.contains(2, 7));
        assert!(sel.contains(2, 10));
        assert!(!sel.contains(2, 4));
        assert!(!sel.contains(2, 11));
        assert!(!sel.contains(0, 7));
    }

    #[test]
    fn contains_backwards_drag() {
        let mut sel = Selection::new(SelectionType::Normal, 5, 10, AnchorSide::Right);
        sel.update(3, 5, AnchorSide::Left);
        // Should work the same as forward selection.
        assert!(sel.contains(3, 5));
        assert!(sel.contains(4, 0));
        assert!(sel.contains(5, 10));
    }

    #[test]
    fn contains_scrollback_rows() {
        let mut sel = Selection::new(SelectionType::Normal, -10, 0, AnchorSide::Left);
        sel.update(-5, 20, AnchorSide::Right);
        assert!(sel.contains(-10, 0));
        assert!(sel.contains(-7, 10));
        assert!(sel.contains(-5, 20));
        assert!(!sel.contains(-4, 0));
        assert!(!sel.contains(-11, 0));
    }

    #[test]
    fn word_boundaries_ascii() {
        let text: Vec<char> = "hello world".chars().collect();
        assert_eq!(word_boundaries(&text, 2), (0, 4)); // "hello"
        assert_eq!(word_boundaries(&text, 6), (6, 10)); // "world"
    }

    #[test]
    fn word_boundaries_punctuation() {
        let text: Vec<char> = "foo.bar".chars().collect();
        assert_eq!(word_boundaries(&text, 1), (0, 2)); // "foo"
        assert_eq!(word_boundaries(&text, 3), (3, 3)); // "."
        assert_eq!(word_boundaries(&text, 4), (4, 6)); // "bar"
    }

    #[test]
    fn word_boundaries_underscore() {
        let text: Vec<char> = "hello_world test".chars().collect();
        assert_eq!(word_boundaries(&text, 3), (0, 10)); // "hello_world"
    }

    #[test]
    fn word_boundaries_whitespace() {
        let text: Vec<char> = "a   b".chars().collect();
        assert_eq!(word_boundaries(&text, 2), (1, 3)); // spaces
    }

    #[test]
    fn word_boundaries_edge() {
        let text: Vec<char> = "hello".chars().collect();
        assert_eq!(word_boundaries(&text, 0), (0, 4));
        assert_eq!(word_boundaries(&text, 4), (0, 4));
    }

    #[test]
    fn word_boundaries_out_of_range() {
        let text: Vec<char> = "hi".chars().collect();
        assert_eq!(word_boundaries(&text, 10), (1, 1)); // clamps
    }
}
