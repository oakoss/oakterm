//! `SwashShaper` — pure Rust glyph rasterization via swash.
//!
//! Phase 0 implementation of the `TextShaper` trait. Maps codepoints to
//! glyph IDs via the font's cmap table and rasterizes using swash's
//! hinting engine. Swappable for platform-native backends later.

use crate::font;
use crate::shaper::{
    FontKey, FontMetrics, GlyphBitmap, PixelFormat, ShapedGlyph, TextRun, TextShaper,
};
use std::collections::HashMap;
use swash::FontRef;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::Format;

/// Font entry in the shaper's font table.
struct FontEntry {
    data: Vec<u8>,
    metrics: FontMetrics,
}

/// `TextShaper` implementation using swash for rasterization.
pub struct SwashShaper {
    fonts: HashMap<FontKey, FontEntry>,
    next_id: u32,
}

impl SwashShaper {
    /// Create a new shaper. Call `load_font` to add fonts before shaping.
    #[must_use]
    pub fn new() -> Self {
        Self {
            fonts: HashMap::new(),
            next_id: 0,
        }
    }

    /// Load a font from raw data and return its key.
    ///
    /// Returns `None` if the font data cannot be parsed.
    pub fn load_font(&mut self, data: Vec<u8>, size: f32) -> Option<FontKey> {
        let face = ttf_parser::Face::parse(&data, 0).ok()?;
        let metrics = font::compute_metrics_from_face(&face, size);
        let key = FontKey::new(self.next_id);
        self.next_id += 1;
        self.fonts.insert(key, FontEntry { data, metrics });
        Some(key)
    }
}

impl Default for SwashShaper {
    fn default() -> Self {
        Self::new()
    }
}

impl TextShaper for SwashShaper {
    #[allow(clippy::cast_possible_truncation)] // glyph IDs fit in u16 for ttf-parser
    fn shape(&self, run: &TextRun<'_>) -> Vec<ShapedGlyph> {
        let Some(entry) = self.fonts.get(&run.font) else {
            return vec![];
        };
        let Ok(face) = ttf_parser::Face::parse(&entry.data, 0) else {
            return vec![];
        };

        let mut glyphs = Vec::new();
        let mut x_offset = 0.0;

        for c in run.text.chars() {
            let glyph_id: u32 = face.glyph_index(c).map_or(0, |id| id.0.into());
            let advance = face
                .glyph_hor_advance(ttf_parser::GlyphId(glyph_id as u16))
                .map_or(entry.metrics.cell_width, |a| {
                    f32::from(a) * run.size / f32::from(face.units_per_em())
                });

            glyphs.push(ShapedGlyph {
                glyph_id,
                x_offset,
                y_offset: 0.0,
                x_advance: advance,
            });
            x_offset += advance;
        }

        glyphs
    }

    fn metrics(&self, font: FontKey, size: f32) -> FontMetrics {
        let Some(entry) = self.fonts.get(&font) else {
            return FontMetrics {
                cell_width: 0.0,
                cell_height: 0.0,
                baseline: 0.0,
                underline_position: 0.0,
            };
        };
        // Recompute from font data at requested size.
        let Ok(face) = ttf_parser::Face::parse(&entry.data, 0) else {
            return entry.metrics;
        };
        font::compute_metrics_from_face(&face, size)
    }

    #[allow(clippy::cast_possible_truncation)] // glyph IDs fit in u16 for swash render
    fn rasterize(&self, font: FontKey, glyph_id: u32, size: f32) -> GlyphBitmap {
        let Some(entry) = self.fonts.get(&font) else {
            return empty_bitmap();
        };

        let Some(font_ref) = FontRef::from_index(&entry.data, 0) else {
            return empty_bitmap();
        };

        let mut context = ScaleContext::new();
        let mut scaler = context.builder(font_ref).size(size).hint(true).build();

        let image = Render::new(&[
            Source::ColorOutline(0),
            Source::ColorBitmap(StrikeWith::BestFit),
            Source::Outline,
        ])
        .format(Format::Alpha)
        .render(&mut scaler, glyph_id as u16);

        match image {
            Some(img) => GlyphBitmap {
                width: img.placement.width,
                height: img.placement.height,
                format: PixelFormat::Alpha8,
                data: img.data,
            },
            None => empty_bitmap(),
        }
    }
}

fn empty_bitmap() -> GlyphBitmap {
    GlyphBitmap {
        width: 0,
        height: 0,
        format: PixelFormat::Alpha8,
        data: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_system_font() -> Option<(FontKey, SwashShaper)> {
        let db = font::system_font_db();
        let (_metrics, data) = font::load_default_metrics(&db, 14.0).ok()?;
        let mut shaper = SwashShaper::new();
        let key = shaper.load_font(data, 14.0)?;
        Some((key, shaper))
    }

    #[test]
    fn load_font_returns_key() {
        let Some((key, _)) = load_system_font() else {
            eprintln!("no system font — skipping");
            return;
        };
        assert_eq!(key, FontKey::new(0));
    }

    #[test]
    fn shape_ascii_produces_glyphs() {
        let Some((key, shaper)) = load_system_font() else {
            return;
        };
        let run = TextRun {
            text: "hello",
            font: key,
            size: 14.0,
        };
        let glyphs = shaper.shape(&run);
        assert_eq!(glyphs.len(), 5);
        for g in &glyphs {
            assert!(g.x_advance > 0.0, "glyph should have positive advance");
        }
    }

    #[test]
    fn metrics_returns_valid_values() {
        let Some((key, shaper)) = load_system_font() else {
            return;
        };
        let m = shaper.metrics(key, 14.0);
        assert!(m.cell_width > 0.0);
        assert!(m.cell_height > 0.0);
        assert!(m.baseline > 0.0);
    }

    #[test]
    fn rasterize_produces_bitmap() {
        let Some((key, shaper)) = load_system_font() else {
            return;
        };
        let run = TextRun {
            text: "A",
            font: key,
            size: 14.0,
        };
        let glyphs = shaper.shape(&run);
        assert!(!glyphs.is_empty());

        let bitmap = shaper.rasterize(key, glyphs[0].glyph_id, 14.0);
        assert!(bitmap.width > 0, "bitmap should have width");
        assert!(bitmap.height > 0, "bitmap should have height");
        assert!(!bitmap.data.is_empty(), "bitmap should have pixel data");
        assert_eq!(bitmap.format, PixelFormat::Alpha8);
    }

    #[test]
    fn rasterize_missing_font_returns_empty() {
        let shaper = SwashShaper::new();
        let bitmap = shaper.rasterize(FontKey::new(999), 0, 14.0);
        assert_eq!(bitmap.width, 0);
        assert!(bitmap.data.is_empty());
    }
}
