//! GPU resources and their setup: the wgpu surface/device/queue, the render
//! pipeline, and the glyph atlas textures. `GpuState` is a plain resource
//! aggregate the `App` reads from directly; the atlas upload helpers are the
//! only entry points beyond construction.

use std::sync::Arc;

use winit::window::Window;

use oakterm_renderer::atlas::AtlasPlane;
use oakterm_renderer::pipeline::RenderPipeline;
use tracing::{info, warn};

use crate::render_grid;

/// GPU state created after the window and surface are available.
pub(crate) struct GpuState {
    pub(crate) surface: wgpu::Surface<'static>,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) config: wgpu::SurfaceConfiguration,
    pub(crate) pipeline: RenderPipeline,
    /// Written only through the `upload_*` methods, which keep it paired
    /// with its view; never read outside this module.
    atlas_texture: wgpu::Texture,
    pub(crate) atlas_view: wgpu::TextureView,
    pub(crate) atlas_sampler: wgpu::Sampler,
    color_atlas_texture: wgpu::Texture,
    pub(crate) color_atlas_view: wgpu::TextureView,
    /// Whether the surface is configured for Display P3 color space.
    pub(crate) p3_active: bool,
}

impl GpuState {
    /// Grows the texture (and refreshes its view) when the atlas outgrew
    /// it; the texture/view pairing stays consistent here rather than in
    /// callers.
    pub(crate) fn upload_glyphs(
        &mut self,
        atlas: &AtlasPlane,
        uploads: &[render_grid::GlyphUpload],
    ) {
        let (atlas_w, atlas_h) = atlas.size();
        let tex_size = self.atlas_texture.size();

        if tex_size.width != atlas_w || tex_size.height != atlas_h {
            let old_texture = std::mem::replace(
                &mut self.atlas_texture,
                self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("glyph_atlas"),
                    size: wgpu::Extent3d {
                        width: atlas_w,
                        height: atlas_h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_DST
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                }),
            );
            // Copy old content so cached glyphs aren't lost on resize.
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            let copy_w = tex_size.width.min(atlas_w);
            let copy_h = tex_size.height.min(atlas_h);
            encoder.copy_texture_to_texture(
                old_texture.as_image_copy(),
                self.atlas_texture.as_image_copy(),
                wgpu::Extent3d {
                    width: copy_w,
                    height: copy_h,
                    depth_or_array_layers: 1,
                },
            );
            self.queue.submit(std::iter::once(encoder.finish()));
            self.atlas_view = self
                .atlas_texture
                .create_view(&wgpu::TextureViewDescriptor::default());
        }

        for upload in uploads {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.atlas_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: upload.x,
                        y: upload.y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &upload.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(upload.width),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: upload.width,
                    height: upload.height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    /// Grows and re-views the texture as [`Self::upload_glyphs`] does.
    pub(crate) fn upload_color_glyphs(
        &mut self,
        color_atlas: &AtlasPlane,
        uploads: &[render_grid::GlyphUpload],
    ) {
        let (atlas_w, atlas_h) = color_atlas.size();
        let tex_size = self.color_atlas_texture.size();

        if tex_size.width != atlas_w || tex_size.height != atlas_h {
            let old_texture = std::mem::replace(
                &mut self.color_atlas_texture,
                self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("color_glyph_atlas"),
                    size: wgpu::Extent3d {
                        width: atlas_w,
                        height: atlas_h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_DST
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                }),
            );
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            let copy_w = tex_size.width.min(atlas_w);
            let copy_h = tex_size.height.min(atlas_h);
            encoder.copy_texture_to_texture(
                old_texture.as_image_copy(),
                self.color_atlas_texture.as_image_copy(),
                wgpu::Extent3d {
                    width: copy_w,
                    height: copy_h,
                    depth_or_array_layers: 1,
                },
            );
            self.queue.submit(std::iter::once(encoder.finish()));
            self.color_atlas_view = self
                .color_atlas_texture
                .create_view(&wgpu::TextureViewDescriptor::default());
        }

        for upload in uploads {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.color_atlas_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: upload.x,
                        y: upload.y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &upload.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(upload.width * 4),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: upload.width,
                    height: upload.height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }
}

pub(crate) async fn init_gpu(window: Arc<Window>, blending_mode: u32) -> Result<GpuState, String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let surface = instance
        .create_surface(window.clone())
        .map_err(|e| format!("failed to create wgpu surface: {e}"))?;

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        })
        .await
        .map_err(|e| format!("no compatible GPU adapter found: {e}"))?;

    let (device, queue): (wgpu::Device, wgpu::Queue) = adapter
        .request_device(&wgpu::DeviceDescriptor::default())
        .await
        .map_err(|e| format!("failed to create GPU device: {e}"))?;

    let caps = surface.get_capabilities(&adapter);
    // Wide-gamut output stays macOS-only: the shaders' P3 path is unexercised
    // on other platforms.
    let want_p3 = cfg!(target_os = "macos");
    let supports_p3 = |f: &wgpu::TextureFormat| {
        caps.color_spaces(*f)
            .contains(wgpu::SurfaceColorSpaces::DISPLAY_P3)
    };
    let format = caps
        .formats
        .iter()
        .find(|f| f.is_srgb() && (!want_p3 || supports_p3(f)))
        .or_else(|| caps.formats.iter().find(|f| f.is_srgb()))
        .or(caps.formats.first())
        .copied()
        .ok_or_else(|| "no compatible surface format found".to_string())?;

    let p3_active = want_p3 && supports_p3(&format);
    let color_space = if p3_active {
        wgpu::SurfaceColorSpace::DisplayP3
    } else {
        wgpu::SurfaceColorSpace::Auto
    };
    if want_p3 && !p3_active {
        warn!(
            ?format,
            "surface does not support Display P3; rendering sRGB"
        );
    }
    info!(?format, ?color_space, "surface color space selected");

    let size = window.inner_size();
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space,
        width: size.width.max(1),
        height: size.height.max(1),
        present_mode: wgpu::PresentMode::AutoVsync,
        alpha_mode: wgpu::CompositeAlphaMode::Auto,
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    };
    surface.configure(&device, &config);

    let pipeline = RenderPipeline::new(&device, format, blending_mode, p3_active);
    // AtlasPlane::new() creates a 256x256 atlas — match the GPU texture.
    let (atlas_w, atlas_h) = AtlasPlane::new().size();
    let (atlas_texture, atlas_view, atlas_sampler) =
        create_atlas_texture(&device, atlas_w, atlas_h);
    let (color_atlas_texture, color_atlas_view) =
        create_color_atlas_texture(&device, atlas_w, atlas_h);

    Ok(GpuState {
        surface,
        device,
        queue,
        config,
        pipeline,
        atlas_texture,
        atlas_view,
        atlas_sampler,
        color_atlas_texture,
        color_atlas_view,
        p3_active,
    })
}

fn create_atlas_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView, wgpu::Sampler) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("glyph_atlas"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
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

fn create_color_atlas_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("color_glyph_atlas"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}
