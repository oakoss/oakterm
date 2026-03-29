//! Convert client-side grid state to GPU render data.

use oakterm_protocol::render::{RenderUpdate, WireCell};
use oakterm_renderer::atlas::{AtlasPlane, GlyphCacheKey};
use oakterm_renderer::pipeline::GlyphVertex;
use oakterm_renderer::shaper::{FontKey, FontMetrics, TextRun, TextShaper};

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

/// Client-side grid state maintained from `RenderUpdate` messages.
pub struct ClientGrid {
    cells: Vec<RenderCell>,
    pub cols: u16,
    pub rows: u16,
    pub cursor_x: u16,
    pub cursor_y: u16,
    pub cursor_visible: bool,
    pub seqno: u64,
    /// Dynamic background color from daemon (OSC 11 or default).
    pub bg_color: [u8; 3],
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
            seqno: 0,
            bg_color: [0, 0, 0],
        }
    }

    /// Apply a `RenderUpdate` from the daemon.
    pub fn apply_update(&mut self, update: &RenderUpdate) {
        self.cursor_x = update.cursor_x;
        self.cursor_y = update.cursor_y;
        self.cursor_visible = update.cursor_visible;
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

    /// Resize the client grid, clearing all cells.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.cells = vec![RenderCell::default(); usize::from(cols) * usize::from(rows)];
    }

    /// Linear cell index of the cursor, if visible and in-bounds.
    fn cursor_cell_index(&self) -> Option<usize> {
        if self.cursor_visible && self.cursor_x < self.cols && self.cursor_y < self.rows {
            Some(usize::from(self.cursor_y) * usize::from(self.cols) + usize::from(self.cursor_x))
        } else {
            None
        }
    }

    /// Build the packed ABGR background color array for the GPU pipeline.
    /// The cursor cell uses reverse video (fg color as bg) when visible.
    #[must_use]
    pub fn bg_colors(&self) -> Vec<u32> {
        let cursor_idx = self.cursor_cell_index();

        self.cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if Some(i) == cursor_idx {
                    pack_bg_color(c.fg)
                } else {
                    pack_bg_color(c.bg)
                }
            })
            .collect()
    }

    /// Build glyph instances and any new bitmap uploads needed.
    #[allow(clippy::cast_precision_loss)] // col/row indices fit in f32
    pub fn glyph_instances(
        &self,
        metrics: &FontMetrics,
        font_key: FontKey,
        font_size: f32,
        shaper: &impl TextShaper,
        atlas: &mut AtlasPlane,
    ) -> (Vec<GlyphVertex>, Vec<GlyphUpload>) {
        let mut glyphs = Vec::new();
        let mut uploads = Vec::new();
        let mut dropped = 0u32;

        let cursor_idx = self.cursor_cell_index();

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
                let (fg_rgb, bg_rgb) = if is_cursor {
                    (cell.bg, cell.fg)
                } else {
                    (cell.fg, cell.bg)
                };

                let fg = [
                    f32::from(fg_rgb[0]) / 255.0,
                    f32::from(fg_rgb[1]) / 255.0,
                    f32::from(fg_rgb[2]) / 255.0,
                    1.0,
                ];

                let bg_lum = 0.2126 * f32::from(bg_rgb[0]) / 255.0
                    + 0.7152 * f32::from(bg_rgb[1]) / 255.0
                    + 0.0722 * f32::from(bg_rgb[2]) / 255.0;

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
            eprintln!("atlas full: {dropped} glyphs could not be allocated");
        }

        (glyphs, uploads)
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
        let colors = grid.bg_colors();
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

        let colors = grid.bg_colors();
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

        let colors = grid.bg_colors();
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

        let colors = grid.bg_colors();
        // Cursor hidden — bg stays black.
        assert_eq!(colors[1], pack_bg_color([0, 0, 0]));
    }

    #[test]
    fn cursor_out_of_bounds_no_panic() {
        let mut grid = ClientGrid::new(4, 2);
        grid.cursor_x = 99;
        grid.cursor_y = 99;
        grid.cursor_visible = true;
        let colors = grid.bg_colors();
        assert_eq!(colors.len(), 8);
    }
}
