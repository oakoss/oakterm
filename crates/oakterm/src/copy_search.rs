//! Copy-mode search (Spec-0008 Search): the query, the bar's keys, and
//! match finding over the row text yank and motions read. Pure: rows
//! arrive through [`RowText`], and fetching more of them is the caller's.

use crate::copy_keys::{CopyKey, CopyPress};
use crate::copy_motion::RowText;
use oakterm_config::CopyModePreset;
use oakterm_terminal::search::{SearchEngine, SearchMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchDirection {
    /// Toward newer rows (down).
    Forward,
    /// Toward older rows (up).
    Backward,
}

impl SearchDirection {
    #[must_use]
    pub(crate) fn reversed(self) -> Self {
        match self {
            Self::Forward => Self::Backward,
            Self::Backward => Self::Forward,
        }
    }
}

/// What a key does while the search bar is open; the preset's table is
/// bypassed then (Spec-0008 Search).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BarKey {
    Insert(char),
    Backspace,
    /// Close the bar, leaving the cursor on the match.
    Accept,
    /// Close the bar and return the cursor to where the search started.
    Cancel,
    /// The preset's start key again: search on in that direction.
    Repeat(SearchDirection),
    Ignore,
}

/// What a bar key does. `typed` is the text the layout composed, which
/// a printable key inserts: Option compositions such as `@` on many
/// layouts arrive as Alt chords the tables normalize away.
#[must_use]
pub(crate) fn bar_key(preset: CopyModePreset, press: CopyPress, typed: Option<char>) -> BarKey {
    let CopyPress::Key(key) = press else {
        return BarKey::Ignore;
    };
    match key {
        CopyKey::Enter => BarKey::Accept,
        CopyKey::Escape => BarKey::Cancel,
        CopyKey::Backspace => BarKey::Backspace,
        CopyKey::Char(c) => BarKey::Insert(typed.unwrap_or(c)),
        CopyKey::Alt { .. } => typed.map_or(BarKey::Ignore, BarKey::Insert),
        CopyKey::Ctrl(c) => match (preset, c) {
            (CopyModePreset::Emacs, 's') => BarKey::Repeat(SearchDirection::Forward),
            (CopyModePreset::Emacs, 'r') | (CopyModePreset::Basic, 'f') => {
                BarKey::Repeat(SearchDirection::Backward)
            }
            _ => BarKey::Ignore,
        },
        CopyKey::Named { .. } => BarKey::Ignore,
    }
}

/// The one printable character a key event's text holds, if any.
#[must_use]
pub(crate) fn typed_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let c = chars.next()?;
    (chars.next().is_none() && !c.is_control()).then_some(c)
}

/// A match on one row: columns `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Match {
    pub(crate) row: i64,
    pub(crate) start: u16,
    pub(crate) end: u16,
}

/// The query compiled under the daemon search's rule, literal and smart
/// case. `None` for an empty query, which matches nothing.
#[must_use]
pub(crate) fn matcher(query: &str) -> Option<SearchEngine> {
    if query.is_empty() {
        return None;
    }
    SearchEngine::new(query, SearchMode::SmartCase).ok()
}

/// Every match on one row, in column order.
#[must_use]
pub(crate) fn row_matches(engine: &SearchEngine, row: i64, chars: &[char]) -> Vec<Match> {
    let text: String = chars.iter().collect();
    let col = |c: usize| u16::try_from(c).unwrap_or(u16::MAX);
    engine
        .char_spans(&text)
        .map(|(start, end)| Match {
            row,
            start: col(start),
            end: col(end),
        })
        .collect()
}

/// The outcome of scanning for the next match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Find {
    Found(Match),
    /// The scan reached a row the caller holds no text for; fetch toward
    /// it and scan again from there.
    Missing(i64),
    /// No match up to the end of the rows in that direction.
    Exhausted,
}

/// Scan from `from` in `direction` for the nearest match, over rows
/// `[first, last]`. On `from`'s own row a match must start after it
/// (forward) or before it (backward), or at it when `inclusive`, so a
/// repeated search moves on while an edited query can stay put.
pub(crate) fn find(
    rows: &impl RowText,
    engine: &SearchEngine,
    from: (i64, u16),
    direction: SearchDirection,
    inclusive: bool,
    (first, last): (i64, i64),
) -> Find {
    let mut row = from.0;
    while (first..=last).contains(&row) {
        let Some(chars) = rows.row(row) else {
            return Find::Missing(row);
        };
        let matches = row_matches(engine, row, &chars);
        let beyond = |m: &&Match| {
            row != from.0
                || match (direction, inclusive) {
                    (SearchDirection::Forward, true) => m.start >= from.1,
                    (SearchDirection::Forward, false) => m.start > from.1,
                    (SearchDirection::Backward, true) => m.start <= from.1,
                    (SearchDirection::Backward, false) => m.start < from.1,
                }
        };
        let hit = match direction {
            SearchDirection::Forward => matches.iter().find(beyond),
            SearchDirection::Backward => matches.iter().rev().find(beyond),
        };
        if let Some(found) = hit {
            return Find::Found(*found);
        }
        row = match direction {
            SearchDirection::Forward => row + 1,
            SearchDirection::Backward => row - 1,
        };
    }
    Find::Exhausted
}

/// Every match in the held rows of `[first, last]`, in reading order.
/// Rows without text are skipped.
#[must_use]
pub(crate) fn held_matches(
    rows: &impl RowText,
    engine: &SearchEngine,
    (first, last): (i64, i64),
) -> Vec<Match> {
    (first..=last)
        .filter_map(|row| rows.row(row).map(|chars| row_matches(engine, row, &chars)))
        .flatten()
        .collect()
}

/// A search's state, kept after the bar closes so `n` and the
/// highlights still have a query (Spec-0008 Search).
pub(crate) struct SearchState {
    query: String,
    engine: Option<SearchEngine>,
    pub(crate) direction: SearchDirection,
    /// Whether the bar is open and keys edit the query.
    pub(crate) editing: bool,
    /// Cursor when the search started; `Escape` returns here.
    pub(crate) origin: (i64, u16),
}

impl SearchState {
    #[must_use]
    pub(crate) fn new(direction: SearchDirection, origin: (i64, u16)) -> Self {
        Self {
            query: String::new(),
            engine: None,
            direction,
            editing: true,
            origin,
        }
    }

    #[must_use]
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    #[must_use]
    pub(crate) fn engine(&self) -> Option<&SearchEngine> {
        self.engine.as_ref()
    }

    pub(crate) fn push(&mut self, c: char) {
        self.query.push(c);
        self.engine = matcher(&self.query);
    }

    pub(crate) fn pop(&mut self) {
        self.query.pop();
        self.engine = matcher(&self.query);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BarKey, Find, Match, SearchDirection, bar_key, find, held_matches, matcher, row_matches,
    };
    use crate::copy_keys::{CopyKey, CopyPress};
    use crate::copy_motion::RowText;
    use oakterm_config::CopyModePreset;
    use std::collections::HashMap;

    struct Rows(HashMap<i64, Vec<char>>);

    impl RowText for Rows {
        fn row(&self, row: i64) -> Option<Vec<char>> {
            self.0.get(&row).cloned()
        }
    }

    fn rows(lines: &[(i64, &str)]) -> Rows {
        Rows(
            lines
                .iter()
                .map(|(row, text)| (*row, text.chars().collect()))
                .collect(),
        )
    }

    fn at(row: i64, start: u16, end: u16) -> Find {
        Find::Found(Match { row, start, end })
    }

    #[test]
    fn matching_is_literal_and_smart_case() {
        let lower = matcher("ab").expect("query");
        let chars: Vec<char> = "AB ab a.b".chars().collect();
        assert_eq!(row_matches(&lower, 0, &chars).len(), 2);
        let upper = matcher("AB").expect("query");
        assert_eq!(row_matches(&upper, 0, &chars).len(), 1);
        let dot = matcher("a.b").expect("query");
        assert_eq!(row_matches(&dot, 0, &chars).len(), 1, "literal, not regex");
        assert!(matcher("").is_none());
    }

    #[test]
    fn find_steps_past_the_cursor_and_crosses_rows_both_ways() {
        use SearchDirection::{Backward, Forward};
        let text = rows(&[(-2, "foo x foo"), (-1, "bar"), (0, "foo")]);
        let foo = matcher("foo").expect("query");
        let bounds = (-2, 0);
        assert_eq!(
            find(&text, &foo, (-2, 0), Forward, false, bounds),
            at(-2, 6, 9)
        );
        assert_eq!(
            find(&text, &foo, (-2, 0), Forward, true, bounds),
            at(-2, 0, 3)
        );
        assert_eq!(
            find(&text, &foo, (-2, 6), Forward, false, bounds),
            at(0, 0, 3)
        );
        assert_eq!(
            find(&text, &foo, (0, 0), Backward, false, bounds),
            at(-2, 6, 9)
        );
        assert_eq!(
            find(&text, &foo, (-2, 6), Backward, false, bounds),
            at(-2, 0, 3)
        );
        assert_eq!(
            find(&text, &foo, (-2, 6), Backward, true, bounds),
            at(-2, 6, 9),
            "an edited query stays on the match under the cursor"
        );
        assert_eq!(
            find(&text, &foo, (0, 0), Forward, false, bounds),
            Find::Exhausted
        );
    }

    #[test]
    fn find_stops_at_a_row_without_text() {
        let text = rows(&[(-1, "bar"), (0, "baz")]);
        let foo = matcher("foo").expect("query");
        assert_eq!(
            find(
                &text,
                &foo,
                (0, 0),
                SearchDirection::Backward,
                false,
                (i64::MIN, 0)
            ),
            Find::Missing(-2)
        );
        assert_eq!(
            find(
                &text,
                &foo,
                (0, 0),
                SearchDirection::Backward,
                false,
                (-1, 0)
            ),
            Find::Exhausted,
            "the oldest history row ends the scan"
        );
    }

    #[test]
    fn held_matches_list_every_match_in_reading_order() {
        let text = rows(&[(-1, "ab ab"), (0, "ab")]);
        let ab = matcher("ab").expect("query");
        let found: Vec<(i64, u16)> = held_matches(&text, &ab, (-3, 0))
            .into_iter()
            .map(|m| (m.row, m.start))
            .collect();
        assert_eq!(found, vec![(-1, 0), (-1, 3), (0, 0)]);
    }

    #[test]
    fn the_bar_reads_text_and_its_own_keys() {
        use CopyModePreset::{Basic, Emacs, Vim};
        let key = |k| CopyPress::Key(k);
        assert_eq!(
            bar_key(Vim, key(CopyKey::Char('n')), None),
            BarKey::Insert('n')
        );
        assert_eq!(bar_key(Vim, key(CopyKey::Enter), None), BarKey::Accept);
        assert_eq!(bar_key(Vim, key(CopyKey::Escape), None), BarKey::Cancel);
        assert_eq!(
            bar_key(Vim, key(CopyKey::Backspace), None),
            BarKey::Backspace
        );
        assert_eq!(
            bar_key(Emacs, key(CopyKey::Ctrl('s')), None),
            BarKey::Repeat(SearchDirection::Forward)
        );
        assert_eq!(
            bar_key(Emacs, key(CopyKey::Ctrl('r')), None),
            BarKey::Repeat(SearchDirection::Backward)
        );
        assert_eq!(
            bar_key(Basic, key(CopyKey::Ctrl('f')), None),
            BarKey::Repeat(SearchDirection::Backward)
        );
        assert_eq!(bar_key(Vim, key(CopyKey::Ctrl('f')), None), BarKey::Ignore);
        assert_eq!(bar_key(Vim, CopyPress::ModifierOnly, None), BarKey::Ignore);
    }

    /// Option compositions reach the tables as Alt chords; the bar types
    /// the composed character instead.
    #[test]
    fn the_bar_types_composed_text() {
        let alt = CopyPress::Key(CopyKey::Alt {
            ch: '2',
            shift: false,
        });
        assert_eq!(
            bar_key(CopyModePreset::Vim, alt, Some('@')),
            BarKey::Insert('@')
        );
        assert_eq!(bar_key(CopyModePreset::Vim, alt, None), BarKey::Ignore);
        assert_eq!(super::typed_char("é"), Some('é'));
        assert_eq!(super::typed_char("\r"), None, "controls are not text");
        assert_eq!(super::typed_char("ab"), None);
    }
}
