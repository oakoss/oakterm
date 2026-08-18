//! Copy-mode cursor motions (Spec-0008 Cursor Movement): where a motion
//! lands, resolved against plain row text with no GUI state involved.
//!
//! Motions read the pane in reading order across rows, so `w` at the end
//! of a row continues on the next one. A row the client holds no text for
//! ends the scan: word motions stop at the edge of the served region, the
//! same edge `j`/`k` clamp at, rather than walking a whole unserved gap
//! to reach text the client cannot show.

use crate::copy_keys::Motion;
use tracing::debug;

/// The rows a motion may read.
pub(crate) trait RowText {
    /// The row's cells as characters. `None` for a row the client holds
    /// no text for; trailing blanks may be omitted.
    fn row(&self, row: i64) -> Option<Vec<char>>;
}

/// The region a motion resolves inside: the rows copy mode can address
/// and the pane's shape. `first_row <= last_row` — the range always holds
/// at least the cursor's own row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MotionBounds {
    /// Oldest addressable row (cache start, or the frozen viewport top).
    pub(crate) first_row: i64,
    /// Newest addressable row: the bottom of the live grid.
    pub(crate) last_row: i64,
    pub(crate) cols: u16,
    /// Visible rows, which is what a page motion moves by.
    pub(crate) visible_rows: u16,
}

impl MotionBounds {
    fn last_col(self) -> u16 {
        self.cols.saturating_sub(1)
    }

    /// Order-tolerant so bounds that arrive inverted land on the nearer
    /// edge; `i64::clamp` panics on them.
    fn clamp_row(self, row: i64) -> i64 {
        row.clamp(
            self.first_row.min(self.last_row),
            self.last_row.max(self.first_row),
        )
    }
}

/// Where `motion` puts the cursor. Pure: the caller applies the result.
pub(crate) fn resolve(
    motion: Motion,
    cursor: (i64, u16),
    bounds: MotionBounds,
    rows: &impl RowText,
) -> (i64, u16) {
    let (row, col) = cursor;
    let page = i64::from(bounds.visible_rows.max(1));
    let half = (page / 2).max(1);
    let vertical = |delta: i64| (bounds.clamp_row(row + delta), col.min(bounds.last_col()));
    match motion {
        Motion::Left => (row, col.saturating_sub(1)),
        Motion::Right => (row, col.saturating_add(1).min(bounds.last_col())),
        Motion::Down => vertical(1),
        Motion::Up => vertical(-1),
        Motion::HalfPageDown => vertical(half),
        Motion::HalfPageUp => vertical(-half),
        Motion::PageDown => vertical(page),
        Motion::PageUp => vertical(-page),
        Motion::LineStart => (row, 0),
        Motion::LineEnd => (row, last_non_blank(rows, row).unwrap_or(0)),
        Motion::FirstNonBlank => (row, first_non_blank(rows, row).unwrap_or(0)),
        Motion::Top => (bounds.first_row, 0),
        Motion::Bottom => (bounds.last_row, 0),
        Motion::WordForward => Scan::new(rows, bounds, cursor).word_forward(),
        Motion::WordBackward => Scan::new(rows, bounds, cursor).word_backward(),
        Motion::WordEnd => Scan::new(rows, bounds, cursor).word_end(),
    }
}

/// Character class for word motions. Vim's `iskeyword` default treats
/// alphanumerics and `_` as word characters and every other non-blank as
/// its own class, so `foo.bar` holds three words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Blank,
    Word,
    Punct,
}

fn class_of(c: char) -> Class {
    if c.is_whitespace() {
        Class::Blank
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

fn row_chars(rows: &impl RowText, row: i64) -> Vec<char> {
    rows.row(row).unwrap_or_default()
}

fn last_non_blank(rows: &impl RowText, row: i64) -> Option<u16> {
    let chars = row_chars(rows, row);
    let index = chars.iter().rposition(|c| !c.is_whitespace())?;
    u16::try_from(index).ok()
}

fn first_non_blank(rows: &impl RowText, row: i64) -> Option<u16> {
    let chars = row_chars(rows, row);
    let index = chars.iter().position(|c| !c.is_whitespace())?;
    u16::try_from(index).ok()
}

/// A cursor walking the pane one cell at a time, holding the row it is
/// on so a scan across a row costs one row read rather than one per cell.
struct Scan<'a, R: RowText> {
    rows: &'a R,
    bounds: MotionBounds,
    /// Where the scan started; a motion with nowhere to go returns here.
    origin: (i64, u16),
    at: (i64, u16),
    chars: Vec<char>,
}

impl<'a, R: RowText> Scan<'a, R> {
    fn new(rows: &'a R, bounds: MotionBounds, cursor: (i64, u16)) -> Self {
        Self {
            rows,
            bounds,
            origin: cursor,
            at: cursor,
            chars: row_chars(rows, cursor.0),
        }
    }

    /// The class at the current cell. Cells past a row's text are blank —
    /// every row is `cols` wide whatever its trailing content.
    fn class(&self) -> Class {
        self.chars
            .get(usize::from(self.at.1))
            .copied()
            .map_or(Class::Blank, class_of)
    }

    /// Move onto a row, or refuse when the client holds no text for it.
    /// One row read per row crossed is what bounds the scan to the served
    /// region rather than the whole addressable range.
    fn enter_row(&mut self, row: i64, col: u16) -> bool {
        let Some(chars) = self.rows.row(row) else {
            // In range but unserved: the dead zone below a scrolled
            // frozen page, where a motion stops for no on-screen reason.
            debug!(row, "copy mode motion stopped at an unserved row");
            return false;
        };
        self.chars = chars;
        self.at = (row, col);
        true
    }

    /// Step one cell in reading order, or `false` at the end of the
    /// served region.
    fn forward(&mut self) -> bool {
        let (row, col) = self.at;
        if col < self.bounds.last_col() {
            self.at = (row, col + 1);
            return true;
        }
        if row >= self.bounds.last_row {
            return false;
        }
        self.enter_row(row + 1, 0)
    }

    fn backward(&mut self) -> bool {
        let (row, col) = self.at;
        if col > 0 {
            self.at = (row, col - 1);
            return true;
        }
        if row <= self.bounds.first_row {
            return false;
        }
        self.enter_row(row - 1, self.bounds.last_col())
    }

    /// Start of the next word. Steps off the current run, then over any
    /// blanks; with no word ahead the cursor stays put rather than
    /// parking on the trailing blank at the end of the buffer.
    fn word_forward(&mut self) -> (i64, u16) {
        let start = self.class();
        if start != Class::Blank {
            while self.class() == start {
                if !self.forward() {
                    return self.origin;
                }
            }
        }
        while self.class() == Class::Blank {
            if !self.forward() {
                return self.origin;
            }
        }
        self.at
    }

    /// Start of the previous word.
    fn word_backward(&mut self) -> (i64, u16) {
        if !self.backward() {
            return self.origin;
        }
        while self.class() == Class::Blank {
            if !self.backward() {
                return self.origin;
            }
        }
        let run = self.class();
        loop {
            let before = self.at;
            if !self.backward() {
                return self.at;
            }
            if self.class() != run {
                self.at = before;
                return before;
            }
        }
    }

    /// End of the word the cursor is in, or of the next one when it is
    /// already at an end. Always advances at least one cell first, which
    /// is what makes repeated `e` walk word to word.
    fn word_end(&mut self) -> (i64, u16) {
        if !self.forward() {
            return self.origin;
        }
        while self.class() == Class::Blank {
            if !self.forward() {
                return self.origin;
            }
        }
        let run = self.class();
        loop {
            let before = self.at;
            if !self.forward() {
                return self.at;
            }
            if self.class() != run {
                self.at = before;
                return before;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MotionBounds, RowText, resolve};
    use crate::copy_keys::Motion;
    use std::cell::Cell;
    use std::collections::BTreeMap;

    /// Rows keyed the way the copy-mode cache keys them: negatives are
    /// scrollback, non-negatives the live grid. A missing row reads blank.
    struct Rows(BTreeMap<i64, Vec<char>>);

    impl Rows {
        fn new(first: i64, lines: &[&str]) -> Self {
            Self(
                lines
                    .iter()
                    .enumerate()
                    .map(|(i, line)| {
                        (
                            first + i64::try_from(i).expect("small"),
                            line.chars().collect(),
                        )
                    })
                    .collect(),
            )
        }
    }

    impl RowText for Rows {
        fn row(&self, row: i64) -> Option<Vec<char>> {
            self.0.get(&row).cloned()
        }
    }

    /// Rows that count the reads a scan makes, so a test can assert on
    /// the work done and not only on where the cursor landed.
    struct Counted(Rows, Cell<usize>);

    impl Counted {
        fn reads(&self) -> usize {
            self.1.get()
        }
    }

    impl RowText for Counted {
        fn row(&self, row: i64) -> Option<Vec<char>> {
            self.1.set(self.1.get() + 1);
            self.0.row(row)
        }
    }

    fn bounds(first_row: i64, last_row: i64, cols: u16) -> MotionBounds {
        MotionBounds {
            first_row,
            last_row,
            cols,
            visible_rows: 4,
        }
    }

    fn go(motion: Motion, cursor: (i64, u16), rows: &Rows, b: MotionBounds) -> (i64, u16) {
        resolve(motion, cursor, b, rows)
    }

    // --- Arrow-equivalent motions ---

    #[test]
    fn hjkl_step_one_cell_and_clamp_at_the_edges() {
        let rows = Rows::new(-2, &["ab", "cd", "ef"]);
        let b = bounds(-2, 0, 10);
        assert_eq!(go(Motion::Right, (-1, 0), &rows, b), (-1, 1));
        assert_eq!(go(Motion::Left, (-1, 1), &rows, b), (-1, 0));
        assert_eq!(go(Motion::Down, (-1, 0), &rows, b), (0, 0));
        assert_eq!(go(Motion::Up, (-1, 0), &rows, b), (-2, 0));

        assert_eq!(go(Motion::Left, (-1, 0), &rows, b), (-1, 0), "column floor");
        assert_eq!(go(Motion::Right, (-1, 9), &rows, b), (-1, 9), "column cap");
        assert_eq!(go(Motion::Up, (-2, 0), &rows, b), (-2, 0), "oldest row");
        assert_eq!(go(Motion::Down, (0, 0), &rows, b), (0, 0), "newest row");
    }

    /// A one-column pane leaves no room to move horizontally; the clamp
    /// must not underflow into a column that does not exist.
    #[test]
    fn a_single_column_pane_pins_the_column_at_zero() {
        let rows = Rows::new(0, &["x"]);
        let b = bounds(0, 0, 1);
        assert_eq!(go(Motion::Right, (0, 0), &rows, b), (0, 0));
        assert_eq!(go(Motion::LineEnd, (0, 0), &rows, b), (0, 0));
    }

    /// A vertical move carries the column, clamped to the pane width so a
    /// motion off a wide row cannot leave the cursor past the edge.
    #[test]
    fn vertical_motions_carry_the_column_within_the_pane_width() {
        let rows = Rows::new(-1, &["abcdef", "gh"]);
        let b = bounds(-1, 0, 4);
        assert_eq!(go(Motion::Down, (-1, 3), &rows, b), (0, 3));
        assert_eq!(go(Motion::Down, (-1, 9), &rows, b), (0, 3), "clamped");
    }

    // --- Page motions ---

    #[test]
    fn page_motions_move_by_the_visible_height_and_half_of_it() {
        let rows = Rows::new(-20, &[]);
        let b = MotionBounds {
            first_row: -20,
            last_row: 7,
            cols: 10,
            visible_rows: 8,
        };
        assert_eq!(go(Motion::PageDown, (-16, 0), &rows, b), (-8, 0));
        assert_eq!(go(Motion::PageUp, (-8, 0), &rows, b), (-16, 0));
        assert_eq!(go(Motion::HalfPageDown, (-16, 0), &rows, b), (-12, 0));
        assert_eq!(go(Motion::HalfPageUp, (-12, 0), &rows, b), (-16, 0));
    }

    /// A page motion past either end lands on the end, not outside it.
    #[test]
    fn page_motions_clamp_to_the_addressable_range() {
        let rows = Rows::new(-4, &[]);
        let b = MotionBounds {
            first_row: -4,
            last_row: 3,
            cols: 10,
            visible_rows: 100,
        };
        assert_eq!(go(Motion::PageUp, (0, 0), &rows, b), (-4, 0));
        assert_eq!(go(Motion::PageDown, (0, 0), &rows, b), (3, 0));
    }

    /// Bounds carry `first_row <= last_row`; an inverted pair lands on an
    /// edge rather than panicking inside `clamp`.
    #[test]
    fn a_vertical_motion_tolerates_inverted_bounds() {
        let rows = Rows::new(0, &[]);
        let b = MotionBounds {
            first_row: 4,
            last_row: -4,
            cols: 10,
            visible_rows: 4,
        };
        assert_eq!(go(Motion::Down, (0, 0), &rows, b), (1, 0));
        assert_eq!(go(Motion::PageDown, (0, 0), &rows, b), (4, 0));
    }

    /// A one-row pane still moves by at least one row on a half page;
    /// `visible_rows / 2` is zero there, which would make Ctrl+d dead.
    #[test]
    fn a_half_page_moves_at_least_one_row() {
        let rows = Rows::new(-3, &[]);
        let b = MotionBounds {
            first_row: -3,
            last_row: 0,
            cols: 10,
            visible_rows: 1,
        };
        assert_eq!(go(Motion::HalfPageDown, (-3, 0), &rows, b), (-2, 0));
        assert_eq!(go(Motion::HalfPageUp, (-1, 0), &rows, b), (-2, 0));
    }

    // --- Line motions ---

    #[test]
    fn line_motions_find_the_ends_and_the_first_non_blank() {
        let rows = Rows::new(-1, &["  hello  ", ""]);
        let b = bounds(-1, 0, 20);
        assert_eq!(go(Motion::LineStart, (-1, 5), &rows, b), (-1, 0));
        assert_eq!(go(Motion::FirstNonBlank, (-1, 8), &rows, b), (-1, 2));
        assert_eq!(go(Motion::LineEnd, (-1, 0), &rows, b), (-1, 6));
    }

    /// A blank row has no non-blank cell to land on, so both motions fall
    /// back to column 0 rather than leaving the cursor where it was.
    #[test]
    fn line_motions_on_a_blank_or_missing_row_land_at_column_zero() {
        let rows = Rows::new(-1, &["     "]);
        let b = bounds(-2, 0, 10);
        assert_eq!(go(Motion::LineEnd, (-1, 4), &rows, b), (-1, 0));
        assert_eq!(go(Motion::FirstNonBlank, (-1, 4), &rows, b), (-1, 0));
        // Row -2 is not in the map at all.
        assert_eq!(go(Motion::LineEnd, (-2, 3), &rows, b), (-2, 0));
    }

    // --- gg / G ---

    #[test]
    fn gg_and_g_jump_to_the_ends_of_the_addressable_range() {
        let rows = Rows::new(-6, &[]);
        let b = bounds(-6, 5, 10);
        assert_eq!(go(Motion::Top, (0, 4), &rows, b), (-6, 0));
        assert_eq!(go(Motion::Bottom, (-6, 4), &rows, b), (5, 0));
    }

    // --- Word motions ---

    #[test]
    fn w_walks_word_starts_across_a_row() {
        let rows = Rows::new(0, &["foo bar  baz"]);
        let b = bounds(0, 0, 12);
        assert_eq!(go(Motion::WordForward, (0, 0), &rows, b), (0, 4));
        assert_eq!(go(Motion::WordForward, (0, 4), &rows, b), (0, 9));
        assert_eq!(
            go(Motion::WordForward, (0, 5), &rows, b),
            (0, 9),
            "mid-word"
        );
    }

    /// Punctuation is its own class (vim's `iskeyword` default), so
    /// `foo.bar` holds three word starts, not one.
    #[test]
    fn punctuation_runs_are_their_own_words() {
        let rows = Rows::new(0, &["foo.bar"]);
        let b = bounds(0, 0, 7);
        assert_eq!(go(Motion::WordForward, (0, 0), &rows, b), (0, 3));
        assert_eq!(go(Motion::WordForward, (0, 3), &rows, b), (0, 4));
        assert_eq!(go(Motion::WordBackward, (0, 4), &rows, b), (0, 3));
    }

    /// Underscores are word characters, so `snake_case` is one word.
    #[test]
    fn underscores_do_not_split_a_word() {
        let rows = Rows::new(0, &["snake_case next"]);
        let b = bounds(0, 0, 16);
        assert_eq!(go(Motion::WordForward, (0, 0), &rows, b), (0, 11));
        assert_eq!(go(Motion::WordEnd, (0, 0), &rows, b), (0, 9));
    }

    /// Rows read in reading order: a motion at the end of a row continues
    /// on the next, over the trailing blanks the grid pads it with.
    #[test]
    fn word_motions_cross_rows_in_reading_order() {
        let rows = Rows::new(-2, &["one", "two", "three"]);
        let b = bounds(-2, 0, 8);
        assert_eq!(go(Motion::WordForward, (-2, 0), &rows, b), (-1, 0));
        assert_eq!(go(Motion::WordBackward, (-1, 0), &rows, b), (-2, 0));
        assert_eq!(go(Motion::WordEnd, (-2, 2), &rows, b), (-1, 2));
    }

    /// A row the client holds no text for ends the scan, so the cursor
    /// stays inside the served region the user can actually see.
    #[test]
    fn word_motions_stop_at_the_edge_of_the_served_rows() {
        let mut map = Rows::new(-3, &["alpha"]).0;
        map.insert(0, "omega".chars().collect());
        let rows = Rows(map);
        let b = bounds(-3, 0, 6);
        assert_eq!(go(Motion::WordForward, (-3, 0), &rows, b), (-3, 0));
        assert_eq!(go(Motion::WordBackward, (0, 0), &rows, b), (0, 0));
    }

    /// `G` then `b` after a deep-scrollback entry: the gap between the
    /// cursor and the cache is unserved, and the scan must refuse it at
    /// the first row rather than walking every cell of every row in it.
    #[test]
    fn a_word_motion_never_walks_an_unserved_gap() {
        let rows = Counted(Rows::new(0, &["omega"]), Cell::new(0));
        let b = bounds(-100_000, 0, 80);

        assert_eq!(resolve(Motion::WordBackward, (0, 0), b, &rows), (0, 0));

        assert!(
            rows.reads() <= 2,
            "read {} rows to refuse one step",
            rows.reads()
        );
    }

    /// Nothing ahead: the cursor holds position rather than parking on
    /// the blank cell at the far corner of the buffer.
    #[test]
    fn word_motions_at_the_ends_of_the_buffer_stay_put() {
        let rows = Rows::new(0, &["only"]);
        let b = bounds(0, 0, 8);
        assert_eq!(go(Motion::WordForward, (0, 2), &rows, b), (0, 2));
        assert_eq!(go(Motion::WordEnd, (0, 3), &rows, b), (0, 3));
        assert_eq!(go(Motion::WordBackward, (0, 0), &rows, b), (0, 0));
        // The very last cell of the very last row: every step fails.
        assert_eq!(go(Motion::WordForward, (0, 7), &rows, b), (0, 7));
        assert_eq!(go(Motion::WordEnd, (0, 7), &rows, b), (0, 7));
    }

    /// A buffer with no text at all cannot move a word motion anywhere,
    /// and must not spin looking.
    #[test]
    fn word_motions_over_an_empty_buffer_do_not_move() {
        let rows = Rows::new(0, &["", "", ""]);
        let b = bounds(-1, 1, 4);
        for motion in [Motion::WordForward, Motion::WordBackward, Motion::WordEnd] {
            assert_eq!(go(motion, (0, 1), &rows, b), (0, 1), "{motion:?}");
        }
    }

    #[test]
    fn e_walks_word_ends() {
        let rows = Rows::new(0, &["foo bar"]);
        let b = bounds(0, 0, 8);
        assert_eq!(go(Motion::WordEnd, (0, 0), &rows, b), (0, 2));
        assert_eq!(go(Motion::WordEnd, (0, 2), &rows, b), (0, 6));
    }

    #[test]
    fn b_walks_back_to_word_starts() {
        let rows = Rows::new(0, &["foo bar  baz"]);
        let b = bounds(0, 0, 12);
        assert_eq!(go(Motion::WordBackward, (0, 11), &rows, b), (0, 9));
        assert_eq!(go(Motion::WordBackward, (0, 9), &rows, b), (0, 4));
        assert_eq!(
            go(Motion::WordBackward, (0, 6), &rows, b),
            (0, 4),
            "mid-word"
        );
    }

    /// Word motions never leave the addressable range, whatever the text
    /// looks like: the clamp is the scan's stepping bound, not a filter
    /// applied afterward.
    #[test]
    fn word_motions_stay_inside_the_addressable_range() {
        let rows = Rows::new(-8, &["a b", "c d", "e f", "g h"]);
        let b = bounds(-6, -5, 4);
        for motion in [Motion::WordForward, Motion::WordBackward, Motion::WordEnd] {
            for start in [(-6i64, 0u16), (-6, 3), (-5, 0), (-5, 3)] {
                let (row, col) = go(motion, start, &rows, b);
                assert!(
                    (-6..=-5).contains(&row) && col < 4,
                    "{motion:?} from {start:?} escaped to ({row}, {col})"
                );
            }
        }
    }
}
