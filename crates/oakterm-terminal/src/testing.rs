//! Test helpers for screen buffer assertions.
//!
//! Provides factory functions and assertion helpers for writing concise
//! VT parser and screen buffer tests.

use crate::grid::cell::{CellFlags, Color};
use crate::grid::row::Row;
use crate::grid::{Grid, ScreenSet};

/// Create a `Grid` with the given dimensions for testing.
#[must_use]
pub fn test_grid(cols: u16, rows: u16) -> Grid {
    Grid::new(cols, rows)
}

/// Create a `ScreenSet` with the given dimensions for testing.
#[must_use]
pub fn test_screen(cols: u16, rows: u16) -> ScreenSet {
    ScreenSet::new(cols, rows)
}

/// Extract the text content of a row as a String.
/// Null bytes (`'\0'`) are converted to spaces for readability,
/// then trailing spaces are trimmed.
#[must_use]
pub fn row_text(row: &Row) -> String {
    let s: String = row
        .cells
        .iter()
        .map(|c| {
            if c.codepoint == '\0' {
                ' '
            } else {
                c.codepoint
            }
        })
        .collect();
    s.trim_end().to_string()
}

/// Assert that a row's text content matches the expected string.
///
/// # Panics
/// Panics if the row text (trimmed) does not match `expected`.
pub fn assert_row_text(grid: &Grid, row: u16, expected: &str) {
    let actual = row_text(&grid.lines[row as usize]);
    assert_eq!(
        actual, expected,
        "row {row}: expected {expected:?}, got {actual:?}"
    );
}

/// Assert the cursor is at the given position.
///
/// # Panics
/// Panics if the cursor position does not match.
pub fn assert_cursor_at(grid: &Grid, row: u16, col: u16) {
    assert_eq!(
        (grid.cursor.row, grid.cursor.col),
        (row, col),
        "cursor: expected ({row}, {col}), got ({}, {})",
        grid.cursor.row,
        grid.cursor.col
    );
}

/// Assert a cell's foreground color.
///
/// # Panics
/// Panics if the cell's fg color does not match.
pub fn assert_cell_fg(grid: &Grid, row: u16, col: u16, expected: Color) {
    let cell = &grid.lines[row as usize].cells[col as usize];
    assert_eq!(
        cell.fg, expected,
        "cell ({row}, {col}) fg: expected {expected:?}, got {:?}",
        cell.fg
    );
}

/// Assert a cell has specific flags set.
///
/// # Panics
/// Panics if the cell does not contain the expected flags.
pub fn assert_cell_flags(grid: &Grid, row: u16, col: u16, expected: CellFlags) {
    let cell = &grid.lines[row as usize].cells[col as usize];
    assert!(
        cell.flags.contains(expected),
        "cell ({row}, {col}) flags: expected {expected:?} to be set, got {:?}",
        cell.flags
    );
}

/// Write a string of characters into a grid row starting at a column.
/// Sets each cell's codepoint and advances. No wrapping, no VT processing.
///
/// # Panics
/// Panics if the text extends beyond the row width.
pub fn write_raw(grid: &mut Grid, row: u16, col: u16, text: &str) {
    let line = &mut grid.lines[row as usize];
    let end = col as usize + text.chars().count();
    assert!(
        end <= line.cells.len(),
        "write_raw: text extends to column {end} but row has only {} columns",
        line.cells.len()
    );
    for (i, ch) in text.chars().enumerate() {
        line.cells[col as usize + i].codepoint = ch;
    }
    let seqno = grid.next_seqno();
    grid.lines[row as usize].seqno = seqno;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grid_factory() {
        let grid = test_grid(80, 24);
        assert_eq!(grid.cols, 80);
        assert_eq!(grid.rows, 24);
    }

    #[test]
    fn test_row_text_empty() {
        let grid = test_grid(10, 1);
        assert_row_text(&grid, 0, "");
    }

    #[test]
    fn test_row_text_with_content() {
        let mut grid = test_grid(10, 1);
        write_raw(&mut grid, 0, 0, "hello");
        assert_row_text(&grid, 0, "hello");
    }

    #[test]
    fn test_cursor_at() {
        let grid = test_grid(80, 24);
        assert_cursor_at(&grid, 0, 0);
    }

    #[test]
    fn test_write_raw_marks_dirty() {
        let mut grid = test_grid(80, 24);
        let before = grid.seqno;
        write_raw(&mut grid, 0, 0, "test");
        assert_eq!(grid.dirty_rows(before), vec![0]);
    }

    #[test]
    fn test_cell_fg_assertion() {
        let mut grid = test_grid(10, 1);
        grid.lines[0].cells[0].fg = Color::Rgb(255, 0, 0);
        assert_cell_fg(&grid, 0, 0, Color::Rgb(255, 0, 0));
    }

    #[test]
    fn test_cell_flags_assertion() {
        let mut grid = test_grid(10, 1);
        grid.lines[0].cells[0].flags.insert(CellFlags::BOLD);
        assert_cell_flags(&grid, 0, 0, CellFlags::BOLD);
    }
}
