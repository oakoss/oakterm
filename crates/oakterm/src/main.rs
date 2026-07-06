mod a11y_bridge;
mod daemon_conn;
mod layout;
mod pane_view;
mod render_grid;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tracing::{debug, error, info, warn};

use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowAttributes, WindowId};

use wgpu::CurrentSurfaceTexture;

use oakterm_protocol::frame::Frame;
use oakterm_protocol::input::{KeyInput, MouseInput, Resize};
use oakterm_protocol::message::{
    FindPrompt, FocusPane, GetLayoutTree, GetScrollback, LayoutTreeNode, MSG_DETACH,
    MSG_FIND_PROMPT, MSG_FOCUS_PANE, MSG_GET_LAYOUT_TREE, MSG_GET_RENDER_UPDATE,
    MSG_GET_SCROLLBACK, MSG_SPLIT_PANE, PromptPosition, ScrollbackData, SearchDirection,
    SplitDirection as WireSplitDirection, SplitPane,
};
use oakterm_protocol::render::{GetRenderUpdate, RenderUpdate};

use oakterm_renderer::atlas::AtlasPlane;
use oakterm_renderer::font;
use oakterm_renderer::pipeline::{BgSection, BgUniforms, RenderPipeline, TextUniforms};
use oakterm_renderer::shaper::FontKey;
use oakterm_renderer::swash_shaper::SwashShaper;

use a11y_bridge::{A11yEvent, A11yModel};
use daemon_conn::{DaemonWriter, connect_to_daemon};
use pane_view::{PaneView, ScrollbackClampOutcome, clamp_viewport};
use render_grid::ClientGrid;

// AccessKit handlers per Spec-0006.

struct TerminalActivationHandler {
    state: Arc<Mutex<Option<A11yModel>>>,
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
        guard.as_ref().map(A11yModel::build_full_tree)
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
    TitleChanged(u32, String),
    Bell,
    AccessKitAction(accesskit::ActionRequest),
    Disconnected,
    ConfigReloaded(Box<oakterm_config::ConfigResult>),
    /// A split was accepted; payload is the new pane's ID.
    SplitCreated(u32),
    /// The daemon's layout tree for the current tab.
    LayoutTree(Box<LayoutTreeNode>),
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
    bold_key: Option<FontKey>,
    italic_key: Option<FontKey>,
    bold_italic_key: Option<FontKey>,
    atlas: AtlasPlane,
    color_atlas: AtlasPlane,
    /// Cache keys of glyphs stored in the color atlas.
    color_keys: std::collections::HashSet<oakterm_renderer::atlas::GlyphCacheKey>,
    font_size: f32,
    metrics: oakterm_renderer::shaper::FontMetrics,
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
    SplitPane(WireSplitDirection),
    Stub,
}

/// Border colors are fixed until the theme system (TREK-212) lands.
const PANE_BORDER_RGB: [u8; 3] = [64, 64, 64];
const FOCUSED_BORDER_RGB: [u8; 3] = [92, 148, 255];

/// Reconcile the pane map with a computed geometry and plan the `Resize`
/// messages to send: creates a view for each new pane, resizes stale
/// local grids (the daemon's `RenderUpdate` doesn't change dimensions),
/// and dedups against `last_sent_dims`. A fresh `PaneView` starts at
/// `last_sent_dims == (0, 0)`, so its first plan always includes the
/// `Resize` that spawns the pane's PTY — tests pin that contract.
/// `last_sent_dims` is deliberately NOT committed here: callers commit
/// after a successful send so failed sends are re-planned.
fn plan_pane_syncs(
    geometry: &layout::LayoutGeometry,
    (cell_width, cell_height): (f32, f32),
    panes: &mut HashMap<u32, PaneView>,
) -> Vec<Resize> {
    let mut resizes = Vec::new();
    for pane in &geometry.panes {
        let (cols, rows) = layout::grid_dims(pane.rect, cell_width, cell_height);
        let view = panes
            .entry(pane.pane_id)
            .or_insert_with(|| PaneView::new(ClientGrid::new(cols, rows)));
        if view.grid.cols != cols || view.grid.rows != rows {
            view.grid.resize(cols, rows);
        }
        if view.last_sent_dims == (cols, rows) {
            continue;
        }
        resizes.push(Resize {
            pane_id: pane.pane_id,
            cols,
            rows,
            pixel_width: u16::try_from(pane.rect.width).unwrap_or(u16::MAX),
            pixel_height: u16::try_from(pane.rect.height).unwrap_or(u16::MAX),
        });
    }
    resizes
}

/// A solid rectangle drawn through the bg pipeline as a 1x1 cell grid
/// whose cell size is the rectangle.
fn solid_section(rect: layout::PixelRect, rgb: [u8; 3], viewport: (f32, f32)) -> BgSection {
    #[allow(clippy::cast_precision_loss)] // pixel coordinates fit in f32
    BgSection::new(
        BgUniforms {
            cols: 1,
            rows: 1,
            cell_width: rect.width as f32,
            cell_height: rect.height as f32,
            viewport_width: viewport.0,
            viewport_height: viewport.1,
            pad_left: rect.x as f32,
            pad_top: rect.y as f32,
        },
        vec![render_grid::pack_bg_color(rgb)],
    )
}

/// Convert a winit `MouseScrollDelta` into integer "wheel notches" (1 notch =
/// 1 line). `LineDelta` is truncated and clears any pending pixel residue.
/// `PixelDelta` is accumulated in `accum` and drained one notch per `cell_h`
/// pixels so macOS smooth-scroll's 60-120Hz event stream doesn't flood the
/// downstream consumer. Direction reversal mid-stream resets the accumulator
/// to kill inertia/rubber-band bounce.
///
/// Returns 0 when no whole notch has accumulated yet, or when `cell_h <= 0.0`
/// (degenerate font state).
fn drain_wheel_notches(delta: winit::event::MouseScrollDelta, cell_h: f64, accum: &mut f64) -> i32 {
    use winit::event::MouseScrollDelta;
    #[allow(clippy::cast_possible_truncation)]
    match delta {
        MouseScrollDelta::LineDelta(_, v) => {
            *accum = 0.0;
            if v == 0.0 {
                return 0;
            }
            v.trunc() as i32
        }
        MouseScrollDelta::PixelDelta(p) => {
            if p.y == 0.0 || cell_h <= 0.0 {
                return 0;
            }
            if p.y.signum() != accum.signum() && *accum != 0.0 {
                *accum = 0.0;
            }
            *accum += p.y;
            let n = (*accum / cell_h).trunc();
            if n == 0.0 {
                return 0;
            }
            *accum -= n * cell_h;
            n as i32
        }
    }
}

#[allow(clippy::struct_excessive_bools)]
struct App {
    window: Option<Arc<Window>>,
    gpu: Option<GpuState>,
    font: Option<FontState>,
    panes: HashMap<u32, PaneView>,
    focused_pane: u32,
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
    /// Set after initial Resize is sent. Gates on first `RedrawRequested`.
    initial_resize_sent: bool,
    /// Last known mouse position in grid coordinates.
    last_mouse_cell: (u16, u16),
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
    a11y_state: Arc<Mutex<Option<A11yModel>>>,
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
    /// Accumulated `PixelDelta` wheel-y, in pixels. Drained one notch per
    /// `cell_height` pixels so high-frequency macOS smooth-scroll events
    /// don't flood the daemon with arrow keys.
    wheel_accum_y: f64,
    /// Layout tree from the daemon (`GetLayoutTree`). `None` until the
    /// first split; single-pane rendering needs no tree.
    layout_tree: Option<LayoutTreeNode>,
    /// Pixel geometry computed from `layout_tree` for the current window
    /// size. Recomputed on window resize and topology change.
    layout_geometry: Option<layout::LayoutGeometry>,
    /// Pane awaiting focus once its view exists. Focus must not move to a
    /// split's new pane before `LayoutTree` arrives — the render fallback
    /// draws the focused pane, and a viewless focus blanks the window.
    pending_focus: Option<u32>,
    /// Monotonic request serial. Pushes use 0 and the reader thread owns
    /// 1; App requests start above both so error frames attribute.
    next_serial: u32,
}

impl App {
    fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            window: None,
            gpu: None,
            font: None,
            panes: HashMap::new(),
            focused_pane: 0,
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
            initial_resize_sent: false,
            last_mouse_cell: (0, 0),
            modifiers: winit::event::Modifiers::default(),
            shift_bypassed_buttons: 0,
            blink_visible: true,
            blink_deadline: None,
            focused: true,
            a11y_state: Arc::new(Mutex::new(None)),
            mouse_pressed: false,
            click_count: 0,
            last_click_time: None,
            last_click_pos: (0, 0),
            last_mouse_pixel: (0.0, 0.0),
            wheel_accum_y: 0.0,
            layout_tree: None,
            layout_geometry: None,
            pending_focus: None,
            next_serial: 10,
        }
    }

    fn focused_view(&self) -> Option<&PaneView> {
        self.panes.get(&self.focused_pane)
    }

    fn focused_view_mut(&mut self) -> Option<&mut PaneView> {
        self.panes.get_mut(&self.focused_pane)
    }

    /// The focused pane's scrollback offset; 0 (live view) when no pane exists.
    fn viewport_offset(&self) -> u32 {
        self.focused_view().map_or(0, |v| v.viewport_offset)
    }

    /// Request scrollback rows from the daemon for the current viewport offset.
    fn request_scrollback(&self) {
        if let (Some(daemon), Some(view)) = (&self.daemon, self.focused_view()) {
            let req = GetScrollback {
                pane_id: self.focused_pane,
                start_row: -i64::from(view.viewport_offset),
                count: u32::from(view.grid.rows),
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
                pane_id: self.focused_pane,
                from_offset: -i64::from(self.viewport_offset()),
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
            if let Some(view) = self.focused_view_mut() {
                #[allow(clippy::cast_sign_loss)]
                view.scroll_up(lines as u32);
            }
            self.request_scrollback();
        } else if lines < 0 && self.viewport_offset() > 0 {
            let Some(view) = self.focused_view_mut() else {
                return;
            };
            if view.scroll_down(lines.unsigned_abs()) {
                self.return_to_live();
            } else {
                self.request_scrollback();
            }
        }
    }

    /// Return to live view from scrollback.
    fn return_to_live(&mut self) {
        if let Some(view) = self.focused_view_mut() {
            view.viewport_offset = 0;
            view.grid.exit_scrollback();
        }
        // Request a full refresh to ensure live view is current.
        if let Some(daemon) = &self.daemon {
            let req = GetRenderUpdate {
                pane_id: self.focused_pane,
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

        let sel_row = i64::from(row) - i64::from(self.viewport_offset());

        match self.click_count {
            2 => {
                // Semantic (word) selection.
                if let Some(view) = self.focused_view_mut() {
                    if row < view.grid.rows {
                        let text: Vec<char> = view.grid.row_text(row).chars().collect();
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
                            view.selection = Some(sel);
                        }
                    }
                }
            }
            3 => {
                // Line selection.
                let mut sel = Selection::new(SelectionType::Line, sel_row, 0, AnchorSide::Left);
                sel.update(sel_row, 0, AnchorSide::Left);
                if let Some(view) = self.focused_view_mut() {
                    view.selection = Some(sel);
                }
            }
            _ => {
                // Normal (single click) selection.
                if let Some(view) = self.focused_view_mut() {
                    view.selection =
                        Some(Selection::new(SelectionType::Normal, sel_row, col, side));
                }
            }
        }

        self.mouse_pressed = true;
        self.sync_selection_a11y(self.focused_pane);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Push a pane's selection into the a11y model and notify AT when it
    /// changed. Call after any selection mutation.
    fn sync_selection_a11y(&mut self, pane_id: u32) {
        let Some(view) = self.panes.get(&pane_id) else {
            return;
        };
        let update =
            a11y_bridge::apply(&self.a11y_state, pane_id, view, A11yEvent::SelectionChanged);
        if let (Some(update), Some(adapter)) = (update, &mut self.accesskit) {
            adapter.update_if_active(|| update);
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
        let Some(view) = self.focused_view() else {
            return false;
        };
        if !view.grid.cursor_visible || view.grid.is_scrolled() {
            return false;
        }
        // Blinking styles: 0=BlinkingBlock, 2=BlinkingUnderline, 4=BlinkingBar
        matches!(view.grid.cursor_style, 0 | 2 | 4)
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
        let (cols, rows) = window_to_grid_dims(size, &font_state.metrics, &config.padding);
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

        let view = PaneView::new(grid);

        // Populate the a11y model so the activation handler can build a tree.
        match self.a11y_state.lock() {
            Ok(mut model) => {
                let mut m = A11yModel::new(
                    self.focused_pane,
                    a11y_bridge::cell_dims(Some(&font_state.metrics)),
                );
                m.register_pane(self.focused_pane, &view);
                *model = Some(m);
            }
            Err(e) => warn!(error = %e, "a11y: mutex poisoned during init"),
        }

        self.window = Some(window);
        self.gpu = Some(gpu);
        self.font = Some(font_state);
        self.panes.insert(self.focused_pane, view);
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
                if size.width > 0 && size.height > 0 {
                    let Some(gpu) = &mut self.gpu else { return };
                    {
                        let pixel_dims_changed =
                            gpu.config.width != size.width || gpu.config.height != size.height;
                        if pixel_dims_changed {
                            gpu.config.width = size.width;
                            gpu.config.height = size.height;
                            gpu.surface.configure(&gpu.device, &gpu.config);
                        }

                        // Resize exits scrollback for the focused pane.
                        if let Some(view) = self.panes.get_mut(&self.focused_pane) {
                            view.viewport_offset = 0;
                        }

                        // With splits, every pane's rect changes: recompute
                        // the geometry and resize each PTY to its rect. The
                        // single-pane path below sizes to the whole window.
                        // Keyed on the tree, not the geometry it produces —
                        // keying on geometry would wedge single-pane mode
                        // if a recompute was ever skipped.
                        if self.layout_tree.is_some() {
                            self.recompute_layout_geometry();
                            let multi_pane = self
                                .layout_geometry
                                .as_ref()
                                .is_some_and(|g| g.panes.len() > 1);
                            if multi_pane {
                                if self.initial_resize_sent {
                                    self.sync_panes_to_geometry();
                                }
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                                return;
                            }
                        }

                        #[allow(clippy::cast_possible_truncation)]
                        if let (Some(font), Some(view)) =
                            (&self.font, self.panes.get_mut(&self.focused_pane))
                        {
                            let grid = &mut view.grid;
                            let (cols, rows) =
                                window_to_grid_dims(size, &font.metrics, &self.config.padding);
                            let dims_changed = grid.rows != rows || grid.cols != cols;
                            if dims_changed {
                                grid.resize(cols, rows);
                            } else {
                                // Dims unchanged but viewport_offset was reset;
                                // still need to exit scrollback mode.
                                grid.exit_scrollback();
                            }

                            // Full a11y tree rebuild on resize (row count changed).
                            if dims_changed {
                                let full_tree = a11y_bridge::apply(
                                    &self.a11y_state,
                                    self.focused_pane,
                                    view,
                                    A11yEvent::Resize,
                                );
                                if let (Some(adapter), Some(full_tree)) =
                                    (&mut self.accesskit, full_tree)
                                {
                                    adapter.update_if_active(|| full_tree);
                                }
                            }

                            // Defer until RedrawRequested; startup fires multiple Resized events.
                            if self.initial_resize_sent && (cols, rows) != view.last_sent_dims {
                                view.last_sent_dims = (cols, rows);
                                if let Some(daemon) = &mut self.daemon {
                                    let msg = Resize {
                                        pane_id: self.focused_pane,
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
                    // Drop any pending wheel pixels so they don't apply to a
                    // future scroll in a different pane / window state.
                    self.wheel_accum_y = 0.0;
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
                let cleared = self
                    .focused_view_mut()
                    .is_some_and(|view| view.selection.take().is_some());
                if cleared {
                    self.sync_selection_a11y(self.focused_pane);
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }

                // Any unbound key while scrolled: snap back to live first.
                if self.viewport_offset() > 0 {
                    self.return_to_live();
                }

                let bytes = key_to_bytes(&logical_key, text.as_deref());
                if let (Some(daemon), Some(bytes)) = (&mut self.daemon, bytes) {
                    let msg = KeyInput {
                        pane_id: self.focused_pane,
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
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    clippy::cast_precision_loss // padding values are small
                )]
                if let Some(font) = &self.font {
                    // Subtract padding so clicks in the gutter map to cell 0.
                    let px = (position.x as f32 - self.config.padding.left as f32).max(0.0);
                    let py = (position.y as f32 - self.config.padding.top as f32).max(0.0);
                    let col = (px / font.metrics.cell_width) as u16;
                    let row = (py / font.metrics.cell_height) as u16;
                    self.last_mouse_cell = (col, row);

                    // Update selection end during drag.
                    if self.mouse_pressed {
                        use oakterm_terminal::grid::selection::{
                            AnchorSide, SelectionType, word_boundaries,
                        };
                        let cw = f64::from(font.metrics.cell_width);
                        let adj_x = (position.x - f64::from(self.config.padding.left)).max(0.0);
                        let side = if (adj_x % cw) > (cw / 2.0) {
                            AnchorSide::Right
                        } else {
                            AnchorSide::Left
                        };
                        if let Some(view) = self.panes.get_mut(&self.focused_pane) {
                            let sel_row = i64::from(row) - i64::from(view.viewport_offset);
                            let grid = &view.grid;
                            if let Some(sel) = &mut view.selection {
                                if sel.ty == SelectionType::Semantic {
                                    // Snap drag to word boundaries.
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
                                } else {
                                    sel.update(sel_row, col, side);
                                }
                            }
                        }
                        self.sync_selection_a11y(self.focused_pane);
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
                        if state == ElementState::Pressed && btn == 0 {
                            let cleared = self
                                .focused_view_mut()
                                .is_some_and(|view| view.selection.take().is_some());
                            if cleared {
                                self.sync_selection_a11y(self.focused_pane);
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                            }
                        }
                        if let Some(daemon) = &mut self.daemon {
                            let (x, y) = self.last_mouse_cell;
                            let event_type = match state {
                                ElementState::Pressed => 0,
                                ElementState::Released => 1,
                            };
                            let msg = MouseInput {
                                pane_id: self.focused_pane,
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
                let cell_h = self
                    .font
                    .as_ref()
                    .map_or(16.0_f64, |f| f64::from(f.metrics.cell_height));
                let notches = drain_wheel_notches(delta, cell_h, &mut self.wheel_accum_y);
                if notches == 0 {
                    return;
                }
                let scroll_up = notches > 0;
                #[allow(clippy::cast_sign_loss)]
                let count = notches.unsigned_abs();
                #[allow(clippy::cast_possible_wrap)]
                let scroll_lines = (3 * count) as i32;

                let shift = self.modifiers.state().shift_key();
                let alt_screen = self.focused_view().is_some_and(|v| v.grid.alt_screen);

                // Routing (matches alacritty/kitty/wezterm):
                // - Already in host scrollback: keep scrolling host until offset == 0.
                // - Shift held, or primary screen: host scrollback.
                // - Alt screen with no Shift: forward to daemon (mouse mode / 1007 alt-scroll).
                let delta = if scroll_up {
                    scroll_lines
                } else {
                    -scroll_lines
                };
                if self.viewport_offset() > 0 || shift || !alt_screen {
                    self.scroll_viewport(delta);
                } else if let Some(daemon) = &mut self.daemon {
                    let (x, y) = self.last_mouse_cell;
                    let event_type = if scroll_up { 3u8 } else { 4u8 };
                    let mods = encode_mouse_modifiers(self.modifiers);
                    for _ in 0..count.min(5) {
                        let msg = MouseInput {
                            pane_id: self.focused_pane,
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
            #[allow(clippy::cast_precision_loss)] // viewport dimensions fit in f32
            WindowEvent::RedrawRequested => {
                let Some(gpu) = &mut self.gpu else { return };

                // First RedrawRequested: window dimensions have settled. Send the
                // initial Resize that triggers PTY spawn on the daemon side.
                if !self.initial_resize_sent {
                    #[allow(clippy::cast_possible_truncation)]
                    if let (Some(font), Some(view), Some(daemon)) = (
                        &self.font,
                        self.panes.get_mut(&self.focused_pane),
                        &mut self.daemon,
                    ) {
                        let size =
                            winit::dpi::PhysicalSize::new(gpu.config.width, gpu.config.height);
                        let (cols, rows) =
                            window_to_grid_dims(size, &font.metrics, &self.config.padding);
                        view.last_sent_dims = (cols, rows);
                        let msg = Resize {
                            pane_id: self.focused_pane,
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

                let render_list: Vec<(u32, u32, u32)> = match &self.layout_geometry {
                    Some(geo) if geo.panes.len() > 1 => geo
                        .panes
                        .iter()
                        .map(|p| (p.pane_id, p.rect.x, p.rect.y))
                        .collect(),
                    _ => vec![(
                        self.focused_pane,
                        self.config.padding.left,
                        self.config.padding.top,
                    )],
                };

                let mut bg_sections: Vec<BgSection> = Vec::new();
                let mut glyph_instances = Vec::new();
                if let Some(font) = &mut self.font {
                    let keys = render_grid::FontKeys {
                        regular: font.font_key,
                        bold: font.bold_key,
                        italic: font.italic_key,
                        bold_italic: font.bold_italic_key,
                    };
                    for &(pane_id, origin_x, origin_y) in &render_list {
                        let Some(pane) = self.panes.get(&pane_id) else {
                            continue;
                        };
                        let grid = &pane.grid;
                        let is_focused = pane_id == self.focused_pane;
                        // Cursor and selection render only in the focused
                        // pane; cursor hidden during blink-off phase.
                        let cursor_vis = is_focused
                            && grid.cursor_visible
                            && (self.blink_visible || !matches!(grid.cursor_style, 0 | 2 | 4));
                        let selection = if is_focused {
                            pane.selection.as_ref()
                        } else {
                            None
                        };

                        let bg = grid.bg_colors(cursor_vis, selection, pane.viewport_offset);
                        let (glyphs, uploads, color_uploads) = grid.glyph_instances(
                            &font.metrics,
                            &keys,
                            font.font_size,
                            &font.shaper,
                            &mut font.atlas,
                            &mut font.color_atlas,
                            &mut font.color_keys,
                            cursor_vis,
                            selection,
                            pane.viewport_offset,
                            origin_x as f32,
                            origin_y as f32,
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

                        bg_sections.push(BgSection::new(
                            BgUniforms {
                                cols: u32::from(grid.cols),
                                rows: u32::from(grid.rows),
                                cell_width: font.metrics.cell_width,
                                cell_height: font.metrics.cell_height,
                                viewport_width: gpu.config.width as f32,
                                viewport_height: gpu.config.height as f32,
                                pad_left: origin_x as f32,
                                pad_top: origin_y as f32,
                            },
                            bg,
                        ));
                        glyph_instances.extend(glyphs);
                    }
                }

                // Pane borders; segments adjacent to the focused pane get
                // the highlight color.
                if let Some(geo) = &self.layout_geometry {
                    if geo.panes.len() > 1 {
                        let focused = layout::focused_border_indices(geo, self.focused_pane);
                        let viewport = (gpu.config.width as f32, gpu.config.height as f32);
                        for (i, border) in geo.borders.iter().enumerate() {
                            let rgb = if focused.contains(&i) {
                                FOCUSED_BORDER_RGB
                            } else {
                                PANE_BORDER_RGB
                            };
                            bg_sections.push(solid_section(*border, rgb, viewport));
                        }
                    }
                }

                let (atlas_w, atlas_h) = self
                    .font
                    .as_ref()
                    .map_or((256u32, 256u32), |f| f.atlas.size());
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

                let clear_color =
                    self.panes
                        .get(&self.focused_pane)
                        .map_or(wgpu::Color::BLACK, |v| {
                            let [r, g, b] = v.grid.bg_color;
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
                    &bg_sections,
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
                // Build a11y incremental data while grid is borrowed,
                // then send it via adapter after the grid borrow ends.
                let mut a11y_update: Option<accesskit::TreeUpdate> = None;

                let is_focused = update.pane_id == self.focused_pane;
                let Some(view) = self.panes.get_mut(&update.pane_id) else {
                    debug!(
                        pane_id = update.pane_id,
                        "render update for unknown pane, dropping"
                    );
                    return;
                };
                view.bracketed_paste = update.bracketed_paste;
                if view.grid.is_scrolled() {
                    view.grid.apply_update_while_scrolled(&update);
                } else {
                    view.grid.apply_update(&update);

                    let dirty_rows: Vec<(u16, String)> = update
                        .dirty_rows
                        .iter()
                        .map(|r| (r.row_index, view.grid.row_text(r.row_index)))
                        .collect();
                    a11y_update = a11y_bridge::apply(
                        &self.a11y_state,
                        update.pane_id,
                        view,
                        A11yEvent::Render {
                            dirty_rows: &dirty_rows,
                        },
                    );

                    if is_focused {
                        // Restart blink — cursor style may have changed.
                        if self.blink_deadline.is_none() && self.should_blink() {
                            self.reset_blink();
                        }
                    }
                    // Background panes produce output too; any visible
                    // pane's update schedules a repaint.
                    let visible = is_focused
                        || self.layout_geometry.as_ref().is_some_and(|g| {
                            g.panes.len() > 1 && g.panes.iter().any(|p| p.pane_id == update.pane_id)
                        });
                    if visible {
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }

                if let (Some(adapter), Some(tree_update)) = (&mut self.accesskit, a11y_update) {
                    adapter.update_if_active(|| tree_update);
                }
            }
            UserEvent::ScrollbackData(data) => {
                if self.viewport_offset() > 0 {
                    match clamp_viewport(self.viewport_offset(), data.total_rows) {
                        ScrollbackClampOutcome::ReturnToLive => {
                            self.return_to_live();
                            return;
                        }
                        ScrollbackClampOutcome::Clamp(clamped) => {
                            if let Some(view) = self.focused_view_mut() {
                                view.viewport_offset = clamped;
                            }
                        }
                    }
                    let mut a11y_scrollback_update: Option<accesskit::TreeUpdate> = None;
                    let scroll_indicator = self.config.scroll_indicator;
                    if let Some(view) = self.panes.get_mut(&self.focused_pane) {
                        #[allow(clippy::cast_possible_truncation)]
                        let offset = view.viewport_offset.min(u32::from(u16::MAX)) as u16;
                        view.grid.apply_scrollback(&data.rows, offset);
                        if scroll_indicator {
                            view.grid.set_scroll_indicator(view.viewport_offset);
                        }
                        a11y_scrollback_update = a11y_bridge::apply(
                            &self.a11y_state,
                            self.focused_pane,
                            view,
                            A11yEvent::Scrollback {
                                total_rows: u64::from(data.total_rows),
                            },
                        );
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
                        if let Some(view) = self.focused_view_mut() {
                            view.viewport_offset = new_offset;
                            if !view.grid.is_scrolled() {
                                view.grid.enter_scrollback();
                            }
                        }
                        self.request_scrollback();
                    }
                }
            }
            UserEvent::TitleChanged(pane_id, title) => {
                if pane_id == self.focused_pane {
                    if let Some(w) = &self.window {
                        let display = if title.is_empty() { "oakterm" } else { &title };
                        w.set_title(display);
                    }
                }
                // Push immediately since no render event follows a title
                // change. The snapshot mutation happens whenever the view
                // exists; only the push depends on the adapter.
                if let Some(view) = self.panes.get(&pane_id) {
                    let update = a11y_bridge::apply(
                        &self.a11y_state,
                        pane_id,
                        view,
                        A11yEvent::Title(&title),
                    );
                    if let (Some(update), Some(adapter)) = (update, &mut self.accesskit) {
                        adapter.update_if_active(|| update);
                    }
                } else {
                    // Title is dropped, not stored: another client's pane on
                    // a shared daemon lands here.
                    debug!(pane_id, "a11y: title change dropped (no client view)");
                }
            }
            UserEvent::Bell => {
                // Announce bell to screen readers (assertive = interrupts).
                if let (Some(view), Some(adapter)) =
                    (self.panes.get(&self.focused_pane), &mut self.accesskit)
                {
                    let ann = oakterm_a11y::Announcement {
                        text: "Bell".into(),
                        level: accesskit::Live::Assertive,
                    };
                    if let Some(update) = a11y_bridge::apply(
                        &self.a11y_state,
                        self.focused_pane,
                        view,
                        A11yEvent::Announce(&ann),
                    ) {
                        adapter.update_if_active(|| update);
                    }
                    // Clear so a repeated bell is a fresh text transition.
                    if let Some(clear) = a11y_bridge::apply(
                        &self.a11y_state,
                        self.focused_pane,
                        view,
                        A11yEvent::ClearAnnouncement,
                    ) {
                        adapter.update_if_active(|| clear);
                    }
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
                        let page = self.focused_view().map_or(24, |v| i32::from(v.grid.rows));
                        self.scroll_viewport(page);
                    }
                    accesskit::Action::ScrollDown => {
                        let page = self.focused_view().map_or(24, |v| i32::from(v.grid.rows));
                        self.scroll_viewport(-page);
                    }
                    accesskit::Action::SetScrollOffset => {
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        if let Some(accesskit::ActionData::SetScrollOffset(point)) = request.data {
                            let target = point.y.max(0.0) as u32;
                            if target == 0 {
                                self.return_to_live();
                            } else {
                                if let Some(view) = self.focused_view_mut() {
                                    if !view.grid.is_scrolled() {
                                        view.grid.enter_scrollback();
                                    }
                                    view.viewport_offset = target;
                                }
                                self.request_scrollback();
                            }
                        } else {
                            debug!("a11y: SetScrollOffset without offset data");
                        }
                    }
                    accesskit::Action::SetTextSelection => {
                        // This is a user action; every rejection logs so
                        // "selection doesn't work with my screen reader" is
                        // diagnosable.
                        let Some(accesskit::ActionData::SetTextSelection(at_sel)) = request.data
                        else {
                            debug!("a11y: SetTextSelection without selection data");
                            return;
                        };
                        // The anchor names the target pane; convert rows
                        // using that pane's viewport offset and validate
                        // against its row count (an unknown pane yields
                        // rows = 0, rejecting every row).
                        let pane_hint = oakterm_a11y::decode_node_id(at_sel.anchor.node)
                            .map(|(pane_id, _)| pane_id);
                        let (offset, rows) = pane_hint
                            .and_then(|id| self.panes.get(&id))
                            .map_or((0, 0), |v| (v.viewport_offset, v.grid.rows));
                        let Some((pane_id, sel)) =
                            a11y_bridge::selection_from_a11y(&at_sel, offset, rows)
                        else {
                            debug!(
                                anchor = ?at_sel.anchor.node,
                                focus = ?at_sel.focus.node,
                                "a11y: SetTextSelection rejected (non-row, cross-pane, or \
                                 out-of-viewport endpoints)"
                            );
                            return;
                        };
                        let applied = self.panes.get_mut(&pane_id).is_some_and(|view| {
                            view.selection = sel;
                            true
                        });
                        if applied {
                            self.sync_selection_a11y(pane_id);
                            if let Some(w) = &self.window {
                                w.request_redraw();
                            }
                        } else {
                            warn!(pane_id, "a11y: SetTextSelection for untracked pane");
                        }
                    }
                    _ => {}
                }
            }
            UserEvent::Disconnected => {
                error!("daemon disconnected");
                event_loop.exit();
            }
            UserEvent::SplitCreated(new_pane_id) => {
                info!(new_pane_id, "split accepted; fetching layout");
                // Spec-0007 moves focus to the new pane, but only once its
                // view exists (after LayoutTree lands) — focusing a
                // viewless pane blanks the render fallback.
                self.pending_focus = Some(new_pane_id);
                self.request_layout_tree();
            }
            UserEvent::LayoutTree(tree) => {
                self.apply_layout_tree(*tree);
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
            // Config directions are placement-relative (oakterm.PaneDirection);
            // the wire protocol carries only the split axis, so left/right
            // and up/down collapse — the daemon always places the new pane
            // after the target (Spec-0007 Split).
            Some(Action::SplitPane { direction, size }) => {
                if (size - 0.5).abs() > f64::EPSILON {
                    // SplitPane (0xA0) has no size field yet.
                    warn!(size, "split_pane size not yet supported; using 0.5");
                }
                match direction.as_str() {
                    "left" | "right" => ActionDesc::SplitPane(WireSplitDirection::Horizontal),
                    "up" | "down" => ActionDesc::SplitPane(WireSplitDirection::Vertical),
                    other => {
                        warn!(direction = other, "unknown split direction in keybind");
                        return false;
                    }
                }
            }
            Some(
                Action::ClosePane
                | Action::FocusPaneDirection(_)
                | Action::NewTab
                | Action::CloseTab
                | Action::ShowCommandPalette,
            ) => ActionDesc::Stub,
            None => return false,
        };

        match action_desc {
            ActionDesc::ScrollUp(lines) => {
                if let Some(view) = self.focused_view_mut() {
                    let amount = if lines == 0 {
                        u32::from(view.grid.rows)
                    } else {
                        lines
                    };
                    view.scroll_up(amount);
                }
                self.request_scrollback();
                true
            }
            ActionDesc::ScrollDown(lines) => {
                if self.viewport_offset() == 0 {
                    return false; // Not scrolled; let key pass through to PTY.
                }
                let Some(view) = self.focused_view_mut() else {
                    return false;
                };
                let amount = if lines == 0 {
                    u32::from(view.grid.rows)
                } else {
                    lines
                };
                if view.scroll_down(amount) {
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
                    if let Some(view) = self.focused_view_mut() {
                        if !view.grid.is_scrolled() {
                            view.grid.enter_scrollback();
                        }
                    }
                    self.request_find_prompt(dir);
                } else if self.viewport_offset() > 0 {
                    self.request_find_prompt(dir);
                }
                true
            }
            ActionDesc::SendString(bytes) => {
                if let Some(daemon) = &mut self.daemon {
                    let msg = KeyInput {
                        pane_id: self.focused_pane,
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
                if let Some((sel, view)) = self
                    .focused_view()
                    .and_then(|v| v.selection.as_ref().map(|sel| (sel, v)))
                {
                    let text = view.grid.extract_selection_text(sel, view.viewport_offset);
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
                            let bracketed = self.focused_view().is_some_and(|v| v.bracketed_paste);
                            if let Some(daemon) = &mut self.daemon {
                                // Normalize line endings: PTY expects \r.
                                let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
                                let key_data = if bracketed {
                                    let mut buf = Vec::with_capacity(normalized.len() + 12);
                                    buf.extend_from_slice(b"\x1b[200~");
                                    buf.extend_from_slice(normalized.as_bytes());
                                    buf.extend_from_slice(b"\x1b[201~");
                                    buf
                                } else {
                                    normalized.into_bytes()
                                };
                                let msg = oakterm_protocol::input::KeyInput {
                                    pane_id: self.focused_pane,
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
            ActionDesc::SplitPane(direction) => {
                let msg = SplitPane {
                    pane_id: self.focused_pane,
                    direction,
                    command: String::new(),
                    cwd: String::new(),
                };
                let serial = self.take_serial();
                match msg
                    .encode()
                    .and_then(|payload| Frame::new(MSG_SPLIT_PANE, serial, payload))
                {
                    Ok(frame) => {
                        self.send_or_disconnect(&frame, "SplitPane");
                    }
                    Err(e) => error!(error = %e, "failed to encode SplitPane"),
                }
                true
            }
            ActionDesc::Stub => false,
        }
    }

    /// Send a frame to the daemon. On write failure the connection is
    /// dropped — a partial write leaves a truncated frame on the stream,
    /// so continuing to write through it would corrupt framing. Returns
    /// whether the frame was sent.
    fn send_or_disconnect(&mut self, frame: &Frame, what: &str) -> bool {
        if let Some(daemon) = &mut self.daemon {
            match daemon.send_frame(frame) {
                Ok(()) => true,
                Err(e) => {
                    error!(error = %e, what, "daemon write failed; disconnecting");
                    // Shutdown unblocks the reader thread so it drives the
                    // Disconnected -> exit path deterministically.
                    daemon.shutdown();
                    self.daemon = None;
                    false
                }
            }
        } else {
            warn!(what, "not connected to daemon; dropping");
            false
        }
    }

    /// Next request serial (App-owned range; see `next_serial`).
    fn take_serial(&mut self) -> u32 {
        let serial = self.next_serial;
        self.next_serial = self.next_serial.checked_add(1).unwrap_or(10);
        serial
    }

    fn request_layout_tree(&mut self) {
        let req = GetLayoutTree {
            workspace_id: 0,
            tab_id: 0,
        };
        let serial = self.take_serial();
        match Frame::new(MSG_GET_LAYOUT_TREE, serial, req.encode()) {
            Ok(frame) => {
                self.send_or_disconnect(&frame, "GetLayoutTree");
            }
            Err(e) => error!(error = %e, "failed to encode GetLayoutTree"),
        }
    }

    /// The window's content area in pixels (window minus padding), where
    /// the layout tree's panes tile.
    fn content_rect(&self) -> Option<layout::PixelRect> {
        let gpu = self.gpu.as_ref()?;
        let pad = &self.config.padding;
        Some(layout::PixelRect {
            x: pad.left,
            y: pad.top,
            width: gpu.config.width.saturating_sub(pad.left + pad.right),
            height: gpu.config.height.saturating_sub(pad.top + pad.bottom),
        })
    }

    /// Recompute pixel geometry from the stored layout tree for the
    /// current window size. Stale geometry is kept (not cleared) when the
    /// GPU is briefly unavailable — vanishing panes are worse than
    /// one-frame-stale rects.
    fn recompute_layout_geometry(&mut self) {
        match (&self.layout_tree, self.content_rect()) {
            (Some(tree), Some(content)) => {
                self.layout_geometry = Some(layout::compute_layout(tree, content));
            }
            (Some(_), None) => {
                warn!("layout geometry not recomputed: gpu unavailable");
            }
            (None, _) => self.layout_geometry = None,
        }
    }

    /// Adopt a layout tree from the daemon: create views for new panes,
    /// size every pane's PTY to its computed rect, apply any pending
    /// focus, and redraw.
    fn apply_layout_tree(&mut self, tree: LayoutTreeNode) {
        self.layout_tree = Some(tree);
        self.recompute_layout_geometry();
        self.sync_panes_to_geometry();
        if let Some(id) = self.pending_focus.take() {
            if self.panes.contains_key(&id) {
                // a11y focus follows in TREK-190 (multi-pane a11y wiring).
                self.focused_pane = id;
                // Keep the daemon's Spec-0007 focus state in step —
                // session persistence saves it.
                let serial = self.take_serial();
                match Frame::new(MSG_FOCUS_PANE, serial, FocusPane { pane_id: id }.encode()) {
                    Ok(frame) => {
                        self.send_or_disconnect(&frame, "FocusPane");
                    }
                    Err(e) => error!(error = %e, "failed to encode FocusPane"),
                }
            } else {
                warn!(pane_id = id, "pending focus target missing from layout");
            }
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Create views for panes new to the geometry and send `Resize` to
    /// every pane whose computed grid dimensions changed. The first
    /// `Resize` for a fresh pane spawns its PTY (Spec-0001 `SplitPane`).
    /// `last_sent_dims` commits only after a successful send, so a failed
    /// spawn-`Resize` is retried by the next sync instead of being
    /// deduplicated away.
    fn sync_panes_to_geometry(&mut self) {
        let Some(geometry) = self.layout_geometry.clone() else {
            warn!("layout tree adopted but no geometry; panes not synced");
            return;
        };
        let Some(metrics) = self.font.as_ref().map(|f| f.metrics) else {
            warn!("layout geometry present but font unavailable; panes not synced");
            return;
        };
        let resizes = plan_pane_syncs(
            &geometry,
            (metrics.cell_width, metrics.cell_height),
            &mut self.panes,
        );
        for msg in resizes {
            let pane_id = msg.pane_id;
            match msg.to_frame() {
                Ok(frame) => {
                    if self.send_or_disconnect(&frame, "Resize") {
                        if let Some(view) = self.panes.get_mut(&pane_id) {
                            view.last_sent_dims = (msg.cols, msg.rows);
                        }
                    }
                }
                Err(e) => error!(error = %e, pane_id, "failed to encode Resize"),
            }
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
                if let (Some(gpu), Some(view)) = (&self.gpu, self.panes.get_mut(&self.focused_pane))
                {
                    let phys = winit::dpi::PhysicalSize::new(gpu.config.width, gpu.config.height);
                    let (cols, rows) =
                        window_to_grid_dims(phys, &font_state.metrics, &self.config.padding);
                    let cols = cols.max(1);
                    let rows = rows.max(1);
                    view.grid.resize(cols, rows);
                    // grid.resize exits scrollback; keep the viewport field
                    // in step (mirrors WindowEvent::Resized).
                    view.viewport_offset = 0;
                    view.last_sent_dims = (cols, rows);

                    // Row bounds derive from cell dimensions; rebuild the
                    // a11y tree at the new metrics.
                    match self.a11y_state.lock() {
                        Ok(mut model) => {
                            if let Some(m) = model.as_mut() {
                                m.set_cell_dims(a11y_bridge::cell_dims(Some(&font_state.metrics)));
                            }
                        }
                        Err(e) => warn!(error = %e, "a11y: mutex poisoned on font change"),
                    }
                    let full_tree = a11y_bridge::apply(
                        &self.a11y_state,
                        self.focused_pane,
                        view,
                        A11yEvent::Resize,
                    );
                    if let (Some(adapter), Some(full_tree)) = (&mut self.accesskit, full_tree) {
                        adapter.update_if_active(|| full_tree);
                    }

                    if let Some(daemon) = &self.daemon {
                        let msg = Resize {
                            pane_id: self.focused_pane,
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
                } else {
                    // The renderer adopts the new metrics below; without a
                    // grid resize the a11y model and daemon keep the old
                    // geometry.
                    warn!("config reload: font changed but gpu/view unavailable");
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
    padding: &oakterm_config::Padding,
) -> (u16, u16) {
    let usable_w = size.width.saturating_sub(padding.left + padding.right);
    let usable_h = size.height.saturating_sub(padding.top + padding.bottom);
    let cols = ((usable_w as f32 / metrics.cell_width) as u16).max(1);
    let rows = ((usable_h as f32 / metrics.cell_height) as u16).max(1);
    (cols, rows)
}

/// Non-panicking font init. Returns Err instead of crashing.
fn try_init_font(
    config: &oakterm_config::ConfigValues,
    font_size: f32,
) -> Result<FontState, String> {
    let db = font::system_font_db();
    let variants = if config.font_family.is_empty() {
        font::load_default_variants(&db, font_size)
            .map_err(|e| format!("no system monospace font: {e}"))?
    } else {
        match font::load_font_variants(&db, &config.font_family, font_size) {
            Ok(result) => result,
            Err(e) => {
                warn!(
                    error = %e,
                    font_family = %config.font_family,
                    "font not found, using system default"
                );
                font::load_default_variants(&db, font_size)
                    .map_err(|e| format!("no system monospace font: {e}"))?
            }
        }
    };

    let (regular_data, metrics) = variants.regular;
    let mut shaper = SwashShaper::new();
    let font_key = shaper
        .load_font(regular_data, font_size)
        .ok_or_else(|| "failed to load font into shaper".to_string())?;

    let bold_key = variants
        .bold
        .and_then(|(data, _)| shaper.load_font(data, font_size));
    let italic_key = variants
        .italic
        .and_then(|(data, _)| shaper.load_font(data, font_size));
    let bold_italic_key = variants
        .bold_italic
        .and_then(|(data, _)| shaper.load_font(data, font_size));

    debug!(
        ?font_key,
        ?bold_key,
        ?italic_key,
        ?bold_italic_key,
        "font variants loaded"
    );

    Ok(FontState {
        shaper,
        font_key,
        bold_key,
        italic_key,
        bold_italic_key,
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
    // Enable P3 in shaders only when the layer was configured.
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

#[cfg(test)]
#[allow(clippy::float_cmp)] // exact arithmetic on small integers in f64
mod tests {
    use super::{drain_wheel_notches, plan_pane_syncs};
    use crate::layout::{LayoutGeometry, PaneRect, PixelRect};
    use crate::pane_view::PaneView;
    use crate::render_grid::ClientGrid;
    use std::collections::HashMap;
    use winit::dpi::PhysicalPosition;
    use winit::event::MouseScrollDelta;

    fn geometry_of(panes: &[(u32, u32, u32)]) -> LayoutGeometry {
        LayoutGeometry {
            panes: panes
                .iter()
                .map(|&(pane_id, width, height)| PaneRect {
                    pane_id,
                    rect: PixelRect {
                        x: 0,
                        y: 0,
                        width,
                        height,
                    },
                })
                .collect(),
            borders: vec![],
        }
    }

    const CELL: (f32, f32) = (10.0, 20.0);

    #[test]
    fn plan_pane_syncs_fresh_pane_always_gets_spawn_resize() {
        // The first Resize spawns the pane's PTY (Spec-0001 SplitPane);
        // a fresh view must never be deduplicated away.
        let geometry = geometry_of(&[(7, 400, 200)]);
        let mut panes = HashMap::new();
        let resizes = plan_pane_syncs(&geometry, CELL, &mut panes);
        assert_eq!(resizes.len(), 1);
        assert_eq!(resizes[0].pane_id, 7);
        assert_eq!((resizes[0].cols, resizes[0].rows), (40, 10));
        assert!(panes.contains_key(&7));
    }

    #[test]
    fn plan_pane_syncs_replans_until_send_is_committed() {
        // The plan does not commit last_sent_dims — the caller commits
        // after a successful send — so a failed send is retried.
        let geometry = geometry_of(&[(7, 400, 200)]);
        let mut panes = HashMap::new();
        assert_eq!(plan_pane_syncs(&geometry, CELL, &mut panes).len(), 1);
        assert_eq!(plan_pane_syncs(&geometry, CELL, &mut panes).len(), 1);

        panes.get_mut(&7).unwrap().last_sent_dims = (40, 10);
        assert!(plan_pane_syncs(&geometry, CELL, &mut panes).is_empty());
    }

    #[test]
    fn plan_pane_syncs_changed_dims_resize_local_grid_and_send() {
        let geometry = geometry_of(&[(7, 400, 200)]);
        let mut panes = HashMap::new();
        plan_pane_syncs(&geometry, CELL, &mut panes);
        panes.get_mut(&7).unwrap().last_sent_dims = (40, 10);

        let grown = geometry_of(&[(7, 800, 200)]);
        let resizes = plan_pane_syncs(&grown, CELL, &mut panes);
        assert_eq!(resizes.len(), 1);
        assert_eq!((resizes[0].cols, resizes[0].rows), (80, 10));
        let view = &panes[&7];
        assert_eq!((view.grid.cols, view.grid.rows), (80, 10));
        assert_eq!(view.last_sent_dims, (40, 10), "plan must not commit");
    }

    #[test]
    fn plan_pane_syncs_preexisting_view_with_matching_dims_sends_nothing() {
        let mut panes = HashMap::new();
        let mut view = PaneView::new(ClientGrid::new(40, 10));
        view.last_sent_dims = (40, 10);
        panes.insert(7, view);
        let geometry = geometry_of(&[(7, 400, 200)]);
        assert!(plan_pane_syncs(&geometry, CELL, &mut panes).is_empty());
    }

    // --- drain_wheel_notches: LineDelta ---

    #[test]
    fn line_delta_truncates_and_clears_residue() {
        let mut accum = 7.5;
        let n = drain_wheel_notches(MouseScrollDelta::LineDelta(0.0, 2.7), 16.0, &mut accum);
        assert_eq!(n, 2);
        assert_eq!(accum, 0.0, "LineDelta must clear pixel residue");
    }

    #[test]
    fn line_delta_zero_is_noop() {
        let mut accum = 5.0;
        let n = drain_wheel_notches(MouseScrollDelta::LineDelta(0.0, 0.0), 16.0, &mut accum);
        assert_eq!(n, 0);
        // LineDelta unconditionally clears even when value is zero.
        assert_eq!(accum, 0.0);
    }

    #[test]
    fn line_delta_negative_truncates_toward_zero() {
        let mut accum = 0.0;
        let n = drain_wheel_notches(MouseScrollDelta::LineDelta(0.0, -2.7), 16.0, &mut accum);
        assert_eq!(n, -2);
    }

    // --- drain_wheel_notches: PixelDelta accumulation ---

    fn px(y: f64) -> MouseScrollDelta {
        MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, y))
    }

    #[test]
    fn pixel_delta_sub_cell_accumulates_no_notch() {
        let mut accum = 0.0;
        for _ in 0..3 {
            assert_eq!(drain_wheel_notches(px(4.0), 16.0, &mut accum), 0);
        }
        // Fourth event reaches 16px → 1 notch.
        assert_eq!(drain_wheel_notches(px(4.0), 16.0, &mut accum), 1);
        assert_eq!(accum, 0.0, "residue should be drained");
    }

    #[test]
    fn pixel_delta_exact_cell_one_notch() {
        let mut accum = 0.0;
        assert_eq!(drain_wheel_notches(px(16.0), 16.0, &mut accum), 1);
        assert_eq!(accum, 0.0);
    }

    #[test]
    fn pixel_delta_multi_notch_single_event_keeps_residue() {
        let mut accum = 0.0;
        // 40px on 16px cell → 2 notches, 8px residue.
        assert_eq!(drain_wheel_notches(px(40.0), 16.0, &mut accum), 2);
        assert_eq!(accum, 8.0);
    }

    #[test]
    fn pixel_delta_negative_truncates_toward_zero() {
        let mut accum = 0.0;
        // -40px on 16px cell → -2 notches, residue -8.
        assert_eq!(drain_wheel_notches(px(-40.0), 16.0, &mut accum), -2);
        assert_eq!(accum, -8.0);
    }

    #[test]
    fn pixel_delta_direction_reversal_resets_accum() {
        let mut accum = 0.0;
        // Build up positive residue.
        let _ = drain_wheel_notches(px(8.0), 16.0, &mut accum);
        assert_eq!(accum, 8.0);
        // Reverse direction: should reset, then accumulate negative.
        let n = drain_wheel_notches(px(-4.0), 16.0, &mut accum);
        assert_eq!(n, 0);
        assert_eq!(accum, -4.0, "reset must occur before applying new delta");
    }

    #[test]
    fn pixel_delta_zero_y_is_noop() {
        let mut accum = 5.0;
        assert_eq!(drain_wheel_notches(px(0.0), 16.0, &mut accum), 0);
        assert_eq!(accum, 5.0, "zero delta must not touch accumulator");
    }

    #[test]
    fn pixel_delta_zero_cell_height_is_noop() {
        let mut accum = 5.0;
        assert_eq!(drain_wheel_notches(px(100.0), 0.0, &mut accum), 0);
        assert_eq!(accum, 5.0, "zero cell_h must not touch accumulator");
    }

    #[test]
    fn line_delta_after_pixel_delta_clears_residue() {
        let mut accum = 0.0;
        let _ = drain_wheel_notches(px(8.0), 16.0, &mut accum);
        assert_eq!(accum, 8.0);
        let n = drain_wheel_notches(MouseScrollDelta::LineDelta(0.0, 1.0), 16.0, &mut accum);
        assert_eq!(n, 1);
        assert_eq!(accum, 0.0, "LineDelta must purge pending pixel residue");
    }
}
