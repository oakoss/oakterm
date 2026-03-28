mod render_grid;

use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

use wgpu::CurrentSurfaceTexture;

use oakterm_protocol::frame::Frame;
use oakterm_protocol::input::{KeyInput, Resize};
use oakterm_protocol::message::{
    ClientHello, ClientType, HandshakeStatus, MSG_DETACH, MSG_DIRTY_NOTIFY, MSG_GET_RENDER_UPDATE,
    MSG_RENDER_UPDATE, MSG_SERVER_HELLO,
};
use oakterm_protocol::render::{GetRenderUpdate, RenderUpdate};

use oakterm_renderer::atlas::AtlasPlane;
use oakterm_renderer::font;
use oakterm_renderer::pipeline::{BgUniforms, RenderPipeline, TextUniforms};
use oakterm_renderer::shaper::FontKey;
use oakterm_renderer::swash_shaper::SwashShaper;

use render_grid::ClientGrid;

// AccessKit no-op handlers per Spec-0006 lazy activation.

struct NoOpActivationHandler;
impl accesskit::ActivationHandler for NoOpActivationHandler {
    fn request_initial_tree(&mut self) -> Option<accesskit::TreeUpdate> {
        None
    }
}

struct NoOpActionHandler;
impl accesskit::ActionHandler for NoOpActionHandler {
    fn do_action(&mut self, _request: accesskit::ActionRequest) {}
}

struct NoOpDeactivationHandler;
impl accesskit::DeactivationHandler for NoOpDeactivationHandler {
    fn deactivate_accessibility(&mut self) {}
}

/// Events sent from the daemon reader thread to the winit event loop.
#[derive(Debug)]
enum UserEvent {
    RenderUpdate(Box<RenderUpdate>),
    Disconnected,
}

/// GPU state created after the window and surface are available.
struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: RenderPipeline,
    atlas_texture: wgpu::Texture,
    atlas_view: wgpu::TextureView,
    atlas_sampler: wgpu::Sampler,
}

/// Font and glyph state for text rendering.
struct FontState {
    shaper: SwashShaper,
    font_key: FontKey,
    atlas: AtlasPlane,
    font_size: f32,
    metrics: oakterm_renderer::shaper::FontMetrics,
}

/// Thread-safe handle for writing frames to the daemon socket.
#[derive(Clone)]
struct DaemonWriter {
    stream: Arc<Mutex<UnixStream>>,
}

impl DaemonWriter {
    fn send_frame(&self, frame: &Frame) -> std::io::Result<()> {
        let data = frame.encode_to_vec();
        let mut stream = self.stream.lock().expect("daemon writer lock poisoned");
        stream.write_all(&data)
    }
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    font: Option<FontState>,
    grid: Option<ClientGrid>,
    daemon: Option<DaemonWriter>,
    proxy: EventLoopProxy<UserEvent>,
    daemon_process: Option<std::process::Child>,
    #[allow(dead_code)] // Must stay alive for the window's lifetime.
    accesskit: Option<accesskit_winit::Adapter>,
    last_sent_dims: (u16, u16),
    /// Set after initial Resize is sent. Gates on first `RedrawRequested`.
    initial_resize_sent: bool,
}

impl App {
    fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            window: None,
            gpu: None,
            font: None,
            grid: None,
            daemon: None,
            proxy,
            daemon_process: None,
            accesskit: None,
            last_sent_dims: (0, 0),
            initial_resize_sent: false,
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = WindowAttributes::default()
            .with_title("oakterm")
            .with_visible(false)
            .with_inner_size(winit::dpi::LogicalSize::new(800, 600));

        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        );

        // AccessKit adapter must be created before the window is shown (Spec-0006).
        let accesskit = accesskit_winit::Adapter::with_direct_handlers(
            event_loop,
            &window,
            NoOpActivationHandler,
            NoOpActionHandler,
            NoOpDeactivationHandler,
        );
        self.accesskit = Some(accesskit);

        window.set_visible(true);

        let gpu = pollster::block_on(init_gpu(window.clone()));

        // Load font.
        let font_size = 14.0;
        let font_state = init_font(font_size);

        let size = window.inner_size();
        let (cols, rows) = window_to_grid_dims(size, &font_state.metrics);
        let grid = ClientGrid::new(cols.max(1), rows.max(1));

        match connect_to_daemon(&self.proxy) {
            Ok((writer, child)) => {
                self.daemon = Some(writer);
                self.daemon_process = child;
            }
            Err(e) => {
                eprintln!("fatal: failed to connect to daemon: {e}");
                event_loop.exit();
                return;
            }
        }

        self.window = Some(window);
        self.gpu = Some(gpu);
        self.font = Some(font_state);
        self.grid = Some(grid);
    }

    #[allow(clippy::too_many_lines)]
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        if let (Some(adapter), Some(window)) = (&mut self.accesskit, &self.window) {
            adapter.process_event(window, &event);
        }

        match event {
            WindowEvent::CloseRequested => {
                if let Some(daemon) = &mut self.daemon {
                    if let Ok(frame) = Frame::new(MSG_DETACH, 0, vec![]) {
                        let _ = daemon.send_frame(&frame); // Best-effort on exit.
                    }
                }
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    if size.width > 0 && size.height > 0 {
                        gpu.config.width = size.width;
                        gpu.config.height = size.height;
                        gpu.surface.configure(&gpu.device, &gpu.config);

                        #[allow(clippy::cast_possible_truncation)]
                        if let (Some(font), Some(grid)) = (&self.font, &mut self.grid) {
                            let (cols, rows) = window_to_grid_dims(size, &font.metrics);
                            grid.resize(cols, rows);

                            // Defer until RedrawRequested; startup fires multiple Resized events.
                            if self.initial_resize_sent && (cols, rows) != self.last_sent_dims {
                                self.last_sent_dims = (cols, rows);
                                if let Some(daemon) = &mut self.daemon {
                                    let msg = Resize {
                                        pane_id: 0,
                                        cols,
                                        rows,
                                        pixel_width: size.width.min(u32::from(u16::MAX)) as u16,
                                        pixel_height: size.height.min(u32::from(u16::MAX)) as u16,
                                    };
                                    match msg.to_frame() {
                                        Ok(frame) => {
                                            if let Err(e) = daemon.send_frame(&frame) {
                                                eprintln!("daemon write failed: {e}");
                                                self.daemon = None;
                                                event_loop.exit();
                                            }
                                        }
                                        Err(e) => eprintln!("failed to encode resize: {e}"),
                                    }
                                }
                            }
                        }

                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        state: ElementState::Pressed,
                        logical_key,
                        text,
                        ..
                    },
                ..
            } => {
                let bytes = key_to_bytes(&logical_key, text.as_deref());
                if let (Some(daemon), Some(bytes)) = (&mut self.daemon, bytes) {
                    let msg = KeyInput {
                        pane_id: 0,
                        key_data: bytes,
                    };
                    match msg.to_frame() {
                        Ok(frame) => {
                            if let Err(e) = daemon.send_frame(&frame) {
                                eprintln!("daemon write failed: {e}");
                                self.daemon = None;
                                event_loop.exit();
                            }
                        }
                        Err(e) => eprintln!("failed to encode key input: {e}"),
                    }
                }
            }
            #[allow(clippy::cast_precision_loss)] // viewport dimensions fit in f32
            WindowEvent::RedrawRequested => {
                let Some(gpu) = &mut self.gpu else { return };

                // First RedrawRequested: window dimensions have settled. Send the
                // initial Resize that triggers PTY spawn on the daemon side.
                if !self.initial_resize_sent {
                    #[allow(clippy::cast_possible_truncation)]
                    if let (Some(font), Some(_), Some(daemon)) =
                        (&self.font, &self.grid, &mut self.daemon)
                    {
                        let size =
                            winit::dpi::PhysicalSize::new(gpu.config.width, gpu.config.height);
                        let (cols, rows) = window_to_grid_dims(size, &font.metrics);
                        self.last_sent_dims = (cols, rows);
                        let msg = Resize {
                            pane_id: 0,
                            cols,
                            rows,
                            pixel_width: size.width.min(u32::from(u16::MAX)) as u16,
                            pixel_height: size.height.min(u32::from(u16::MAX)) as u16,
                        };
                        match msg.to_frame() {
                            Ok(frame) => {
                                if let Err(e) = daemon.send_frame(&frame) {
                                    eprintln!("daemon write failed: {e}");
                                    self.daemon = None;
                                    event_loop.exit();
                                    return;
                                }
                                self.initial_resize_sent = true;
                            }
                            Err(e) => {
                                eprintln!("fatal: failed to encode initial resize: {e}");
                                event_loop.exit();
                                return;
                            }
                        }
                    }
                    // If font/grid/daemon not ready, retry on next RedrawRequested.
                }
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

                let (bg_colors, glyph_instances) =
                    if let (Some(grid), Some(font)) = (&self.grid, &mut self.font) {
                        let bg = grid.bg_colors();
                        let (glyphs, uploads) = grid.glyph_instances(
                            &font.metrics,
                            font.font_key,
                            font.font_size,
                            &font.shaper,
                            &mut font.atlas,
                        );

                        upload_glyphs_to_atlas(
                            &gpu.device,
                            &gpu.queue,
                            &mut gpu.atlas_texture,
                            &mut gpu.atlas_view,
                            &font.atlas,
                            &uploads,
                        );

                        (bg, glyphs)
                    } else {
                        (vec![], vec![])
                    };

                let (cols, rows) = self
                    .grid
                    .as_ref()
                    .map_or((0u32, 0u32), |g| (u32::from(g.cols), u32::from(g.rows)));

                let (atlas_w, atlas_h) = self
                    .font
                    .as_ref()
                    .map_or((256u32, 256u32), |f| f.atlas.size());

                let bg_uniforms = BgUniforms {
                    cols,
                    rows,
                    cell_width: self.font.as_ref().map_or(8.0, |f| f.metrics.cell_width),
                    cell_height: self.font.as_ref().map_or(16.0, |f| f.metrics.cell_height),
                    viewport_width: gpu.config.width as f32,
                    viewport_height: gpu.config.height as f32,
                    pad: [0.0; 2],
                };
                let text_uniforms = TextUniforms {
                    cell_width: self.font.as_ref().map_or(8.0, |f| f.metrics.cell_width),
                    cell_height: self.font.as_ref().map_or(16.0, |f| f.metrics.cell_height),
                    viewport_width: gpu.config.width as f32,
                    viewport_height: gpu.config.height as f32,
                    atlas_width: atlas_w as f32,
                    atlas_height: atlas_h as f32,
                    text_contrast: 1.2,
                    pad: 0.0,
                };

                gpu.pipeline.render(
                    &gpu.device,
                    &gpu.queue,
                    &view,
                    &bg_uniforms,
                    &bg_colors,
                    &text_uniforms,
                    &glyph_instances,
                    &gpu.atlas_view,
                    &gpu.atlas_sampler,
                );

                if let Some(w) = &self.window {
                    w.pre_present_notify();
                }
                frame.present();
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::RenderUpdate(update) => {
                if let Some(grid) = &mut self.grid {
                    grid.apply_update(&update);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            UserEvent::Disconnected => {
                eprintln!("daemon disconnected");
                event_loop.exit();
            }
        }
    }
}

/// Convert a winit key event to PTY bytes.
fn key_to_bytes(key: &Key, text: Option<&str>) -> Option<Vec<u8>> {
    if let Some(t) = text {
        if !t.is_empty() {
            return Some(t.as_bytes().to_vec());
        }
    }

    if let Key::Named(named) = key {
        let seq: &[u8] = match named {
            NamedKey::ArrowUp => b"\x1b[A",
            NamedKey::ArrowDown => b"\x1b[B",
            NamedKey::ArrowRight => b"\x1b[C",
            NamedKey::ArrowLeft => b"\x1b[D",
            NamedKey::Home => b"\x1b[H",
            NamedKey::End => b"\x1b[F",
            NamedKey::Insert => b"\x1b[2~",
            NamedKey::Delete => b"\x1b[3~",
            NamedKey::PageUp => b"\x1b[5~",
            NamedKey::PageDown => b"\x1b[6~",
            NamedKey::Escape => b"\x1b",
            NamedKey::Tab => b"\t",
            NamedKey::Enter => b"\r",
            NamedKey::Backspace => b"\x7f",
            NamedKey::F1 => b"\x1bOP",
            NamedKey::F2 => b"\x1bOQ",
            NamedKey::F3 => b"\x1bOR",
            NamedKey::F4 => b"\x1bOS",
            NamedKey::F5 => b"\x1b[15~",
            NamedKey::F6 => b"\x1b[17~",
            NamedKey::F7 => b"\x1b[18~",
            NamedKey::F8 => b"\x1b[19~",
            NamedKey::F9 => b"\x1b[20~",
            NamedKey::F10 => b"\x1b[21~",
            NamedKey::F11 => b"\x1b[23~",
            NamedKey::F12 => b"\x1b[24~",
            _ => return None,
        };
        return Some(seq.to_vec());
    }

    None
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn window_to_grid_dims(
    size: winit::dpi::PhysicalSize<u32>,
    metrics: &oakterm_renderer::shaper::FontMetrics,
) -> (u16, u16) {
    let cols = ((size.width as f32 / metrics.cell_width) as u16).max(1);
    let rows = ((size.height as f32 / metrics.cell_height) as u16).max(1);
    (cols, rows)
}

fn init_font(font_size: f32) -> FontState {
    let db = font::system_font_db();
    let (metrics, data) =
        font::load_default_metrics(&db, font_size).expect("no system monospace font found");

    let mut shaper = SwashShaper::new();
    let font_key = shaper
        .load_font(data, font_size)
        .expect("failed to load font into shaper");

    FontState {
        shaper,
        font_key,
        atlas: AtlasPlane::new(),
        font_size,
        metrics,
    }
}

/// Upload new glyph bitmaps to the GPU atlas texture.
fn upload_glyphs_to_atlas(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    atlas_texture: &mut wgpu::Texture,
    atlas_view: &mut wgpu::TextureView,
    atlas: &AtlasPlane,
    uploads: &[render_grid::GlyphUpload],
) {
    let (atlas_w, atlas_h) = atlas.size();
    let tex_size = atlas_texture.size();

    if tex_size.width != atlas_w || tex_size.height != atlas_h {
        *atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
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
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        *atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
    }

    for upload in uploads {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: atlas_texture,
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

/// Connect to the daemon, spawning it if needed.
fn connect_to_daemon(
    proxy: &EventLoopProxy<UserEvent>,
) -> std::io::Result<(DaemonWriter, Option<std::process::Child>)> {
    let socket_path = oakterm_daemon::socket::socket_path()?;
    let mut child = None;

    if !socket_path.exists() {
        let daemon_bin = std::env::current_exe()?
            .parent()
            .expect("exe has parent dir")
            .join("oakterm-daemon");

        child = Some(
            std::process::Command::new(&daemon_bin)
                .spawn()
                .map_err(|e| {
                    std::io::Error::new(
                        e.kind(),
                        format!("failed to spawn daemon at {}: {e}", daemon_bin.display()),
                    )
                })?,
        );

        for _ in 0..50 {
            if socket_path.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        if !socket_path.exists() {
            let detail = match child.as_mut().and_then(|c| c.try_wait().ok()) {
                Some(Some(status)) => format!("daemon exited with {status}"),
                Some(None) => "daemon running but socket not created after 2.5s".into(),
                None => "could not check daemon status".into(),
            };
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "daemon socket not available at {}: {detail}",
                    socket_path.display()
                ),
            ));
        }
    }

    let stream = UnixStream::connect(&socket_path)?;
    let mut read_stream = stream.try_clone()?;
    let write_stream = Arc::new(Mutex::new(stream));

    let writer = DaemonWriter {
        stream: Arc::clone(&write_stream),
    };
    handshake(&writer, &mut read_stream)?;

    let reader_writer = writer.clone();
    let proxy = proxy.clone();
    std::thread::spawn(move || {
        daemon_reader(read_stream, &reader_writer, &proxy);
    });

    Ok((writer, child))
}

/// Perform the protocol handshake per Spec-0001.
fn handshake(writer: &DaemonWriter, read_stream: &mut UnixStream) -> std::io::Result<()> {
    let hello = ClientHello {
        protocol_version_major: ClientHello::VERSION_MAJOR,
        protocol_version_minor: ClientHello::VERSION_MINOR,
        client_type: ClientType::Gui,
        client_name: "oakterm".to_string(),
    };
    let frame = hello.to_frame(1)?;
    writer.send_frame(&frame)?;

    let response = read_frame(read_stream)?;
    if response.msg_type != MSG_SERVER_HELLO {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "expected ServerHello",
        ));
    }

    let server_hello = oakterm_protocol::message::ServerHello::decode(&response.payload)?;
    if server_hello.status != HandshakeStatus::Accepted {
        return Err(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            format!("handshake rejected: {:?}", server_hello.status),
        ));
    }

    Ok(())
}

/// Background thread: read frames, request render updates on `DirtyNotify`.
fn daemon_reader(
    mut read_stream: UnixStream,
    writer: &DaemonWriter,
    proxy: &EventLoopProxy<UserEvent>,
) {
    let mut seqno: u64 = 0;

    loop {
        match read_frame(&mut read_stream) {
            Ok(frame) => match frame.msg_type {
                MSG_DIRTY_NOTIFY => {
                    let req = GetRenderUpdate {
                        pane_id: 0,
                        since_seqno: seqno,
                    };
                    let payload = req.encode();
                    let req_frame = Frame::new(MSG_GET_RENDER_UPDATE, 1, payload)
                        .expect("GetRenderUpdate payload fits in frame");
                    if let Err(e) = writer.send_frame(&req_frame) {
                        eprintln!("daemon write error: {e}");
                        let _ = proxy.send_event(UserEvent::Disconnected);
                        break;
                    }
                }
                MSG_RENDER_UPDATE => match RenderUpdate::decode(&frame.payload) {
                    Ok(update) => {
                        seqno = update.seqno;
                        let _ = proxy.send_event(UserEvent::RenderUpdate(Box::new(update)));
                    }
                    Err(e) => {
                        eprintln!(
                            "failed to decode RenderUpdate ({} bytes), disconnecting: {e}",
                            frame.payload.len()
                        );
                        let _ = proxy.send_event(UserEvent::Disconnected);
                        break;
                    }
                },
                other => {
                    eprintln!("unhandled daemon message: 0x{other:04x}");
                }
            },
            Err(e) => {
                eprintln!("daemon read error: {e}");
                let _ = proxy.send_event(UserEvent::Disconnected);
                break;
            }
        }
    }
}

/// Read a single frame from a blocking stream.
fn read_frame(stream: &mut impl std::io::Read) -> std::io::Result<Frame> {
    use oakterm_protocol::frame::{HEADER_SIZE, MAGIC, MAX_PAYLOAD};

    let mut header = [0u8; HEADER_SIZE];
    stream.read_exact(&mut header)?;

    if header[0..2] != MAGIC {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid magic bytes",
        ));
    }

    let msg_type = u16::from_le_bytes([header[3], header[4]]);
    let serial = u32::from_le_bytes([header[5], header[6], header[7], header[8]]);
    let payload_len = u32::from_le_bytes([header[9], header[10], header[11], header[12]]);

    if payload_len > MAX_PAYLOAD {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("payload too large: {payload_len}"),
        ));
    }

    let mut payload = vec![0u8; payload_len as usize];
    if !payload.is_empty() {
        stream.read_exact(&mut payload)?;
    }

    Frame::new(msg_type, serial, payload)
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
    // AtlasPlane::new() creates a 256x256 atlas — match the GPU texture.
    let (atlas_w, atlas_h) = AtlasPlane::new().size();
    let (atlas_texture, atlas_view, atlas_sampler) =
        create_atlas_texture(&device, atlas_w, atlas_h);

    GpuState {
        surface,
        device,
        queue,
        config,
        pipeline,
        atlas_texture,
        atlas_view,
        atlas_sampler,
    }
}

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("{}", version_string());
        return;
    }

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let proxy = event_loop.create_proxy();
    let mut app = App::new(proxy);
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
