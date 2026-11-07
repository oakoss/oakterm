//! Row text where a wide character's two cells are one character.
//!
//! Copy mode and the a11y bridge hold a row as one `char` per column, so an
//! index is a column. The second cell of a two-cell character is
//! [`WIDE_TAIL`] there; these helpers turn such a row into the text a
//! search or screen reader sees and map its character offsets back to
//! columns.

/// Stands for the second cell of a two-cell character in a per-column row.
/// Empty cells read as spaces, so NUL never stands for real content.
pub(crate) const WIDE_TAIL: char = '\0';

/// The row's characters with each one's starting column, plus one closing
/// entry: the column after the last character. A two-cell character spans
/// `cols[i]..cols[i + 1]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ColumnText {
    pub(crate) text: String,
    pub(crate) cols: Vec<u16>,
}

impl ColumnText {
    #[must_use]
    pub(crate) fn new(row: &[char]) -> Self {
        let mut text = String::with_capacity(row.len());
        let mut cols = Vec::with_capacity(row.len() + 1);
        for (col, &c) in row.iter().enumerate() {
            if c != WIDE_TAIL {
                text.push(c);
                cols.push(u16::try_from(col).unwrap_or(u16::MAX));
            }
        }
        cols.push(u16::try_from(row.len()).unwrap_or(u16::MAX));
        Self { text, cols }
    }

    /// The columns `start..end` (end exclusive) that character offsets
    /// `start..end` cover.
    #[must_use]
    pub(crate) fn column_span(&self, start: usize, end: usize) -> (u16, u16) {
        let at = |i: usize| self.cols[i.min(self.cols.len() - 1)];
        (at(start), at(end))
    }

    /// The character offset whose cells include `col`; past the row's end,
    /// the offset after its last character.
    #[must_use]
    pub(crate) fn char_at_column(&self, col: u16) -> usize {
        self.cols.partition_point(|&c| c <= col).saturating_sub(1)
    }

    /// Characters starting before boundary `col`; one inside a wide char
    /// counts it.
    #[must_use]
    pub(crate) fn offset_at_boundary(&self, col: usize) -> usize {
        self.cols[..self.cols.len() - 1].partition_point(|&c| usize::from(c) < col)
    }

    /// Where character `offset` starts, or the row's end past the last.
    #[must_use]
    pub(crate) fn column_at_offset(&self, offset: usize) -> u16 {
        self.cols[offset.min(self.cols.len() - 1)]
    }
}

/// The column of the first cell of the character `col` belongs to.
#[must_use]
pub(crate) fn head_column(row: &[char], col: u16) -> u16 {
    if col > 0 && row.get(usize::from(col)) == Some(&WIDE_TAIL) {
        col - 1
    } else {
        col
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(s: &str) -> Vec<char> {
        s.chars()
            .map(|c| if c == '_' { WIDE_TAIL } else { c })
            .collect()
    }

    #[test]
    fn a_wide_char_is_one_character_spanning_two_columns() {
        let ct = ColumnText::new(&row("a中_b"));
        assert_eq!(ct.text, "a中b");
        assert_eq!(ct.cols, [0, 1, 3, 4]);
        assert_eq!(ct.column_span(1, 2), (1, 3));
        assert_eq!(ct.column_span(0, 3), (0, 4));
    }

    #[test]
    fn a_column_maps_to_the_character_covering_it() {
        let ct = ColumnText::new(&row("a中_b"));
        assert_eq!(ct.char_at_column(0), 0);
        assert_eq!(ct.char_at_column(1), 1);
        assert_eq!(ct.char_at_column(2), 1, "the tail belongs to its head");
        assert_eq!(ct.char_at_column(3), 2);
        assert_eq!(ct.char_at_column(9), 3, "past the end");
    }

    #[test]
    fn boundaries_convert_between_columns_and_offsets() {
        let ct = ColumnText::new(&row("a中_b"));
        assert_eq!(ct.offset_at_boundary(0), 0);
        assert_eq!(ct.offset_at_boundary(1), 1);
        assert_eq!(ct.offset_at_boundary(2), 2, "inside 中 counts it");
        assert_eq!(ct.offset_at_boundary(3), 2);
        assert_eq!(ct.offset_at_boundary(usize::MAX), 3);
        assert_eq!(ct.column_at_offset(0), 0);
        assert_eq!(ct.column_at_offset(2), 3);
        assert_eq!(ct.column_at_offset(3), 4);
        assert_eq!(ct.column_at_offset(99), 4);
    }

    #[test]
    fn a_tail_column_snaps_to_its_head() {
        let r = row("a中_b");
        assert_eq!(head_column(&r, 2), 1);
        assert_eq!(head_column(&r, 1), 1);
        assert_eq!(head_column(&r, 3), 3);
    }

    #[test]
    fn a_narrow_row_maps_one_to_one() {
        let ct = ColumnText::new(&row("abc"));
        assert_eq!(ct.text, "abc");
        assert_eq!(ct.cols, [0, 1, 2, 3]);
        assert_eq!(ct.char_at_column(2), 2);
    }
}
