use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

use wgpu::CurrentSurfaceTexture;

use oakterm_protocol::frame::Frame;
use oakterm_protocol::input::{KeyInput, Resize};
use oakterm_protocol::message::{
    ClientHello, ClientType, HandshakeStatus, MSG_DETACH, MSG_DIRTY_NOTIFY, MSG_SERVER_HELLO,
};

use oakterm_renderer::pipeline::{BgUniforms, RenderPipeline, TextUniforms};

/// Events sent from the daemon reader thread to the winit event loop.
#[derive(Debug)]
enum UserEvent {
    DirtyNotify,
    Disconnected,
}

/// GPU state created after the window and surface are available.
struct GpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: RenderPipeline,
}

/// Connection to the daemon for writing messages.
struct DaemonWriter {
    stream: UnixStream,
}

impl DaemonWriter {
    fn send_frame(&mut self, frame: &Frame) -> std::io::Result<()> {
        let data = frame.encode_to_vec();
        self.stream.write_all(&data)
    }
}

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    daemon: Option<DaemonWriter>,
    proxy: EventLoopProxy<UserEvent>,
    daemon_process: Option<std::process::Child>,
}

impl App {
    fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            window: None,
            gpu: None,
            daemon: None,
            proxy,
            daemon_process: None,
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
            .with_inner_size(winit::dpi::LogicalSize::new(800, 600));

        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        );

        let gpu = pollster::block_on(init_gpu(window.clone()));

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
    }

    #[allow(clippy::too_many_lines)]
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
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

                        #[allow(clippy::cast_possible_truncation)] // cols/rows fit in u16
                        if let Some(daemon) = &mut self.daemon {
                            let msg = Resize {
                                pane_id: 0,
                                cols: (size.width / 8) as u16,
                                rows: (size.height / 16) as u16,
                                pixel_width: size.width.min(u32::from(u16::MAX)) as u16,
                                pixel_height: size.height.min(u32::from(u16::MAX)) as u16,
                            };
                            if let Ok(frame) = msg.to_frame() {
                                if let Err(e) = daemon.send_frame(&frame) {
                                    eprintln!("daemon write failed: {e}");
                                    self.daemon = None;
                                    event_loop.exit();
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
                    if let Ok(frame) = msg.to_frame() {
                        if let Err(e) = daemon.send_frame(&frame) {
                            eprintln!("daemon write failed: {e}");
                            self.daemon = None;
                            event_loop.exit();
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

                // TODO: replace with render_grid in Slice 3.
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

                // Temporary placeholder atlas.
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

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::DirtyNotify => {
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

/// Connect to the daemon, spawning it if needed. Returns the write handle
/// and optionally the child process.
fn connect_to_daemon(
    proxy: &EventLoopProxy<UserEvent>,
) -> std::io::Result<(DaemonWriter, Option<std::process::Child>)> {
    let socket_path = oakterm_daemon::socket::socket_path()?;
    let mut child = None;

    // Spawn daemon if socket doesn't exist.
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

        // Wait for socket to appear.
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

    let mut writer = DaemonWriter { stream };
    handshake(&mut writer, &mut read_stream)?;

    // Spawn reader thread.
    let proxy = proxy.clone();
    std::thread::spawn(move || daemon_reader(read_stream, &proxy));

    Ok((writer, child))
}

/// Perform the protocol handshake per Spec-0001.
fn handshake(writer: &mut DaemonWriter, read_stream: &mut UnixStream) -> std::io::Result<()> {
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

/// Background thread: read frames from daemon, dispatch events via proxy.
fn daemon_reader(mut stream: UnixStream, proxy: &EventLoopProxy<UserEvent>) {
    loop {
        match read_frame(&mut stream) {
            Ok(frame) => match frame.msg_type {
                MSG_DIRTY_NOTIFY => {
                    let _ = proxy.send_event(UserEvent::DirtyNotify);
                }
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
