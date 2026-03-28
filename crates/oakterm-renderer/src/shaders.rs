//! WGSL shaders for the two-pass terminal renderer.

/// Background pass: renders cell background colors.
/// Reads from a storage buffer of packed RGBA colors, one per cell.
pub const BACKGROUND_SHADER: &str = r"
struct Uniforms {
    cols: u32,
    rows: u32,
    cell_width: f32,
    cell_height: f32,
    viewport_width: f32,
    viewport_height: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> bg_colors: array<u32>;

struct VertexOutput {
    @builtin(position) position: vec4f,
    @location(0) color: vec4f,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VertexOutput {
    let col = ii % u.cols;
    let row = ii / u.cols;

    let x0 = f32(col) * u.cell_width;
    let y0 = f32(row) * u.cell_height;

    // Quad vertices: 0=TL, 1=TR, 2=BL, 3=BR. Triangle strip: 0,1,2,3.
    let x = select(x0, x0 + u.cell_width, vi == 1u || vi == 3u);
    let y = select(y0, y0 + u.cell_height, vi == 2u || vi == 3u);

    // Convert pixel coords to NDC (-1..1).
    let ndc_x = (x / u.viewport_width) * 2.0 - 1.0;
    let ndc_y = 1.0 - (y / u.viewport_height) * 2.0;

    // Unpack RGBA from u32 (ABGR packed).
    let packed = bg_colors[ii];
    let r = f32(packed & 0xFFu) / 255.0;
    let g = f32((packed >> 8u) & 0xFFu) / 255.0;
    let b = f32((packed >> 16u) & 0xFFu) / 255.0;
    let a = f32((packed >> 24u) & 0xFFu) / 255.0;

    var out: VertexOutput;
    out.position = vec4f(ndc_x, ndc_y, 0.0, 1.0);
    out.color = vec4f(r, g, b, a);
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4f {
    return in.color;
}
";

/// Text pass: renders glyph quads from the atlas.
/// Each instance is one glyph with position, atlas UV, and foreground color.
pub const TEXT_SHADER: &str = r"
struct Uniforms {
    cell_width: f32,
    cell_height: f32,
    viewport_width: f32,
    viewport_height: f32,
    atlas_width: f32,
    atlas_height: f32,
    text_contrast: f32,
    _pad: f32,
}

struct GlyphInstance {
    // Pixel position of the glyph.
    @location(0) pos: vec2f,
    // Size of the glyph in pixels.
    @location(1) size: vec2f,
    // UV origin in the atlas (pixels).
    @location(2) uv_origin: vec2f,
    // Foreground color (linear RGB + alpha).
    @location(3) fg_color: vec4f,
    // Background luminance (for text contrast adjustment).
    @location(4) bg_luminance: f32,
}

struct VertexOutput {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
    @location(1) fg_color: vec4f,
    @location(2) bg_luminance: f32,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas_texture: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, glyph: GlyphInstance) -> VertexOutput {
    let x = select(glyph.pos.x, glyph.pos.x + glyph.size.x, vi == 1u || vi == 3u);
    let y = select(glyph.pos.y, glyph.pos.y + glyph.size.y, vi == 2u || vi == 3u);

    let ndc_x = (x / u.viewport_width) * 2.0 - 1.0;
    let ndc_y = 1.0 - (y / u.viewport_height) * 2.0;

    let uv_x = select(glyph.uv_origin.x, glyph.uv_origin.x + glyph.size.x, vi == 1u || vi == 3u);
    let uv_y = select(glyph.uv_origin.y, glyph.uv_origin.y + glyph.size.y, vi == 2u || vi == 3u);

    var out: VertexOutput;
    out.position = vec4f(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = vec2f(uv_x / u.atlas_width, uv_y / u.atlas_height);
    out.fg_color = glyph.fg_color;
    out.bg_luminance = glyph.bg_luminance;
    return out;
}

fn luminance(c: vec3f) -> f32 {
    return 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4f {
    let alpha = textureSample(atlas_texture, atlas_sampler, in.uv).r;

    // Text contrast adjustment: boost alpha when fg/bg contrast is low.
    let fg_lum = luminance(in.fg_color.rgb);
    let contrast_diff = abs(fg_lum - in.bg_luminance);
    let adjusted_alpha = alpha * mix(u.text_contrast, 1.0, contrast_diff);

    return vec4f(in.fg_color.rgb, in.fg_color.a * adjusted_alpha);
}
";
