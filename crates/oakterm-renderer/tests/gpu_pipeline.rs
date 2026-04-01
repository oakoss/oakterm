//! Headless GPU tests for the render pipeline.
//! Only compiled with `--features gpu-tests`.
//! Run: `cargo test -p oakterm-renderer --features gpu-tests`
#![cfg(feature = "gpu-tests")]

use oakterm_renderer::pipeline::{BgUniforms, GlyphVertex, RenderPipeline, TextUniforms};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

// --- Helpers ---

async fn create_test_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .ok()?;

    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor::default())
        .await
        .ok()?;

    Some((device, queue))
}

fn create_render_target(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test_render_target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn create_test_atlas(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView, wgpu::Sampler) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("test_atlas"),
        size: wgpu::Extent3d {
            width: 16,
            height: 16,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    (texture, view, sampler)
}

#[allow(clippy::cast_precision_loss)] // cols/rows are small test values
fn bg_uniforms(cols: u32, rows: u32, cell_w: f32, cell_h: f32) -> BgUniforms {
    BgUniforms {
        cols,
        rows,
        cell_width: cell_w,
        cell_height: cell_h,
        viewport_width: cols as f32 * cell_w,
        viewport_height: rows as f32 * cell_h,
        pad: [0.0; 2],
    }
}

fn text_uniforms(cell_w: f32, cell_h: f32, vp_w: f32, vp_h: f32) -> TextUniforms {
    TextUniforms {
        cell_width: cell_w,
        cell_height: cell_h,
        viewport_width: vp_w,
        viewport_height: vp_h,
        atlas_width: 16.0,
        atlas_height: 16.0,
        text_gamma: 1.7,
        pad: 0.0,
    }
}

/// Read back RGBA pixels from a render target texture.
fn read_pixels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let unpadded_row = width * 4;
    let padded_row = (unpadded_row + 255) & !255; // align to 256

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(padded_row * height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));

    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    slice.map_async(wgpu::MapMode::Read, move |result| {
        tx.send(result).expect("readback: channel send failed");
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("readback: device poll failed");
    rx.recv()
        .expect("readback: callback never fired")
        .expect("readback: GPU buffer mapping failed");

    let data = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        let start = (row * padded_row) as usize;
        let end = start + unpadded_row as usize;
        pixels.extend_from_slice(&data[start..end]);
    }
    pixels
}

/// Get the RGBA pixel at (x, y) from a flat pixel buffer.
fn pixel_at(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let offset = ((y * width + x) * 4) as usize;
    [
        pixels[offset],
        pixels[offset + 1],
        pixels[offset + 2],
        pixels[offset + 3],
    ]
}

// --- Tests ---

#[test]
fn pipeline_creation_succeeds() {
    let (device, _queue) = pollster::block_on(create_test_device()).expect("no GPU adapter");
    let _pipeline = RenderPipeline::new(
        &device,
        FORMAT,
        oakterm_renderer::shaders::BLENDING_LINEAR_CORRECTED,
    );
}

#[test]
fn render_empty_grid() {
    let (device, queue) = pollster::block_on(create_test_device()).expect("no GPU adapter");
    let pipeline = RenderPipeline::new(
        &device,
        FORMAT,
        oakterm_renderer::shaders::BLENDING_LINEAR_CORRECTED,
    );

    // 1x1 pixel target — degenerate grid with no cells.
    let target = create_render_target(&device, 1, 1);
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let (_atlas_tex, atlas_view, atlas_sampler) = create_test_atlas(&device);

    let uniforms = bg_uniforms(0, 0, 8.0, 16.0);
    let text = text_uniforms(8.0, 16.0, 0.0, 0.0);

    pipeline.render(
        &device,
        &queue,
        &target_view,
        &uniforms,
        &[],
        &text,
        &[],
        &atlas_view,
        &atlas_sampler,
        wgpu::Color::BLACK,
    );
}

#[test]
fn render_partial_zero_grid() {
    let (device, queue) = pollster::block_on(create_test_device()).expect("no GPU adapter");
    let pipeline = RenderPipeline::new(
        &device,
        FORMAT,
        oakterm_renderer::shaders::BLENDING_LINEAR_CORRECTED,
    );

    let target = create_render_target(&device, 1, 1);
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let (_atlas_tex, atlas_view, atlas_sampler) = create_test_atlas(&device);

    // cols > 0 but rows = 0 — cell_count is still 0.
    let uniforms = bg_uniforms(5, 0, 8.0, 16.0);
    let text = text_uniforms(8.0, 16.0, 40.0, 0.0);

    pipeline.render(
        &device,
        &queue,
        &target_view,
        &uniforms,
        &[],
        &text,
        &[],
        &atlas_view,
        &atlas_sampler,
        wgpu::Color::BLACK,
    );
}

#[test]
fn render_background_produces_correct_colors() {
    let (device, queue) = pollster::block_on(create_test_device()).expect("no GPU adapter");
    let pipeline = RenderPipeline::new(
        &device,
        FORMAT,
        oakterm_renderer::shaders::BLENDING_LINEAR_CORRECTED,
    );

    let target = create_render_target(&device, 160, 48);
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let (_atlas_tex, atlas_view, atlas_sampler) = create_test_atlas(&device);

    // 2x2 grid: red, green, blue, white (ABGR packed).
    let bg_colors: Vec<u32> = vec![0xFF_00_00_FF, 0xFF_00_FF_00, 0xFF_FF_00_00, 0xFF_FF_FF_FF];
    let uniforms = bg_uniforms(2, 2, 80.0, 24.0);
    let text = text_uniforms(80.0, 24.0, 160.0, 48.0);

    pipeline.render(
        &device,
        &queue,
        &target_view,
        &uniforms,
        &bg_colors,
        &text,
        &[],
        &atlas_view,
        &atlas_sampler,
        wgpu::Color::BLACK,
    );

    let pixels = read_pixels(&device, &queue, &target, 160, 48);

    // Sample center of each quadrant.
    let top_left = pixel_at(&pixels, 160, 40, 12);
    let top_right = pixel_at(&pixels, 160, 120, 12);
    let bot_left = pixel_at(&pixels, 160, 40, 36);
    let bot_right = pixel_at(&pixels, 160, 120, 36);

    // sRGB encoding is identity for 0 and 255, so exact match works.
    assert_eq!(top_left, [255, 0, 0, 255], "top-left should be red");
    assert_eq!(top_right, [0, 255, 0, 255], "top-right should be green");
    assert_eq!(bot_left, [0, 0, 255, 255], "bottom-left should be blue");
    assert_eq!(
        bot_right,
        [255, 255, 255, 255],
        "bottom-right should be white"
    );
}

#[test]
fn render_single_glyph() {
    let (device, queue) = pollster::block_on(create_test_device()).expect("no GPU adapter");
    let pipeline = RenderPipeline::new(
        &device,
        FORMAT,
        oakterm_renderer::shaders::BLENDING_LINEAR_CORRECTED,
    );

    let target = create_render_target(&device, 160, 48);
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let (_atlas_tex, atlas_view, atlas_sampler) = create_test_atlas(&device);

    let bg_colors: Vec<u32> = vec![0xFF_00_00_00; 4];
    let uniforms = bg_uniforms(2, 2, 80.0, 24.0);
    let text = text_uniforms(80.0, 24.0, 160.0, 48.0);

    let glyphs = vec![GlyphVertex {
        pos: [10.0, 4.0],
        size: [8.0, 14.0],
        uv_origin: [0.0, 0.0],
        fg_color: [1.0, 1.0, 1.0, 1.0],
        bg_luminance: 0.0,
        pad: [0.0; 3],
    }];

    pipeline.render(
        &device,
        &queue,
        &target_view,
        &uniforms,
        &bg_colors,
        &text,
        &glyphs,
        &atlas_view,
        &atlas_sampler,
        wgpu::Color::BLACK,
    );
}

#[test]
fn render_text_produces_visible_pixels() {
    let (device, queue) = pollster::block_on(create_test_device()).expect("no GPU adapter");
    let pipeline = RenderPipeline::new(
        &device,
        FORMAT,
        oakterm_renderer::shaders::BLENDING_LINEAR_CORRECTED,
    );

    let target = create_render_target(&device, 160, 48);
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let (atlas_tex, atlas_view, atlas_sampler) = create_test_atlas(&device);

    // Write opaque white block to the atlas so glyphs produce visible output.
    let atlas_data = vec![0xFF_u8; 8 * 14];
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &atlas_tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &atlas_data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(8),
            rows_per_image: None,
        },
        wgpu::Extent3d {
            width: 8,
            height: 14,
            depth_or_array_layers: 1,
        },
    );

    let bg_colors: Vec<u32> = vec![0xFF_00_00_00; 4];
    let uniforms = bg_uniforms(2, 2, 80.0, 24.0);
    let text = text_uniforms(80.0, 24.0, 160.0, 48.0);

    let glyphs = vec![
        GlyphVertex {
            pos: [10.0, 4.0],
            size: [8.0, 14.0],
            uv_origin: [0.0, 0.0],
            fg_color: [1.0, 1.0, 1.0, 1.0],
            bg_luminance: 0.0,
            pad: [0.0; 3],
        },
        GlyphVertex {
            pos: [18.0, 4.0],
            size: [8.0, 14.0],
            uv_origin: [0.0, 0.0],
            fg_color: [0.0, 1.0, 0.0, 1.0],
            bg_luminance: 0.0,
            pad: [0.0; 3],
        },
    ];

    pipeline.render(
        &device,
        &queue,
        &target_view,
        &uniforms,
        &bg_colors,
        &text,
        &glyphs,
        &atlas_view,
        &atlas_sampler,
        wgpu::Color::BLACK,
    );

    let pixels = read_pixels(&device, &queue, &target, 160, 48);

    // Center of the first glyph region (10..18, 4..18) should have white pixels.
    let glyph_pixel = pixel_at(&pixels, 160, 14, 10);
    assert!(
        glyph_pixel[0] > 0 || glyph_pixel[1] > 0 || glyph_pixel[2] > 0,
        "glyph area should have non-black pixels, got {glyph_pixel:?}"
    );

    // Area far from any glyph should remain black (cleared by bg pass).
    let empty_pixel = pixel_at(&pixels, 160, 150, 40);
    assert_eq!(
        empty_pixel[0..3],
        [0, 0, 0],
        "empty area should be black, got {empty_pixel:?}"
    );
}
