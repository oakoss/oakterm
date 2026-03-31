//! Scrollback search engine with literal and regex support.

use crate::scroll::HotBuffer;
use std::io;

/// Maximum number of matches stored before search stops collecting.
const MAX_MATCHES: usize = 100_000;

/// A single match within the scrollback buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    /// Row index in the hot buffer (0 = oldest).
    pub row: usize,
    /// Byte offset of match start within the row's text.
    pub start: usize,
    /// Byte offset of match end within the row's text.
    pub end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Case-insensitive unless the query contains uppercase.
    SmartCase,
    CaseSensitive,
    Regex,
}

pub struct SearchEngine {
    regex: regex::Regex,
    query_empty: bool,
    matches: Vec<SearchMatch>,
    active: Option<usize>,
    capped: bool,
}

impl SearchEngine {
    /// Compile a search pattern from the given query and mode.
    ///
    /// All modes compile to a `regex::Regex` so that byte offsets from
    /// `find_iter` are always valid positions in the original text.
    /// Literal patterns are escaped with `regex::escape()`.
    ///
    /// # Errors
    ///
    /// Returns an error if `mode` is `Regex` and the query is not valid.
    pub fn new(query: &str, mode: SearchMode) -> io::Result<Self> {
        let re_pattern = match mode {
            SearchMode::SmartCase => {
                let escaped = regex::escape(query);
                let case_sensitive = query.chars().any(char::is_uppercase);
                if case_sensitive {
                    escaped
                } else {
                    format!("(?i){escaped}")
                }
            }
            SearchMode::CaseSensitive => regex::escape(query),
            SearchMode::Regex => query.to_string(),
        };
        let regex = regex::Regex::new(&re_pattern)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        Ok(Self {
            query_empty: query.is_empty(),
            regex,
            matches: Vec::new(),
            active: None,
            capped: false,
        })
    }

    /// Scan the hot buffer and populate matches. Selects the last match
    /// (nearest to viewport bottom) as active.
    pub fn search(&mut self, buffer: &HotBuffer) {
        self.matches.clear();
        self.active = None;
        self.capped = false;
        if buffer.is_empty() || self.query_empty {
            return;
        }
        for row_idx in 0..buffer.len() {
            if self.matches.len() >= MAX_MATCHES {
                self.capped = true;
                break;
            }
            let Some(row) = buffer.get(row_idx) else {
                continue;
            };
            let text = row.text();
            self.find_in_text(row_idx, &text);
        }
        // Default active: last match (nearest to viewport bottom).
        if !self.matches.is_empty() {
            self.active = Some(self.matches.len() - 1);
        }
    }

    fn find_in_text(&mut self, row_idx: usize, text: &str) {
        for m in self.regex.find_iter(text) {
            if m.start() == m.end() {
                continue;
            }
            self.matches.push(SearchMatch {
                row: row_idx,
                start: m.start(),
                end: m.end(),
            });
            if self.matches.len() >= MAX_MATCHES {
                self.capped = true;
                return;
            }
        }
    }

    #[must_use]
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// True if the search hit the match cap and stopped early.
    #[must_use]
    pub fn is_capped(&self) -> bool {
        self.capped
    }

    #[must_use]
    pub fn active_index(&self) -> Option<usize> {
        self.active
    }

    #[must_use]
    pub fn active_match(&self) -> Option<&SearchMatch> {
        self.active.and_then(|i| self.matches.get(i))
    }

    #[must_use]
    pub fn matches(&self) -> &[SearchMatch] {
        &self.matches
    }

    /// Advance to the next match (wraps around).
    pub fn next(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.active = Some(match self.active {
            Some(i) if i + 1 < self.matches.len() => i + 1,
            _ => 0,
        });
    }

    /// Go to the previous match (wraps around).
    pub fn prev(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.active = Some(match self.active {
            Some(0) | None => self.matches.len() - 1,
            Some(i) => i - 1,
        });
    }

    /// Return matches whose row index falls within `[row_start, row_end)`.
    #[must_use]
    pub fn matches_in_range(&self, row_start: usize, row_end: usize) -> Vec<&SearchMatch> {
        self.matches
            .iter()
            .filter(|m| m.row >= row_start && m.row < row_end)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::row::Row;

    fn buffer_from_strings(lines: &[&str]) -> HotBuffer {
        let mut buf = HotBuffer::new(1024 * 1024);
        for line in lines {
            let mut row = Row::new(line.len().max(1));
            for (i, ch) in line.chars().enumerate() {
                if i < row.cells.len() {
                    row.cells[i].codepoint = ch;
                }
            }
            let _ = buf.push(row);
        }
        buf
    }

    #[test]
    fn literal_search_finds_matches() {
        let buf = buffer_from_strings(&["hello world", "hello again", "no match here"]);
        let mut engine = SearchEngine::new("hello", SearchMode::SmartCase).unwrap();
        engine.search(&buf);
        assert_eq!(engine.match_count(), 2);
        assert_eq!(engine.matches()[0].row, 0);
        assert_eq!(engine.matches()[1].row, 1);
    }

    #[test]
    fn smart_case_insensitive_by_default() {
        let buf = buffer_from_strings(&["Hello World", "HELLO again"]);
        let mut engine = SearchEngine::new("hello", SearchMode::SmartCase).unwrap();
        engine.search(&buf);
        assert_eq!(engine.match_count(), 2);
    }

    #[test]
    fn smart_case_sensitive_with_uppercase() {
        let buf = buffer_from_strings(&["Hello World", "hello again"]);
        let mut engine = SearchEngine::new("Hello", SearchMode::SmartCase).unwrap();
        engine.search(&buf);
        assert_eq!(engine.match_count(), 1);
        assert_eq!(engine.matches()[0].row, 0);
    }

    #[test]
    fn case_sensitive_mode() {
        let buf = buffer_from_strings(&["Hello", "hello"]);
        let mut engine = SearchEngine::new("hello", SearchMode::CaseSensitive).unwrap();
        engine.search(&buf);
        assert_eq!(engine.match_count(), 1);
        assert_eq!(engine.matches()[0].row, 1);
    }

    #[test]
    fn regex_search() {
        let buf = buffer_from_strings(&["error: bad input", "warning: low", "error: timeout"]);
        let mut engine = SearchEngine::new("error:.*", SearchMode::Regex).unwrap();
        engine.search(&buf);
        assert_eq!(engine.match_count(), 2);
        assert_eq!(engine.matches()[0].row, 0);
        assert_eq!(engine.matches()[1].row, 2);
    }

    #[test]
    fn invalid_regex_returns_error() {
        assert!(SearchEngine::new("[invalid", SearchMode::Regex).is_err());
    }

    #[test]
    fn empty_query_no_matches() {
        let buf = buffer_from_strings(&["hello"]);
        let mut engine = SearchEngine::new("", SearchMode::SmartCase).unwrap();
        engine.search(&buf);
        assert_eq!(engine.match_count(), 0);
    }

    #[test]
    fn no_matches_in_buffer() {
        let buf = buffer_from_strings(&["hello world"]);
        let mut engine = SearchEngine::new("xyz", SearchMode::SmartCase).unwrap();
        engine.search(&buf);
        assert_eq!(engine.match_count(), 0);
        assert!(engine.active_match().is_none());
    }

    #[test]
    fn active_defaults_to_last_match() {
        let buf = buffer_from_strings(&["aaa", "bbb", "aaa"]);
        let mut engine = SearchEngine::new("aaa", SearchMode::SmartCase).unwrap();
        engine.search(&buf);
        assert_eq!(engine.match_count(), 2);
        // Active should be the last match (nearest to bottom).
        assert_eq!(engine.active_index(), Some(1));
        assert_eq!(engine.active_match().unwrap().row, 2);
    }

    #[test]
    fn next_wraps_around() {
        let buf = buffer_from_strings(&["a", "b", "a"]);
        let mut engine = SearchEngine::new("a", SearchMode::SmartCase).unwrap();
        engine.search(&buf);
        assert_eq!(engine.active_index(), Some(1)); // last match
        engine.next();
        assert_eq!(engine.active_index(), Some(0)); // wraps to first
        engine.next();
        assert_eq!(engine.active_index(), Some(1)); // back to last
    }

    #[test]
    fn prev_wraps_around() {
        let buf = buffer_from_strings(&["a", "b", "a"]);
        let mut engine = SearchEngine::new("a", SearchMode::SmartCase).unwrap();
        engine.search(&buf);
        assert_eq!(engine.active_index(), Some(1));
        engine.prev();
        assert_eq!(engine.active_index(), Some(0));
        engine.prev();
        assert_eq!(engine.active_index(), Some(1)); // wraps to last
    }

    #[test]
    fn next_prev_on_empty_is_noop() {
        let buf = buffer_from_strings(&["hello"]);
        let mut engine = SearchEngine::new("xyz", SearchMode::SmartCase).unwrap();
        engine.search(&buf);
        engine.next();
        engine.prev();
        assert!(engine.active_match().is_none());
    }

    #[test]
    fn matches_in_range() {
        let buf = buffer_from_strings(&["aa", "bb", "aa", "bb", "aa"]);
        let mut engine = SearchEngine::new("aa", SearchMode::SmartCase).unwrap();
        engine.search(&buf);
        assert_eq!(engine.match_count(), 3);
        let visible = engine.matches_in_range(1, 4);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].row, 2);
    }

    #[test]
    fn multiple_matches_per_row() {
        let buf = buffer_from_strings(&["abcabc"]);
        let mut engine = SearchEngine::new("abc", SearchMode::SmartCase).unwrap();
        engine.search(&buf);
        assert_eq!(engine.match_count(), 2);
        assert_eq!(engine.matches()[0].start, 0);
        assert_eq!(engine.matches()[0].end, 3);
        assert_eq!(engine.matches()[1].start, 3);
        assert_eq!(engine.matches()[1].end, 6);
    }

    #[test]
    fn byte_offsets_correct_for_unicode() {
        let buf = buffer_from_strings(&["café error"]);
        let mut engine = SearchEngine::new("error", SearchMode::SmartCase).unwrap();
        engine.search(&buf);
        assert_eq!(engine.match_count(), 1);
        // 'é' is 2 bytes in UTF-8, so "café " is 6 bytes
        assert_eq!(engine.matches()[0].start, 6);
        assert_eq!(engine.matches()[0].end, 11);
    }

    #[test]
    fn empty_buffer_no_crash() {
        let buf = HotBuffer::new(1024);
        let mut engine = SearchEngine::new("test", SearchMode::SmartCase).unwrap();
        engine.search(&buf);
        assert_eq!(engine.match_count(), 0);
    }

    #[test]
    fn match_count_capped() {
        // Each row has 10 'a' chars = 10 matches for regex "a".
        // 10_001 rows * 10 matches = 100_010 > MAX_MATCHES.
        let mut buf = HotBuffer::new(512 * 1024 * 1024);
        for _ in 0..10_001 {
            let mut row = Row::new(10);
            for cell in &mut row.cells {
                cell.codepoint = 'a';
            }
            let _ = buf.push(row);
        }
        let mut engine = SearchEngine::new("a", SearchMode::SmartCase).unwrap();
        engine.search(&buf);
        assert_eq!(engine.match_count(), super::MAX_MATCHES);
        assert!(engine.is_capped());
    }
}
