//! `SwashShaper` — pure Rust glyph rasterization via swash.
//!
//! Phase 0 implementation of the `TextShaper` trait. Maps codepoints to
//! glyph IDs via the font's cmap table and rasterizes using swash's
//! hinting engine. Swappable for platform-native backends later.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::font;
use crate::shaper::{
    FontKey, FontMetrics, GlyphBitmap, GlyphPlacement, GlyphRef, PixelFormat, ShapedGlyph, TextRun,
    TextShaper,
};
use std::collections::HashMap;
use swash::FontRef;
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::Format;

/// Font entry in the shaper's font table.
struct FontEntry {
    data: Vec<u8>,
    /// Face index within `data`; non-zero for a face inside a `.ttc`
    /// collection (e.g. Apple Color Emoji). Every parse of `data` goes
    /// through [`FontEntry::ttf_face`], which applies it.
    face_index: u32,
    metrics: FontMetrics,
}

impl FontEntry {
    /// Routing every `ttf-parser` parse through here keeps `face_index` from
    /// becoming a per-call-site convention that a `.ttc` fallback face could
    /// break.
    fn ttf_face(&self) -> Result<ttf_parser::Face<'_>, ttf_parser::FaceParsingError> {
        ttf_parser::Face::parse(&self.data, self.face_index)
    }
}

/// Outcome of [`SwashShaper::install_fallbacks`]: families found on the system
/// versus those that also parsed and loaded. `loaded == 0` means no emoji/symbol
/// coverage at all.
pub struct FallbackReport {
    pub found: usize,
    pub loaded: usize,
}

/// `TextShaper` implementation using swash for rasterization.
pub struct SwashShaper {
    /// Fallback-chain keys alias into this map; a font-removal API (none
    /// today) would have to re-filter `fallback`.
    fonts: HashMap<FontKey, FontEntry>,
    next_id: u32,
    /// Ordered fallback faces, consulted per-codepoint when the run's font
    /// lacks a glyph (emoji, symbols, CJK).
    fallback: Vec<FontKey>,
}

impl SwashShaper {
    /// Create a new shaper. Call `load_font` to add fonts before shaping.
    #[must_use]
    pub fn new() -> Self {
        Self {
            fonts: HashMap::new(),
            next_id: 0,
            fallback: Vec::new(),
        }
    }

    /// Load a font from raw data at the given face index and return its key.
    /// `face_index` selects a face within a `.ttc` collection (0 for a plain
    /// `.ttf`/`.otf`).
    ///
    /// Returns `None` if the font data cannot be parsed.
    pub fn load_font(&mut self, data: Vec<u8>, face_index: u32, size: f32) -> Option<FontKey> {
        let face = ttf_parser::Face::parse(&data, face_index).ok()?;
        let metrics = font::compute_metrics_from_face(&face, size);
        let key = FontKey::new(self.next_id);
        self.next_id += 1;
        self.fonts.insert(
            key,
            FontEntry {
                data,
                face_index,
                metrics,
            },
        );
        Some(key)
    }

    /// Set the ordered fallback chain. Keys not loaded on this shaper are
    /// dropped with a warning rather than silently ignored at shape time.
    pub fn set_fallback_chain(&mut self, keys: Vec<FontKey>) {
        self.fallback = keys
            .into_iter()
            .filter(|k| {
                let known = self.fonts.contains_key(k);
                if !known {
                    tracing::warn!(font = ?k, "set_fallback_chain: unknown font key dropped");
                }
                known
            })
            .collect();
    }

    /// Load the platform fallback families (see [`font::load_fallback_fonts`])
    /// and install them as the fallback chain. Warns on any family found on the
    /// system but unparseable; the returned [`FallbackReport`] lets the caller
    /// react to zero coverage (all emoji would render as tofu).
    pub fn install_fallbacks(&mut self, db: &fontdb::Database, size: f32) -> FallbackReport {
        let fonts = font::load_fallback_fonts(db);
        let found = fonts.len();
        let keys: Vec<_> = fonts
            .into_iter()
            .filter_map(|(data, index)| self.load_font(data, index, size))
            .collect();
        let loaded = keys.len();
        if loaded < found {
            tracing::warn!(
                loaded,
                found,
                "some fallback fonts were found but failed to parse"
            );
        }
        self.set_fallback_chain(keys);
        FallbackReport { found, loaded }
    }

    /// Resolve which loaded face covers `c`, preferring `primary` then the
    /// fallback chain, and compute its advance from that same face. Falls back
    /// to `.notdef` (glyph 0) on `primary` when nothing covers it.
    ///
    /// The single face parse here also yields the advance, so callers must not
    /// re-parse — this runs once per visible cell per frame.
    fn resolve_glyph(&self, primary: FontKey, c: char, size: f32) -> (GlyphRef, f32) {
        for &font in std::iter::once(&primary).chain(&self.fallback) {
            let Some(entry) = self.fonts.get(&font) else {
                continue;
            };
            let Ok(face) = entry.ttf_face() else {
                continue;
            };
            if let Some(id) = face.glyph_index(c) {
                let advance = face
                    .glyph_hor_advance(id)
                    .map_or(entry.metrics.cell_width, |a| {
                        f32::from(a) * size / f32::from(face.units_per_em())
                    });
                return (
                    GlyphRef {
                        font,
                        glyph_id: u32::from(id.0),
                    },
                    advance,
                );
            }
        }
        let cell_width = self.fonts[&primary].metrics.cell_width;
        (
            GlyphRef {
                font: primary,
                glyph_id: 0,
            },
            cell_width,
        )
    }
}

impl Default for SwashShaper {
    fn default() -> Self {
        Self::new()
    }
}

impl TextShaper for SwashShaper {
    fn shape(&self, run: &TextRun<'_>) -> Vec<ShapedGlyph> {
        if !self.fonts.contains_key(&run.font) {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!(font = ?run.font, "shape: font key not found");
            }
            return vec![];
        }

        let mut glyphs = Vec::new();
        let mut x_offset = 0.0;

        for c in run.text.chars() {
            let (glyph, advance) = self.resolve_glyph(run.font, c, run.size);
            glyphs.push(ShapedGlyph {
                glyph,
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
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!(?font, "metrics: font key not found");
            }
            return FontMetrics {
                cell_width: 0.0,
                cell_height: 0.0,
                baseline: 0.0,
                underline_position: 0.0,
            };
        };
        // Recompute from font data at requested size.
        let Ok(face) = entry.ttf_face() else {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    ?font,
                    size,
                    "metrics: font data failed to parse, using cached metrics"
                );
            }
            return entry.metrics;
        };
        font::compute_metrics_from_face(&face, size)
    }

    #[allow(clippy::cast_possible_truncation)] // glyph IDs fit in u16 for swash render
    fn rasterize(&self, glyph: GlyphRef, size: f32) -> GlyphBitmap {
        let GlyphRef { font, glyph_id } = glyph;
        let Some(entry) = self.fonts.get(&font) else {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!(?font, glyph_id, "rasterize: font key not found");
            }
            return empty_bitmap();
        };

        let Some(font_ref) = FontRef::from_index(&entry.data, entry.face_index as usize) else {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!(?font, glyph_id, "rasterize: font data failed to parse");
            }
            return empty_bitmap();
        };

        let mut context = ScaleContext::new();
        let mut scaler = context.builder(font_ref).size(size).hint(true).build();

        let image = Render::new(&[
            Source::ColorBitmap(StrikeWith::BestFit),
            Source::ColorOutline(0),
            Source::Outline,
        ])
        // Format::Alpha applies to Outline sources only; ColorBitmap/ColorOutline
        // always produce RGBA regardless of this hint. We detect via Content::Color.
        .format(Format::Alpha)
        .render(&mut scaler, glyph_id as u16);

        if let Some(img) = image {
            let is_color = img.content == Content::Color;
            let bpp: usize = if is_color { 4 } else { 1 };
            debug_assert_eq!(
                img.data.len(),
                (img.placement.width * img.placement.height) as usize * bpp,
                "rasterized bitmap data length mismatch"
            );
            let format = if is_color {
                PixelFormat::Rgba32
            } else {
                PixelFormat::Alpha8
            };
            GlyphBitmap {
                width: img.placement.width,
                height: img.placement.height,
                placement: GlyphPlacement {
                    top: img.placement.top,
                    left: img.placement.left,
                },
                format,
                data: img.data,
            }
        } else {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!(?font, glyph_id, size, "rasterize: swash returned no image");
            }
            empty_bitmap()
        }
    }
}

fn empty_bitmap() -> GlyphBitmap {
    GlyphBitmap {
        width: 0,
        height: 0,
        placement: GlyphPlacement::default(),
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
        let key = shaper.load_font(data, 0, 14.0)?;
        Some((key, shaper))
    }

    /// Build a shaper with a system primary and every available fallback face,
    /// returning the primary key and the fallback keys. `None` when no primary
    /// or no fallback font is installed (minimal CI) — callers skip.
    fn load_font_with_fallback() -> Option<(FontKey, Vec<FontKey>, SwashShaper)> {
        let db = font::system_font_db();
        let (_m, data) = font::load_default_metrics(&db, 14.0).ok()?;
        let mut shaper = SwashShaper::new();
        let primary = shaper.load_font(data, 0, 14.0)?;
        shaper.install_fallbacks(&db, 14.0);
        if shaper.fallback.is_empty() {
            return None;
        }
        let fallback = shaper.fallback.clone();
        Some((primary, fallback, shaper))
    }

    /// Whether a face lacks a glyph for `c` (so fallback resolution is the only
    /// way it can render). Guards emoji tests against a primary that happens to
    /// cover the codepoint.
    fn face_lacks(shaper: &SwashShaper, key: FontKey, c: char) -> bool {
        shaper.fonts[&key]
            .ttf_face()
            .ok()
            .and_then(|f| f.glyph_index(c))
            .is_none()
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

        let bitmap = shaper.rasterize(glyphs[0].glyph, 14.0);
        assert!(bitmap.width > 0, "bitmap should have width");
        assert!(bitmap.height > 0, "bitmap should have height");
        assert!(!bitmap.data.is_empty(), "bitmap should have pixel data");
        assert_eq!(bitmap.format, PixelFormat::Alpha8);
    }

    #[test]
    fn rasterize_missing_font_returns_empty() {
        let shaper = SwashShaper::new();
        let bitmap = shaper.rasterize(
            GlyphRef {
                font: FontKey::new(999),
                glyph_id: 0,
            },
            14.0,
        );
        assert_eq!(bitmap.width, 0);
        assert!(bitmap.data.is_empty());
    }

    #[test]
    fn shape_tags_covered_chars_with_primary_font() {
        let Some((key, shaper)) = load_system_font() else {
            return;
        };
        let run = TextRun {
            text: "abc",
            font: key,
            size: 14.0,
        };
        for g in shaper.shape(&run) {
            assert_eq!(
                g.glyph.font, key,
                "ASCII must resolve to the run's own font"
            );
            assert_ne!(g.glyph.glyph_id, 0, "covered char must not be .notdef");
        }
    }

    #[test]
    fn primary_wins_over_fallback_when_both_cover() {
        // A duplicate of the primary registered as fallback must never be
        // chosen for a char the primary already covers — precedence, not
        // coverage, decides.
        let db = font::system_font_db();
        let Ok((_m, data)) = font::load_default_metrics(&db, 14.0) else {
            return;
        };
        let mut shaper = SwashShaper::new();
        let Some(primary) = shaper.load_font(data.clone(), 0, 14.0) else {
            return;
        };
        let fallback = shaper.load_font(data, 0, 14.0).expect("duplicate loads");
        shaper.set_fallback_chain(vec![fallback]);

        let run = TextRun {
            text: "A",
            font: primary,
            size: 14.0,
        };
        assert_eq!(shaper.shape(&run)[0].glyph.font, primary);
    }

    #[test]
    fn unresolved_codepoint_notdef_on_primary_with_empty_chain() {
        // The default (no fallback) path: an uncovered codepoint lands on
        // glyph 0 tagged with the primary.
        let Some((primary, shaper)) = load_system_font() else {
            return;
        };
        let run = TextRun {
            text: "\u{10FFFD}", // last private-use codepoint, unmapped
            font: primary,
            size: 14.0,
        };
        let g = shaper.shape(&run)[0].glyph;
        assert_eq!(g.glyph_id, 0, "unmapped char must be .notdef");
        assert_eq!(g.font, primary, ".notdef must stay on the primary face");
    }

    #[test]
    fn unresolved_codepoint_notdef_on_primary_with_fallback() {
        // Same guarantee when the chain is non-empty: a codepoint no face
        // covers is never misattributed to a fallback key.
        let Some((primary, mut shaper)) = load_system_font() else {
            return;
        };
        let db = font::system_font_db();
        if let Ok((_m, data)) = font::load_default_metrics(&db, 14.0) {
            if let Some(fb) = shaper.load_font(data, 0, 14.0) {
                shaper.set_fallback_chain(vec![fb]);
            }
        }
        let run = TextRun {
            text: "\u{10FFFD}",
            font: primary,
            size: 14.0,
        };
        let g = shaper.shape(&run)[0].glyph;
        assert_eq!(g.glyph_id, 0, "unmapped char must be .notdef");
        assert_eq!(g.font, primary, ".notdef must stay on the primary face");
    }

    #[test]
    fn emoji_resolves_to_a_fallback_face() {
        // Only meaningful where the primary lacks the emoji and a color-emoji
        // fallback is installed. Skips otherwise (e.g. minimal CI).
        let Some((primary, fallback, shaper)) = load_font_with_fallback() else {
            eprintln!("no fallback emoji font installed — skipping");
            return;
        };
        if !face_lacks(&shaper, primary, '🦀') {
            eprintln!("primary unexpectedly covers the crab — skipping");
            return;
        }
        let crab = TextRun {
            text: "🦀",
            font: primary,
            size: 14.0,
        };
        // Primary lacks it, so resolution must reach a fallback face.
        let g = shaper.shape(&crab)[0].glyph;
        assert_ne!(g.font, primary, "must not stay on the primary");
        assert!(fallback.contains(&g.font), "resolved to a fallback key");
        assert_ne!(g.glyph_id, 0, "fallback must supply a real glyph");
    }

    #[test]
    #[allow(clippy::float_cmp)] // exact: 0.0 and a copied advance value
    fn mixed_run_falls_back_per_char_and_accumulates_offsets() {
        let Some((primary, fallback, shaper)) = load_font_with_fallback() else {
            return;
        };
        if !face_lacks(&shaper, primary, '🦀') {
            return;
        }
        let run = TextRun {
            text: "a🦀b",
            font: primary,
            size: 14.0,
        };
        let g = shaper.shape(&run);
        assert_eq!(g.len(), 3);
        assert_eq!(g[0].glyph.font, primary, "leading ASCII stays on primary");
        assert!(
            fallback.contains(&g[1].glyph.font),
            "middle emoji falls back"
        );
        assert_eq!(
            g[2].glyph.font, primary,
            "trailing ASCII returns to primary"
        );
        assert_eq!(g[0].x_offset, 0.0);
        assert_eq!(g[1].x_offset, g[0].x_advance, "offsets accumulate");
        assert!(g[1].x_offset <= g[2].x_offset, "offsets are monotonic");
        assert!(
            g[1].x_advance > 0.0,
            "fallback glyph has a positive advance"
        );
    }

    #[test]
    fn set_fallback_chain_drops_unloaded_keys() {
        // A key never minted on this shaper is filtered out, not stored — so it
        // can't silently no-op at shape time.
        let Some((primary, mut shaper)) = load_system_font() else {
            return;
        };
        shaper.set_fallback_chain(vec![primary, FontKey::new(9999)]);
        assert_eq!(shaper.fallback, vec![primary], "unknown key dropped");
    }
}
