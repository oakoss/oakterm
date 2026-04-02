mod render_grid;

use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use tracing::{debug, error, warn};

use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

use wgpu::CurrentSurfaceTexture;

use oakterm_protocol::frame::Frame;
use oakterm_protocol::input::{KeyInput, MouseInput, Resize};
use oakterm_protocol::message::{
    ClientHello, ClientType, FindPrompt, GetScrollback, HandshakeStatus, MSG_BELL, MSG_DETACH,
    MSG_DIRTY_NOTIFY, MSG_FIND_PROMPT, MSG_GET_RENDER_UPDATE, MSG_GET_SCROLLBACK,
    MSG_PROMPT_POSITION, MSG_RENDER_UPDATE, MSG_SCROLLBACK_DATA, MSG_SERVER_HELLO,
    MSG_TITLE_CHANGED, PromptPosition, ScrollbackData, SearchDirection, TitleChanged,
};
use oakterm_protocol::render::{GetRenderUpdate, RenderUpdate};

use oakterm_renderer::atlas::AtlasPlane;
use oakterm_renderer::font;
use oakterm_renderer::pipeline::{BgUniforms, RenderPipeline, TextUniforms};
use oakterm_renderer::shaper::FontKey;
use oakterm_renderer::swash_shaper::SwashShaper;

use render_grid::ClientGrid;

// AccessKit handlers per Spec-0006.

/// Snapshot of state needed to build the accessibility tree.
/// Shared between App and the activation handler via `Arc<Mutex<>>`.
struct A11ySnapshot {
    rows: u16,
    cols: u16,
    row_texts: Vec<String>,
    cursor_row: u16,
    cursor_col: u16,
    title: String,
    scrollback_lines: u64,
    cell_width: f64,
    cell_height: f64,
    /// Set when title changes; cleared after the next incremental update.
    title_changed: bool,
}

struct TerminalActivationHandler {
    state: Arc<Mutex<Option<A11ySnapshot>>>,
}

impl accesskit::ActivationHandler for TerminalActivationHandler {
    fn request_initial_tree(&mut self) -> Option<accesskit::TreeUpdate> {
        let guard = match self.state.lock() {
            Ok(g) => g,
            Err(e) => {
                warn!(error = %e, "a11y: mutex poisoned in activation handler");
                return None;
            }
        };
        let snap = guard.as_ref()?;
        let input = oakterm_a11y::TreeInput {
            rows: snap.rows,
            cols: snap.cols,
            row_texts: &snap.row_texts,
            cursor_row: snap.cursor_row,
            cursor_col: snap.cursor_col,
            title: &snap.title,
            scrollback_lines: snap.scrollback_lines,
            cell_width: snap.cell_width,
            cell_height: snap.cell_height,
        };
        Some(oakterm_a11y::build_initial_tree(&input))
    }
}

struct TerminalActionHandler {
    proxy: EventLoopProxy<UserEvent>,
}

impl accesskit::ActionHandler for TerminalActionHandler {
    fn do_action(&mut self, request: accesskit::ActionRequest) {
        let _ = self.proxy.send_event(UserEvent::AccessKitAction(request));
    }
}

struct NoOpDeactivationHandler;
impl accesskit::DeactivationHandler for NoOpDeactivationHandler {
    fn deactivate_accessibility(&mut self) {}
}

/// Events sent from background threads to the winit event loop.
#[derive(Debug)]
enum UserEvent {
    RenderUpdate(Box<RenderUpdate>),
    ScrollbackData(Box<ScrollbackData>),
    PromptPosition(PromptPosition),
    TitleChanged(String),
    Bell,
    AccessKitAction(accesskit::ActionRequest),
    Disconnected,
    ConfigReloaded(Box<oakterm_config::ConfigResult>),
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
    color_atlas_texture: wgpu::Texture,
    color_atlas_view: wgpu::TextureView,
    /// Whether the surface is configured for Display P3 color space.
    p3_active: bool,
}

/// Font and glyph state for text rendering.
struct FontState {
    shaper: SwashShaper,
    font_key: FontKey,
    atlas: AtlasPlane,
    color_atlas: AtlasPlane,
    /// Cache keys of glyphs stored in the color atlas.
    color_keys: std::collections::HashSet<oakterm_renderer::atlas::GlyphCacheKey>,
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

/// Copyable action descriptor to break the borrow on `keybind_registry`
/// during `dispatch_action_at`. `Callback` stores the index back into the
/// registry since `RegistryKey` is not `Clone`.
enum ActionDesc {
    ScrollUp(u32),
    ScrollDown(u32),
    ScrollToPrompt(i32),
    SendString(Vec<u8>),
    Copy,
    Paste,
    ToggleFullscreen,
    ReloadConfig,
    Callback(usize),
    Stub,
}

#[allow(clippy::struct_excessive_bools)]
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
    config: oakterm_config::ConfigValues,
    /// Lua VM kept alive for event handler invocation.
    lua_vm: Option<oakterm_config::Lua>,
    /// Registered event handlers from config evaluation.
    event_registry: oakterm_config::EventRegistry,
    /// Registered keybinds from config evaluation.
    keybind_registry: oakterm_config::KeybindRegistry,
    /// Stored for future in-window error banner rendering.
    #[allow(dead_code)]
    config_error: Option<String>,
    /// File watcher for config hot-reload. Must stay alive.
    #[allow(dead_code)]
    config_watcher: Option<
        notify_debouncer_full::Debouncer<
            notify::RecommendedWatcher,
            notify_debouncer_full::RecommendedCache,
        >,
    >,
    last_sent_dims: (u16, u16),
    /// Set after initial Resize is sent. Gates on first `RedrawRequested`.
    initial_resize_sent: bool,
    /// Last known mouse position in grid coordinates.
    last_mouse_cell: (u16, u16),
    /// Lines scrolled up from bottom. 0 = live view (at bottom).
    viewport_offset: u32,
    /// Current keyboard modifier state for intercepting Shift+key.
    modifiers: winit::event::Modifiers,
    /// Buttons whose press was Shift-bypassed; suppress their release too.
    shift_bypassed_buttons: u8,
    /// Blink phase: true = cursor visible, false = cursor hidden.
    blink_visible: bool,
    /// Next blink toggle deadline. `None` when blink is paused.
    blink_deadline: Option<std::time::Instant>,
    /// Whether the window currently has focus.
    focused: bool,
    /// Shared state for the AccessKit activation handler.
    a11y_state: Arc<Mutex<Option<A11ySnapshot>>>,
    /// Debounce: last time an a11y announcement was sent.
    last_announcement: Option<std::time::Instant>,
    /// Whether the terminal has DECSET 2004 (bracketed paste) active.
    bracketed_paste: bool,
    /// Active text selection, if any.
    selection: Option<oakterm_terminal::grid::selection::Selection>,
    /// Left mouse button held for drag tracking.
    mouse_pressed: bool,
    /// Click count for double/triple click detection.
    click_count: u8,
    /// Timestamp of last click for multi-click detection.
    last_click_time: Option<std::time::Instant>,
    /// Cell position of last click for multi-click detection.
    last_click_pos: (u16, u16),
    /// Last known mouse position in pixel coordinates.
    last_mouse_pixel: (f64, f64),
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
            config: oakterm_config::ConfigValues::default(),
            lua_vm: None,
            event_registry: oakterm_config::EventRegistry::new(),
            keybind_registry: oakterm_config::KeybindRegistry::new(),
            config_error: None,
            config_watcher: None,
            last_sent_dims: (0, 0),
            initial_resize_sent: false,
            last_mouse_cell: (0, 0),
            viewport_offset: 0,
            modifiers: winit::event::Modifiers::default(),
            shift_bypassed_buttons: 0,
            blink_visible: true,
            blink_deadline: None,
            focused: true,
            a11y_state: Arc::new(Mutex::new(None)),
            last_announcement: None,
            bracketed_paste: false,
            selection: None,
            mouse_pressed: false,
            click_count: 0,
            last_click_time: None,
            last_click_pos: (0, 0),
            last_mouse_pixel: (0.0, 0.0),
        }
    }

    /// Request scrollback rows from the daemon for the current viewport offset.
    fn request_scrollback(&self) {
        if let (Some(daemon), Some(grid)) = (&self.daemon, &self.grid) {
            let req = GetScrollback {
                pane_id: 0,
                start_row: -i64::from(self.viewport_offset),
                count: u32::from(grid.rows),
            };
            match Frame::new(MSG_GET_SCROLLBACK, 0, req.encode()) {
                Ok(frame) => {
                    if let Err(e) = daemon.send_frame(&frame) {
                        error!(error = %e, "failed to send GetScrollback");
                    }
                }
                Err(e) => error!(error = %e, "failed to create GetScrollback frame"),
            }
        }
    }

    /// Ask the daemon to find the next/previous prompt relative to the
    /// current viewport offset.
    fn request_find_prompt(&self, direction: SearchDirection) {
        if let Some(daemon) = &self.daemon {
            let req = FindPrompt {
                pane_id: 0,
                from_offset: -i64::from(self.viewport_offset),
                direction,
            };
            match Frame::new(MSG_FIND_PROMPT, 0, req.encode()) {
                Ok(frame) => {
                    if let Err(e) = daemon.send_frame(&frame) {
                        error!(error = %e, "failed to send FindPrompt");
                    }
                }
                Err(e) => error!(error = %e, "failed to create FindPrompt frame"),
            }
        }
    }

    /// Scroll the viewport by `lines`. Positive = up (into scrollback),
    /// negative = down (toward live). Handles enter/exit scrollback.
    fn scroll_viewport(&mut self, lines: i32) {
        if lines > 0 {
            if let Some(grid) = &mut self.grid {
                if !grid.is_scrolled() {
                    grid.enter_scrollback();
                }
                #[allow(clippy::cast_sign_loss)]
                {
                    self.viewport_offset = self.viewport_offset.saturating_add(lines as u32);
                }
            }
            self.request_scrollback();
        } else if lines < 0 && self.viewport_offset > 0 {
            self.viewport_offset = self.viewport_offset.saturating_sub(lines.unsigned_abs());
            if self.viewport_offset == 0 {
                self.return_to_live();
            } else {
                self.request_scrollback();
            }
        }
    }

    /// Return to live view from scrollback.
    fn return_to_live(&mut self) {
        self.viewport_offset = 0;
        if let Some(grid) = &mut self.grid {
            grid.exit_scrollback();
        }
        // Request a full refresh to ensure live view is current.
        if let Some(daemon) = &self.daemon {
            let req = GetRenderUpdate {
                pane_id: 0,
                since_seqno: 0,
            };
            match Frame::new(MSG_GET_RENDER_UPDATE, 1, req.encode()) {
                Ok(frame) => {
                    if let Err(e) = daemon.send_frame(&frame) {
                        error!(error = %e, "daemon write failed during return_to_live");
                    }
                }
                Err(e) => error!(error = %e, "failed to encode render update request"),
            }
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Start or update a text selection based on current mouse state.
    /// Handles single, double (word), and triple (line) click detection.
    fn start_selection(&mut self) {
        use oakterm_terminal::grid::selection::{
            AnchorSide, Selection, SelectionType, word_boundaries,
        };

        let (col, row) = self.last_mouse_cell;
        let now = std::time::Instant::now();
        let cw = self
            .font
            .as_ref()
            .map_or(8.0, |f| f64::from(f.metrics.cell_width));
        let side = if (self.last_mouse_pixel.0 % cw) > (cw / 2.0) {
            AnchorSide::Right
        } else {
            AnchorSide::Left
        };

        // Multi-click detection: same cell within 300ms increments click count.
        let same_cell = self.last_click_pos == (col, row);
        let within_timeout = self
            .last_click_time
            .is_some_and(|t| now.duration_since(t).as_millis() < 300);

        if same_cell && within_timeout {
            self.click_count = (self.click_count + 1).min(3);
        } else {
            self.click_count = 1;
        }
        self.last_click_time = Some(now);
        self.last_click_pos = (col, row);

        let sel_row = i64::from(row) - i64::from(self.viewport_offset);

        match self.click_count {
            2 => {
                // Semantic (word) selection.
                if let Some(grid) = &self.grid {
                    if row < grid.rows {
                        let text: Vec<char> = grid.row_text(row).chars().collect();
                        // Click past end of text: no word to select.
                        if (col as usize) < text.len() {
                            let (start_col, end_col) = word_boundaries(&text, col);
                            let mut sel = Selection::new(
                                SelectionType::Semantic,
                                sel_row,
                                start_col,
                                AnchorSide::Left,
                            );
                            sel.update(sel_row, end_col, AnchorSide::Right);
                            self.selection = Some(sel);
                        }
                    }
                }
            }
            3 => {
                // Line selection.
                let mut sel = Selection::new(SelectionType::Line, sel_row, 0, AnchorSide::Left);
                sel.update(sel_row, 0, AnchorSide::Left);
                self.selection = Some(sel);
            }
            _ => {
                // Normal (single click) selection.
                self.selection = Some(Selection::new(SelectionType::Normal, sel_row, col, side));
            }
        }

        self.mouse_pressed = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Reset blink to visible and restart the timer.
    fn reset_blink(&mut self) {
        self.blink_visible = true;
        if self.should_blink() {
            self.blink_deadline =
                Some(std::time::Instant::now() + std::time::Duration::from_millis(530));
        } else {
            self.blink_deadline = None;
        }
    }

    /// Whether the cursor should currently be blinking.
    fn should_blink(&self) -> bool {
        if !self.config.cursor_blink || !self.focused {
            return false;
        }
        let Some(grid) = &self.grid else {
            return false;
        };
        if !grid.cursor_visible || grid.is_scrolled() {
            return false;
        }
        // Blinking styles: 0=BlinkingBlock, 2=BlinkingUnderline, 4=BlinkingBar
        matches!(grid.cursor_style, 0 | 2 | 4)
    }
}

impl ApplicationHandler<UserEvent> for App {
    #[allow(clippy::too_many_lines)]
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
            TerminalActivationHandler {
                state: self.a11y_state.clone(),
            },
            TerminalActionHandler {
                proxy: self.proxy.clone(),
            },
            NoOpDeactivationHandler,
        );
        self.accesskit = Some(accesskit);

        // Detect initial system appearance before config loads.
        if let Some(theme) = window.theme() {
            oakterm_config::set_appearance(theme == winit::window::Theme::Light);
        }

        window.set_visible(true);

        // Load config before GPU init so blending mode is available for pipeline.
        let cr = oakterm_config::load_config();
        if let Some(err) = &cr.error {
            warn!(error = %err, "config error");
        }
        let config = cr.config.clone();

        let blending_mode = match config.text_blending {
            oakterm_config::TextBlending::Linear => oakterm_renderer::shaders::BLENDING_LINEAR,
            oakterm_config::TextBlending::LinearCorrected => {
                oakterm_renderer::shaders::BLENDING_LINEAR_CORRECTED
            }
        };

        let gpu = match pollster::block_on(init_gpu(window.clone(), blending_mode)) {
            Ok(state) => state,
            Err(e) => {
                error!(error = %e, "fatal: GPU initialization failed");
                event_loop.exit();
                return;
            }
        };

        // Load font at display-native pixel size.
        #[allow(clippy::cast_possible_truncation)] // f64 -> f32 for font size
        let font_size_pt = config.font_size as f32;
        #[allow(clippy::cast_possible_truncation)] // scale factor fits in f32
        let font_size = font_size_pt * window.scale_factor() as f32;
        let font_state = match try_init_font(&config, font_size) {
            Ok(state) => state,
            Err(e) => {
                error!(error = %e, "fatal: font initialization failed");
                event_loop.exit();
                return;
            }
        };

        let size = window.inner_size();
        let (cols, rows) = window_to_grid_dims(size, &font_state.metrics);
        let grid = ClientGrid::new(cols.max(1), rows.max(1));

        match connect_to_daemon(&self.proxy) {
            Ok((writer, child)) => {
                self.daemon = Some(writer);
                self.daemon_process = child;
            }
            Err(e) => {
                error!(error = %e, "fatal: failed to connect to daemon");
                event_loop.exit();
                return;
            }
        }

        // Populate the a11y snapshot so the activation handler can build a tree.
        match self.a11y_state.lock() {
            Ok(mut snap) => {
                *snap = Some(A11ySnapshot {
                    rows: grid.rows,
                    cols: grid.cols,
                    row_texts: grid.row_texts(),
                    cursor_row: grid.cursor_y,
                    cursor_col: grid.cursor_x,
                    title: String::new(),
                    scrollback_lines: 0,
                    cell_width: f64::from(font_state.metrics.cell_width),
                    cell_height: f64::from(font_state.metrics.cell_height),
                    title_changed: false,
                });
            }
            Err(e) => warn!(error = %e, "a11y: mutex poisoned during init"),
        }

        self.window = Some(window);
        self.gpu = Some(gpu);
        self.font = Some(font_state);
        self.grid = Some(grid);
        self.config = config;
        self.config_error = cr.error;
        self.event_registry = cr.registry;
        self.keybind_registry = cr.keybinds;
        self.lua_vm = cr.lua;
        // Fire config.loaded event for initial load.
        if self.config_error.is_none() {
            if let Some(lua) = &self.lua_vm {
                for result in self.event_registry.fire(lua, "config.loaded", &[]) {
                    match result {
                        oakterm_config::HandlerResult::Error(e) => {
                            warn!(error = %e, "config.loaded handler error");
                        }
                        oakterm_config::HandlerResult::Timeout => {
                            warn!("config.loaded handler timed out (100ms limit)");
                        }
                        _ => {}
                    }
                }
            }
        }
        self.config_watcher = start_config_watcher(&self.proxy);
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

                        // Resize exits scrollback (grid.resize clears snapshot).
                        self.viewport_offset = 0;

                        #[allow(clippy::cast_possible_truncation)]
                        if let (Some(font), Some(grid)) = (&self.font, &mut self.grid) {
                            let (cols, rows) = window_to_grid_dims(size, &font.metrics);
                            let dims_changed = grid.rows != rows || grid.cols != cols;
                            grid.resize(cols, rows);

                            // Full a11y tree rebuild on resize (row count changed).
                            if dims_changed {
                                let row_texts = grid.row_texts();
                                let cw = f64::from(font.metrics.cell_width);
                                let ch = f64::from(font.metrics.cell_height);
                                let title = match self.a11y_state.lock() {
                                    Ok(mut s) => {
                                        if let Some(snap) = s.as_mut() {
                                            snap.rows = rows;
                                            snap.cols = cols;
                                            snap.row_texts.clone_from(&row_texts);
                                            snap.cursor_row = grid.cursor_y;
                                            snap.cursor_col = grid.cursor_x;
                                            snap.title_changed = false;
                                            snap.title.clone()
                                        } else {
                                            String::new()
                                        }
                                    }
                                    Err(e) => {
                                        warn!(error = %e, "a11y: mutex poisoned during resize");
                                        String::new()
                                    }
                                };
                                let input = oakterm_a11y::TreeInput {
                                    rows,
                                    cols,
                                    row_texts: &row_texts,
                                    cursor_row: grid.cursor_y,
                                    cursor_col: grid.cursor_x,
                                    title: &title,
                                    scrollback_lines: 0,
                                    cell_width: cw,
                                    cell_height: ch,
                                };
                                let full_tree = oakterm_a11y::build_initial_tree(&input);
                                if let Some(adapter) = &mut self.accesskit {
                                    adapter.update_if_active(|| full_tree);
                                }
                            }

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
                                                error!(error = %e, "daemon write failed");
                                                self.daemon = None;
                                                event_loop.exit();
                                            }
                                        }
                                        Err(e) => error!(error = %e, "failed to encode resize"),
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
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods;
            }
            WindowEvent::ThemeChanged(theme) => {
                oakterm_config::set_appearance(theme == winit::window::Theme::Light);
                if let Some(lua) = &self.lua_vm {
                    let appearance = oakterm_config::current_appearance();
                    if let Ok(val) = lua.create_string(appearance) {
                        for result in self.event_registry.fire(
                            lua,
                            "appearance.changed",
                            &[oakterm_config::mlua::Value::String(val.clone())],
                        ) {
                            match result {
                                oakterm_config::HandlerResult::Error(e) => {
                                    warn!(error = %e, "appearance.changed handler error");
                                }
                                oakterm_config::HandlerResult::Timeout => {
                                    warn!("appearance.changed handler timed out (100ms limit)");
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                if focused {
                    self.reset_blink();
                } else {
                    // Show solid cursor when unfocused.
                    self.blink_visible = true;
                    self.blink_deadline = None;
                    if let Some(w) = &self.window {
                        w.request_redraw();
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
                // Look up keybind BEFORE clearing selection so Copy can read it.
                if let Some(chord) = winit_to_chord(self.modifiers, &logical_key) {
                    if let Some(idx) = self.keybind_registry.lookup_index(&chord) {
                        if self.dispatch_action_at(idx) {
                            self.reset_blink();
                            return;
                        }
                        // Action returned false (e.g., scroll down when not
                        // scrolled) — let the key fall through to PTY.
                    }
                }

                // Clear selection on non-copy keystrokes.
                if self.selection.is_some() {
                    self.selection = None;
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }

                // Any unbound key while scrolled: snap back to live first.
                if self.viewport_offset > 0 {
                    self.return_to_live();
                }

                let bytes = key_to_bytes(&logical_key, text.as_deref());
                if let (Some(daemon), Some(bytes)) = (&mut self.daemon, bytes) {
                    let msg = KeyInput {
                        pane_id: 0,
                        key_data: bytes,
                    };
                    match msg.to_frame() {
                        Ok(frame) => {
                            if let Err(e) = daemon.send_frame(&frame) {
                                error!(error = %e, "daemon write failed");
                                self.daemon = None;
                                event_loop.exit();
                            }
                        }
                        Err(e) => error!(error = %e, "failed to encode key input"),
                    }
                }
                self.reset_blink();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse_pixel = (position.x, position.y);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                if let Some(font) = &self.font {
                    let col = (position.x as f32 / font.metrics.cell_width) as u16;
                    let row = (position.y as f32 / font.metrics.cell_height) as u16;
                    self.last_mouse_cell = (col, row);

                    // Update selection end during drag.
                    if self.mouse_pressed {
                        use oakterm_terminal::grid::selection::{
                            AnchorSide, SelectionType, word_boundaries,
                        };
                        let cw = f64::from(font.metrics.cell_width);
                        let side = if (position.x % cw) > (cw / 2.0) {
                            AnchorSide::Right
                        } else {
                            AnchorSide::Left
                        };
                        let sel_row = i64::from(row) - i64::from(self.viewport_offset);
                        if let Some(sel) = &mut self.selection {
                            if sel.ty == SelectionType::Semantic {
                                // Snap drag to word boundaries.
                                if let Some(grid) = &self.grid {
                                    if row < grid.rows {
                                        let text: Vec<char> = grid.row_text(row).chars().collect();
                                        if (col as usize) < text.len() {
                                            let (start_col, end_col) = word_boundaries(&text, col);
                                            // Snap to near edge based on drag direction.
                                            let backward = sel_row < sel.start.row
                                                || (sel_row == sel.start.row
                                                    && col < sel.start.col);
                                            if backward {
                                                sel.update(sel_row, start_col, AnchorSide::Left);
                                            } else {
                                                sel.update(sel_row, end_col, AnchorSide::Right);
                                            }
                                        } else {
                                            sel.update(sel_row, col, side);
                                        }
                                    }
                                }
                            } else {
                                sel.update(sel_row, col, side);
                            }
                        }
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let btn = match button {
                    winit::event::MouseButton::Middle => 1u8,
                    winit::event::MouseButton::Right => 2,
                    _ => 0,
                };
                let btn_bit = 1u8 << btn;
                let shift = self.modifiers.state().shift_key();

                match state {
                    ElementState::Pressed if shift => {
                        // Shift bypass: suppress press and track for release.
                        self.shift_bypassed_buttons |= btn_bit;

                        // Start selection on Shift+left click.
                        if btn == 0 {
                            self.start_selection();
                        }
                    }
                    ElementState::Released if self.shift_bypassed_buttons & btn_bit != 0 => {
                        // Suppress release for a Shift-bypassed press.
                        self.shift_bypassed_buttons &= !btn_bit;
                        if btn == 0 {
                            self.mouse_pressed = false;
                        }
                    }
                    _ => {
                        // Clear selection on non-shift click.
                        if state == ElementState::Pressed && btn == 0 && self.selection.is_some() {
                            self.selection = None;
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                        }
                        if let Some(daemon) = &mut self.daemon {
                            let (x, y) = self.last_mouse_cell;
                            let event_type = match state {
                                ElementState::Pressed => 0,
                                ElementState::Released => 1,
                            };
                            let msg = MouseInput {
                                pane_id: 0,
                                event_type,
                                x,
                                y,
                                modifiers: encode_mouse_modifiers(self.modifiers),
                                button: btn,
                            };
                            match msg.to_frame() {
                                Ok(frame) => {
                                    if let Err(e) = daemon.send_frame(&frame) {
                                        error!(error = %e, "daemon write failed");
                                        self.daemon = None;
                                        event_loop.exit();
                                    }
                                }
                                Err(e) => error!(error = %e, "failed to encode mouse input"),
                            }
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let (scroll_up, count) = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, v) => (v > 0.0, v.abs() as u32),
                    winit::event::MouseScrollDelta::PixelDelta(p) => (p.y > 0.0, 1u32),
                };
                #[allow(clippy::cast_possible_wrap)]
                let scroll_lines = (3 * count) as i32;

                let shift = self.modifiers.state().shift_key();

                if scroll_up {
                    self.scroll_viewport(scroll_lines);
                } else if self.viewport_offset > 0 {
                    self.scroll_viewport(-scroll_lines);
                } else if !shift {
                    // Forward to daemon only when Shift is not held.
                    if let Some(daemon) = &mut self.daemon {
                        let (x, y) = self.last_mouse_cell;
                        let event_type = if scroll_up { 3u8 } else { 4u8 };
                        let mods = encode_mouse_modifiers(self.modifiers);
                        for _ in 0..count.min(5) {
                            let msg = MouseInput {
                                pane_id: 0,
                                event_type,
                                x,
                                y,
                                modifiers: mods,
                                button: 0,
                            };
                            match msg.to_frame() {
                                Ok(frame) => {
                                    if let Err(e) = daemon.send_frame(&frame) {
                                        error!(error = %e, "daemon write failed");
                                        self.daemon = None;
                                        event_loop.exit();
                                        return;
                                    }
                                }
                                Err(e) => error!(error = %e, "failed to encode mouse wheel"),
                            }
                        }
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
                                    error!(error = %e, "daemon write failed");
                                    self.daemon = None;
                                    event_loop.exit();
                                    return;
                                }
                                self.initial_resize_sent = true;
                            }
                            Err(e) => {
                                error!(error = %e, "fatal: failed to encode initial resize");
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
                        error!("wgpu surface validation error; skipping frame");
                        return;
                    }
                };

                let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
                    format: Some(gpu.config.format),
                    ..Default::default()
                });

                let (bg_colors, glyph_instances) = if let (Some(grid), Some(font)) =
                    (&self.grid, &mut self.font)
                {
                    // Effective cursor visibility: hidden during blink-off phase.
                    let cursor_vis = grid.cursor_visible
                        && (self.blink_visible || !matches!(grid.cursor_style, 0 | 2 | 4));

                    let bg =
                        grid.bg_colors(cursor_vis, self.selection.as_ref(), self.viewport_offset);
                    let (glyphs, uploads, color_uploads) = grid.glyph_instances(
                        &font.metrics,
                        font.font_key,
                        font.font_size,
                        &font.shaper,
                        &mut font.atlas,
                        &mut font.color_atlas,
                        &mut font.color_keys,
                        cursor_vis,
                        self.selection.as_ref(),
                        self.viewport_offset,
                    );

                    upload_glyphs_to_atlas(
                        &gpu.device,
                        &gpu.queue,
                        &mut gpu.atlas_texture,
                        &mut gpu.atlas_view,
                        &font.atlas,
                        &uploads,
                    );
                    upload_color_glyphs_to_atlas(
                        &gpu.device,
                        &gpu.queue,
                        &mut gpu.color_atlas_texture,
                        &mut gpu.color_atlas_view,
                        &font.color_atlas,
                        &color_uploads,
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
                    #[allow(clippy::cast_possible_truncation)] // gamma is small (0-5)
                    text_gamma: self.config.text_gamma as f32,
                    color_atlas_width: self
                        .font
                        .as_ref()
                        .map_or(256.0, |f| f.color_atlas.size().0 as f32),
                    color_atlas_height: self
                        .font
                        .as_ref()
                        .map_or(256.0, |f| f.color_atlas.size().1 as f32),
                    pad: 0.0,
                };

                let clear_color = self.grid.as_ref().map_or(wgpu::Color::BLACK, |g| {
                    let [r, g, b] = g.bg_color;
                    wgpu::Color {
                        r: f64::from(r) / 255.0,
                        g: f64::from(g) / 255.0,
                        b: f64::from(b) / 255.0,
                        a: 1.0,
                    }
                });

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
                    &gpu.color_atlas_view,
                    clear_color,
                );

                if let Some(w) = &self.window {
                    w.pre_present_notify();
                }
                frame.present();
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_lines)]
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::RenderUpdate(update) => {
                self.bracketed_paste = update.bracketed_paste;

                // Build a11y incremental data while grid is borrowed,
                // then send it via adapter after the grid borrow ends.
                let mut a11y_update: Option<accesskit::TreeUpdate> = None;

                if let Some(grid) = &mut self.grid {
                    if grid.is_scrolled() {
                        grid.apply_update_while_scrolled(&update);
                    } else {
                        grid.apply_update(&update);

                        // Build incremental a11y update from dirty rows.
                        let dirty_indices: Vec<u16> =
                            update.dirty_rows.iter().map(|r| r.row_index).collect();
                        let dirty_texts: Vec<String> =
                            dirty_indices.iter().map(|&i| grid.row_text(i)).collect();

                        // Detect cursor/title changes and update snapshot.
                        let (cursor_changed, title_changed, current_title) =
                            match self.a11y_state.lock() {
                                Ok(mut snap) => {
                                    if let Some(s) = snap.as_mut() {
                                        let cc = s.cursor_row != grid.cursor_y
                                            || s.cursor_col != grid.cursor_x;
                                        let tc = s.title_changed;
                                        s.title_changed = false;
                                        let title = s.title.clone();
                                        s.rows = grid.rows;
                                        s.cols = grid.cols;
                                        s.row_texts = grid.row_texts();
                                        s.cursor_row = grid.cursor_y;
                                        s.cursor_col = grid.cursor_x;
                                        (cc, tc, title)
                                    } else {
                                        (false, false, String::new())
                                    }
                                }
                                Err(e) => {
                                    warn!(error = %e, "a11y: mutex poisoned");
                                    (false, false, String::new())
                                }
                            };

                        // Announce new output at bottom of terminal (debounced).
                        let announcement = if dirty_indices.is_empty() || grid.rows == 0 {
                            None
                        } else {
                            let bottom = grid.rows.saturating_sub(1);
                            let has_bottom = dirty_indices.contains(&bottom);
                            let debounce_ok = self
                                .last_announcement
                                .is_none_or(|t| t.elapsed().as_millis() >= 100);
                            if has_bottom && debounce_ok {
                                // Collect text from dirty rows near the bottom.
                                let text: String = dirty_indices
                                    .iter()
                                    .zip(dirty_texts.iter())
                                    .filter(|(i, _)| **i >= bottom.saturating_sub(2))
                                    .map(|(_, t)| t.as_str())
                                    .filter(|t| !t.is_empty())
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                if text.is_empty() {
                                    None
                                } else {
                                    self.last_announcement = Some(std::time::Instant::now());
                                    Some(oakterm_a11y::Announcement {
                                        text,
                                        level: accesskit::Live::Polite,
                                    })
                                }
                            } else {
                                None
                            }
                        };

                        if !dirty_indices.is_empty() || cursor_changed || title_changed {
                            let cursor_row_text = grid.row_text(grid.cursor_y);
                            let font = self.font.as_ref();
                            let input = oakterm_a11y::IncrementalInput {
                                rows: grid.rows,
                                cols: grid.cols,
                                dirty_row_indices: &dirty_indices,
                                dirty_row_texts: &dirty_texts,
                                cursor_row: grid.cursor_y,
                                cursor_col: grid.cursor_x,
                                cursor_changed,
                                cursor_row_text: &cursor_row_text,
                                title: &current_title,
                                title_changed,
                                announcement: announcement.as_ref(),
                                cell_width: font.map_or(8.0, |f| f64::from(f.metrics.cell_width)),
                                cell_height: font
                                    .map_or(16.0, |f| f64::from(f.metrics.cell_height)),
                            };
                            a11y_update = Some(oakterm_a11y::build_incremental_update(&input));
                        }

                        // Restart blink — cursor style may have changed.
                        if self.blink_deadline.is_none() && self.should_blink() {
                            self.reset_blink();
                        }
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }

                // Send incremental a11y update (grid borrow is released).
                if let (Some(adapter), Some(tree_update)) = (&mut self.accesskit, a11y_update) {
                    adapter.update_if_active(|| tree_update);
                }
            }
            UserEvent::ScrollbackData(data) => {
                if self.viewport_offset > 0 {
                    // Clamp offset if we reached the top of scrollback.
                    let requested = self.grid.as_ref().map_or(24usize, |g| usize::from(g.rows));
                    if data.rows.len() < requested && !data.has_more {
                        #[allow(clippy::cast_possible_truncation)]
                        let actual = data.rows.len() as u32;
                        self.viewport_offset = self.viewport_offset.min(actual);
                    }
                    let mut a11y_scrollback_update: Option<accesskit::TreeUpdate> = None;
                    if let Some(grid) = &mut self.grid {
                        #[allow(clippy::cast_possible_truncation)]
                        let offset = self.viewport_offset.min(u32::from(u16::MAX)) as u16;
                        grid.apply_scrollback(&data.rows, offset);
                        if self.config.scroll_indicator {
                            grid.set_scroll_indicator(self.viewport_offset);
                        }
                        // All visible rows changed — update a11y with full row set.
                        let all_indices: Vec<u16> = (0..grid.rows).collect();
                        let all_texts: Vec<String> =
                            all_indices.iter().map(|&i| grid.row_text(i)).collect();
                        let font = self.font.as_ref();
                        let cursor_row_text = grid.row_text(grid.cursor_y);
                        let title = self
                            .a11y_state
                            .lock()
                            .ok()
                            .and_then(|s| s.as_ref().map(|s| s.title.clone()))
                            .unwrap_or_default();
                        let input = oakterm_a11y::IncrementalInput {
                            rows: grid.rows,
                            cols: grid.cols,
                            dirty_row_indices: &all_indices,
                            dirty_row_texts: &all_texts,
                            cursor_row: grid.cursor_y,
                            cursor_col: grid.cursor_x,
                            cursor_changed: true,
                            cursor_row_text: &cursor_row_text,
                            title: &title,
                            title_changed: false,
                            announcement: None,
                            cell_width: font.map_or(8.0, |f| f64::from(f.metrics.cell_width)),
                            cell_height: font.map_or(16.0, |f| f64::from(f.metrics.cell_height)),
                        };
                        a11y_scrollback_update =
                            Some(oakterm_a11y::build_incremental_update(&input));
                    }
                    if let (Some(adapter), Some(tree_update)) =
                        (&mut self.accesskit, a11y_scrollback_update)
                    {
                        adapter.update_if_active(|| tree_update);
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            UserEvent::PromptPosition(pos) => {
                if let Some(offset) = pos.offset {
                    // offset is negative; negate to get positive viewport_offset.
                    let Some(new_offset) = offset.checked_neg().and_then(|v| u32::try_from(v).ok())
                    else {
                        warn!(offset, "PromptPosition offset out of range");
                        return;
                    };
                    if new_offset == 0 {
                        self.return_to_live();
                    } else {
                        self.viewport_offset = new_offset;
                        if let Some(grid) = &mut self.grid {
                            if !grid.is_scrolled() {
                                grid.enter_scrollback();
                            }
                        }
                        self.request_scrollback();
                    }
                }
            }
            UserEvent::TitleChanged(title) => {
                if let Some(w) = &self.window {
                    let display = if title.is_empty() { "oakterm" } else { &title };
                    w.set_title(display);
                }
                // Update snapshot and immediately push to AT (no render event needed).
                match self.a11y_state.lock() {
                    Ok(mut snap) => {
                        if let Some(s) = snap.as_mut() {
                            s.title = title;
                            s.title_changed = false; // consumed immediately below
                        }
                    }
                    Err(e) => warn!(error = %e, "a11y: mutex poisoned on title change"),
                }
                if let (Some(grid), Some(adapter)) = (&self.grid, &mut self.accesskit) {
                    let current_title = self
                        .a11y_state
                        .lock()
                        .ok()
                        .and_then(|s| s.as_ref().map(|s| s.title.clone()))
                        .unwrap_or_default();
                    let cursor_row_text = grid.row_text(grid.cursor_y);
                    let font = self.font.as_ref();
                    let input = oakterm_a11y::IncrementalInput {
                        rows: grid.rows,
                        cols: grid.cols,
                        dirty_row_indices: &[],
                        dirty_row_texts: &[],
                        cursor_row: grid.cursor_y,
                        cursor_col: grid.cursor_x,
                        cursor_changed: false,
                        cursor_row_text: &cursor_row_text,
                        title: &current_title,
                        title_changed: true,
                        announcement: None,
                        cell_width: font.map_or(8.0, |f| f64::from(f.metrics.cell_width)),
                        cell_height: font.map_or(16.0, |f| f64::from(f.metrics.cell_height)),
                    };
                    let update = oakterm_a11y::build_incremental_update(&input);
                    adapter.update_if_active(|| update);
                }
            }
            UserEvent::Bell => {
                // Announce bell to screen readers (assertive = interrupts).
                if let (Some(grid), Some(adapter)) = (&self.grid, &mut self.accesskit) {
                    let cursor_row_text = grid.row_text(grid.cursor_y);
                    let title = self
                        .a11y_state
                        .lock()
                        .ok()
                        .and_then(|s| s.as_ref().map(|s| s.title.clone()))
                        .unwrap_or_default();
                    let ann = oakterm_a11y::Announcement {
                        text: "Bell".into(),
                        level: accesskit::Live::Assertive,
                    };
                    let font = self.font.as_ref();
                    let input = oakterm_a11y::IncrementalInput {
                        rows: grid.rows,
                        cols: grid.cols,
                        dirty_row_indices: &[],
                        dirty_row_texts: &[],
                        cursor_row: grid.cursor_y,
                        cursor_col: grid.cursor_x,
                        cursor_changed: false,
                        cursor_row_text: &cursor_row_text,
                        title: &title,
                        title_changed: false,
                        announcement: Some(&ann),
                        cell_width: font.map_or(8.0, |f| f64::from(f.metrics.cell_width)),
                        cell_height: font.map_or(16.0, |f| f64::from(f.metrics.cell_height)),
                    };
                    let bell_update = oakterm_a11y::build_incremental_update(&input);
                    adapter.update_if_active(|| bell_update);
                    // Clear so a repeated bell is a fresh text transition.
                    let clear_input = oakterm_a11y::IncrementalInput {
                        announcement: None,
                        ..input
                    };
                    let clear = oakterm_a11y::build_incremental_update(&clear_input);
                    adapter.update_if_active(|| clear);
                }
            }
            UserEvent::AccessKitAction(request) => {
                match request.action {
                    accesskit::Action::Focus => {
                        if let Some(w) = &self.window {
                            w.focus_window();
                        }
                    }
                    accesskit::Action::ScrollUp => {
                        let page = self.grid.as_ref().map_or(24, |g| i32::from(g.rows));
                        self.scroll_viewport(page);
                    }
                    accesskit::Action::ScrollDown => {
                        let page = self.grid.as_ref().map_or(24, |g| i32::from(g.rows));
                        self.scroll_viewport(-page);
                    }
                    accesskit::Action::SetScrollOffset => {
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        if let Some(accesskit::ActionData::SetScrollOffset(point)) = request.data {
                            let target = point.y.max(0.0) as u32;
                            if target == 0 {
                                self.return_to_live();
                            } else {
                                if let Some(grid) = &mut self.grid {
                                    if !grid.is_scrolled() {
                                        grid.enter_scrollback();
                                    }
                                }
                                self.viewport_offset = target;
                                self.request_scrollback();
                            }
                        }
                    }
                    _ => {} // SetTextSelection deferred
                }
            }
            UserEvent::Disconnected => {
                error!("daemon disconnected");
                event_loop.exit();
            }
            UserEvent::ConfigReloaded(cr) => {
                self.handle_config_reload(*cr);
            }
        }
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        // Blink timeout reached: toggle cursor visibility.
        if matches!(cause, winit::event::StartCause::ResumeTimeReached { .. }) {
            if self.should_blink() {
                self.blink_visible = !self.blink_visible;
                self.blink_deadline =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(530));
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            } else {
                // Conditions changed; stop blinking.
                self.blink_visible = true;
                self.blink_deadline = None;
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(deadline) = self.blink_deadline {
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

impl App {
    /// Dispatch the keybind action at the given registry index.
    ///
    /// Copies the action data out of the registry to avoid holding a borrow
    /// on `self.keybind_registry` while calling `&mut self` methods.
    #[allow(clippy::too_many_lines)]
    /// Returns `true` if the action was handled (key consumed), `false` if the
    /// key should fall through to PTY forwarding.
    fn dispatch_action_at(&mut self, index: usize) -> bool {
        use oakterm_config::Action;

        // Copy action data out to release the registry borrow.
        let action_desc = match self.keybind_registry.get(index) {
            Some(Action::ScrollUp(n)) => ActionDesc::ScrollUp(*n),
            Some(Action::ScrollDown(n)) => ActionDesc::ScrollDown(*n),
            Some(Action::ScrollToPrompt(d)) => ActionDesc::ScrollToPrompt(*d),
            Some(Action::SendString(b)) => ActionDesc::SendString(b.clone()),
            Some(Action::Copy) => ActionDesc::Copy,
            Some(Action::Paste) => ActionDesc::Paste,
            Some(Action::ToggleFullscreen) => ActionDesc::ToggleFullscreen,
            Some(Action::ReloadConfig) => ActionDesc::ReloadConfig,
            Some(Action::Callback(_)) => ActionDesc::Callback(index),
            Some(
                Action::SplitPane { .. }
                | Action::ClosePane
                | Action::FocusPaneDirection(_)
                | Action::NewTab
                | Action::CloseTab
                | Action::ShowCommandPalette,
            ) => ActionDesc::Stub,
            None => return false,
        };

        match action_desc {
            ActionDesc::ScrollUp(lines) => {
                if let Some(grid) = &mut self.grid {
                    if !grid.is_scrolled() {
                        grid.enter_scrollback();
                    }
                    let amount = if lines == 0 {
                        u32::from(grid.rows)
                    } else {
                        lines
                    };
                    self.viewport_offset = self.viewport_offset.saturating_add(amount);
                }
                self.request_scrollback();
                true
            }
            ActionDesc::ScrollDown(lines) => {
                if self.viewport_offset == 0 {
                    return false; // Not scrolled; let key pass through to PTY.
                }
                let amount = if lines == 0 {
                    self.grid.as_ref().map_or(24, |g| u32::from(g.rows))
                } else {
                    lines
                };
                self.viewport_offset = self.viewport_offset.saturating_sub(amount);
                if self.viewport_offset == 0 {
                    self.return_to_live();
                } else {
                    self.request_scrollback();
                }
                true
            }
            ActionDesc::ScrollToPrompt(direction) => {
                let dir = if direction < 0 {
                    SearchDirection::Older
                } else {
                    SearchDirection::Newer
                };
                if dir == SearchDirection::Older {
                    if let Some(grid) = &mut self.grid {
                        if !grid.is_scrolled() {
                            grid.enter_scrollback();
                        }
                    }
                    self.request_find_prompt(dir);
                } else if self.viewport_offset > 0 {
                    self.request_find_prompt(dir);
                }
                true
            }
            ActionDesc::SendString(bytes) => {
                if let Some(daemon) = &mut self.daemon {
                    let msg = KeyInput {
                        pane_id: 0,
                        key_data: bytes,
                    };
                    match msg.to_frame() {
                        Ok(frame) => {
                            if let Err(e) = daemon.send_frame(&frame) {
                                error!(error = %e, "failed to send keybind string");
                            }
                        }
                        Err(e) => error!(error = %e, "failed to encode keybind string"),
                    }
                }
                true
            }
            ActionDesc::ToggleFullscreen => {
                if let Some(window) = &self.window {
                    if window.fullscreen().is_some() {
                        window.set_fullscreen(None);
                    } else {
                        window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
                    }
                }
                true
            }
            ActionDesc::ReloadConfig => {
                let cr = oakterm_config::load_config();
                self.handle_config_reload(cr);
                true
            }
            ActionDesc::Callback(idx) => {
                let (Some(lua), Some(oakterm_config::Action::Callback(key))) =
                    (&self.lua_vm, self.keybind_registry.get(idx))
                else {
                    warn!("keybind callback skipped: no Lua VM or action mismatch");
                    return true;
                };
                let func = match lua.registry_value::<oakterm_config::mlua::Function>(key) {
                    Ok(f) => f,
                    Err(e) => {
                        warn!(error = %e, "keybind callback error");
                        return true;
                    }
                };
                if let Err(e) = lua.set_hook(
                    oakterm_config::mlua::HookTriggers::new().every_nth_instruction(10_000),
                    {
                        let start = std::time::Instant::now();
                        let timeout = std::time::Duration::from_millis(100);
                        move |_lua, _debug| {
                            if start.elapsed() > timeout {
                                Err(oakterm_config::mlua::Error::RuntimeError(
                                    "keybind callback timed out (100ms)".to_string(),
                                ))
                            } else {
                                Ok(oakterm_config::mlua::VmState::Continue)
                            }
                        }
                    },
                ) {
                    warn!(error = %e, "keybind callback: failed to install timeout hook");
                    return true;
                }
                if let Err(e) = func.call::<()>(()) {
                    warn!(error = %e, "keybind callback error");
                }
                lua.remove_hook();
                true
            }
            ActionDesc::Copy => {
                if let (Some(sel), Some(grid)) = (&self.selection, &self.grid) {
                    let text = grid.extract_selection_text(sel, self.viewport_offset);
                    if !text.is_empty() {
                        match arboard::Clipboard::new() {
                            Ok(mut cb) => {
                                if let Err(e) = cb.set_text(&text) {
                                    warn!(error = %e, "clipboard set failed");
                                }
                            }
                            Err(e) => warn!(error = %e, "clipboard init failed"),
                        }
                    }
                }
                true
            }
            ActionDesc::Paste => {
                match arboard::Clipboard::new() {
                    Ok(mut cb) => match cb.get_text() {
                        Ok(text) if !text.is_empty() => {
                            if let Some(daemon) = &mut self.daemon {
                                // Normalize line endings: PTY expects \r.
                                let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
                                let key_data = if self.bracketed_paste {
                                    let mut buf = Vec::with_capacity(normalized.len() + 12);
                                    buf.extend_from_slice(b"\x1b[200~");
                                    buf.extend_from_slice(normalized.as_bytes());
                                    buf.extend_from_slice(b"\x1b[201~");
                                    buf
                                } else {
                                    normalized.into_bytes()
                                };
                                let msg = oakterm_protocol::input::KeyInput {
                                    pane_id: 0,
                                    key_data,
                                };
                                match msg.to_frame() {
                                    Ok(frame) => {
                                        if let Err(e) = daemon.send_frame(&frame) {
                                            error!(error = %e, "daemon write failed");
                                            self.daemon = None;
                                        }
                                    }
                                    Err(e) => error!(error = %e, "failed to encode paste"),
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(e) => warn!(error = %e, "clipboard get failed"),
                    },
                    Err(e) => warn!(error = %e, "clipboard init failed"),
                }
                true
            }
            ActionDesc::Stub => false,
        }
    }

    #[allow(clippy::too_many_lines)] // One block per config change type.
    fn handle_config_reload(&mut self, mut cr: oakterm_config::ConfigResult) {
        if let Some(ref err) = cr.error {
            warn!(error = %err, "config reload error");
            // Clean up the failed result's registries before its VM is dropped.
            if let Some(lua) = &cr.lua {
                cr.registry.cleanup(lua);
                cr.keybinds.cleanup(lua);
            }
            self.config_error = cr.error;
            return;
        }

        // Clean up old event handlers and keybinds before swapping in new ones.
        if let Some(old_lua) = &self.lua_vm {
            self.event_registry.cleanup(old_lua);
            self.keybind_registry.cleanup(old_lua);
        }

        let font_changed = (cr.config.font_size - self.config.font_size).abs() > f64::EPSILON
            || cr.config.font_family != self.config.font_family;
        let blending_changed = cr.config.text_blending != self.config.text_blending;

        let had_error = self.config_error.is_some();
        self.config = cr.config;
        self.config_error = None;
        self.event_registry = cr.registry;
        self.keybind_registry = cr.keybinds;
        self.lua_vm = cr.lua;

        if had_error {
            debug!("config reloaded successfully");
        }

        if font_changed {
            if let Some(window) = &self.window {
                #[allow(clippy::cast_possible_truncation)]
                #[allow(clippy::cast_possible_truncation)]
                let font_size_pt = self.config.font_size as f32;
                #[allow(clippy::cast_possible_truncation)]
                let font_size = font_size_pt * window.scale_factor() as f32;

                let font_state = match try_init_font(&self.config, font_size) {
                    Ok(fs) => fs,
                    Err(e) => {
                        warn!(error = %e, "config reload: font init failed");
                        self.config_error = Some(e);
                        return;
                    }
                };

                #[allow(clippy::cast_possible_truncation)]
                if let (Some(gpu), Some(grid)) = (&self.gpu, &mut self.grid) {
                    let phys = winit::dpi::PhysicalSize::new(gpu.config.width, gpu.config.height);
                    let (cols, rows) = window_to_grid_dims(phys, &font_state.metrics);
                    let cols = cols.max(1);
                    let rows = rows.max(1);
                    grid.resize(cols, rows);
                    self.last_sent_dims = (cols, rows);

                    if let Some(daemon) = &self.daemon {
                        let msg = Resize {
                            pane_id: 0,
                            cols,
                            rows,
                            pixel_width: phys.width.min(u32::from(u16::MAX)) as u16,
                            pixel_height: phys.height.min(u32::from(u16::MAX)) as u16,
                        };
                        match msg.to_frame() {
                            Ok(frame) => {
                                if let Err(e) = daemon.send_frame(&frame) {
                                    error!(error = %e, "daemon write failed during config reload");
                                }
                            }
                            Err(e) => {
                                error!(error = %e, "failed to encode resize after config reload");
                            }
                        }
                    }
                }

                self.font = Some(font_state);
            }
        }

        // Recreate GPU pipeline if blending mode changed (baked into shader).
        if blending_changed {
            if let Some(gpu) = &mut self.gpu {
                let blending_mode = match self.config.text_blending {
                    oakterm_config::TextBlending::Linear => {
                        oakterm_renderer::shaders::BLENDING_LINEAR
                    }
                    oakterm_config::TextBlending::LinearCorrected => {
                        oakterm_renderer::shaders::BLENDING_LINEAR_CORRECTED
                    }
                };
                gpu.pipeline = oakterm_renderer::pipeline::RenderPipeline::new(
                    &gpu.device,
                    gpu.config.format,
                    blending_mode,
                    gpu.p3_active,
                );
            }
        }

        // Fire config.reloaded event on the new handlers.
        if let Some(lua) = &self.lua_vm {
            for result in self.event_registry.fire(lua, "config.reloaded", &[]) {
                match result {
                    oakterm_config::HandlerResult::Error(e) => {
                        warn!(error = %e, "config.reloaded handler error");
                    }
                    oakterm_config::HandlerResult::Timeout => {
                        warn!("config.reloaded handler timed out (100ms limit)");
                    }
                    _ => {}
                }
            }
        }

        if let Some(w) = &self.window {
            w.request_redraw();
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

/// Convert winit modifier state + logical key to a `KeyChord` for registry lookup.
fn winit_to_chord(
    modifiers: winit::event::Modifiers,
    logical_key: &Key,
) -> Option<oakterm_config::KeyChord> {
    use oakterm_config::{KeyChord, KeyName, NamedKeyId};

    let state = modifiers.state();
    let key = match logical_key {
        Key::Named(named) => {
            let id = match named {
                NamedKey::ArrowUp => NamedKeyId::ArrowUp,
                NamedKey::ArrowDown => NamedKeyId::ArrowDown,
                NamedKey::ArrowLeft => NamedKeyId::ArrowLeft,
                NamedKey::ArrowRight => NamedKeyId::ArrowRight,
                NamedKey::Home => NamedKeyId::Home,
                NamedKey::End => NamedKeyId::End,
                NamedKey::PageUp => NamedKeyId::PageUp,
                NamedKey::PageDown => NamedKeyId::PageDown,
                NamedKey::Tab => NamedKeyId::Tab,
                NamedKey::Enter => NamedKeyId::Enter,
                NamedKey::Backspace => NamedKeyId::Backspace,
                NamedKey::Escape => NamedKeyId::Escape,
                NamedKey::Delete => NamedKeyId::Delete,
                NamedKey::Insert => NamedKeyId::Insert,
                NamedKey::Space => NamedKeyId::Space,
                NamedKey::F1 => NamedKeyId::F1,
                NamedKey::F2 => NamedKeyId::F2,
                NamedKey::F3 => NamedKeyId::F3,
                NamedKey::F4 => NamedKeyId::F4,
                NamedKey::F5 => NamedKeyId::F5,
                NamedKey::F6 => NamedKeyId::F6,
                NamedKey::F7 => NamedKeyId::F7,
                NamedKey::F8 => NamedKeyId::F8,
                NamedKey::F9 => NamedKeyId::F9,
                NamedKey::F10 => NamedKeyId::F10,
                NamedKey::F11 => NamedKeyId::F11,
                NamedKey::F12 => NamedKeyId::F12,
                _ => return None,
            };
            KeyName::Named(id)
        }
        Key::Character(text) => {
            // Only match single-character inputs. Multi-character strings
            // (e.g., IME composition) should not trigger keybinds.
            let mut chars = text.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyName::Character(ch.to_lowercase().next().unwrap_or(ch))
        }
        _ => return None,
    };

    Some(KeyChord {
        ctrl: state.control_key(),
        alt: state.alt_key(),
        shift: state.shift_key(),
        super_key: state.super_key(),
        key,
    })
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

/// Non-panicking font init. Returns Err instead of crashing.
fn try_init_font(
    config: &oakterm_config::ConfigValues,
    font_size: f32,
) -> Result<FontState, String> {
    let db = font::system_font_db();
    let (metrics, data) = if config.font_family.is_empty() {
        font::load_default_metrics(&db, font_size)
            .map_err(|e| format!("no system monospace font: {e}"))?
    } else {
        match font::load_font_by_name(&db, &config.font_family, font_size) {
            Ok(result) => result,
            Err(e) => {
                warn!(
                    error = %e,
                    font_family = %config.font_family,
                    "font not found, using system default"
                );
                font::load_default_metrics(&db, font_size)
                    .map_err(|e| format!("no system monospace font: {e}"))?
            }
        }
    };

    let mut shaper = SwashShaper::new();
    let font_key = shaper
        .load_font(data, font_size)
        .ok_or_else(|| "failed to load font into shaper".to_string())?;

    Ok(FontState {
        shaper,
        font_key,
        atlas: AtlasPlane::new(),
        color_atlas: AtlasPlane::new(),
        color_keys: std::collections::HashSet::new(),
        font_size,
        metrics,
    })
}

fn start_config_watcher(
    proxy: &EventLoopProxy<UserEvent>,
) -> Option<
    notify_debouncer_full::Debouncer<
        notify::RecommendedWatcher,
        notify_debouncer_full::RecommendedCache,
    >,
> {
    let config_dir = oakterm_config::config_dir();
    if !config_dir.exists() {
        return None;
    }

    let config_path = config_dir.join("config.lua");
    let proxy = proxy.clone();

    let debouncer = notify_debouncer_full::new_debouncer(
        std::time::Duration::from_millis(300),
        None,
        move |result: notify_debouncer_full::DebounceEventResult| {
            let events = match result {
                Ok(events) => events,
                Err(errors) => {
                    for e in &errors {
                        warn!(error = %e, "config watcher error");
                    }
                    return;
                }
            };
            let lua_changed = events.iter().any(|e| {
                e.paths
                    .iter()
                    .any(|p| p.extension().is_some_and(|ext| ext == "lua"))
            });
            if !lua_changed {
                return;
            }
            let cr = oakterm_config::load_config_from(&config_path);
            // Event loop may be closed during shutdown; best-effort.
            let _ = proxy.send_event(UserEvent::ConfigReloaded(Box::new(cr)));
        },
    );

    match debouncer {
        Ok(mut watcher) => {
            if let Err(e) = watcher.watch(&config_dir, notify::RecursiveMode::Recursive) {
                warn!(error = %e, "could not watch config directory");
                return None;
            }
            Some(watcher)
        }
        Err(e) => {
            warn!(error = %e, "could not start config watcher");
            None
        }
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
        let old_texture = std::mem::replace(
            atlas_texture,
            device.create_texture(&wgpu::TextureDescriptor {
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
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let copy_w = tex_size.width.min(atlas_w);
        let copy_h = tex_size.height.min(atlas_h);
        encoder.copy_texture_to_texture(
            old_texture.as_image_copy(),
            atlas_texture.as_image_copy(),
            wgpu::Extent3d {
                width: copy_w,
                height: copy_h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(encoder.finish()));
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

/// Upload new color glyph bitmaps to the GPU color atlas texture.
fn upload_color_glyphs_to_atlas(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    color_atlas_texture: &mut wgpu::Texture,
    color_atlas_view: &mut wgpu::TextureView,
    color_atlas: &AtlasPlane,
    uploads: &[render_grid::GlyphUpload],
) {
    let (atlas_w, atlas_h) = color_atlas.size();
    let tex_size = color_atlas_texture.size();

    if tex_size.width != atlas_w || tex_size.height != atlas_h {
        let old_texture = std::mem::replace(
            color_atlas_texture,
            device.create_texture(&wgpu::TextureDescriptor {
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
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let copy_w = tex_size.width.min(atlas_w);
        let copy_h = tex_size.height.min(atlas_h);
        encoder.copy_texture_to_texture(
            old_texture.as_image_copy(),
            color_atlas_texture.as_image_copy(),
            wgpu::Extent3d {
                width: copy_w,
                height: copy_h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(encoder.finish()));
        *color_atlas_view =
            color_atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
    }

    for upload in uploads {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: color_atlas_texture,
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

/// Encode winit modifier state to xterm mouse modifier bits.
/// Shift=4, Alt/Meta=8, Ctrl=16.
fn encode_mouse_modifiers(mods: winit::event::Modifiers) -> u8 {
    let s = mods.state();
    let mut bits = 0u8;
    if s.shift_key() {
        bits |= 4;
    }
    if s.alt_key() {
        bits |= 8;
    }
    if s.control_key() {
        bits |= 16;
    }
    bits
}

/// Connect to the daemon, spawning it if needed.
///
/// Uses tmux-style connect-and-check with a lock file to handle stale
/// sockets and prevent two clients from racing to start the daemon.
fn connect_to_daemon(
    proxy: &EventLoopProxy<UserEvent>,
) -> std::io::Result<(DaemonWriter, Option<std::process::Child>)> {
    let socket_path = oakterm_daemon::socket::socket_path()?;

    // Try connecting to an existing daemon first.
    match UnixStream::connect(&socket_path) {
        Ok(stream) => return finish_connect(stream, proxy, None),
        Err(e)
            if e.kind() == std::io::ErrorKind::ConnectionRefused
                || e.kind() == std::io::ErrorKind::NotFound =>
        {
            // Stale socket or no socket. Fall through to spawn.
        }
        Err(e) => return Err(e),
    }

    // Acquire exclusive lock to serialize daemon startup.
    let _lock = oakterm_daemon::socket::acquire_startup_lock()?;

    // After acquiring the lock, retry connect: another client may have
    // started the daemon while we waited.
    match UnixStream::connect(&socket_path) {
        Ok(stream) => return finish_connect(stream, proxy, None),
        Err(e)
            if e.kind() == std::io::ErrorKind::ConnectionRefused
                || e.kind() == std::io::ErrorKind::NotFound =>
        {
            // Still no daemon. Proceed to spawn.
        }
        Err(e) => return Err(e),
    }

    // We hold the lock and no daemon is running. Clean up stale socket.
    if let Err(e) = std::fs::remove_file(&socket_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(std::io::Error::new(
                e.kind(),
                format!(
                    "failed to remove stale socket at {}: {e}",
                    socket_path.display()
                ),
            ));
        }
    }

    let child = spawn_daemon(&socket_path)?;

    // Brief retry: socket file appears at bind() but may not be listening yet.
    let stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            std::thread::sleep(std::time::Duration::from_millis(50));
            UnixStream::connect(&socket_path)?
        }
        Err(e) => return Err(e),
    };
    finish_connect(stream, proxy, Some(child))
}

/// Spawn the daemon binary and poll until the socket appears.
fn spawn_daemon(socket_path: &std::path::Path) -> std::io::Result<std::process::Child> {
    let daemon_bin = std::env::current_exe()?
        .parent()
        .expect("exe has parent dir")
        .join("oakterm-daemon");

    let mut child = std::process::Command::new(&daemon_bin)
        .spawn()
        .map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("failed to spawn daemon at {}: {e}", daemon_bin.display()),
            )
        })?;

    for _ in 0..50 {
        if socket_path.exists() {
            return Ok(child);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let detail = match child.try_wait() {
        Ok(Some(status)) => format!("daemon exited with {status}"),
        Ok(None) => "daemon running but socket not created after 2.5s".into(),
        Err(e) => format!("could not check daemon status: {e}"),
    };
    // Clean up to avoid zombie/orphan processes.
    let _ = child.kill();
    let _ = child.wait();
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!(
            "daemon socket not available at {}: {detail}",
            socket_path.display()
        ),
    ))
}

/// Complete connection setup: clone stream, create writer, handshake, spawn reader.
fn finish_connect(
    stream: UnixStream,
    proxy: &EventLoopProxy<UserEvent>,
    child: Option<std::process::Child>,
) -> std::io::Result<(DaemonWriter, Option<std::process::Child>)> {
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
                        error!(error = %e, "daemon write error");
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
                        error!(
                            error = %e,
                            payload_len = frame.payload.len(),
                            "failed to decode RenderUpdate, disconnecting"
                        );
                        let _ = proxy.send_event(UserEvent::Disconnected);
                        break;
                    }
                },
                MSG_TITLE_CHANGED => match TitleChanged::decode(&frame.payload) {
                    Ok(msg) => {
                        let _ = proxy.send_event(UserEvent::TitleChanged(msg.title));
                    }
                    Err(e) => {
                        error!(error = %e, "failed to decode TitleChanged");
                    }
                },
                MSG_SCROLLBACK_DATA => match ScrollbackData::decode(&frame.payload) {
                    Ok(data) => {
                        let _ = proxy.send_event(UserEvent::ScrollbackData(Box::new(data)));
                    }
                    Err(e) => {
                        error!(error = %e, "failed to decode ScrollbackData");
                    }
                },
                MSG_PROMPT_POSITION => match PromptPosition::decode(&frame.payload) {
                    Ok(pos) => {
                        let _ = proxy.send_event(UserEvent::PromptPosition(pos));
                    }
                    Err(e) => {
                        error!(error = %e, "failed to decode PromptPosition");
                    }
                },
                MSG_BELL => {
                    let _ = proxy.send_event(UserEvent::Bell);
                }
                other => {
                    warn!(
                        msg_type = format_args!("0x{other:04x}"),
                        "unhandled daemon message"
                    );
                }
            },
            Err(e) => {
                error!(error = %e, "daemon read error");
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

async fn init_gpu(window: Arc<Window>, blending_mode: u32) -> Result<GpuState, String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let surface = instance
        .create_surface(window.clone())
        .map_err(|e| format!("failed to create wgpu surface: {e}"))?;

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        })
        .await
        .map_err(|e| format!("no compatible GPU adapter found: {e}"))?;

    let (device, queue): (wgpu::Device, wgpu::Queue) = adapter
        .request_device(&wgpu::DeviceDescriptor::default())
        .await
        .map_err(|e| format!("failed to create GPU device: {e}"))?;

    let caps = surface.get_capabilities(&adapter);
    let format = caps
        .formats
        .iter()
        .find(|f| f.is_srgb())
        .or(caps.formats.first())
        .copied()
        .ok_or_else(|| "no compatible surface format found".to_string())?;

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

    // Set Display P3 color space on macOS for wide-gamut rendering.
    // Only enable P3 in shaders if the layer was actually configured.
    #[cfg(target_os = "macos")]
    let p3_active = set_surface_p3_colorspace(&window);
    #[cfg(not(target_os = "macos"))]
    let p3_active = false;

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

/// Set the `CAMetalLayer`'s color space to Display P3 on macOS.
///
/// wgpu doesn't expose color space configuration. We access the
/// `CAMetalLayer` through the window's `NSView` layer and set it directly.
/// Returns `true` if the layer was successfully set to P3.
#[cfg(target_os = "macos")]
fn set_surface_p3_colorspace(window: &Window) -> bool {
    use objc2_core_graphics::{CGColorSpace, kCGColorSpaceDisplayP3};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        warn!("failed to get window handle for P3 color space");
        return false;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        warn!("expected AppKit window handle on macOS");
        return false;
    };

    // Safety: kCGColorSpaceDisplayP3 is a well-known constant string.
    #[allow(unsafe_code)]
    let p3_name = unsafe { kCGColorSpaceDisplayP3 };
    let Some(p3) = CGColorSpace::with_name(Some(p3_name)) else {
        warn!("failed to create Display P3 color space");
        return false;
    };

    // Safety: the NSView pointer is valid for the window's lifetime.
    // wgpu may set the view's layer to a CAMetalLayer directly, or the
    // CAMetalLayer may be a sublayer of a backing layer. Search both.
    #[allow(unsafe_code)]
    unsafe {
        use objc2::msg_send;
        use objc2::runtime::{AnyClass, AnyObject, Bool};
        use objc2_quartz_core::CAMetalLayer;

        let ns_view: *mut AnyObject = appkit.ns_view.as_ptr().cast();
        let layer: *mut AnyObject = msg_send![ns_view, layer];
        if layer.is_null() {
            warn!("NSView has no layer for P3 color space");
            return false;
        }

        let metal_class = AnyClass::get(c"CAMetalLayer");
        let Some(metal_class) = metal_class else {
            warn!("CAMetalLayer class not found");
            return false;
        };

        // Check if the view's layer is directly a CAMetalLayer.
        let is_metal: Bool = msg_send![layer, isKindOfClass: metal_class];
        if is_metal.as_bool() {
            let metal_layer: &CAMetalLayer = &*(layer.cast::<CAMetalLayer>());
            metal_layer.setColorspace(Some(&p3));
            return true;
        }

        // Search sublayers for the CAMetalLayer.
        let sublayers: *mut AnyObject = msg_send![layer, sublayers];
        if !sublayers.is_null() {
            let count: usize = msg_send![sublayers, count];
            for i in 0..count {
                let sublayer: *mut AnyObject = msg_send![sublayers, objectAtIndex: i];
                let is_metal: Bool = msg_send![sublayer, isKindOfClass: metal_class];
                if is_metal.as_bool() {
                    let metal_layer: &CAMetalLayer = &*(sublayer.cast::<CAMetalLayer>());
                    metal_layer.setColorspace(Some(&p3));
                    return true;
                }
            }
        }

        warn!("no CAMetalLayer found on NSView for P3 color space");
        false
    }
}

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("{}", version_string());
        return;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(true)
        .with_file(false)
        .with_line_number(false)
        .init();

    if std::env::args().any(|a| a == "--init-config") {
        run_init_config();
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

fn run_init_config() {
    let config_dir = oakterm_config::config_dir();
    match oakterm_config::init_config(&config_dir) {
        Ok(result) => {
            println!("Config directory: {}", result.config_dir.display());
            if result.created_config {
                println!("  Created config.lua");
            } else {
                println!("  config.lua already exists (unchanged)");
            }
            if result.created_luarc {
                println!("  Created .luarc.json");
            } else {
                println!("  .luarc.json already exists (unchanged)");
            }
            if result.updated_stubs {
                println!("  Updated types/oakterm.lua");
            } else {
                println!("  types/oakterm.lua is up to date");
            }
        }
        Err(e) => {
            error!(error = %e, "failed to initialize config");
            std::process::exit(1);
        }
    }
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
