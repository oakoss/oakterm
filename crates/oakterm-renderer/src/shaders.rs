//! WGSL shaders for the two-pass terminal renderer.

/// Shared color space functions used by both passes.
const COLOR_FUNCTIONS: &str = "
// sRGB → linear (IEC 61966-2-1).
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}

fn srgb_to_linear3(c: vec3f) -> vec3f {
    return vec3f(srgb_to_linear(c.r), srgb_to_linear(c.g), srgb_to_linear(c.b));
}

fn luminance(c: vec3f) -> f32 {
    return 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
}
";

/// Background pass: renders cell background colors.
/// Reads from a storage buffer of packed RGBA colors, one per cell.
#[must_use]
pub fn background_shader(blending_mode: u32, p3: bool) -> String {
    let p3_flag = u32::from(p3);
    format!(
        r"
{COLOR_FUNCTIONS}

// sRGB linear to Display P3 linear (W3C CSS Color 4, D65 white).
// Applied on macOS where CAMetalLayer is set to P3 color space.
// Column-major: each vec3f is a column of the row-major conversion matrix.
const SRGB_TO_P3: mat3x3f = mat3x3f(
    vec3f(0.8225, 0.0332, 0.0171),
    vec3f(0.1775, 0.9668, 0.0724),
    vec3f(0.0000, 0.0000, 0.9105),
);

const BLENDING_MODE: u32 = {blending_mode}u;
const P3_ENABLED: u32 = {p3_flag}u;

struct Uniforms {{
    cols: u32,
    rows: u32,
    cell_width: f32,
    cell_height: f32,
    viewport_width: f32,
    viewport_height: f32,
    pad_left: f32,
    pad_top: f32,
}}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> bg_colors: array<u32>;

struct VertexOutput {{
    @builtin(position) position: vec4f,
    @location(0) color: vec4f,
}}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VertexOutput {{
    let col = ii % u.cols;
    let row = ii / u.cols;

    let x0 = f32(col) * u.cell_width + u.pad_left;
    let y0 = f32(row) * u.cell_height + u.pad_top;

    // Quad vertices: 0=TL, 1=TR, 2=BL, 3=BR. Triangle strip: 0,1,2,3.
    let x = select(x0, x0 + u.cell_width, vi == 1u || vi == 3u);
    let y = select(y0, y0 + u.cell_height, vi == 2u || vi == 3u);

    // Convert pixel coords to NDC (-1..1).
    let ndc_x = (x / u.viewport_width) * 2.0 - 1.0;
    let ndc_y = 1.0 - (y / u.viewport_height) * 2.0;

    // Unpack sRGB from u32 (ABGR packed).
    let packed = bg_colors[ii];
    let r = f32(packed & 0xFFu) / 255.0;
    let g = f32((packed >> 8u) & 0xFFu) / 255.0;
    let b = f32((packed >> 16u) & 0xFFu) / 255.0;
    let a = f32((packed >> 24u) & 0xFFu) / 255.0;

    // Linearize sRGB for correct framebuffer output.
    var color = srgb_to_linear3(vec3f(r, g, b));
    if P3_ENABLED != 0u {{
        color = SRGB_TO_P3 * color;
    }}

    var out: VertexOutput;
    out.position = vec4f(ndc_x, ndc_y, 0.0, 1.0);
    out.color = vec4f(color, a);
    return out;
}}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4f {{
    return in.color;
}}
"
    )
}

/// Text pass: renders glyph quads from the atlas.
/// Each instance is one glyph with position, atlas UV, and foreground color.
#[must_use]
pub fn text_shader(blending_mode: u32, p3: bool) -> String {
    let p3_flag = u32::from(p3);
    format!(
        r"
{COLOR_FUNCTIONS}

const BLENDING_MODE: u32 = {blending_mode}u;
const P3_ENABLED: u32 = {p3_flag}u;

// sRGB linear to Display P3 linear (W3C CSS Color 4, D65 white).
// Column-major: each vec3f is a column of the row-major conversion matrix.
const SRGB_TO_P3: mat3x3f = mat3x3f(
    vec3f(0.8225, 0.0332, 0.0171),
    vec3f(0.1775, 0.9668, 0.0724),
    vec3f(0.0000, 0.0000, 0.9105),
);

struct Uniforms {{
    cell_width: f32,
    cell_height: f32,
    viewport_width: f32,
    viewport_height: f32,
    atlas_width: f32,
    atlas_height: f32,
    text_gamma: f32,
    color_atlas_width: f32,
    color_atlas_height: f32,
    _pad: f32,
}}

struct GlyphInstance {{
    // Pixel position of the glyph.
    @location(0) pos: vec2f,
    // Size of the glyph in pixels.
    @location(1) size: vec2f,
    // UV origin in the atlas (pixels).
    @location(2) uv_origin: vec2f,
    // Foreground color (sRGB, linearized in shader).
    @location(3) fg_color: vec4f,
    // Background luminance (linear, for text gamma adjustment).
    @location(4) bg_luminance: f32,
    // 1.0 for color emoji, 0.0 for mono text.
    @location(5) is_color: f32,
}}

struct VertexOutput {{
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
    @location(1) fg_color: vec4f,
    @location(2) bg_luminance: f32,
    @location(3) is_color: f32,
}}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas_texture: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;
@group(0) @binding(3) var color_atlas_texture: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, glyph: GlyphInstance) -> VertexOutput {{
    let x = select(glyph.pos.x, glyph.pos.x + glyph.size.x, vi == 1u || vi == 3u);
    let y = select(glyph.pos.y, glyph.pos.y + glyph.size.y, vi == 2u || vi == 3u);

    let ndc_x = (x / u.viewport_width) * 2.0 - 1.0;
    let ndc_y = 1.0 - (y / u.viewport_height) * 2.0;

    let uv_x = select(glyph.uv_origin.x, glyph.uv_origin.x + glyph.size.x, vi == 1u || vi == 3u);
    let uv_y = select(glyph.uv_origin.y, glyph.uv_origin.y + glyph.size.y, vi == 2u || vi == 3u);

    var out: VertexOutput;
    out.position = vec4f(ndc_x, ndc_y, 0.0, 1.0);
    // Normalize UV against the correct atlas dimensions.
    let aw = select(u.atlas_width, u.color_atlas_width, glyph.is_color > 0.5);
    let ah = select(u.atlas_height, u.color_atlas_height, glyph.is_color > 0.5);
    out.uv = vec2f(uv_x / aw, uv_y / ah);
    out.fg_color = glyph.fg_color;
    out.bg_luminance = glyph.bg_luminance;
    out.is_color = glyph.is_color;
    return out;
}}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4f {{
    // Solid-color quad (cursor underline/bar): skip atlas sampling.
    let fg_srgb_linear = srgb_to_linear3(in.fg_color.rgb);
    // Compute luminance in sRGB linear (matches bg_luminance from CPU).
    let fg_lum = luminance(fg_srgb_linear);

    // Convert to P3 for framebuffer output.
    var fg_linear = fg_srgb_linear;
    if P3_ENABLED != 0u {{
        fg_linear = SRGB_TO_P3 * fg_srgb_linear;
    }}
    if in.bg_luminance < -0.5 {{
        return vec4f(fg_linear, in.fg_color.a);
    }}

    // Color emoji: sample from the Rgba8UnormSrgb color atlas.
    // The GPU auto-converts sRGB->linear on read, so values are linear-space.
    if in.is_color > 0.5 {{
        var emoji = textureSample(color_atlas_texture, atlas_sampler, in.uv);
        if P3_ENABLED != 0u {{
            emoji = vec4f(SRGB_TO_P3 * emoji.rgb, emoji.a);
        }}
        return emoji;
    }}

    let alpha = textureSample(atlas_texture, atlas_sampler, in.uv).r;
    let contrast_diff = abs(fg_lum - in.bg_luminance);

    // Text gamma compensation: font rasterizers assume sRGB-space blending,
    // so linear blending makes text too thin/thick. Compensate with a
    // luminance-dependent gamma adjustment (Kitty's approach).
    var adjusted_alpha = alpha;
    if BLENDING_MODE == 2u {{
        // linear_corrected: apply gamma compensation
        let gamma_adj = mix(u.text_gamma, 1.0, contrast_diff);
        adjusted_alpha = pow(alpha, gamma_adj);
    }}

    return vec4f(fg_linear, in.fg_color.a * adjusted_alpha);
}}
"
    )
}

/// Blending mode constants matching `TextBlending` enum.
pub const BLENDING_LINEAR: u32 = 1;
pub const BLENDING_LINEAR_CORRECTED: u32 = 2;
