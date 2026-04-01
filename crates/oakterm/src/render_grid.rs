//! Convert client-side grid state to GPU render data.

use oakterm_protocol::render::{DirtyRow, RenderUpdate, WireCell};
use oakterm_renderer::atlas::{AtlasPlane, GlyphCacheKey};
use oakterm_renderer::pipeline::GlyphVertex;
use oakterm_renderer::shaper::{FontKey, FontMetrics, TextRun, TextShaper};
use oakterm_terminal::grid::selection::Selection;

/// sRGB to linear (IEC 61966-2-1). Matches the shader's `srgb_to_linear`.
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// A glyph bitmap that needs uploading to the GPU atlas texture.
pub struct GlyphUpload {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// A cell in the client-side render grid.
#[derive(Debug, Clone)]
struct RenderCell {
    codepoint: u32,
    fg: [u8; 3],
    bg: [u8; 3],
}

impl Default for RenderCell {
    fn default() -> Self {
        Self {
            codepoint: 0,
            fg: [255, 255, 255],
            bg: [0, 0, 0],
        }
    }
}

/// Saved live-view state when the viewport is scrolled up.
struct LiveSnapshot {
    cells: Vec<RenderCell>,
    cursor_x: u16,
    cursor_y: u16,
    cursor_visible: bool,
    cursor_style: u8,
}

/// Client-side grid state maintained from `RenderUpdate` messages.
pub struct ClientGrid {
    cells: Vec<RenderCell>,
    pub cols: u16,
    pub rows: u16,
    pub cursor_x: u16,
    pub cursor_y: u16,
    pub cursor_visible: bool,
    /// Wire-encoded cursor style (0-5). See `CursorStyle::to_wire`.
    pub cursor_style: u8,
    pub seqno: u64,
    /// Dynamic background color from daemon (OSC 11 or default).
    pub bg_color: [u8; 3],
    /// Saved live state while viewport is scrolled up.
    live_snapshot: Option<LiveSnapshot>,
}

impl ClientGrid {
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cells: vec![RenderCell::default(); usize::from(cols) * usize::from(rows)],
            cols,
            rows,
            cursor_x: 0,
            cursor_y: 0,
            cursor_visible: true,
            cursor_style: 0,
            seqno: 0,
            bg_color: [0, 0, 0],
            live_snapshot: None,
        }
    }

    /// Apply a `RenderUpdate` from the daemon.
    pub fn apply_update(&mut self, update: &RenderUpdate) {
        self.cursor_x = update.cursor_x;
        self.cursor_y = update.cursor_y;
        self.cursor_visible = update.cursor_visible;
        self.cursor_style = update.cursor_style;
        self.bg_color = [update.bg_r, update.bg_g, update.bg_b];
        self.seqno = update.seqno;

        for row in &update.dirty_rows {
            let row_idx = usize::from(row.row_index);
            if row_idx >= usize::from(self.rows) {
                continue;
            }
            for (col_idx, cell) in row.cells.iter().enumerate() {
                if col_idx >= usize::from(self.cols) {
                    break;
                }
                let idx = row_idx * usize::from(self.cols) + col_idx;
                self.cells[idx] = wire_cell_to_render(cell);
            }
        }
    }

    /// Resize the client grid, clearing all cells and exiting scrollback.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.cells = vec![RenderCell::default(); usize::from(cols) * usize::from(rows)];
        self.live_snapshot = None;
    }

    /// Whether the viewport is currently showing scrollback.
    #[must_use]
    pub fn is_scrolled(&self) -> bool {
        self.live_snapshot.is_some()
    }

    /// Save the current live view and enter scrollback mode.
    /// No-op if already scrolled.
    pub fn enter_scrollback(&mut self) {
        if self.live_snapshot.is_some() {
            return;
        }
        self.live_snapshot = Some(LiveSnapshot {
            cells: self.cells.clone(),
            cursor_x: self.cursor_x,
            cursor_y: self.cursor_y,
            cursor_visible: self.cursor_visible,
            cursor_style: self.cursor_style,
        });
    }

    /// Restore the live view and exit scrollback mode.
    pub fn exit_scrollback(&mut self) {
        if let Some(snap) = self.live_snapshot.take() {
            self.cells = snap.cells;
            self.cursor_x = snap.cursor_x;
            self.cursor_y = snap.cursor_y;
            self.cursor_visible = snap.cursor_visible;
            self.cursor_style = snap.cursor_style;
        }
    }

    /// Apply a `RenderUpdate` to the saved live snapshot (not the visible cells).
    /// Used to keep the live state current while the viewport shows scrollback.
    pub fn apply_update_while_scrolled(&mut self, update: &RenderUpdate) {
        self.seqno = update.seqno;
        self.bg_color = [update.bg_r, update.bg_g, update.bg_b];
        let Some(snap) = &mut self.live_snapshot else {
            return;
        };
        snap.cursor_x = update.cursor_x;
        snap.cursor_y = update.cursor_y;
        snap.cursor_visible = update.cursor_visible;
        snap.cursor_style = update.cursor_style;

        let cols = usize::from(self.cols);
        let rows = usize::from(self.rows);
        for row in &update.dirty_rows {
            let row_idx = usize::from(row.row_index);
            if row_idx >= rows {
                continue;
            }
            for (col_idx, cell) in row.cells.iter().enumerate() {
                if col_idx >= cols {
                    break;
                }
                snap.cells[row_idx * cols + col_idx] = wire_cell_to_render(cell);
            }
        }
    }

    /// Compose the visible grid from scrollback rows and the live snapshot.
    ///
    /// `scrollback_rows` are displayed at the top of the viewport.
    /// If `offset < rows`, the remaining bottom rows come from the live snapshot.
    /// If `offset >= rows`, the entire viewport shows scrollback.
    pub fn apply_scrollback(&mut self, scrollback_rows: &[DirtyRow], offset: u16) {
        let cols = usize::from(self.cols);
        let rows = usize::from(self.rows);
        let sb_display_rows = usize::from(offset).min(rows);

        // Clear cells.
        for cell in &mut self.cells {
            *cell = RenderCell::default();
        }

        // Top portion: scrollback rows.
        for (display_row, dirty_row) in scrollback_rows.iter().enumerate() {
            if display_row >= sb_display_rows {
                break;
            }
            for (col, wire_cell) in dirty_row.cells.iter().enumerate() {
                if col >= cols {
                    break;
                }
                self.cells[display_row * cols + col] = wire_cell_to_render(wire_cell);
            }
        }

        // Bottom portion: live snapshot rows (if partial scroll).
        if sb_display_rows < rows {
            if let Some(snap) = &self.live_snapshot {
                let live_start = 0;
                let live_count = rows - sb_display_rows;
                for i in 0..live_count {
                    let src_idx = (live_start + i) * cols;
                    let dst_idx = (sb_display_rows + i) * cols;
                    if src_idx + cols <= snap.cells.len() && dst_idx + cols <= self.cells.len() {
                        self.cells[dst_idx..dst_idx + cols]
                            .clone_from_slice(&snap.cells[src_idx..src_idx + cols]);
                    }
                }
            }
        }

        // Hide cursor while showing scrollback.
        self.cursor_visible = false;
    }

    /// Overwrite the bottom-right cells with a scroll position indicator.
    pub fn set_scroll_indicator(&mut self, offset: u32) {
        let text = format!(" [{offset} lines] ");
        let cols = usize::from(self.cols);
        let rows = usize::from(self.rows);
        if cols == 0 || rows == 0 {
            return;
        }
        let last_row_start = (rows - 1) * cols;
        let start_col = cols.saturating_sub(text.len());
        for (i, ch) in text.chars().enumerate() {
            let col = start_col + i;
            if col >= cols {
                break;
            }
            self.cells[last_row_start + col] = RenderCell {
                codepoint: ch as u32,
                fg: [0, 0, 0],
                bg: [200, 200, 200],
            };
        }
    }

    /// Linear cell index of the cursor, if visible and in-bounds.
    fn cursor_cell_index(&self, visible: bool) -> Option<usize> {
        if visible && self.cursor_x < self.cols && self.cursor_y < self.rows {
            Some(usize::from(self.cursor_y) * usize::from(self.cols) + usize::from(self.cursor_x))
        } else {
            None
        }
    }

    /// Build the packed ABGR background color array for the GPU pipeline.
    /// Cursor cell uses reverse video (fg as bg) for all shapes.
    /// `cursor_visible`: effective visibility (accounts for blink phase).
    // TODO: underline/bar should render as partial-cell quads once the
    // GPU pipeline supports sub-cell geometry.
    #[must_use]
    pub fn bg_colors(
        &self,
        cursor_visible: bool,
        selection: Option<&Selection>,
        viewport_offset: u32,
    ) -> Vec<u32> {
        let cursor_idx = self.cursor_cell_index(cursor_visible);
        let cols = usize::from(self.cols);

        self.cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let row = i / cols;
                let col = i % cols;
                #[allow(clippy::cast_possible_wrap)]
                let sel_row = row as i64 - i64::from(viewport_offset);
                #[allow(clippy::cast_possible_truncation)]
                let selected = selection.is_some_and(|s| s.contains(sel_row, col as u16));

                if Some(i) == cursor_idx || selected {
                    pack_bg_color(c.fg)
                } else {
                    pack_bg_color(c.bg)
                }
            })
            .collect()
    }

    /// Build glyph instances and any new bitmap uploads needed.
    #[allow(clippy::cast_precision_loss, clippy::too_many_arguments)] // col/row indices fit in f32
    pub fn glyph_instances(
        &self,
        metrics: &FontMetrics,
        font_key: FontKey,
        font_size: f32,
        shaper: &impl TextShaper,
        atlas: &mut AtlasPlane,
        cursor_visible: bool,
        selection: Option<&Selection>,
        viewport_offset: u32,
    ) -> (Vec<GlyphVertex>, Vec<GlyphUpload>) {
        let mut glyphs = Vec::new();
        let mut uploads = Vec::new();
        let mut dropped = 0u32;

        let cursor_idx = self.cursor_cell_index(cursor_visible);

        for row in 0..usize::from(self.rows) {
            for col in 0..usize::from(self.cols) {
                let idx = row * usize::from(self.cols) + col;
                let cell = &self.cells[idx];
                if cell.codepoint == 0 || cell.codepoint == u32::from(b' ') {
                    continue;
                }

                let Some(ch) = char::from_u32(cell.codepoint) else {
                    continue;
                };

                let text = ch.to_string();
                let run = TextRun {
                    text: &text,
                    font: font_key,
                    size: font_size,
                };
                let result = shaper.shape(&run);
                let Some(glyph) = result.first() else {
                    continue;
                };

                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let cache_key = GlyphCacheKey {
                    font_id: 0,
                    glyph_id: glyph.glyph_id,
                    size_tenths: (font_size * 10.0) as u32,
                };

                let region = if let Some(r) = atlas.get(&cache_key) {
                    r
                } else {
                    let bitmap = shaper.rasterize(font_key, glyph.glyph_id, font_size);
                    if bitmap.width == 0 || bitmap.height == 0 {
                        continue;
                    }
                    let Some(r) =
                        atlas.insert(cache_key, bitmap.width, bitmap.height, bitmap.placement)
                    else {
                        dropped += 1;
                        continue;
                    };
                    uploads.push(GlyphUpload {
                        x: r.x,
                        y: r.y,
                        width: bitmap.width,
                        height: bitmap.height,
                        data: bitmap.data,
                    });
                    r
                };

                atlas.mark_in_use(&cache_key);

                let x = col as f32 * metrics.cell_width;
                let y = row as f32 * metrics.cell_height;

                let is_cursor = Some(idx) == cursor_idx;
                #[allow(clippy::cast_possible_wrap)]
                let sel_row = row as i64 - i64::from(viewport_offset);
                #[allow(clippy::cast_possible_truncation)]
                let selected = selection.is_some_and(|s| s.contains(sel_row, col as u16));
                let (fg_rgb, bg_rgb) = if is_cursor || selected {
                    (cell.bg, cell.fg)
                } else {
                    (cell.fg, cell.bg)
                };

                // sRGB values passed to shader (linearized in fragment shader).
                let fg = [
                    f32::from(fg_rgb[0]) / 255.0,
                    f32::from(fg_rgb[1]) / 255.0,
                    f32::from(fg_rgb[2]) / 255.0,
                    1.0,
                ];

                // Linearize bg for luminance (must match shader's linear space).
                let bg_lum = {
                    let lr = srgb_to_linear(f32::from(bg_rgb[0]) / 255.0);
                    let lg = srgb_to_linear(f32::from(bg_rgb[1]) / 255.0);
                    let lb = srgb_to_linear(f32::from(bg_rgb[2]) / 255.0);
                    0.2126 * lr + 0.7152 * lg + 0.0722 * lb
                };

                glyphs.push(GlyphVertex {
                    pos: [
                        x + region.placement.left as f32,
                        y + metrics.baseline - region.placement.top as f32,
                    ],
                    size: [region.width as f32, region.height as f32],
                    uv_origin: [region.x as f32, region.y as f32],
                    fg_color: fg,
                    bg_luminance: bg_lum,
                    pad: [0.0; 3],
                });
            }
        }

        atlas.clear_in_use();

        if dropped > 0 {
            tracing::warn!(dropped, "atlas full: glyphs could not be allocated");
        }

        (glyphs, uploads)
    }

    /// Extract text from a single visible row. Null codepoints become spaces;
    /// invalid codepoints become U+FFFD. Trailing spaces are trimmed.
    ///
    /// # Panics
    ///
    /// Panics if `row >= self.rows`.
    #[must_use]
    pub fn row_text(&self, row: u16) -> String {
        let start = usize::from(row) * usize::from(self.cols);
        let end = start + usize::from(self.cols);
        let mut s = String::with_capacity(usize::from(self.cols));
        for cell in &self.cells[start..end] {
            if cell.codepoint == 0 {
                s.push(' ');
            } else {
                s.push(char::from_u32(cell.codepoint).unwrap_or('\u{FFFD}'));
            }
        }
        let trimmed = s.trim_end_matches(' ').len();
        s.truncate(trimmed);
        s
    }

    /// Extract text from all visible rows.
    #[must_use]
    pub fn row_texts(&self) -> Vec<String> {
        (0..self.rows).map(|r| self.row_text(r)).collect()
    }

    /// Extract the text covered by a selection from the visible grid.
    ///
    /// `viewport_offset` maps visible row 0 to selection row
    /// `-(viewport_offset)`. Each line is trimmed of trailing whitespace.
    #[must_use]
    pub fn extract_selection_text(&self, selection: &Selection, viewport_offset: u32) -> String {
        let (start, end) = selection.normalized();
        let cols = usize::from(self.cols);
        let rows = usize::from(self.rows);
        let mut lines: Vec<String> = Vec::new();

        for vis_row in 0..rows {
            #[allow(clippy::cast_possible_wrap)]
            let sel_row = vis_row as i64 - i64::from(viewport_offset);
            if sel_row < start.row || sel_row > end.row {
                continue;
            }

            let row_start = vis_row * cols;
            let mut line = String::with_capacity(cols);

            for col in 0..cols {
                #[allow(clippy::cast_possible_truncation)]
                if !selection.contains(sel_row, col as u16) {
                    continue;
                }
                let cell = &self.cells[row_start + col];
                if cell.codepoint == 0 {
                    line.push(' ');
                } else {
                    line.push(char::from_u32(cell.codepoint).unwrap_or('\u{FFFD}'));
                }
            }

            // Trim trailing whitespace from each line.
            let trimmed_len = line.trim_end().len();
            line.truncate(trimmed_len);
            lines.push(line);
        }

        lines.join("\n")
    }
}

/// Pack RGB bytes into the ABGR u32 format the shader expects.
#[must_use]
pub fn pack_bg_color(rgb: [u8; 3]) -> u32 {
    0xFF_00_00_00 | (u32::from(rgb[2]) << 16) | (u32::from(rgb[1]) << 8) | u32::from(rgb[0])
}

fn wire_cell_to_render(cell: &WireCell) -> RenderCell {
    RenderCell {
        codepoint: cell.codepoint,
        fg: [cell.fg_r, cell.fg_g, cell.fg_b],
        bg: [cell.bg_r, cell.bg_g, cell.bg_b],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oakterm_protocol::render::{DirtyRow, RenderUpdate};

    #[test]
    fn pack_bg_color_black() {
        assert_eq!(pack_bg_color([0, 0, 0]), 0xFF_00_00_00);
    }

    #[test]
    fn pack_bg_color_red() {
        assert_eq!(pack_bg_color([255, 0, 0]), 0xFF_00_00_FF);
    }

    #[test]
    fn pack_bg_color_white() {
        assert_eq!(pack_bg_color([255, 255, 255]), 0xFF_FF_FF_FF);
    }

    #[test]
    fn client_grid_new_has_correct_size() {
        let grid = ClientGrid::new(80, 24);
        assert_eq!(grid.cells.len(), 80 * 24);
    }

    #[test]
    fn client_grid_default_bg_is_black() {
        let mut grid = ClientGrid::new(2, 2);
        grid.cursor_visible = false;
        let colors = grid.bg_colors(grid.cursor_visible, None, 0);
        assert!(colors.iter().all(|&c| c == 0xFF_00_00_00));
    }

    #[test]
    fn apply_update_sets_cells() {
        let mut grid = ClientGrid::new(4, 2);

        let update = RenderUpdate {
            pane_id: 0,
            seqno: 1,
            cursor_x: 3,
            cursor_y: 1,
            cursor_style: 0,
            cursor_visible: true,
            bg_r: 0,
            bg_g: 0,
            bg_b: 0,
            bracketed_paste: false,
            dirty_rows: vec![DirtyRow {
                row_index: 0,
                cells: vec![
                    WireCell {
                        codepoint: u32::from(b'H'),
                        fg_r: 255,
                        fg_g: 255,
                        fg_b: 255,
                        fg_type: 1,
                        bg_r: 255,
                        bg_g: 0,
                        bg_b: 0,
                        bg_type: 1,
                        flags: 0,
                        extra: vec![],
                    },
                    WireCell {
                        codepoint: u32::from(b'i'),
                        fg_r: 0,
                        fg_g: 255,
                        fg_b: 0,
                        fg_type: 1,
                        bg_r: 0,
                        bg_g: 0,
                        bg_b: 0,
                        bg_type: 0,
                        flags: 0,
                        extra: vec![],
                    },
                ],
                semantic_mark: 0,
                mark_metadata: vec![],
            }],
        };

        grid.apply_update(&update);

        let colors = grid.bg_colors(grid.cursor_visible, None, 0);
        assert_eq!(colors[0], pack_bg_color([255, 0, 0]), "first cell red bg");
        assert_eq!(colors[1], pack_bg_color([0, 0, 0]), "second cell black bg");
        assert_eq!(grid.cells[0].codepoint, u32::from(b'H'));
        assert_eq!(grid.cells[1].codepoint, u32::from(b'i'));
    }

    #[test]
    fn apply_update_sets_cursor() {
        let mut grid = ClientGrid::new(80, 24);
        let update = RenderUpdate {
            pane_id: 0,
            seqno: 5,
            cursor_x: 10,
            cursor_y: 3,
            cursor_style: 0,
            cursor_visible: false,
            bg_r: 0,
            bg_g: 0,
            bg_b: 0,
            bracketed_paste: false,
            dirty_rows: vec![],
        };
        grid.apply_update(&update);
        assert_eq!(grid.cursor_x, 10);
        assert_eq!(grid.cursor_y, 3);
        assert!(!grid.cursor_visible);
        assert_eq!(grid.seqno, 5);
    }

    #[test]
    fn apply_update_ignores_out_of_bounds_rows() {
        let mut grid = ClientGrid::new(4, 2);
        let update = RenderUpdate {
            pane_id: 0,
            seqno: 1,
            cursor_x: 0,
            cursor_y: 0,
            cursor_style: 0,
            cursor_visible: true,
            bg_r: 0,
            bg_g: 0,
            bg_b: 0,
            bracketed_paste: false,
            dirty_rows: vec![DirtyRow {
                row_index: 99,
                cells: vec![WireCell {
                    codepoint: u32::from(b'X'),
                    fg_r: 0,
                    fg_g: 0,
                    fg_b: 0,
                    fg_type: 0,
                    bg_r: 0,
                    bg_g: 0,
                    bg_b: 0,
                    bg_type: 0,
                    flags: 0,
                    extra: vec![],
                }],
                semantic_mark: 0,
                mark_metadata: vec![],
            }],
        };
        grid.apply_update(&update);
        // No panic, cells unchanged.
        assert!(grid.cells.iter().all(|c| c.codepoint == 0));
    }

    #[test]
    fn resize_clears_grid() {
        let mut grid = ClientGrid::new(4, 2);
        grid.cells[0].codepoint = u32::from(b'A');
        grid.resize(10, 5);
        assert_eq!(grid.cols, 10);
        assert_eq!(grid.rows, 5);
        assert_eq!(grid.cells.len(), 50);
        assert!(grid.cells.iter().all(|c| c.codepoint == 0));
    }

    #[test]
    fn cursor_visible_reverses_bg_color() {
        let mut grid = ClientGrid::new(4, 2);
        // Set cell at (1, 0) with white fg, black bg.
        grid.cells[1] = RenderCell {
            codepoint: u32::from(b'A'),
            fg: [255, 255, 255],
            bg: [0, 0, 0],
        };
        grid.cursor_x = 1;
        grid.cursor_y = 0;
        grid.cursor_visible = true;

        let colors = grid.bg_colors(grid.cursor_visible, None, 0);
        // Cursor cell bg should be the fg color (reverse video).
        assert_eq!(colors[1], pack_bg_color([255, 255, 255]));
        // Non-cursor cells stay black.
        assert_eq!(colors[0], pack_bg_color([0, 0, 0]));
    }

    #[test]
    fn cursor_hidden_no_reverse() {
        let mut grid = ClientGrid::new(4, 2);
        grid.cells[1] = RenderCell {
            codepoint: u32::from(b'A'),
            fg: [255, 255, 255],
            bg: [0, 0, 0],
        };
        grid.cursor_x = 1;
        grid.cursor_y = 0;
        grid.cursor_visible = false;

        let colors = grid.bg_colors(grid.cursor_visible, None, 0);
        // Cursor hidden — bg stays black.
        assert_eq!(colors[1], pack_bg_color([0, 0, 0]));
    }

    #[test]
    fn cursor_out_of_bounds_no_panic() {
        let mut grid = ClientGrid::new(4, 2);
        grid.cursor_x = 99;
        grid.cursor_y = 99;
        grid.cursor_visible = true;
        let colors = grid.bg_colors(grid.cursor_visible, None, 0);
        assert_eq!(colors.len(), 8);
    }

    // --- Scrollback viewport tests ---

    fn make_wire_cell(ch: u8) -> WireCell {
        WireCell {
            codepoint: u32::from(ch),
            fg_r: 255,
            fg_g: 255,
            fg_b: 255,
            fg_type: 0,
            bg_r: 0,
            bg_g: 0,
            bg_b: 0,
            bg_type: 0,
            flags: 0,
            extra: vec![],
        }
    }

    fn make_dirty_row(index: u16, text: &[u8]) -> DirtyRow {
        DirtyRow {
            row_index: index,
            cells: text.iter().map(|&ch| make_wire_cell(ch)).collect(),
            semantic_mark: 0,
            mark_metadata: vec![],
        }
    }

    #[test]
    fn enter_exit_scrollback_roundtrip() {
        let mut grid = ClientGrid::new(4, 2);
        grid.cells[0].codepoint = u32::from(b'A');
        grid.cursor_x = 2;
        grid.cursor_visible = true;

        grid.enter_scrollback();
        assert!(grid.is_scrolled());

        // Modify visible cells (simulating scrollback display).
        grid.cells[0].codepoint = u32::from(b'Z');
        grid.cursor_visible = false;

        grid.exit_scrollback();
        assert!(!grid.is_scrolled());
        assert_eq!(grid.cells[0].codepoint, u32::from(b'A'));
        assert_eq!(grid.cursor_x, 2);
        assert!(grid.cursor_visible);
    }

    #[test]
    fn apply_scrollback_full_page() {
        let mut grid = ClientGrid::new(3, 2);
        grid.cells[0].codepoint = u32::from(b'L'); // live content
        grid.enter_scrollback();

        let rows = vec![make_dirty_row(0, b"abc"), make_dirty_row(1, b"def")];
        grid.apply_scrollback(&rows, 2); // offset=2 >= rows=2, full scrollback

        assert_eq!(grid.cells[0].codepoint, u32::from(b'a'));
        assert_eq!(grid.cells[3].codepoint, u32::from(b'd'));
        assert!(!grid.cursor_visible);
    }

    #[test]
    fn apply_scrollback_partial_page() {
        let mut grid = ClientGrid::new(3, 3);
        // Set up live content.
        grid.cells[0].codepoint = u32::from(b'R'); // row 0
        grid.cells[3].codepoint = u32::from(b'S'); // row 1
        grid.cells[6].codepoint = u32::from(b'T'); // row 2
        grid.enter_scrollback();

        // Scroll up by 1 line: top 1 row from scrollback, bottom 2 from live.
        let rows = vec![make_dirty_row(0, b"xxx")];
        grid.apply_scrollback(&rows, 1);

        // Row 0: scrollback
        assert_eq!(grid.cells[0].codepoint, u32::from(b'x'));
        // Row 1: live row 0
        assert_eq!(grid.cells[3].codepoint, u32::from(b'R'));
        // Row 2: live row 1
        assert_eq!(grid.cells[6].codepoint, u32::from(b'S'));
    }

    #[test]
    fn apply_update_while_scrolled_updates_snapshot() {
        let mut grid = ClientGrid::new(4, 2);
        grid.cells[0].codepoint = u32::from(b'O');
        grid.enter_scrollback();

        let update = RenderUpdate {
            pane_id: 0,
            seqno: 10,
            cursor_x: 3,
            cursor_y: 1,
            cursor_style: 0,
            cursor_visible: true,
            bg_r: 0,
            bg_g: 0,
            bg_b: 0,
            bracketed_paste: false,
            dirty_rows: vec![make_dirty_row(0, b"NEW!")],
        };
        grid.apply_update_while_scrolled(&update);

        // Visible cells unchanged.
        assert_eq!(grid.cells[0].codepoint, u32::from(b'O'));
        // Seqno updated.
        assert_eq!(grid.seqno, 10);

        // Exit scrollback — should restore the updated snapshot.
        grid.exit_scrollback();
        assert_eq!(grid.cells[0].codepoint, u32::from(b'N'));
        assert_eq!(grid.cursor_x, 3);
    }

    #[test]
    fn resize_clears_scrollback_state() {
        let mut grid = ClientGrid::new(4, 2);
        grid.enter_scrollback();
        assert!(grid.is_scrolled());
        grid.resize(10, 5);
        assert!(!grid.is_scrolled());
    }

    #[test]
    fn scroll_indicator_overwrites_bottom_right() {
        let mut grid = ClientGrid::new(20, 3);
        grid.set_scroll_indicator(42);
        // Indicator: " [42 lines] " = 13 chars, starts at col 20-13=7 on row 2.
        let last_row_start = 2 * 20;
        // Last cell should be trailing space with indicator background.
        let last_cell = &grid.cells[last_row_start + 19];
        assert_eq!(last_cell.codepoint, u32::from(b' '));
        assert_eq!(last_cell.bg, [200, 200, 200]);
        // " [42 lines] " = 12 chars, starts at col 8.
        // Col 8 is ' ', col 9 is '[', col 10 is '4'.
        assert_eq!(grid.cells[last_row_start + 9].codepoint, u32::from(b'['));
        assert_eq!(grid.cells[last_row_start + 10].codepoint, u32::from(b'4'));
        // Col 7 should be untouched (default black bg).
        assert_eq!(grid.cells[last_row_start + 7].bg, [0, 0, 0]);
    }

    #[test]
    fn apply_scrollback_fewer_rows_than_offset() {
        // Daemon returns 1 row but offset is 3. Top row 0 gets scrollback,
        // rows 1-2 stay cleared, bottom rows come from live snapshot.
        let mut grid = ClientGrid::new(3, 4);
        grid.cells[0].codepoint = u32::from(b'A'); // row 0
        grid.cells[3].codepoint = u32::from(b'B'); // row 1
        grid.enter_scrollback();

        let rows = vec![make_dirty_row(0, b"zzz")];
        grid.apply_scrollback(&rows, 3);

        // Row 0: scrollback data.
        assert_eq!(grid.cells[0].codepoint, u32::from(b'z'));
        // Row 1-2: no scrollback data, cleared to default.
        assert_eq!(grid.cells[3].codepoint, 0);
        assert_eq!(grid.cells[6].codepoint, 0);
        // Row 3: live snapshot row 0.
        assert_eq!(grid.cells[9].codepoint, u32::from(b'A'));
    }

    #[test]
    fn apply_update_while_scrolled_noop_when_not_scrolled() {
        let mut grid = ClientGrid::new(4, 2);
        grid.cells[0].codepoint = u32::from(b'X');

        let update = RenderUpdate {
            pane_id: 0,
            seqno: 99,
            cursor_x: 0,
            cursor_y: 0,
            cursor_style: 0,
            cursor_visible: true,
            bg_r: 0,
            bg_g: 0,
            bg_b: 0,
            bracketed_paste: false,
            dirty_rows: vec![make_dirty_row(0, b"NEW!")],
        };
        grid.apply_update_while_scrolled(&update);

        // Seqno updated even when not scrolled.
        assert_eq!(grid.seqno, 99);
        // Visible cells unchanged (no snapshot to write to).
        assert_eq!(grid.cells[0].codepoint, u32::from(b'X'));
    }

    #[test]
    fn enter_scrollback_twice_preserves_original() {
        let mut grid = ClientGrid::new(4, 2);
        grid.cells[0].codepoint = u32::from(b'O'); // original

        grid.enter_scrollback();
        // Overwrite visible cells (simulating scrollback display).
        grid.cells[0].codepoint = u32::from(b'S');

        // Second enter should be a no-op (not overwrite snapshot with 'S').
        grid.enter_scrollback();

        grid.exit_scrollback();
        assert_eq!(
            grid.cells[0].codepoint,
            u32::from(b'O'),
            "double enter should not overwrite original snapshot"
        );
    }

    #[test]
    fn row_text_with_content() {
        let mut grid = ClientGrid::new(10, 1);
        for (i, ch) in "hello".chars().enumerate() {
            grid.cells[i].codepoint = ch as u32;
        }
        assert_eq!(grid.row_text(0), "hello");
    }

    #[test]
    fn row_text_empty_row() {
        let grid = ClientGrid::new(10, 1);
        assert_eq!(grid.row_text(0), "");
    }

    #[test]
    fn row_texts_all_rows() {
        let mut grid = ClientGrid::new(5, 2);
        grid.cells[0].codepoint = u32::from(b'A');
        grid.cells[5].codepoint = u32::from(b'B');
        let texts = grid.row_texts();
        assert_eq!(texts.len(), 2);
        assert_eq!(texts[0], "A");
        assert_eq!(texts[1], "B");
    }
}
