use criterion::{Criterion, criterion_group, criterion_main};
use oakterm_renderer::atlas::{AtlasPlane, GlyphCacheKey};
use oakterm_renderer::pipeline::{BgUniforms, GlyphVertex, TextUniforms};
use oakterm_renderer::shaper::GlyphPlacement;

fn prepopulated_atlas(count: u32) -> AtlasPlane {
    // Use a large atlas so allocation doesn't limit the glyph count.
    let mut atlas = AtlasPlane::with_size(2048, 2048);
    for i in 0..count {
        let key = GlyphCacheKey {
            font_id: 0,
            glyph_id: i,
            size_tenths: 140,
        };
        atlas.insert(key, 8, 14, GlyphPlacement { top: 12, left: 0 });
    }
    atlas
}

fn atlas_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("atlas");

    // Same-key repeated hit at different atlas sizes.
    for &count in &[100u32, 256, 512, 1024] {
        group.bench_function(format!("cache_hit_{count}"), |b| {
            let mut atlas = prepopulated_atlas(count);
            let key = GlyphCacheKey {
                font_id: 0,
                glyph_id: count / 2,
                size_tenths: 140,
            };
            b.iter(|| {
                std::hint::black_box(atlas.get(&key));
            });
        });
    }

    // Sequential access pattern — different key each iteration (worst case for LRU promote).
    for &count in &[100u32, 256, 512, 1024] {
        group.bench_function(format!("cache_hit_sequential_{count}"), |b| {
            let mut atlas = prepopulated_atlas(count);
            let mut i = 0u32;
            b.iter(|| {
                let key = GlyphCacheKey {
                    font_id: 0,
                    glyph_id: i % count,
                    size_tenths: 140,
                };
                std::hint::black_box(atlas.get(&key));
                i = i.wrapping_add(1);
            });
        });
    }

    group.bench_function("cache_miss_insert", |b| {
        b.iter_batched(
            || prepopulated_atlas(100),
            |mut atlas| {
                let key = GlyphCacheKey {
                    font_id: 0,
                    glyph_id: 999,
                    size_tenths: 140,
                };
                std::hint::black_box(atlas.insert(key, 8, 14, GlyphPlacement { top: 12, left: 0 }));
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

fn build_bg_colors(c: &mut Criterion) {
    let cols = 120u32;
    let rows = 40u32;
    let count = (cols * rows) as usize;

    #[allow(clippy::cast_possible_truncation)]
    let cells: Vec<[u8; 3]> = (0..count).map(|i| [(i % 256) as u8, 0, 0]).collect();

    c.bench_function("build_bg_colors_120x40", |b| {
        b.iter(|| {
            let colors: Vec<u32> = cells
                .iter()
                .map(|c| {
                    0xFF_00_00_00
                        | (u32::from(c[2]) << 16)
                        | (u32::from(c[1]) << 8)
                        | u32::from(c[0])
                })
                .collect();
            std::hint::black_box(colors);
        });
    });
}

fn build_uniforms(c: &mut Criterion) {
    c.bench_function("build_uniforms", |b| {
        b.iter(|| {
            let bg = BgUniforms {
                cols: 120,
                rows: 40,
                cell_width: 8.0,
                cell_height: 16.0,
                viewport_width: 960.0,
                viewport_height: 640.0,
                pad: [0.0; 2],
            };
            let text = TextUniforms {
                cell_width: 8.0,
                cell_height: 16.0,
                viewport_width: 960.0,
                viewport_height: 640.0,
                atlas_width: 256.0,
                atlas_height: 256.0,
                text_contrast: 1.2,
                pad: 0.0,
            };
            std::hint::black_box((bg, text));
        });
    });
}

#[allow(clippy::cast_precision_loss)]
fn build_glyph_vertices(c: &mut Criterion) {
    let count = 2000;
    c.bench_function("build_glyph_vertices_2000", |b| {
        b.iter(|| {
            let glyphs: Vec<GlyphVertex> = (0..count)
                .map(|i| {
                    let col = i % 120;
                    let row = i / 120;
                    GlyphVertex {
                        pos: [col as f32 * 8.0, row as f32 * 16.0],
                        size: [8.0, 14.0],
                        uv_origin: [0.0, 0.0],
                        fg_color: [1.0, 1.0, 1.0, 1.0],
                        bg_luminance: 0.0,
                        pad: [0.0; 3],
                    }
                })
                .collect();
            std::hint::black_box(glyphs);
        });
    });
}

criterion_group!(
    benches,
    atlas_lookup,
    build_bg_colors,
    build_uniforms,
    build_glyph_vertices
);
criterion_main!(benches);
