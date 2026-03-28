//! Headless GPU tests for the render pipeline.
//! Requires a GPU (or software adapter) to run.

use oakterm_renderer::pipeline::{BgUniforms, RenderPipeline, TextUniforms};

/// Create a headless wgpu device for testing.
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
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
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

#[test]
fn pipeline_creation_succeeds() {
    let Some((device, _queue)) = pollster::block_on(create_test_device()) else {
        eprintln!("no GPU available — skipping");
        return;
    };

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let _pipeline = RenderPipeline::new(&device, format);
}

#[test]
fn render_background_pass() {
    let Some((device, queue)) = pollster::block_on(create_test_device()) else {
        eprintln!("no GPU available — skipping");
        return;
    };

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let pipeline = RenderPipeline::new(&device, format);

    let target = create_render_target(&device, 160, 48);
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let (_atlas_tex, atlas_view, atlas_sampler) = create_test_atlas(&device);

    // Red, green, blue, white backgrounds (ABGR packed).
    let bg_colors: Vec<u32> = vec![0xFF_00_00_FF, 0xFF_00_FF_00, 0xFF_FF_00_00, 0xFF_FF_FF_FF];

    let bg_uniforms = BgUniforms {
        cols: 2,
        rows: 2,
        cell_width: 80.0,
        cell_height: 24.0,
        viewport_width: 160.0,
        viewport_height: 48.0,
        pad: [0.0; 2],
    };

    let text_uniforms = TextUniforms {
        cell_width: 80.0,
        cell_height: 24.0,
        viewport_width: 160.0,
        viewport_height: 48.0,
        atlas_width: 16.0,
        atlas_height: 16.0,
        text_contrast: 1.2,
        pad: 0.0,
    };

    pipeline.render(
        &device,
        &queue,
        &target_view,
        &bg_uniforms,
        &bg_colors,
        &text_uniforms,
        &[],
        &atlas_view,
        &atlas_sampler,
    );
}
