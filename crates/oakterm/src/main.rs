use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId};

use wgpu::CurrentSurfaceTexture;

use oakterm_renderer::pipeline::{BgUniforms, RenderPipeline, TextUniforms};

/// GPU state created after the window and surface are available.
struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: RenderPipeline,
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            gpu: None,
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = WindowAttributes::default()
            .with_title("oakterm")
            .with_inner_size(winit::dpi::LogicalSize::new(800, 600));

        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        );

        let gpu = pollster::block_on(init_gpu(window.clone()));
        self.window = Some(window);
        self.gpu = Some(gpu);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    if size.width > 0 && size.height > 0 {
                        gpu.config.width = size.width;
                        gpu.config.height = size.height;
                        gpu.surface.configure(&gpu.device, &gpu.config);
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }
            }
            #[allow(clippy::cast_precision_loss)] // viewport dimensions fit in f32
            WindowEvent::RedrawRequested => {
                let Some(gpu) = &self.gpu else { return };
                let frame = match gpu.surface.get_current_texture() {
                    CurrentSurfaceTexture::Success(frame)
                    | CurrentSurfaceTexture::Suboptimal(frame) => frame,
                    CurrentSurfaceTexture::Outdated | CurrentSurfaceTexture::Lost => {
                        gpu.surface.configure(&gpu.device, &gpu.config);
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                        return;
                    }
                    CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => return,
                    CurrentSurfaceTexture::Validation => {
                        eprintln!("wgpu surface validation error; skipping frame");
                        return;
                    }
                };

                let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
                    format: Some(gpu.config.format),
                    ..Default::default()
                });

                // For now, render an empty grid (black screen).
                let bg_uniforms = BgUniforms {
                    cols: 0,
                    rows: 0,
                    cell_width: 8.0,
                    cell_height: 16.0,
                    viewport_width: gpu.config.width as f32,
                    viewport_height: gpu.config.height as f32,
                    pad: [0.0; 2],
                };
                let text_uniforms = TextUniforms {
                    cell_width: 8.0,
                    cell_height: 16.0,
                    viewport_width: gpu.config.width as f32,
                    viewport_height: gpu.config.height as f32,
                    atlas_width: 256.0,
                    atlas_height: 256.0,
                    text_contrast: 1.2,
                    pad: 0.0,
                };

                // Empty atlas for now.
                let atlas_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("empty_atlas"),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                let atlas_view = atlas_tex.create_view(&wgpu::TextureViewDescriptor::default());
                let atlas_sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
                    mag_filter: wgpu::FilterMode::Nearest,
                    min_filter: wgpu::FilterMode::Nearest,
                    ..Default::default()
                });

                gpu.pipeline.render(
                    &gpu.device,
                    &gpu.queue,
                    &view,
                    &bg_uniforms,
                    &[],
                    &text_uniforms,
                    &[],
                    &atlas_view,
                    &atlas_sampler,
                );

                if let Some(w) = &self.window {
                    w.pre_present_notify();
                }
                frame.present();
            }
            _ => {}
        }
    }
}

async fn init_gpu(window: Arc<Window>) -> GpuState {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let surface = instance
        .create_surface(window.clone())
        .expect("failed to create wgpu surface");

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        })
        .await
        .expect("no compatible GPU adapter found");

    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor::default())
        .await
        .expect("failed to create GPU device");

    let caps = surface.get_capabilities(&adapter);
    let format = caps
        .formats
        .iter()
        .find(|f| f.is_srgb())
        .or(caps.formats.first())
        .copied()
        .expect("no compatible surface format found");

    let size = window.inner_size();
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width: size.width.max(1),
        height: size.height.max(1),
        present_mode: wgpu::PresentMode::AutoVsync,
        alpha_mode: wgpu::CompositeAlphaMode::Auto,
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    };
    surface.configure(&device, &config);

    let pipeline = RenderPipeline::new(&device, format);

    GpuState {
        surface,
        device,
        queue,
        config,
        pipeline,
    }
}

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("{}", version_string());
        return;
    }

    let event_loop = EventLoop::new().expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::new();
    event_loop.run_app(&mut app).expect("event loop error");
}

fn version_string() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let channel = env!("RELEASE_CHANNEL");
    let source = env!("INSTALL_SOURCE");
    let sha = option_env!("VERGEN_GIT_SHA").unwrap_or("unknown");
    let short_sha = &sha[..sha.len().min(7)];

    match channel {
        "dev" => format!("oakterm {version}-dev+{short_sha} ({channel}, {source})"),
        _ => format!("oakterm {version} ({channel}, {source})"),
    }
}
