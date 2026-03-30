//! Hot ring buffer for recent scrollback rows.
//!
//! Bounded by byte count (`max_bytes`). When the limit is exceeded,
//! oldest rows are pruned from the front with 10% headroom. Uses
//! `VecDeque<Row>` for O(1) push/pop/index. See Spec-0004.

use crate::grid::cell::Cell;
use crate::grid::row::Row;
use std::collections::VecDeque;
use std::ops::RangeBounds;

/// Default scrollback limit: 50 MB.
const DEFAULT_MAX_BYTES: usize = 50 * 1024 * 1024;

/// Bounded ring buffer holding recent scrollback rows.
pub struct HotBuffer {
    rows: VecDeque<Row>,
    max_bytes: usize,
    used_bytes: usize,
}

impl HotBuffer {
    /// Create a buffer with the given byte limit.
    #[must_use]
    pub fn new(max_bytes: usize) -> Self {
        Self {
            rows: VecDeque::new(),
            max_bytes,
            used_bytes: 0,
        }
    }

    /// Push a row into the buffer. Returns pruned rows (oldest first)
    /// if the limit was exceeded. The caller can archive these before
    /// dropping them.
    pub fn push(&mut self, row: Row) -> Vec<Row> {
        self.used_bytes += row_byte_size(&row);
        self.rows.push_back(row);
        self.prune_if_needed()
    }

    /// Number of rows in the buffer.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Access a row by index (0 = oldest row in the buffer).
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&Row> {
        self.rows.get(index)
    }

    /// Iterate over a range of rows.
    pub fn range(&self, range: impl RangeBounds<usize>) -> impl Iterator<Item = &Row> {
        self.rows.range(range)
    }

    /// Iterate over all rows (oldest first).
    pub fn iter(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter()
    }

    /// Estimated memory usage in bytes.
    #[must_use]
    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    /// Maximum capacity in bytes.
    #[must_use]
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Update the byte limit. Returns pruned rows if the new limit is smaller.
    pub fn set_max_bytes(&mut self, max_bytes: usize) -> Vec<Row> {
        self.max_bytes = max_bytes;
        self.prune_if_needed()
    }

    /// Prune oldest rows until `used_bytes` is below 90% of `max_bytes`.
    fn prune_if_needed(&mut self) -> Vec<Row> {
        if self.used_bytes <= self.max_bytes {
            return Vec::new();
        }
        let target = self.max_bytes * 9 / 10;
        let mut pruned = Vec::new();
        while self.used_bytes > target {
            let Some(row) = self.rows.pop_front() else {
                break;
            };
            let size = row_byte_size(&row);
            debug_assert!(
                self.used_bytes >= size,
                "used_bytes underflow: {} < {size}",
                self.used_bytes
            );
            self.used_bytes = self.used_bytes.saturating_sub(size);
            pruned.push(row);
        }
        pruned
    }
}

impl Default for HotBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_BYTES)
    }
}

/// Estimate the memory footprint of a row (struct + cell heap data).
///
/// Intentionally excludes small heap contributions from `MarkMetadata`
/// strings and `CellExtra` (graphemes, underline color, hyperlinks)
/// since these are rare. The estimate is sufficient for pruning decisions.
fn row_byte_size(row: &Row) -> usize {
    std::mem::size_of::<Row>() + row.cells.capacity() * std::mem::size_of::<Cell>()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row(cols: usize) -> Row {
        Row::new(cols)
    }

    #[test]
    fn push_and_access() {
        let mut buf = HotBuffer::new(1024 * 1024);
        buf.push(make_row(80));
        buf.push(make_row(80));
        assert_eq!(buf.len(), 2);
        assert!(buf.get(0).is_some());
        assert!(buf.get(1).is_some());
        assert!(buf.get(2).is_none());
    }

    #[test]
    fn empty_buffer() {
        let buf = HotBuffer::new(1024);
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
        assert!(buf.get(0).is_none());
    }

    #[test]
    fn byte_tracking() {
        let mut buf = HotBuffer::new(1024 * 1024);
        assert_eq!(buf.used_bytes(), 0);
        buf.push(make_row(80));
        assert!(buf.used_bytes() > 0);
        let size_one = buf.used_bytes();
        buf.push(make_row(80));
        assert_eq!(buf.used_bytes(), size_one * 2);
    }

    #[test]
    fn prune_on_overflow() {
        let row_size = row_byte_size(&make_row(80));
        let max = row_size * 5;
        let mut buf = HotBuffer::new(max);
        for _ in 0..10 {
            buf.push(make_row(80));
        }
        assert!(buf.used_bytes() <= max);
        assert!(buf.len() < 10);
    }

    #[test]
    fn prune_headroom() {
        let row_size = row_byte_size(&make_row(80));
        let max = row_size * 10;
        let mut buf = HotBuffer::new(max);
        for _ in 0..11 {
            buf.push(make_row(80));
        }
        let target = max * 9 / 10;
        assert!(buf.used_bytes() <= target);
    }

    #[test]
    fn set_max_bytes_triggers_prune() {
        let mut buf = HotBuffer::new(1024 * 1024);
        for _ in 0..100 {
            buf.push(make_row(80));
        }
        let rows_before = buf.len();
        let row_size = row_byte_size(&make_row(80));
        let pruned = buf.set_max_bytes(row_size * 10);
        assert!(!pruned.is_empty());
        assert!(buf.len() < rows_before);
        assert!(buf.used_bytes() <= row_size * 10);
    }

    #[test]
    fn oldest_rows_pruned_first() {
        let row_size = row_byte_size(&make_row(80));
        let max = row_size * 3;
        let mut buf = HotBuffer::new(max);

        for i in 0..5_u32 {
            let mut row = make_row(80);
            row.cells[0].codepoint = char::from_u32(u32::from(b'A') + i).unwrap();
            buf.push(row);
        }

        let first = buf.get(0).unwrap().cells[0].codepoint;
        assert!(first > 'A', "oldest rows should be pruned, got '{first}'");
    }

    #[test]
    fn default_max_bytes() {
        let buf = HotBuffer::default();
        assert_eq!(buf.max_bytes(), 50 * 1024 * 1024);
    }

    #[test]
    fn push_returns_pruned_rows() {
        let row_size = row_byte_size(&make_row(80));
        let max = row_size * 3;
        let mut buf = HotBuffer::new(max);
        for _ in 0..3 {
            assert!(buf.push(make_row(80)).is_empty());
        }
        let pruned = buf.push(make_row(80));
        assert!(!pruned.is_empty());
    }

    #[test]
    fn iter_returns_all_rows() {
        let mut buf = HotBuffer::new(1024 * 1024);
        for _ in 0..5 {
            buf.push(make_row(80));
        }
        assert_eq!(buf.iter().count(), 5);
    }

    #[test]
    fn range_returns_subset() {
        let mut buf = HotBuffer::new(1024 * 1024);
        for _ in 0..10 {
            buf.push(make_row(80));
        }
        assert_eq!(buf.range(2..5).count(), 3);
    }
}
