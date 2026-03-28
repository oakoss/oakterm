use super::*;
use cell::{Cell, CellFlags, Color, NamedColor, UnderlineStyle, WideState};
use cursor::CursorStyle;
use row::{Direction, Row, RowFlags, SemanticMark};
use selection::{AnchorSide, SelectionAnchor};

#[test]
fn cell_default_is_empty() {
    let cell = Cell::default();
    assert_eq!(cell.codepoint, '\0');
    assert_eq!(cell.fg, Color::Default);
    assert_eq!(cell.bg, Color::Default);
    assert!(!cell.has_style());
    assert_eq!(cell.wide, WideState::Narrow);
}

#[test]
fn cell_has_style_detects_flags() {
    let mut cell = Cell::default();
    assert!(!cell.has_style());

    cell.flags.insert(CellFlags::BOLD);
    assert!(cell.has_style());
}

#[test]
fn cell_has_style_detects_color() {
    let cell = Cell {
        fg: Color::Named(NamedColor::Red),
        ..Cell::default()
    };
    assert!(cell.has_style());
}

#[test]
fn cell_has_style_detects_underline() {
    let cell = Cell {
        underline_style: UnderlineStyle::Curly,
        ..Cell::default()
    };
    assert!(cell.has_style());
}

#[test]
fn cell_reset_clears_all() {
    let mut cell = Cell {
        codepoint: 'A',
        fg: Color::Rgb(255, 0, 0),
        ..Cell::default()
    };
    cell.flags.insert(CellFlags::BOLD);
    cell.reset();
    assert_eq!(cell, Cell::default());
}

#[test]
fn cell_flags_union_and_contains() {
    let flags = CellFlags::BOLD.union(CellFlags::ITALIC);
    assert!(flags.contains(CellFlags::BOLD));
    assert!(flags.contains(CellFlags::ITALIC));
    assert!(!flags.contains(CellFlags::DIM));
}

#[test]
fn cell_flags_insert_and_remove() {
    let mut flags = CellFlags::empty();
    flags.insert(CellFlags::STRIKETHROUGH);
    assert!(flags.contains(CellFlags::STRIKETHROUGH));
    flags.remove(CellFlags::STRIKETHROUGH);
    assert!(!flags.contains(CellFlags::STRIKETHROUGH));
}

#[test]
fn color_named_and_indexed_are_distinct() {
    let named = Color::Named(NamedColor::Red);
    let indexed = Color::Indexed(1); // Same palette entry, different representation.
    assert_ne!(named, indexed);
}

#[test]
fn row_new_has_correct_width() {
    let row = Row::new(80);
    assert_eq!(row.cells.len(), 80);
    assert_eq!(row.direction, Direction::Ltr);
    assert_eq!(row.semantic_mark, SemanticMark::None);
}

#[test]
fn row_reset_clears_content() {
    let mut row = Row::new(10);
    row.cells[0].codepoint = 'X';
    row.cells[0].flags.insert(CellFlags::BOLD);
    row.flags.set_wrapped(true);
    row.reset(42);
    assert_eq!(row.cells[0].codepoint, '\0');
    assert!(!row.flags.wrapped());
    assert_eq!(row.seqno, 42);
}

#[test]
fn row_resize_extends_with_defaults() {
    let mut row = Row::new(10);
    row.cells[0].codepoint = 'A';
    row.resize(20);
    assert_eq!(row.cells.len(), 20);
    assert_eq!(row.cells[0].codepoint, 'A');
    assert_eq!(row.cells[19].codepoint, '\0');
}

#[test]
fn row_resize_truncates() {
    let mut row = Row::new(10);
    row.cells[9].codepoint = 'Z';
    row.resize(5);
    assert_eq!(row.cells.len(), 5);
}

#[test]
fn row_flags_optimization_hints() {
    let mut flags = RowFlags::default();
    assert!(!flags.has_styles());
    flags.mark_has_styles();
    assert!(flags.has_styles());
}

#[test]
fn grid_new_dimensions() {
    let grid = Grid::new(80, 24);
    assert_eq!(grid.cols, 80);
    assert_eq!(grid.rows, 24);
    assert_eq!(grid.lines.len(), 24);
    assert_eq!(grid.lines[0].cells.len(), 80);
}

#[test]
fn grid_tab_stops_default() {
    let grid = Grid::new(80, 24);
    assert!(!grid.tab_stops[0]);
    assert!(grid.tab_stops[8]);
    assert!(grid.tab_stops[16]);
    assert!(!grid.tab_stops[7]);
}

#[test]
fn grid_dirty_tracking() {
    let mut grid = Grid::new(80, 24);
    let initial = grid.seqno;

    grid.touch_row(5);
    assert_eq!(grid.dirty_rows(initial), vec![5]);

    grid.touch_row(10);
    assert_eq!(grid.dirty_rows(initial), vec![5, 10]);

    // Querying with the latest seqno returns nothing.
    assert!(grid.dirty_rows(grid.seqno).is_empty());
}

#[test]
fn grid_touch_all_marks_everything_dirty() {
    let mut grid = Grid::new(80, 24);
    let before = grid.seqno;
    grid.touch_all();
    let dirty = grid.dirty_rows(before);
    assert_eq!(dirty.len(), 24);
}

#[test]
fn grid_palette_base_colors() {
    let grid = Grid::new(80, 24);
    assert_eq!(grid.palette.len(), 256);
    assert_eq!(grid.palette[0], Rgb { r: 0, g: 0, b: 0 });
    assert_eq!(
        grid.palette[7],
        Rgb {
            r: 229,
            g: 229,
            b: 229
        }
    );
}

#[test]
fn grid_palette_color_cube() {
    let grid = Grid::new(80, 24);
    // Index 16: cube (0,0,0) = black.
    assert_eq!(grid.palette[16], Rgb { r: 0, g: 0, b: 0 });
    // Index 196: cube (5,0,0) = bright red.
    assert_eq!(grid.palette[196], Rgb { r: 255, g: 0, b: 0 });
    // Index 21: cube (0,0,5) = bright blue.
    assert_eq!(grid.palette[21], Rgb { r: 0, g: 0, b: 255 });
}

#[test]
fn grid_palette_grayscale() {
    let grid = Grid::new(80, 24);
    assert_eq!(grid.palette[232], Rgb { r: 8, g: 8, b: 8 });
    assert_eq!(
        grid.palette[255],
        Rgb {
            r: 238,
            g: 238,
            b: 238
        }
    );
}

#[test]
fn screen_set_starts_primary() {
    let ss = ScreenSet::new(80, 24);
    assert_eq!(ss.active_screen(), ScreenId::Primary);
    assert!(!ss.has_alternate());
}

#[test]
fn screen_set_alternate_lazy_allocation() {
    let mut ss = ScreenSet::new(80, 24);
    assert!(!ss.has_alternate());
    ss.enter_alternate();
    assert!(ss.has_alternate());
    assert_eq!(ss.active_screen(), ScreenId::Alternate);
}

#[test]
fn screen_set_exit_alternate() {
    let mut ss = ScreenSet::new(80, 24);
    ss.enter_alternate();
    ss.exit_alternate();
    assert_eq!(ss.active_screen(), ScreenId::Primary);
    assert!(ss.has_alternate());
}

#[test]
fn mode_flags_set_and_get() {
    let mut modes = ModeFlags::new();
    assert!(!modes.get(25));
    modes.set(25, true);
    assert!(modes.get(25));
    modes.set(25, false);
    assert!(!modes.get(25));
}

#[test]
fn mode_flags_out_of_range() {
    let mut modes = ModeFlags::new();
    modes.set(1000, true);
    assert!(!modes.get(1000));
}

#[test]
fn selection_anchor_scrollback() {
    let anchor = SelectionAnchor {
        row: -100,
        col: 5,
        side: AnchorSide::Left,
    };
    assert!(anchor.row < 0);
}

#[test]
fn cursor_default() {
    let cursor = cursor::Cursor::default();
    assert_eq!(cursor.row, 0);
    assert_eq!(cursor.col, 0);
    assert_eq!(cursor.style, CursorStyle::BlinkingBlock);
    assert!(cursor.visible);
    assert!(cursor.blink_override.is_none());
}

#[test]
fn grapheme_data_operations() {
    let mut g = cell::GraphemeData::default();
    assert!(g.is_empty());
    g.push('\u{0301}');
    assert!(!g.is_empty());
    assert_eq!(g.chars(), &['\u{0301}']);
    g.clear();
    assert!(g.is_empty());
}

// --- Grid resize tests ---

#[test]
fn resize_grow_cols() {
    let mut grid = Grid::new(80, 24);
    grid.resize(120, 24);
    assert_eq!(grid.cols, 120);
    assert_eq!(grid.rows, 24);
    assert_eq!(grid.lines.len(), 24);
    assert_eq!(grid.lines[0].cells.len(), 120);
}

#[test]
fn resize_shrink_cols() {
    let mut grid = Grid::new(80, 24);
    grid.cursor.col = 79;
    grid.resize(40, 24);
    assert_eq!(grid.cols, 40);
    assert_eq!(grid.lines[0].cells.len(), 40);
    assert_eq!(grid.cursor.col, 39, "cursor should clamp to new width");
}

#[test]
fn resize_grow_rows() {
    let mut grid = Grid::new(80, 24);
    grid.resize(80, 40);
    assert_eq!(grid.rows, 40);
    assert_eq!(grid.lines.len(), 40);
}

#[test]
fn resize_shrink_rows() {
    let mut grid = Grid::new(80, 24);
    grid.cursor.row = 23;
    grid.resize(80, 10);
    assert_eq!(grid.rows, 10);
    assert_eq!(grid.lines.len(), 10);
    assert_eq!(grid.cursor.row, 9, "cursor should clamp to new height");
}

#[test]
fn resize_marks_all_dirty() {
    let mut grid = Grid::new(80, 24);
    let before = grid.seqno;
    grid.resize(100, 30);
    assert!(grid.seqno > before);
    let dirty = grid.dirty_rows(before);
    assert_eq!(dirty.len(), 30, "all rows should be dirty after resize");
}

#[test]
fn resize_clears_scroll_region() {
    let mut grid = Grid::new(80, 24);
    grid.scroll_region = Some(ScrollRegion { top: 5, bottom: 20 });
    grid.resize(80, 30);
    assert!(
        grid.scroll_region.is_none(),
        "scroll region should be cleared on resize"
    );
}

#[test]
fn resize_zero_dimensions_is_noop() {
    let mut grid = Grid::new(80, 24);
    let before_seqno = grid.seqno;
    grid.resize(0, 24);
    assert_eq!(grid.cols, 80, "zero cols should be rejected");
    assert_eq!(grid.seqno, before_seqno);

    grid.resize(80, 0);
    assert_eq!(grid.rows, 24, "zero rows should be rejected");

    grid.resize(0, 0);
    assert_eq!(grid.cols, 80);
    assert_eq!(grid.rows, 24);
}

#[test]
fn resize_clamps_saved_cursor() {
    let mut grid = Grid::new(80, 24);
    grid.saved_cursor.col = 79;
    grid.saved_cursor.row = 23;
    grid.resize(40, 10);
    assert_eq!(grid.saved_cursor.col, 39);
    assert_eq!(grid.saved_cursor.row, 9);
}
