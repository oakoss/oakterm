/// Opaque handle to a loaded font.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FontKey(u32);

impl FontKey {
    #[expect(dead_code, reason = "used when renderer creates font handles")]
    pub(crate) fn new(id: u32) -> Self {
        Self(id)
    }
}

/// A run of consecutive cells with the same font and attributes.
pub struct TextRun<'a> {
    pub text: &'a str,
    pub font: FontKey,
    pub size: f32,
}

/// A positioned glyph produced by shaping.
#[non_exhaustive]
pub struct ShapedGlyph {
    pub glyph_id: u32,
    pub x_offset: f32,
    pub y_offset: f32,
    pub x_advance: f32,
}

/// Font metrics for cell sizing. All values in pixels at the requested point size.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct FontMetrics {
    pub cell_width: f32,
    pub cell_height: f32,
    /// Distance from top of cell to baseline (positive).
    pub baseline: f32,
    /// Signed offset from baseline (negative = below baseline).
    pub underline_position: f32,
}

/// Pixel format for rasterized glyphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Alpha8,
    Rgba32,
}

/// Pixel buffer for a rasterized glyph.
#[non_exhaustive]
pub struct GlyphBitmap {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub data: Vec<u8>,
}

/// Shapes text runs into positioned glyphs and rasterizes individual glyphs.
///
/// Phase 0: `SimpleShaper` maps each character to its glyph ID via the font's
/// cmap table. Ligature-capable shapers (`HarfBuzz`, Core Text, `DirectWrite`)
/// slot in behind this trait.
pub trait TextShaper {
    fn shape(&self, run: &TextRun<'_>) -> Vec<ShapedGlyph>;
    fn metrics(&self, font: FontKey, size: f32) -> FontMetrics;
    fn rasterize(&self, font: FontKey, glyph_id: u32, size: f32) -> GlyphBitmap;
}
