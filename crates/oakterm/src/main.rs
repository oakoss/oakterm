mod a11y_bridge;
mod daemon_conn;
mod frame;
mod gpu;
mod input;
mod layout;
mod layout_state;
mod palette;
mod pane_view;
mod render;
mod render_grid;
mod status_bar;
mod tab_bar;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tracing::{debug, error, info, warn};

use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId};

use oakterm_protocol::frame::Frame;
use oakterm_protocol::input::{KeyInput, MouseInput, Resize};
use oakterm_protocol::message::{
    ClosePane, CloseTab, FindPrompt, FocusPane, GetLayoutTree, GetScrollback, LIST_TABS_MIN_MINOR,
    LayoutTreeNode, MSG_CLOSE_PANE, MSG_CLOSE_TAB, MSG_DETACH, MSG_FIND_PROMPT, MSG_FOCUS_PANE,
    MSG_GET_LAYOUT_TREE, MSG_GET_RENDER_UPDATE, MSG_GET_SCROLLBACK, MSG_LIST_TABS, MSG_NEW_TAB,
    MSG_RESIZE_PANE, MSG_SPLIT_PANE, MSG_SWITCH_TAB, NewTab, PromptPosition, ResizePane,
    ScrollbackData, SearchDirection, SplitDirection as WireSplitDirection, SplitPane, SwitchTab,
    TabList,
};
use oakterm_protocol::render::{GetRenderUpdate, RenderUpdate};

use oakterm_config::StatusBarPosition;
use oakterm_terminal::grid::MAX_GRID_DIMENSION;

use a11y_bridge::{A11yEvent, A11yModel};
use daemon_conn::{DaemonWriter, connect_to_daemon};
use frame::{FontState, try_init_font};
use gpu::GpuState;
use layout_state::PaneLayout;
use pane_view::{PaneView, ScrollbackClampOutcome};
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
    /// The active workspace's tabs (`ListTabs` response).
    TabList(Box<TabList>),
    /// A `NewTab` was accepted; the tab is now active daemon-side.
    TabCreated {
        tab_id: u32,
        pane_id: u32,
    },
    /// A `CloseTab` completed; another tab is now active daemon-side.
    TabClosed,
    /// A `ClosePane` completed; focus moved daemon-side (Spec-0007
    /// nearest sibling), and the close may have cascaded to the tab.
    /// The serial identifies which pending close succeeded.
    PaneClosed {
        serial: u32,
    },
    /// The daemon answered the request at `serial` with an error frame;
    /// pending state keyed to it (or anything older) is dead.
    RequestFailed {
        serial: u32,
    },
}

/// Copyable action descriptor to break the borrow on `keybind_registry`
/// during `dispatch_action_at`. `Callback`/`LeaderCallback` store the
/// index back into their source table since `RegistryKey` is not `Clone`.
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
    LeaderCallback(usize),
    SplitPane(WireSplitDirection),
    FocusPane(layout::FocusDirection),
    NewTab,
    CloseTab,
    ClosePane,
    SwitchTab(std::num::NonZeroU32),
    NextTab,
    PreviousTab,
    ShowCommandPalette,
}

/// Copy-out result for one action: its descriptor, "consume the key"
/// (config typo), or "callback — the caller maps it to an indexed
/// descriptor for its source table".
enum DescOutcome {
    Desc(ActionDesc),
    Consume,
    Callback,
}

/// Copy an action into its dispatch descriptor (see [`ActionDesc`]).
fn desc_of_action(action: &oakterm_config::Action) -> DescOutcome {
    use oakterm_config::Action;
    let desc = match action {
        Action::ScrollUp(n) => ActionDesc::ScrollUp(*n),
        Action::ScrollDown(n) => ActionDesc::ScrollDown(*n),
        Action::ScrollToPrompt(d) => ActionDesc::ScrollToPrompt(*d),
        Action::SendString(b) => ActionDesc::SendString(b.clone()),
        Action::Copy => ActionDesc::Copy,
        Action::Paste => ActionDesc::Paste,
        Action::ToggleFullscreen => ActionDesc::ToggleFullscreen,
        Action::ReloadConfig => ActionDesc::ReloadConfig,
        Action::Callback(_) => return DescOutcome::Callback,
        // Config directions are placement-relative (oakterm.PaneDirection);
        // the wire protocol carries only the split axis, so left/right
        // and up/down collapse — the daemon always places the new pane
        // after the target (Spec-0007 Split).
        Action::SplitPane { direction, size } => {
            if (size - 0.5).abs() > f64::EPSILON {
                // SplitPane (0xA0) has no size field yet.
                warn!(size, "split_pane size not yet supported; using 0.5");
            }
            match direction.as_str() {
                "left" | "right" => ActionDesc::SplitPane(WireSplitDirection::Horizontal),
                "up" | "down" => ActionDesc::SplitPane(WireSplitDirection::Vertical),
                other => {
                    // Consume the chord: a config typo must not leak
                    // the bound key's bytes into the shell.
                    warn!(direction = other, "unknown split direction in keybind");
                    return DescOutcome::Consume;
                }
            }
        }
        Action::FocusPaneDirection(direction) => {
            use layout::FocusDirection;
            match direction.as_str() {
                "left" => ActionDesc::FocusPane(FocusDirection::Left),
                "right" => ActionDesc::FocusPane(FocusDirection::Right),
                "up" => ActionDesc::FocusPane(FocusDirection::Up),
                "down" => ActionDesc::FocusPane(FocusDirection::Down),
                other => {
                    // Consume the chord: a config typo must not leak
                    // the bound key's bytes into the shell.
                    warn!(direction = other, "unknown focus direction in keybind");
                    return DescOutcome::Consume;
                }
            }
        }
        Action::NewTab => ActionDesc::NewTab,
        Action::CloseTab => ActionDesc::CloseTab,
        Action::ClosePane => ActionDesc::ClosePane,
        Action::SwitchTab(n) => ActionDesc::SwitchTab(*n),
        Action::NextTab => ActionDesc::NextTab,
        Action::PreviousTab => ActionDesc::PreviousTab,
        Action::ShowCommandPalette => ActionDesc::ShowCommandPalette,
    };
    DescOutcome::Desc(desc)
}

/// Run a Lua keybind callback under the standard 100ms timeout hook.
fn run_keybind_callback(lua: &oakterm_config::Lua, key: &oakterm_config::mlua::RegistryKey) {
    let func = match lua.registry_value::<oakterm_config::mlua::Function>(key) {
        Ok(f) => f,
        Err(e) => {
            warn!(error = %e, "keybind callback error");
            return;
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
        return;
    }
    if let Err(e) = func.call::<()>(()) {
        warn!(error = %e, "keybind callback error");
    }
    lua.remove_hook();
}

/// A leader press awaiting its follow-up key (ADR-0011 layer 1).
struct LeaderPending {
    /// When the wait expires and the buffered bytes flush to the PTY.
    deadline: std::time::Instant,
    /// The leader chord's own PTY encoding, sent if no follow-up match.
    buffered: Option<Vec<u8>>,
}

/// Pixel slop around a 1px split border that still grabs it.
const BORDER_GRAB_PAD: f64 = 3.0;

/// An active split-border drag. The flanked pane pair is captured at
/// press time and fixed for the drag (Spec-0007 Resize adjusts one
/// sibling pair); whole-cell deltas are sent as the cursor crosses
/// cell boundaries, with the remainder carried in `last_pos`.
struct BorderDrag {
    /// Flanked pair in layout order; a positive wire delta grows `before`.
    before: u32,
    after: u32,
    /// Vertical border: the drag moves horizontally.
    vertical: bool,
    /// Cursor position on the drag axis as of the last sent delta.
    last_pos: f64,
}

/// Build a whole-window `Resize`, clamping pixel dimensions to the
/// wire's u16 range.
fn window_resize(pane_id: u32, (cols, rows): (u16, u16), size: PhysicalSize<u32>) -> Resize {
    #[allow(clippy::cast_possible_truncation)]
    Resize {
        pane_id,
        cols,
        rows,
        pixel_width: size.width.min(u32::from(u16::MAX)) as u16,
        pixel_height: size.height.min(u32::from(u16::MAX)) as u16,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingPaneClose {
    serial: u32,
    pane_id: u32,
}

/// Resolve a `ClosePaneResponse` against the in-flight queue: the
/// exact-serial match identifies the closed pane, and entries at or
/// below the serial are dropped — responses arrive in request order, so
/// older unmatched entries were rejected (their `Error` frames carry no
/// `PaneClosed`). Assumes serials don't wrap while a close is in
/// flight; `take_serial` wraps only after `u32::MAX` requests.
fn resolve_pane_close(
    queue: &mut std::collections::VecDeque<PendingPaneClose>,
    serial: u32,
) -> Option<u32> {
    let closed = queue.iter().find(|p| p.serial == serial).map(|p| p.pane_id);
    queue.retain(|p| p.serial > serial);
    closed
}

/// Focus liveness against a layout geometry: `Live` when `focused` is
/// present, `Refocus` with the first pane when it is not, `Stranded`
/// when there is no pane to fall back to.
#[derive(Debug, PartialEq, Eq)]
enum FocusHealth {
    Live,
    Refocus(u32),
    Stranded,
}

fn check_focus(geometry: Option<&layout::LayoutGeometry>, focused: u32) -> FocusHealth {
    let Some(geo) = geometry else {
        return FocusHealth::Stranded;
    };
    if geo.panes.iter().any(|p| p.pane_id == focused) {
        return FocusHealth::Live;
    }
    match geo.panes.first() {
        Some(p) => FocusHealth::Refocus(p.pane_id),
        None => FocusHealth::Stranded,
    }
}

/// When to publish the tab strip to assistive technology after adopting a
/// `TabList`. `AfterLayout` defers the sync to `apply_layout_tree` because a
/// new `LayoutTree` is inbound and its panes arrive later — syncing now would
/// publish the new selection over the old tab's panes. `Now` pushes
/// immediately for mutations that carry no new panes (rename, reorder,
/// bar-visibility change).
#[derive(Debug, PartialEq, Eq)]
enum TabSyncTiming {
    Now,
    AfterLayout,
}

/// The signals that decide tab-a11y sync timing, grouped so the three flags
/// can't be transposed at the call site (they are all `bool`).
#[derive(Clone, Copy)]
struct TabAdoption {
    /// The active tab id differs from before this `TabList`.
    active_changed: bool,
    /// An active tab exists after adoption (false only when the last tab closed).
    has_active: bool,
    /// A pane just closed on the same tab, so its shrunken tree is inbound.
    refocus: bool,
}

/// A layout tree is inbound — so defer — exactly when the active tab changed
/// to a real tab, or a post-close refocus is pending; both fetch the
/// now-active tab's tree. This is the load-bearing coupling between
/// `apply_tab_list` (which requests the tree) and `apply_layout_tree` (which
/// completes the deferred sync); keep the two in step by routing both through
/// this predicate.
fn tab_sync_timing(adoption: TabAdoption) -> TabSyncTiming {
    let TabAdoption {
        active_changed,
        has_active,
        refocus,
    } = adoption;
    let layout_inbound = if active_changed { has_active } else { refocus };
    if layout_inbound {
        TabSyncTiming::AfterLayout
    } else {
        TabSyncTiming::Now
    }
}

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
        if view.grid().cols != cols || view.grid().rows != rows {
            view.resize(cols, rows);
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
    /// Action catalog with keybind hints; rebuilt whenever
    /// `keybind_registry` is replaced.
    action_registry: oakterm_config::ActionRegistry,
    /// Command palette overlay (Spec-0009). Captures all keys while
    /// visible.
    palette: palette::PaletteState,
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
    /// Next status bar clock repaint (minute boundary). `None` until a
    /// frame with a clock re-arms it.
    clock_deadline: Option<std::time::Instant>,
    /// Active modal key table (copy mode, resize mode): while set,
    /// unmatched keys are dropped rather than forwarded to the PTY.
    /// Constructed programmatically by the mode that owns it (no Lua
    /// registration API in Phase 1); `None` in normal operation.
    active_key_table: Option<oakterm_config::KeyTable>,
    /// A leader press awaiting its follow-up key.
    leader_pending: Option<LeaderPending>,
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
    /// Split layout state: the daemon's tree, its pixel geometry, and
    /// pending split focus.
    layout: PaneLayout,
    /// The active workspace's tabs, mirrored from `TabList`.
    tabs: tab_bar::TabsState,
    /// Buttons whose press window chrome (tab bar, status bar) consumed;
    /// their release is swallowed too.
    chrome_pressed_buttons: u8,
    /// A `ClosePane` completed and the next `TabList` should adopt the
    /// daemon's post-close focus (Spec-0007 nearest sibling). Consumed
    /// by whichever `TabList` arrives next — on this serialized
    /// connection that is the post-close refresh.
    refocus_after_close: bool,
    /// In-flight `ClosePane` requests: the empty response only
    /// correlates by serial. Entries are pruned by the response serial
    /// (success or error), so rejected closes don't accumulate.
    pending_pane_closes: std::collections::VecDeque<PendingPaneClose>,
    /// The daemon's advertised protocol minor version; gates request
    /// types newer than the daemon (Spec-0001 client obligation).
    server_minor: u16,
    /// Split border currently under the cursor (drives the cursor icon).
    hovered_border: Option<usize>,
    /// Split border drag in progress; owns the left button while set.
    border_drag: Option<BorderDrag>,
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
            action_registry: oakterm_config::ActionRegistry::core(
                &oakterm_config::KeybindRegistry::new(),
            ),
            palette: palette::PaletteState::new(),
            config_error: None,
            config_watcher: None,
            initial_resize_sent: false,
            last_mouse_cell: (0, 0),
            modifiers: winit::event::Modifiers::default(),
            shift_bypassed_buttons: 0,
            blink_visible: true,
            blink_deadline: None,
            clock_deadline: None,
            active_key_table: None,
            leader_pending: None,
            focused: true,
            a11y_state: Arc::new(Mutex::new(None)),
            mouse_pressed: false,
            click_count: 0,
            last_click_time: None,
            last_click_pos: (0, 0),
            last_mouse_pixel: (0.0, 0.0),
            wheel_accum_y: 0.0,
            layout: PaneLayout::default(),
            tabs: tab_bar::TabsState::default(),
            chrome_pressed_buttons: 0,
            refocus_after_close: false,
            pending_pane_closes: std::collections::VecDeque::new(),
            server_minor: 0,
            hovered_border: None,
            border_drag: None,
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
        self.pane_viewport_offset(self.focused_pane)
    }

    /// A pane's scrollback offset; 0 (live view) when it has no view.
    fn pane_viewport_offset(&self, pane_id: u32) -> u32 {
        self.panes
            .get(&pane_id)
            .map_or(0, PaneView::viewport_offset)
    }

    /// Request scrollback rows from the daemon for the pane's viewport offset.
    fn request_scrollback(&self, pane_id: u32) {
        if let (Some(daemon), Some(view)) = (&self.daemon, self.panes.get(&pane_id)) {
            let req = GetScrollback {
                pane_id,
                start_row: -i64::from(view.viewport_offset()),
                count: u32::from(view.grid().rows),
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

    /// Scroll a pane's viewport by `lines`. Positive = up (into
    /// scrollback), negative = down (toward live). Handles enter/exit
    /// scrollback.
    fn scroll_viewport(&mut self, pane_id: u32, lines: i32) {
        if lines > 0 {
            if let Some(view) = self.panes.get_mut(&pane_id) {
                #[allow(clippy::cast_sign_loss)]
                view.scroll_up(lines as u32);
            }
            self.request_scrollback(pane_id);
        } else if lines < 0 && self.pane_viewport_offset(pane_id) > 0 {
            let Some(view) = self.panes.get_mut(&pane_id) else {
                return;
            };
            if view.scroll_down(lines.unsigned_abs()) {
                self.return_to_live(pane_id);
            } else {
                self.request_scrollback(pane_id);
            }
        }
    }

    /// Return a pane to live view from scrollback.
    fn return_to_live(&mut self, pane_id: u32) {
        if let Some(view) = self.panes.get_mut(&pane_id) {
            view.return_to_live();
        }
        // Request a full refresh to ensure live view is current.
        if let Some(daemon) = &self.daemon {
            let req = GetRenderUpdate {
                pane_id,
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
            .map_or(8.0, |f| f64::from(f.metrics().cell_width));
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
                    if row < view.grid().rows {
                        let text: Vec<char> = view.grid().row_text(row).chars().collect();
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

    /// The tab strip as published to the a11y tree. `bar_visible` is the
    /// single owner of "is the tab bar shown" — the a11y tree mirrors the
    /// rendered bar, so this is empty when the bar is down.
    fn a11y_tab_strip(&self) -> Vec<a11y_bridge::TabStripEntry<'_>> {
        if !self.tabs.bar_visible() {
            return Vec::new();
        }
        let active = self.tabs.active_tab();
        self.tabs
            .tabs()
            .iter()
            .map(|t| a11y_bridge::TabStripEntry {
                tab_id: t.tab_id,
                name: t.name.as_str(),
                active: Some(t.tab_id) == active,
            })
            .collect()
    }

    /// Notify AT of the current tab strip when it changed. Call after every
    /// `TabList` adoption; a rename, reorder, or active-tab change all
    /// surface through here.
    fn sync_tabs_a11y(&mut self) {
        let tabs = self.a11y_tab_strip();
        let update = a11y_bridge::sync_tabs(&self.a11y_state, &tabs);
        if let (Some(update), Some(adapter)) = (update, &mut self.accesskit) {
            adapter.update_if_active(|| update);
        }
    }

    /// Reconcile panes and tabs into one a11y tree update. Used at layout
    /// adoption so a tab switch's new panes and new selection publish
    /// together — AT never sees the new panes under the old selection.
    /// Falls back to a tab-only sync when geometry is absent.
    fn sync_layout_and_tabs_a11y(&mut self) {
        let Some(origins) = self.a11y_origins() else {
            self.sync_tabs_a11y();
            return;
        };
        let tabs = self.a11y_tab_strip();
        let update =
            a11y_bridge::sync_layout_and_tabs(&self.a11y_state, &self.panes, &origins, &tabs);
        if let (Some(update), Some(adapter)) = (update, &mut self.accesskit) {
            adapter.update_if_active(|| update);
        }
    }

    /// Pane pixel origins for the current geometry, or `None` when no
    /// geometry is available (row bounds derive from origins).
    fn a11y_origins(&self) -> Option<Vec<(u32, (f64, f64))>> {
        self.layout.geometry().map(|geo| {
            geo.panes
                .iter()
                .map(|p| (p.pane_id, (f64::from(p.rect.x), f64::from(p.rect.y))))
                .collect()
        })
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
        if !view.grid().cursor_visible || view.is_scrolled() {
            return false;
        }
        // Blinking styles: 0=BlinkingBlock, 2=BlinkingUnderline, 4=BlinkingBar
        matches!(view.grid().cursor_style, 0 | 2 | 4)
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

        let gpu = match pollster::block_on(gpu::init_gpu(window.clone(), blending_mode)) {
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
        // No tab bar at startup (single tab); the status bar reserves its
        // row from the first frame.
        let status_px =
            status_bar::status_bar_height(config.status_bar, Some(font_state.metrics()));
        let (top_px, bottom_px) = chrome_split(0, status_px, config.status_bar_position);
        let (cols, rows) = window_to_grid_dims(
            size,
            font_state.metrics(),
            &config.padding,
            top_px,
            bottom_px,
        );
        let grid = ClientGrid::new(cols.max(1), rows.max(1));

        match connect_to_daemon(&self.proxy) {
            Ok(conn) => {
                self.daemon = Some(conn.writer);
                self.daemon_process = conn.child;
                self.server_minor = conn.server_minor;
                if self.server_minor < LIST_TABS_MIN_MINOR {
                    info!(
                        server_minor = self.server_minor,
                        "daemon predates ListTabs; the tab bar and tab keybinds are inactive"
                    );
                }
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
                    a11y_bridge::cell_dims(Some(font_state.metrics())),
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
        self.action_registry = oakterm_config::ActionRegistry::core(&self.keybind_registry);
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
        // Seed tab state; matters when the daemon already has tabs.
        self.request_list_tabs();
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
                if size.width == 0 || size.height == 0 {
                    return;
                }
                let Some(gpu) = &mut self.gpu else { return };
                let pixel_dims_changed =
                    gpu.config.width != size.width || gpu.config.height != size.height;
                if pixel_dims_changed {
                    gpu.config.width = size.width;
                    gpu.config.height = size.height;
                    gpu.surface.configure(&gpu.device, &gpu.config);
                }
                self.relayout_panes();
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
                    // A mid-drag focus steal (Cmd+Tab, modal) sends the
                    // release to the other app; a surviving drag would resize
                    // with no button held and eat the next click's release.
                    if self.border_drag.take().is_some() {
                        let (x, y) = self.last_mouse_pixel;
                        self.update_border_hover(x, y);
                    }
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    event @ winit::event::KeyEvent {
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                // An open palette captures every key ahead of keybind
                // dispatch and PTY forwarding (Spec-0009 Palette
                // Lifecycle).
                if self.palette.is_visible() {
                    self.handle_palette_key(&event);
                    self.reset_blink();
                    return;
                }

                // Resolve through the dispatch layers BEFORE clearing
                // selection so Copy can read it. Chords resolve against
                // the layout key, not the platform-composed character —
                // macOS Option+H arrives as "˙" in logical_key, which can
                // never match an alt+h binding (Spec-0011 Keybind Lookup
                // Layer). On a logical miss, each layer retries against
                // the physical key so position-based binds (oak_mod+[1-9])
                // fire on layouts where the digit's base character
                // differs (AZERTY; TREK-268).
                let chord_key = event.key_without_modifiers();
                let logical = input::winit_to_chord(self.modifiers, &chord_key);
                let physical = input::physical_to_chord(self.modifiers, event.physical_key);
                let ctx = input::DispatchContext {
                    registry: &self.keybind_registry,
                    table: self.active_key_table.as_ref(),
                    leader: self.config.leader.as_ref(),
                    leader_pending: self.leader_pending.is_some(),
                };
                match input::resolve_key(&ctx, logical.as_ref(), physical.as_ref()) {
                    input::KeyDispatch::LeaderArm(timeout_ms) => {
                        if self.active_key_table.is_some() {
                            tracing::debug!("leader chord armed while a modal key table is active");
                        }
                        let buffered =
                            input::key_to_bytes(&event.logical_key, event.text.as_deref());
                        self.leader_pending = Some(LeaderPending {
                            deadline: std::time::Instant::now()
                                + std::time::Duration::from_millis(timeout_ms),
                            buffered,
                        });
                        self.reset_blink();
                        return;
                    }
                    input::KeyDispatch::LeaderAction(idx) => {
                        self.leader_pending = None;
                        self.dispatch_leader_action_at(idx);
                        self.reset_blink();
                        return;
                    }
                    input::KeyDispatch::LeaderMiss => {
                        // Both the leader key and this key go to the
                        // application (ADR-0011); flush the buffer and
                        // fall through to normal forwarding below.
                        if let Some(pending) = self.leader_pending.take() {
                            if let Some(bytes) = pending.buffered {
                                self.send_key_bytes(bytes, event_loop);
                            }
                        }
                    }
                    input::KeyDispatch::TableAction(idx) => {
                        self.dispatch_table_action_at(idx);
                        self.reset_blink();
                        return;
                    }
                    input::KeyDispatch::TableDrop => {
                        self.reset_blink();
                        return;
                    }
                    input::KeyDispatch::Binding(idx) => {
                        if self.dispatch_action_at(idx) {
                            self.reset_blink();
                            return;
                        }
                        // Action returned false (e.g., scroll down when
                        // not scrolled, or not performable) — let the
                        // key fall through to PTY.
                    }
                    input::KeyDispatch::Forward => {}
                }
                let (logical_key, text) = (event.logical_key, event.text);

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
                    self.return_to_live(self.focused_pane);
                }

                if let Some(bytes) = input::key_to_bytes(&logical_key, text.as_deref()) {
                    self.send_key_bytes(bytes, event_loop);
                }
                self.reset_blink();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse_pixel = (position.x, position.y);
                if self.border_drag.is_some() {
                    self.drag_border(position.x, position.y);
                    return;
                }
                self.update_border_hover(position.x, position.y);
                let (top_px, _) = self.chrome_px();
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    clippy::cast_precision_loss // padding values are small
                )]
                if let Some(font) = &self.font {
                    // Subtract padding and top chrome so clicks in the
                    // gutter map to cell 0.
                    let px = (position.x as f32 - self.config.padding.left as f32).max(0.0);
                    let py = (position.y as f32 - self.config.padding.top as f32 - top_px as f32)
                        .max(0.0);
                    let col = (px / font.metrics().cell_width) as u16;
                    let row = (py / font.metrics().cell_height) as u16;
                    self.last_mouse_cell = (col, row);

                    // Update selection end during drag.
                    if self.mouse_pressed {
                        use oakterm_terminal::grid::selection::{
                            AnchorSide, SelectionType, word_boundaries,
                        };
                        let cw = f64::from(font.metrics().cell_width);
                        let adj_x = (position.x - f64::from(self.config.padding.left)).max(0.0);
                        let side = if (adj_x % cw) > (cw / 2.0) {
                            AnchorSide::Right
                        } else {
                            AnchorSide::Left
                        };
                        if let Some(view) = self.panes.get_mut(&self.focused_pane) {
                            let sel_row = i64::from(row) - i64::from(view.viewport_offset());
                            let grid_rows = view.grid().rows;
                            let semantic = view
                                .selection
                                .as_ref()
                                .is_some_and(|s| s.ty == SelectionType::Semantic);
                            let text: Vec<char> = if semantic && row < grid_rows {
                                view.grid().row_text(row).chars().collect()
                            } else {
                                Vec::new()
                            };
                            if let Some(sel) = &mut view.selection {
                                if sel.ty == SelectionType::Semantic {
                                    // Snap drag to word boundaries.
                                    if row < grid_rows {
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
                // Chrome (tab bar, status bar) owns presses inside it: a
                // left press in the tab strip switches tabs, everything
                // else is swallowed rather than reaching the PTY, and the
                // press's own release is swallowed too. Releases of
                // pane-started presses fall through so selection and
                // shift-bypass state still clear. An active border drag
                // keeps the button (its release ends the drag).
                let bar_bit = chrome_button_bit(button);
                if self.border_drag.is_none()
                    && state == ElementState::Pressed
                    && self.in_chrome_row(self.last_mouse_pixel.1)
                {
                    if button == winit::event::MouseButton::Left
                        && self.last_mouse_pixel.1 < f64::from(self.tab_bar_px())
                    {
                        if let Some(tab_id) = self.tab_at_pixel(self.last_mouse_pixel.0) {
                            self.switch_tab(tab_id);
                        }
                    }
                    self.chrome_pressed_buttons |= bar_bit;
                    return;
                }
                if state == ElementState::Released && self.chrome_pressed_buttons & bar_bit != 0 {
                    self.chrome_pressed_buttons &= !bar_bit;
                    return;
                }
                // A split border under the cursor owns the left button:
                // the press starts a drag instead of a selection or a
                // PTY mouse event, and the matching release ends it.
                if button == winit::event::MouseButton::Left {
                    match state {
                        ElementState::Pressed if self.border_drag.is_none() => {
                            if let Some(drag) = self.begin_border_drag() {
                                self.border_drag = Some(drag);
                                return;
                            }
                        }
                        ElementState::Released if self.border_drag.is_some() => {
                            self.border_drag = None;
                            let (x, y) = self.last_mouse_pixel;
                            self.update_border_hover(x, y);
                            return;
                        }
                        _ => {}
                    }
                }
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
                                modifiers: input::encode_mouse_modifiers(self.modifiers),
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
                    .map_or(16.0_f64, |f| f64::from(f.metrics().cell_height));
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
                let alt_screen = self.focused_view().is_some_and(|v| v.grid().alt_screen);

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
                    self.scroll_viewport(self.focused_pane, delta);
                } else if let Some(daemon) = &mut self.daemon {
                    let (x, y) = self.last_mouse_cell;
                    let event_type = if scroll_up { 3u8 } else { 4u8 };
                    let mods = input::encode_mouse_modifiers(self.modifiers);
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
            WindowEvent::RedrawRequested => self.redraw(),
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
                view.apply_update(&update);
                if !view.is_scrolled() {
                    let dirty_rows: Vec<(u16, String)> = update
                        .dirty_rows
                        .iter()
                        .map(|r| (r.row_index, view.grid().row_text(r.row_index)))
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
                    if self
                        .layout
                        .pane_is_visible(update.pane_id, self.focused_pane)
                    {
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
                // Route by the response's pane: AT can scroll a
                // background pane, so the reply must not land on the
                // focused one.
                let pane_id = data.pane_id;
                if self.pane_viewport_offset(pane_id) == 0 {
                    debug!(pane_id, "dropping stale scrollback response");
                    return;
                }
                {
                    let clamp = self
                        .panes
                        .get_mut(&pane_id)
                        .map(|view| view.clamp_scrollback(data.total_rows));
                    if clamp == Some(ScrollbackClampOutcome::ReturnToLive) {
                        self.return_to_live(pane_id);
                        return;
                    }
                    let mut a11y_scrollback_update: Option<accesskit::TreeUpdate> = None;
                    let scroll_indicator = self.config.scroll_indicator;
                    if let Some(view) = self.panes.get_mut(&pane_id) {
                        view.apply_scrollback(&data.rows, scroll_indicator);
                        a11y_scrollback_update = a11y_bridge::apply(
                            &self.a11y_state,
                            pane_id,
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
                        self.return_to_live(self.focused_pane);
                    } else {
                        if let Some(view) = self.focused_view_mut() {
                            view.set_scroll_offset(new_offset);
                        }
                        self.request_scrollback(self.focused_pane);
                    }
                }
            }
            UserEvent::TitleChanged(pane_id, title) => {
                if let Some(view) = self.panes.get_mut(&pane_id) {
                    view.title.clone_from(&title);
                }
                if pane_id == self.focused_pane {
                    if let Some(w) = &self.window {
                        let display = if title.is_empty() { "oakterm" } else { &title };
                        w.set_title(display);
                        // The status bar shows the focused pane's title.
                        w.request_redraw();
                    }
                }
                // Unnamed tab labels mirror pane titles; re-ask the daemon
                // (its naming rule is authoritative) while the bar shows.
                if self.tabs.bar_visible() {
                    self.request_list_tabs();
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
                // AT actions name a target node; route to its pane
                // (Spec-0006 advertises the actions per pane terminal).
                // The window and announcement nodes decode to None and
                // fall back to the focused pane.
                let target_pane = oakterm_a11y::decode_node_id(request.target_node)
                    .map_or(self.focused_pane, |(pane_id, _)| pane_id);
                match request.action {
                    accesskit::Action::Focus => {
                        if let Some(w) = &self.window {
                            w.focus_window();
                        }
                        if target_pane == self.focused_pane {
                        } else if self.panes.contains_key(&target_pane) {
                            self.focus_pane(target_pane);
                        } else {
                            debug!(target_pane, "a11y: focus action for untracked pane");
                        }
                    }
                    accesskit::Action::Click => {
                        // Only tab nodes advertise Click. Resolve against the
                        // published a11y snapshot (what AT sees), not the live
                        // strip, so a mid-switch click lands on the presented
                        // tab. A miss (non-tab or stale node) logs and is
                        // ignored, like the sibling stale-node arms.
                        if let Some(tab_id) =
                            a11y_bridge::resolve_tab_click(&self.a11y_state, request.target_node)
                        {
                            self.switch_tab(tab_id);
                        } else {
                            debug!(
                                node = ?request.target_node,
                                "a11y: click target did not resolve to a tab"
                            );
                        }
                    }
                    accesskit::Action::ScrollUp | accesskit::Action::ScrollDown => {
                        let Some(view) = self.panes.get(&target_pane) else {
                            // Stale node after a pane closed; diagnosable
                            // like the SetTextSelection rejections below.
                            debug!(target_pane, "a11y: scroll action for untracked pane");
                            return;
                        };
                        let mut page = i32::from(view.grid().rows);
                        if request.action == accesskit::Action::ScrollDown {
                            page = -page;
                        }
                        self.scroll_viewport(target_pane, page);
                    }
                    accesskit::Action::SetScrollOffset => {
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        if let Some(accesskit::ActionData::SetScrollOffset(point)) = request.data {
                            let target = point.y.max(0.0) as u32;
                            if !self.panes.contains_key(&target_pane) {
                                debug!(target_pane, "a11y: scroll offset for untracked pane");
                            } else if target == 0 {
                                self.return_to_live(target_pane);
                            } else {
                                if let Some(view) = self.panes.get_mut(&target_pane) {
                                    view.set_scroll_offset(target);
                                }
                                self.request_scrollback(target_pane);
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
                            .map_or((0, 0), |v| (v.viewport_offset(), v.grid().rows));
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
                self.layout.set_pending_focus(new_pane_id);
                self.request_layout_tree();
            }
            UserEvent::LayoutTree(tree) => {
                self.apply_layout_tree(*tree);
            }
            UserEvent::TabList(list) => {
                self.apply_tab_list(*list);
            }
            UserEvent::TabCreated { tab_id, pane_id } => {
                info!(tab_id, pane_id, "tab created; refreshing tab state");
                // apply_tab_list re-sets this on the TabList path; on the
                // pre-1.2 fallback (no TabList ever arrives) it is the
                // only focus driver for the incoming tree.
                self.layout.set_pending_focus(pane_id);
                self.request_list_tabs();
            }
            UserEvent::TabClosed => {
                // Another tab is active now; which one is the daemon's
                // call, so refresh and let apply_tab_list follow it.
                self.request_list_tabs();
            }
            UserEvent::PaneClosed { serial } => {
                if let Some(pane_id) = resolve_pane_close(&mut self.pending_pane_closes, serial) {
                    info!(pane_id, "pane close accepted; refreshing tab state");
                    // The a11y node prunes with the follow-up layout
                    // sync (sync_layout retains only in-layout panes).
                    self.panes.remove(&pane_id);
                } else {
                    warn!(serial, "ClosePaneResponse with no pending close");
                }
                if self.server_minor >= LIST_TABS_MIN_MINOR {
                    // The daemon's post-close focus arrives with the
                    // TabList; apply_tab_list adopts it and fetches the
                    // tree.
                    self.refocus_after_close = true;
                    self.request_list_tabs();
                } else {
                    // Pre-1.2: no TabList will come; fetch the tree and
                    // rely on apply_layout_tree's stale-focus fallback.
                    self.request_layout_tree();
                }
            }
            UserEvent::RequestFailed { serial } => {
                // Responses arrive in request order: a failed serial
                // means every pending close at or below it was answered
                // (this error or an earlier response).
                self.pending_pane_closes.retain(|p| p.serial > serial);
            }
            UserEvent::ConfigReloaded(cr) => {
                self.handle_config_reload(*cr);
            }
        }
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        // A timer fired: act only on reached deadlines, so the clock
        // waking early never toggles the blink phase (and vice versa).
        if matches!(cause, winit::event::StartCause::ResumeTimeReached { .. }) {
            let now = std::time::Instant::now();
            if self
                .leader_pending
                .as_ref()
                .is_some_and(|p| now >= p.deadline)
            {
                // The follow-up window closed: the leader key was a
                // plain keypress after all (ADR-0011).
                if let Some(pending) = self.leader_pending.take() {
                    if let Some(bytes) = pending.buffered {
                        self.send_key_bytes(bytes, event_loop);
                    }
                }
            }
            if self.blink_deadline.is_some_and(|d| now >= d) {
                if self.should_blink() {
                    self.blink_visible = !self.blink_visible;
                    self.blink_deadline = Some(now + std::time::Duration::from_millis(530));
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                } else {
                    // Conditions changed; stop blinking.
                    self.blink_visible = true;
                    self.blink_deadline = None;
                }
            }
            if self.clock_deadline.is_some_and(|d| now >= d) {
                // The redraw re-arms the deadline at the next boundary.
                self.clock_deadline = None;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let deadlines = [
            self.blink_deadline,
            self.clock_deadline,
            self.leader_pending.as_ref().map(|p| p.deadline),
        ];
        if let Some(deadline) = next_wakeup(deadlines) {
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
    ///
    /// Returns `true` if the action was handled (key consumed), `false` if the
    /// key should fall through to PTY forwarding.
    fn dispatch_action_at(&mut self, index: usize) -> bool {
        let Some(action) = self.keybind_registry.get(index) else {
            return false;
        };
        // Performable gate (ADR-0011): an action that cannot run in the
        // current context releases the key to the PTY instead of
        // consuming it (Ghostty's `performable:` semantics, per-action).
        if let Some(id) = oakterm_config::action_id_of(action) {
            if !id.is_performable(self.action_context()) {
                tracing::debug!(action = ?id, "keybind not performable here; key released to PTY");
                return false;
            }
        }
        let action_desc = match desc_of_action(action) {
            DescOutcome::Desc(d) => d,
            DescOutcome::Consume => return true,
            DescOutcome::Callback => ActionDesc::Callback(index),
        };
        self.execute_action_desc(action_desc)
    }

    /// Send raw key bytes to the focused pane's PTY, exiting the event
    /// loop when the daemon connection drops.
    fn send_key_bytes(&mut self, bytes: Vec<u8>, event_loop: &ActiveEventLoop) {
        if !self.try_send_key_bytes(bytes) {
            event_loop.exit();
        }
    }

    /// Send raw key bytes to the focused pane's PTY. Returns `false`
    /// when the daemon write failed (connection dropped).
    fn try_send_key_bytes(&mut self, bytes: Vec<u8>) -> bool {
        let Some(daemon) = &mut self.daemon else {
            return true;
        };
        let msg = KeyInput {
            pane_id: self.focused_pane,
            key_data: bytes,
        };
        match msg.to_frame() {
            Ok(frame) => {
                if let Err(e) = daemon.send_frame(&frame) {
                    error!(error = %e, "daemon write failed");
                    self.daemon = None;
                    return false;
                }
            }
            Err(e) => error!(error = %e, "failed to encode key input"),
        }
        true
    }

    /// Dispatch a matched `leader+X` binding. The leader chord was
    /// already swallowed, so the key is consumed regardless of outcome;
    /// an unperformable action is a silent no-op.
    fn dispatch_leader_action_at(&mut self, index: usize) {
        let Some(action) = self.keybind_registry.get_leader(index) else {
            warn!(
                index,
                "leader dispatch index unresolved; registry changed between lookup and dispatch"
            );
            return;
        };
        if let Some(id) = oakterm_config::action_id_of(action) {
            if !id.is_performable(self.action_context()) {
                tracing::debug!(action = ?id, "leader action not performable here; consumed");
                return;
            }
        }
        let action_desc = match desc_of_action(action) {
            DescOutcome::Desc(d) => d,
            DescOutcome::Consume => return,
            DescOutcome::Callback => ActionDesc::LeaderCallback(index),
        };
        let _ = self.execute_action_desc(action_desc);
    }

    /// Dispatch a matched key-table binding. Tables are modal: the key
    /// is consumed regardless of outcome.
    fn dispatch_table_action_at(&mut self, index: usize) {
        let Some(action) = self.active_key_table.as_ref().and_then(|t| t.get(index)) else {
            warn!(
                index,
                "key table dispatch index unresolved; table changed between lookup and dispatch"
            );
            return;
        };
        let action_desc = match desc_of_action(action) {
            DescOutcome::Desc(d) => d,
            DescOutcome::Consume => return,
            DescOutcome::Callback => {
                // Phase 1 tables are built-in presets with no Lua
                // registration API; a callback here is unreachable
                // until that API exists.
                warn!("key table callbacks are not supported");
                return;
            }
        };
        let _ = self.execute_action_desc(action_desc);
    }

    /// Execute a resolved action descriptor. Returns `true` if handled (key
    /// consumed), `false` to fall through to PTY forwarding.
    #[allow(clippy::too_many_lines)]
    fn execute_action_desc(&mut self, action_desc: ActionDesc) -> bool {
        match action_desc {
            ActionDesc::ScrollUp(lines) => {
                if let Some(view) = self.focused_view_mut() {
                    let amount = if lines == 0 {
                        u32::from(view.grid().rows)
                    } else {
                        lines
                    };
                    view.scroll_up(amount);
                }
                self.request_scrollback(self.focused_pane);
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
                    u32::from(view.grid().rows)
                } else {
                    lines
                };
                if view.scroll_down(amount) {
                    self.return_to_live(self.focused_pane);
                } else {
                    self.request_scrollback(self.focused_pane);
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
                        view.freeze_live();
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
            // Callback indexes the main bindings table and LeaderCallback
            // the leader table; the get()/get_leader() pairing below must
            // stay matched to the descriptor's source.
            ActionDesc::Callback(idx) => {
                let (Some(lua), Some(oakterm_config::Action::Callback(key))) =
                    (&self.lua_vm, self.keybind_registry.get(idx))
                else {
                    warn!("keybind callback skipped: no Lua VM or action mismatch");
                    return true;
                };
                run_keybind_callback(lua, key);
                true
            }
            ActionDesc::LeaderCallback(idx) => {
                let (Some(lua), Some(oakterm_config::Action::Callback(key))) =
                    (&self.lua_vm, self.keybind_registry.get_leader(idx))
                else {
                    warn!("keybind callback skipped: no Lua VM or action mismatch");
                    return true;
                };
                run_keybind_callback(lua, key);
                true
            }
            ActionDesc::Copy => {
                if let Some((sel, view)) = self
                    .focused_view()
                    .and_then(|v| v.selection.as_ref().map(|sel| (sel, v)))
                {
                    let text = view
                        .grid()
                        .extract_selection_text(sel, view.viewport_offset());
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
                match msg.encode() {
                    Ok(payload) => {
                        self.send_request(MSG_SPLIT_PANE, payload, "SplitPane");
                    }
                    Err(e) => error!(error = %e, "failed to encode SplitPane"),
                }
                true
            }
            ActionDesc::FocusPane(direction) => {
                // Consumed even when nothing changes (single pane, screen
                // edge): a focus chord must never leak bytes to the PTY.
                let target = match self.layout.active_geometry() {
                    Some(geo) if geo.panes.iter().any(|p| p.pane_id == self.focused_pane) => {
                        layout::focus_target(geo, self.focused_pane, direction)
                    }
                    Some(_) => {
                        // Would render focus navigation silently dead;
                        // distinguish it from the intentional edge no-op.
                        warn!(
                            focused_pane = self.focused_pane,
                            "focused pane missing from split geometry"
                        );
                        None
                    }
                    None => None,
                };
                if let Some(pane_id) = target {
                    self.focus_pane(pane_id);
                }
                true
            }
            ActionDesc::NewTab => {
                // workspace_id targets nothing yet: the daemon routes new
                // tabs to the active workspace (Spec-0001 NewTab).
                let msg = NewTab {
                    workspace_id: 0,
                    command: String::new(),
                    cwd: String::new(),
                };
                match msg.encode() {
                    Ok(payload) => {
                        self.send_request(MSG_NEW_TAB, payload, "NewTab");
                    }
                    Err(e) => error!(error = %e, "failed to encode NewTab"),
                }
                true
            }
            ActionDesc::CloseTab => {
                // Before the first TabList the only tab is the seeded 0;
                // closing the last tab is the daemon's refusal to make.
                let tab_id = self.tabs.active_tab().unwrap_or(0);
                self.send_request(MSG_CLOSE_TAB, CloseTab { tab_id }.encode(), "CloseTab");
                true
            }
            ActionDesc::ClosePane => {
                // The daemon refuses to close the last pane; its
                // LayoutRejected reply rings the bell.
                let pane_id = self.focused_pane;
                if let Some(serial) = self.send_request_serial(
                    MSG_CLOSE_PANE,
                    ClosePane { pane_id }.encode(),
                    "ClosePane",
                ) {
                    self.pending_pane_closes
                        .push_back(PendingPaneClose { serial, pane_id });
                }
                true
            }
            ActionDesc::SwitchTab(n) => {
                if let Some(tab_id) = self.tabs.tab_at_index(n) {
                    self.switch_tab(tab_id);
                } else {
                    // Match the tab-bar click convention: a chord for an
                    // absent index rings rather than appearing dead. Tab
                    // state is empty against pre-1.2 daemons, so these
                    // binds ring there too.
                    let _ = self.proxy.send_event(UserEvent::Bell);
                }
                true
            }
            ActionDesc::NextTab => {
                if let Some(tab_id) = self.tabs.next_tab_id() {
                    self.switch_tab(tab_id);
                }
                true
            }
            ActionDesc::PreviousTab => {
                if let Some(tab_id) = self.tabs.previous_tab_id() {
                    self.switch_tab(tab_id);
                }
                true
            }
            ActionDesc::ShowCommandPalette => {
                self.palette
                    .open(&self.action_registry, self.action_context());
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                true
            }
        }
    }

    /// Snapshot the GUI state that action performability depends on.
    fn action_context(&self) -> oakterm_config::ActionContext {
        use layout::FocusDirection;
        let geo = self.layout.active_geometry();
        let can = |dir| {
            geo.and_then(|g| layout::focus_target(g, self.focused_pane, dir))
                .is_some()
        };
        oakterm_config::ActionContext {
            pane_count: geo.map_or(1, |g| g.panes.len()),
            // Before the first TabList only the seeded tab exists.
            tab_count: self.tabs.tabs().len().max(1),
            can_focus_left: can(FocusDirection::Left),
            can_focus_right: can(FocusDirection::Right),
            can_focus_up: can(FocusDirection::Up),
            can_focus_down: can(FocusDirection::Down),
        }
    }

    fn action_desc_of_id(id: oakterm_config::ActionId) -> ActionDesc {
        use layout::FocusDirection;
        use oakterm_config::ActionId;
        match id {
            ActionId::SplitPaneRight => ActionDesc::SplitPane(WireSplitDirection::Horizontal),
            ActionId::SplitPaneDown => ActionDesc::SplitPane(WireSplitDirection::Vertical),
            ActionId::ClosePane => ActionDesc::ClosePane,
            ActionId::FocusPaneLeft => ActionDesc::FocusPane(FocusDirection::Left),
            ActionId::FocusPaneRight => ActionDesc::FocusPane(FocusDirection::Right),
            ActionId::FocusPaneUp => ActionDesc::FocusPane(FocusDirection::Up),
            ActionId::FocusPaneDown => ActionDesc::FocusPane(FocusDirection::Down),
            ActionId::NewTab => ActionDesc::NewTab,
            ActionId::CloseTab => ActionDesc::CloseTab,
            ActionId::NextTab => ActionDesc::NextTab,
            ActionId::PreviousTab => ActionDesc::PreviousTab,
            ActionId::ToggleFullscreen => ActionDesc::ToggleFullscreen,
            ActionId::ShowCommandPalette => ActionDesc::ShowCommandPalette,
            ActionId::ReloadConfig => ActionDesc::ReloadConfig,
        }
    }

    /// All keys are consumed while the palette is visible; nothing reaches
    /// keybinds or the PTY.
    fn handle_palette_key(&mut self, event: &winit::event::KeyEvent) {
        use input::PaletteKeyEffect;

        let ctx = self.action_context();
        let effect = input::palette_key_effect(
            &event.logical_key,
            self.modifiers.state(),
            event.text.as_deref(),
        );
        match effect {
            PaletteKeyEffect::Close => self.palette.close(),
            PaletteKeyEffect::Confirm => {
                if let Some(kind) = self.palette.confirm() {
                    match kind {
                        palette::PaletteResultKind::Action(id) => {
                            self.execute_action_desc(Self::action_desc_of_id(id));
                        }
                        // No providers for these yet (Spec-0009 scopes for
                        // workspaces, layouts, settings).
                        palette::PaletteResultKind::Workspace(_)
                        | palette::PaletteResultKind::Layout(_)
                        | palette::PaletteResultKind::Setting(_) => {}
                    }
                }
            }
            PaletteKeyEffect::MoveUp => self.palette.move_up(),
            PaletteKeyEffect::MoveDown => self.palette.move_down(),
            PaletteKeyEffect::Backspace => {
                self.palette.backspace(&self.action_registry, ctx);
            }
            PaletteKeyEffect::Input(text) => {
                for c in text.chars() {
                    self.palette.input_char(c, &self.action_registry, ctx);
                }
            }
            PaletteKeyEffect::Ignore => {}
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Move focus to `pane_id`, keeping the daemon's Spec-0007 focus
    /// state (session persistence saves it) and assistive technology's
    /// focus in step.
    fn focus_pane(&mut self, pane_id: u32) {
        self.focused_pane = pane_id;
        if let (Some(update), Some(adapter)) = (
            a11y_bridge::set_focus(&self.a11y_state, pane_id),
            &mut self.accesskit,
        ) {
            adapter.update_if_active(|| update);
        }
        self.send_request(MSG_FOCUS_PANE, FocusPane { pane_id }.encode(), "FocusPane");
        self.reset_blink();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Reconcile the a11y tree with the visible split layout: new panes
    /// join as terminal subtrees at their pixel origins, departed panes
    /// leave. Call after any layout adoption or geometry recompute.
    /// Keyed on the unfiltered geometry so a collapse to a single-leaf
    /// tree still prunes departed panes from the AT tree.
    fn sync_a11y_layout(&mut self) {
        let Some(origins) = self.a11y_origins() else {
            return;
        };
        let update = a11y_bridge::sync_layout(&self.a11y_state, &self.panes, &origins);
        if let (Some(update), Some(adapter)) = (update, &mut self.accesskit) {
            adapter.update_if_active(|| update);
        }
    }

    /// Start a border drag when the cursor is on a split border,
    /// capturing the flanked pane pair at the cursor's cross-axis
    /// position (Spec-0007 Resize adjusts exactly that pair).
    fn begin_border_drag(&self) -> Option<BorderDrag> {
        let (x, y) = self.last_mouse_pixel;
        let geo = self.layout.active_geometry()?;
        let border = layout::border_at(geo, x, y, BORDER_GRAB_PAD)?;
        let vertical = geo.borders.get(border)?.is_vertical_border();
        let (axis_pos, cross) = if vertical { (x, y) } else { (y, x) };
        let Some(pair) = layout::border_panes(geo, border, cross) else {
            // Distinguishes a declined resize from a normal click when
            // the hover icon promised a drag (degenerate geometry).
            debug!(border, "border press without a flanking pane pair");
            return None;
        };
        Some(BorderDrag {
            before: pair.before,
            after: pair.after,
            vertical,
            last_pos: axis_pos,
        })
    }

    /// Advance an active border drag: send one `ResizePane` per whole
    /// cell the cursor crossed (positive grows the before-pane, matching
    /// the wire's grow-`pane_id` sign), then refetch the layout tree so
    /// the geometry tracks the drag live.
    fn drag_border(&mut self, x: f64, y: f64) {
        let Some(metrics) = self.font.as_ref().map(|f| *f.metrics()) else {
            return;
        };
        let msg = {
            let Some(drag) = self.border_drag.as_mut() else {
                return;
            };
            let (pos, cell) = if drag.vertical {
                (x, f64::from(metrics.cell_width))
            } else {
                (y, f64::from(metrics.cell_height))
            };
            if cell <= 0.0 {
                return;
            }
            let cells = ((pos - drag.last_pos) / cell).trunc();
            if cells == 0.0 {
                return;
            }
            drag.last_pos += cells * cell;
            #[allow(clippy::cast_possible_truncation)]
            let delta = cells.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
            ResizePane {
                pane_id: drag.before,
                neighbor_pane_id: drag.after,
                delta,
            }
        };
        // ResizePane is a push (serial 0) per Spec-0001.
        match Frame::new(MSG_RESIZE_PANE, 0, msg.encode()) {
            Ok(frame) => {
                if self.send_or_disconnect(&frame, "ResizePane") {
                    self.request_layout_tree();
                } else {
                    self.border_drag = None;
                }
            }
            Err(e) => error!(error = %e, "failed to encode ResizePane"),
        }
    }

    /// Track which split border is under the cursor and set the resize
    /// cursor icon accordingly. Suppressed while a selection drag is in
    /// progress so sweeping across a border doesn't flip the cursor.
    fn update_border_hover(&mut self, x: f64, y: f64) {
        let hovered = if self.mouse_pressed {
            None
        } else {
            self.layout
                .active_geometry()
                .and_then(|geo| layout::border_at(geo, x, y, BORDER_GRAB_PAD))
        };
        if hovered == self.hovered_border {
            return;
        }
        let icon = match hovered {
            Some(i) => {
                let vertical = self
                    .layout
                    .active_geometry()
                    .and_then(|geo| geo.borders.get(i))
                    .is_some_and(|b| b.is_vertical_border());
                if vertical {
                    CursorIcon::ColResize
                } else {
                    CursorIcon::RowResize
                }
            }
            None => CursorIcon::Default,
        };
        self.hovered_border = hovered;
        if let Some(w) = &self.window {
            w.set_cursor(icon);
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

    /// Frame `payload` as a fresh-serial request and send it. Returns
    /// whether the frame was sent.
    fn send_request(&mut self, msg_type: u16, payload: Vec<u8>, what: &str) -> bool {
        self.send_request_serial(msg_type, payload, what).is_some()
    }

    /// Like [`Self::send_request`], returning the request serial on a
    /// successful send for callers that correlate the response.
    fn send_request_serial(&mut self, msg_type: u16, payload: Vec<u8>, what: &str) -> Option<u32> {
        let serial = self.take_serial();
        match Frame::new(msg_type, serial, payload) {
            Ok(frame) => self.send_or_disconnect(&frame, what).then_some(serial),
            Err(e) => {
                error!(error = %e, what, "failed to encode request");
                None
            }
        }
    }

    /// Encode and send a `Resize`, committing the pane's
    /// `last_sent_dims` only on success so failed sends are re-planned.
    /// Every PTY resize routes through here so the write-failure policy
    /// (disconnect) and the commit-after-send contract live in one
    /// place. Returns whether the frame was sent.
    fn send_resize(&mut self, msg: Resize) -> bool {
        let (pane_id, dims) = (msg.pane_id, (msg.cols, msg.rows));
        let sent = match msg.to_frame() {
            Ok(frame) => self.send_or_disconnect(&frame, "Resize"),
            Err(e) => {
                error!(error = %e, pane_id, "failed to encode Resize");
                false
            }
        };
        if sent {
            if let Some(view) = self.panes.get_mut(&pane_id) {
                view.last_sent_dims = dims;
            }
        }
        sent
    }

    /// First `RedrawRequested`: window dimensions have settled. Send the
    /// initial `Resize` that triggers PTY spawn on the daemon side
    /// (Spec-0001). No-op (caller retries next frame) while font, view,
    /// or daemon are still unavailable.
    fn try_send_initial_resize(&mut self) {
        let (top_px, bottom_px) = self.chrome_px();
        let pending = match (
            &self.gpu,
            &self.font,
            self.panes.get(&self.focused_pane),
            &self.daemon,
        ) {
            (Some(gpu), Some(font), Some(_), Some(_)) => {
                let size = PhysicalSize::new(gpu.config.width, gpu.config.height);
                let (cols, rows) = window_to_grid_dims(
                    size,
                    font.metrics(),
                    &self.config.padding,
                    top_px,
                    bottom_px,
                );
                Some(window_resize(self.focused_pane, (cols, rows), size))
            }
            _ => None,
        };
        let Some(msg) = pending else { return };
        if self.send_resize(msg) {
            self.initial_resize_sent = true;
        } else if let Some(w) = &self.window {
            // Without a spawned PTY no output event will produce the next
            // redraw, so the retry must drive itself.
            w.request_redraw();
        }
    }

    /// Fetch the active tab's layout tree. `GetLayoutTree` takes a
    /// literal tab id; the seeded default tab is 0, so the fallback is
    /// correct before the first `TabList` arrives.
    fn request_layout_tree(&mut self) {
        let tab_id = self.tabs.active_tab().unwrap_or(0);
        let req = GetLayoutTree {
            workspace_id: 0,
            tab_id,
        };
        self.send_request(MSG_GET_LAYOUT_TREE, req.encode(), "GetLayoutTree");
    }

    /// Refresh tab state, or degrade gracefully against a pre-1.2 daemon
    /// that would ignore `ListTabs`: fetch the active tab's layout
    /// directly (old daemons serve the active tab for any `tab_id`), so
    /// tab operations still refresh panes — the bar just never shows.
    fn request_list_tabs(&mut self) {
        if self.server_minor < LIST_TABS_MIN_MINOR {
            debug!(
                server_minor = self.server_minor,
                "daemon predates ListTabs; falling back to layout fetch"
            );
            self.request_layout_tree();
            return;
        }
        self.send_request(MSG_LIST_TABS, vec![], "ListTabs");
    }

    /// Switch to `tab_id`. The daemon confirms nothing for the push, so
    /// the refreshed `TabList` (handled in `user_event`) drives the
    /// layout refetch and focus move.
    fn switch_tab(&mut self, tab_id: u32) {
        if self.tabs.active_tab() == Some(tab_id) {
            return;
        }
        self.send_request(MSG_SWITCH_TAB, SwitchTab { tab_id }.encode(), "SwitchTab");
        self.request_list_tabs();
    }

    /// Adopt a `TabList`: relayout when the bar appeared or vanished,
    /// and follow an active-tab change by fetching the now-active tab's
    /// tree and moving focus to its focused pane. Every tab mutation
    /// funnels through here — switch, create, and close all end with a
    /// `ListTabs` refresh.
    fn apply_tab_list(&mut self, list: TabList) {
        let was_visible = self.tabs.bar_visible();
        let previous_active = self.tabs.apply(list);
        if self.tabs.bar_visible() != was_visible {
            // The content area gained or lost the bar row: every pane's
            // rect and PTY size shifts.
            self.relayout_panes();
        }
        let refocus = std::mem::take(&mut self.refocus_after_close);
        let active = self.tabs.active_tab();
        let active_changed = active != previous_active;
        match tab_sync_timing(TabAdoption {
            active_changed,
            has_active: active.is_some(),
            refocus,
        }) {
            TabSyncTiming::AfterLayout => {
                // Overwrite any pending focus with the now-active tab's
                // focused pane so the inbound tree does not drain a stale
                // target (a tab created moments ago) into a failed focus.
                if let Some(focus) = active.and_then(|a| self.tabs.focused_pane_of(a)) {
                    self.layout.set_pending_focus(focus);
                }
                self.request_layout_tree();
            }
            TabSyncTiming::Now => self.sync_tabs_a11y(),
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Tab bar height in pixels: one cell row when the bar is visible
    /// (Spec-0009: tabs > 1), else 0.
    fn tab_bar_px(&self) -> u32 {
        tab_bar::tab_bar_height(
            self.tabs.bar_visible(),
            self.font.as_ref().map(FontState::metrics),
        )
    }

    /// Status bar height in pixels: one cell row when enabled, else 0.
    fn status_bar_px(&self) -> u32 {
        status_bar::status_bar_height(
            self.config.status_bar,
            self.font.as_ref().map(FontState::metrics),
        )
    }

    /// `(top, bottom)` chrome heights in pixels: the tab bar always sits
    /// at the top; the status bar joins whichever edge is configured.
    fn chrome_px(&self) -> (u32, u32) {
        chrome_split(
            self.tab_bar_px(),
            self.status_bar_px(),
            self.config.status_bar_position,
        )
    }

    /// Whether pixel row `y` lies in window chrome (tab bar or status
    /// bar) rather than pane content; chrome owns mouse presses there.
    fn in_chrome_row(&self, y: f64) -> bool {
        let (top_px, bottom_px) = self.chrome_px();
        if y < f64::from(top_px) {
            return true;
        }
        let Some(gpu) = &self.gpu else { return false };
        y >= f64::from(gpu.config.height.saturating_sub(bottom_px))
    }

    /// The tab under pixel column `x` of the tab bar, resolved through
    /// the same strip layout the renderer draws.
    fn tab_at_pixel(&self, x: f64) -> Option<u32> {
        let metrics = self.font.as_ref()?.metrics();
        let gpu = self.gpu.as_ref()?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let col = (x.max(0.0) / f64::from(metrics.cell_width)) as u16;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let cols = ((gpu.config.width as f32 / metrics.cell_width).max(0.0) as u16)
            .clamp(1, MAX_GRID_DIMENSION);
        let spans = tab_bar::layout_strip(self.tabs.tabs(), cols);
        tab_bar::hit_test(&spans, col)
    }

    /// The window's content area in pixels (window minus padding and
    /// chrome), where the layout tree's panes tile.
    fn content_rect(&self) -> Option<layout::PixelRect> {
        let gpu = self.gpu.as_ref()?;
        let pad = &self.config.padding;
        let (top_px, bottom_px) = self.chrome_px();
        Some(layout::PixelRect {
            x: pad.left,
            y: pad.top.saturating_add(top_px),
            width: gpu.config.width.saturating_sub(pad.left + pad.right),
            height: gpu
                .config
                .height
                .saturating_sub(pad.top + pad.bottom)
                .saturating_sub(top_px)
                .saturating_sub(bottom_px),
        })
    }

    /// Recompute pane geometry and PTY sizes for the current window size
    /// and chrome. Shared by window resizes and tab-bar visibility
    /// changes — both move the content area.
    fn relayout_panes(&mut self) {
        // Resize exits scrollback for the focused pane.
        if let Some(view) = self.panes.get_mut(&self.focused_pane) {
            view.return_to_live();
        }

        // With splits, every pane's rect changes: recompute the
        // geometry and resize each PTY to its rect. The
        // single-pane path below sizes to the whole window.
        if self.layout.has_tree() {
            let content = self.content_rect();
            if let Some(c) = &content {
                if c.width == 0 || c.height == 0 {
                    // Grid sizing floors at 1x1, so panes silently stop
                    // painting without this trace of why.
                    warn!(
                        width = c.width,
                        height = c.height,
                        "content area collapsed; window smaller than padding + chrome"
                    );
                }
            }
            self.layout.recompute(content);
            if self.layout.active_geometry().is_some() {
                if self.initial_resize_sent {
                    self.sync_panes_to_geometry();
                }
                // Pane origins and dimensions moved; row bounds
                // derive from both.
                self.sync_a11y_layout();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                return;
            }
        }

        let size = match &self.gpu {
            Some(gpu) => PhysicalSize::new(gpu.config.width, gpu.config.height),
            None => return,
        };
        let (top_px, bottom_px) = self.chrome_px();
        let pending_resize = match (&self.font, self.panes.get_mut(&self.focused_pane)) {
            (Some(font), Some(view)) => {
                let (cols, rows) = window_to_grid_dims(
                    size,
                    font.metrics(),
                    &self.config.padding,
                    top_px,
                    bottom_px,
                );
                let dims_changed = view.grid().rows != rows || view.grid().cols != cols;
                if dims_changed {
                    view.resize(cols, rows);
                }

                // Full a11y tree rebuild on resize (row count changed).
                if dims_changed {
                    let full_tree = a11y_bridge::apply(
                        &self.a11y_state,
                        self.focused_pane,
                        view,
                        A11yEvent::Resize,
                    );
                    if let (Some(adapter), Some(full_tree)) = (&mut self.accesskit, full_tree) {
                        adapter.update_if_active(|| full_tree);
                    }
                }

                // Defer until RedrawRequested; startup fires multiple
                // Resized events.
                (self.initial_resize_sent && (cols, rows) != view.last_sent_dims)
                    .then(|| window_resize(self.focused_pane, (cols, rows), size))
            }
            _ => None,
        };
        if let Some(msg) = pending_resize {
            self.send_resize(msg);
        }

        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Adopt a layout tree from the daemon: create views for new panes,
    /// size every pane's PTY to its computed rect, apply any pending
    /// focus, and redraw.
    fn apply_layout_tree(&mut self, tree: LayoutTreeNode) {
        let content = self.content_rect();
        let pending_focus = self.layout.adopt_tree(tree, content);
        // A drag whose pane pair left the topology must not keep sending
        // ResizePane for a dead id.
        let drag_stale = self.border_drag.as_ref().is_some_and(|drag| {
            let alive = |id: u32| {
                self.layout
                    .geometry()
                    .is_some_and(|g| g.panes.iter().any(|p| p.pane_id == id))
            };
            !alive(drag.before) || !alive(drag.after)
        });
        if drag_stale {
            self.border_drag = None;
        }
        self.sync_panes_to_geometry();
        // Panes and tabs commit in one a11y update: a tab switch defers its
        // tab sync to here (apply_tab_list took the tab_sync_timing
        // AfterLayout branch), so the new selection and the new tab's panes
        // publish together, never as two trees with a stale-selection frame
        // between. A no-op for the tabs half when they're unchanged (splits,
        // resizes).
        self.sync_layout_and_tabs_a11y();
        if let Some(id) = pending_focus {
            if self.panes.contains_key(&id) {
                self.focus_pane(id);
            } else {
                warn!(pane_id = id, "pending focus target missing from layout");
            }
        }
        // A pane close (or any topology change this client missed) can
        // leave the focused pane dangling; refocus so input has a live
        // target. FocusPane re-aligns the daemon with the choice.
        match check_focus(self.layout.geometry(), self.focused_pane) {
            FocusHealth::Live => {}
            FocusHealth::Refocus(id) => {
                warn!(
                    stale = self.focused_pane,
                    fallback = id,
                    "focused pane left the layout; refocusing"
                );
                self.focus_pane(id);
            }
            FocusHealth::Stranded => {
                warn!(
                    stale = self.focused_pane,
                    "focused pane left the layout and no pane remains to refocus"
                );
            }
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Create views for panes new to the geometry and send `Resize` to
    /// every pane whose computed grid dimensions changed. The first
    /// `Resize` for a fresh pane spawns its PTY (Spec-0001 `SplitPane`).
    fn sync_panes_to_geometry(&mut self) {
        let Some(metrics) = self.font.as_ref().map(|f| *f.metrics()) else {
            warn!("layout geometry present but font unavailable; panes not synced");
            return;
        };
        let resizes = {
            let Some(geometry) = self.layout.geometry() else {
                warn!("layout tree adopted but no geometry; panes not synced");
                return;
            };
            plan_pane_syncs(
                geometry,
                (metrics.cell_width, metrics.cell_height),
                &mut self.panes,
            )
        };
        for msg in resizes {
            self.send_resize(msg);
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
        let chrome_changed = cr.config.status_bar != self.config.status_bar
            || cr.config.status_bar_position != self.config.status_bar_position;

        let had_error = self.config_error.is_some();
        self.config = cr.config;
        self.config_error = None;
        self.event_registry = cr.registry;
        self.keybind_registry = cr.keybinds;
        self.action_registry = oakterm_config::ActionRegistry::core(&self.keybind_registry);
        // New bindings may change hints or performability; a stale open
        // palette would show them wrong.
        self.palette.close();
        // A pending leader references the old leader config; the file
        // watcher can fire mid-wait, so flush the buffered key to the
        // PTY rather than losing the user's keystroke.
        if let Some(pending) = self.leader_pending.take() {
            tracing::debug!("config reload with a leader press pending; flushing its key");
            if let Some(bytes) = pending.buffered {
                self.try_send_key_bytes(bytes);
            }
        }
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

                // self.font still holds the old metrics; use font_state's.
                let tab_px =
                    tab_bar::tab_bar_height(self.tabs.bar_visible(), Some(font_state.metrics()));
                let status_px = status_bar::status_bar_height(
                    self.config.status_bar,
                    Some(font_state.metrics()),
                );
                let (top_px, bottom_px) =
                    chrome_split(tab_px, status_px, self.config.status_bar_position);
                let pending_resize = if let (Some(gpu), Some(view)) =
                    (&self.gpu, self.panes.get_mut(&self.focused_pane))
                {
                    let phys = PhysicalSize::new(gpu.config.width, gpu.config.height);
                    let (cols, rows) = window_to_grid_dims(
                        phys,
                        font_state.metrics(),
                        &self.config.padding,
                        top_px,
                        bottom_px,
                    );
                    let cols = cols.max(1);
                    let rows = rows.max(1);
                    view.resize(cols, rows);

                    // Row bounds derive from cell dimensions; rebuild the
                    // a11y tree at the new metrics.
                    match self.a11y_state.lock() {
                        Ok(mut model) => {
                            if let Some(m) = model.as_mut() {
                                m.set_cell_dims(a11y_bridge::cell_dims(Some(font_state.metrics())));
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

                    Some(window_resize(self.focused_pane, (cols, rows), phys))
                } else {
                    // The renderer adopts the new metrics below; without a
                    // grid resize the a11y model and daemon keep the old
                    // geometry.
                    warn!("config reload: font changed but gpu/view unavailable");
                    None
                };
                if let Some(msg) = pending_resize {
                    self.send_resize(msg);
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

        // Status bar changes move the content area. Runs after the font
        // block so a combined reload lays out splits against the new
        // metrics; last_sent_dims suppresses redundant per-pane resizes.
        if chrome_changed {
            self.relayout_panes();
        }
        if !self.config.status_bar && self.clock_deadline.is_some() {
            self.clock_deadline = None;
            tracing::trace!("status bar disabled; clock repaint disarmed");
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

/// Bit for tracking a button press window chrome consumed. Matches the
/// PTY encoding's button order (left=0, middle=1, right=2).
fn chrome_button_bit(button: winit::event::MouseButton) -> u8 {
    match button {
        winit::event::MouseButton::Middle => 1 << 1,
        winit::event::MouseButton::Right => 1 << 2,
        _ => 1 << 0,
    }
}

/// Earliest of the armed timer deadlines, or `None` when all are off
/// (the event loop then waits indefinitely).
fn next_wakeup<const N: usize>(
    deadlines: [Option<std::time::Instant>; N],
) -> Option<std::time::Instant> {
    deadlines.into_iter().flatten().min()
}

/// Split tab-bar and status-bar heights into `(top, bottom)` chrome:
/// the tab bar is always top; the status bar joins its configured edge.
fn chrome_split(tab_px: u32, status_px: u32, position: StatusBarPosition) -> (u32, u32) {
    match position {
        StatusBarPosition::Top => (tab_px.saturating_add(status_px), 0),
        StatusBarPosition::Bottom => (tab_px, status_px),
    }
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
    top_chrome_px: u32,
    bottom_chrome_px: u32,
) -> (u16, u16) {
    let usable_w = size.width.saturating_sub(padding.left + padding.right);
    let usable_h = size
        .height
        .saturating_sub(padding.top + padding.bottom)
        .saturating_sub(top_chrome_px)
        .saturating_sub(bottom_chrome_px);
    // Clamp to the daemon's cap (as grid_dims does) so a very wide display or
    // tiny font can't produce a Resize the daemon rejects.
    let cols = ((usable_w as f32 / metrics.cell_width) as u16).clamp(1, MAX_GRID_DIMENSION);
    let rows = ((usable_h as f32 / metrics.cell_height) as u16).clamp(1, MAX_GRID_DIMENSION);
    (cols, rows)
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
    use super::{
        ActionDesc, App, FocusHealth, PendingPaneClose, TabAdoption, TabSyncTiming, check_focus,
        chrome_split, drain_wheel_notches, plan_pane_syncs, resolve_pane_close, tab_sync_timing,
        try_init_font, window_to_grid_dims,
    };
    use crate::layout::{LayoutGeometry, PaneRect, PixelRect};
    use crate::pane_view::PaneView;
    use crate::render_grid::ClientGrid;
    use std::collections::HashMap;
    use winit::dpi::{PhysicalPosition, PhysicalSize};
    use winit::event::MouseScrollDelta;

    /// `FontMetrics` is `#[non_exhaustive]`; take a real one and pin the
    /// cell dimensions the assertions depend on. `None` when no system
    /// font is available (test then skips, matching sibling tests).
    fn metrics(cell_width: f32, cell_height: f32) -> Option<oakterm_renderer::shaper::FontMetrics> {
        let font = try_init_font(&oakterm_config::ConfigValues::default(), 14.0).ok()?;
        let mut m = *font.metrics();
        m.cell_width = cell_width;
        m.cell_height = cell_height;
        Some(m)
    }

    #[test]
    fn tab_sync_timing_defers_on_active_tab_change() {
        // Switch or create with a real active tab: a LayoutTree is inbound.
        // The pending refocus flag can't override an active-tab change, so
        // both refocus values defer.
        for refocus in [false, true] {
            assert_eq!(
                tab_sync_timing(TabAdoption {
                    active_changed: true,
                    has_active: true,
                    refocus,
                }),
                TabSyncTiming::AfterLayout,
            );
        }
    }

    #[test]
    fn tab_sync_timing_defers_on_post_close_refocus() {
        // Same tab, a pane closed: fetch the shrunken tree, defer.
        assert_eq!(
            tab_sync_timing(TabAdoption {
                active_changed: false,
                has_active: true,
                refocus: true,
            }),
            TabSyncTiming::AfterLayout,
        );
    }

    #[test]
    fn tab_sync_timing_refocus_defers_even_with_no_active_tab() {
        // The has_active guard applies only to the active-changed arm; a
        // refocus on the same (absent) tab still defers. Pins the asymmetry
        // against a future edit that mistakenly guards the refocus arm too.
        assert_eq!(
            tab_sync_timing(TabAdoption {
                active_changed: false,
                has_active: false,
                refocus: true,
            }),
            TabSyncTiming::AfterLayout,
        );
    }

    #[test]
    fn tab_sync_timing_now_for_in_place_mutations() {
        // Rename / reorder / bar-visibility, and the all-quiet baseline: no
        // new panes, so push the strip immediately.
        for has_active in [false, true] {
            assert_eq!(
                tab_sync_timing(TabAdoption {
                    active_changed: false,
                    has_active,
                    refocus: false,
                }),
                TabSyncTiming::Now,
            );
        }
    }

    #[test]
    fn tab_sync_timing_now_when_active_change_leaves_no_tab() {
        // Active changed to no tab (last tab closed): no tree to fetch, so
        // the refocus flag is ignored and the strip syncs now. Pins the
        // edge the merged AfterLayout arm must not over-trigger on.
        for refocus in [false, true] {
            assert_eq!(
                tab_sync_timing(TabAdoption {
                    active_changed: true,
                    has_active: false,
                    refocus,
                }),
                TabSyncTiming::Now,
            );
        }
    }

    #[test]
    fn window_to_grid_dims_subtracts_top_and_bottom_chrome() {
        let Some(m) = metrics(10.0, 20.0) else { return };
        let size = PhysicalSize::new(800, 600);
        let pad = oakterm_config::Padding::default();
        let (cols_bare, rows_bare) = window_to_grid_dims(size, &m, &pad, 0, 0);
        let (cols_top, rows_top) = window_to_grid_dims(size, &m, &pad, 20, 0);
        assert_eq!(cols_top, cols_bare);
        assert_eq!(rows_top, rows_bare - 1);
        let (cols_both, rows_both) = window_to_grid_dims(size, &m, &pad, 20, 20);
        assert_eq!(cols_both, cols_bare);
        assert_eq!(rows_both, rows_bare - 2);
    }

    #[test]
    fn chrome_split_places_the_status_bar_on_its_configured_edge() {
        use oakterm_config::StatusBarPosition;
        assert_eq!(chrome_split(17, 17, StatusBarPosition::Bottom), (17, 17));
        assert_eq!(chrome_split(17, 17, StatusBarPosition::Top), (34, 0));
        assert_eq!(chrome_split(0, 17, StatusBarPosition::Top), (17, 0));
    }

    #[test]
    fn desc_of_action_consumes_config_typos_and_defers_callbacks() {
        use super::{DescOutcome, desc_of_action};
        use oakterm_config::Action;
        // A typo'd direction consumes the chord (never leaks key bytes
        // into the shell); callbacks defer to the caller for indexing.
        assert!(matches!(
            desc_of_action(&Action::SplitPane {
                direction: "diagonal".to_string(),
                size: 0.5
            }),
            DescOutcome::Consume
        ));
        assert!(matches!(
            desc_of_action(&Action::FocusPaneDirection("bogus".to_string())),
            DescOutcome::Consume
        ));
        assert!(matches!(
            desc_of_action(&Action::NewTab),
            DescOutcome::Desc(ActionDesc::NewTab)
        ));
        assert!(matches!(
            desc_of_action(&Action::SplitPane {
                direction: "right".to_string(),
                size: 0.5
            }),
            DescOutcome::Desc(ActionDesc::SplitPane(super::WireSplitDirection::Horizontal))
        ));
        let lua = oakterm_config::Lua::new();
        let f = lua.create_function(|_, ()| Ok(())).unwrap();
        let key = lua.create_registry_value(f).unwrap();
        assert!(matches!(
            desc_of_action(&Action::Callback(key)),
            DescOutcome::Callback
        ));
    }

    #[test]
    fn run_keybind_callback_times_out_a_runaway_callback() {
        let lua = oakterm_config::Lua::new();
        let f = lua
            .load("while true do end")
            .into_function()
            .expect("loop compiles");
        let key = lua.create_registry_value(f).unwrap();
        let start = std::time::Instant::now();
        super::run_keybind_callback(&lua, &key);
        // The 100ms watchdog must abort the loop; generous bound for CI.
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn next_wakeup_picks_the_earliest_armed_deadline() {
        let now = std::time::Instant::now();
        let soon = now + std::time::Duration::from_millis(100);
        let later = now + std::time::Duration::from_secs(30);
        assert_eq!(super::next_wakeup([Some(soon), Some(later)]), Some(soon));
        assert_eq!(super::next_wakeup([Some(later), Some(soon)]), Some(soon));
        assert_eq!(super::next_wakeup([None, Some(later)]), Some(later));
        assert_eq!(super::next_wakeup([Some(soon), None]), Some(soon));
        assert_eq!(super::next_wakeup([None, None]), None);
        assert_eq!(
            super::next_wakeup([Some(later), None, Some(soon)]),
            Some(soon)
        );
    }

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

    fn close_queue(entries: &[(u32, u32)]) -> std::collections::VecDeque<PendingPaneClose> {
        entries
            .iter()
            .map(|&(serial, pane_id)| PendingPaneClose { serial, pane_id })
            .collect()
    }

    #[test]
    fn resolve_pane_close_matches_by_serial_and_consumes() {
        let mut q = close_queue(&[(10, 5)]);
        assert_eq!(resolve_pane_close(&mut q, 10), Some(5));
        assert!(q.is_empty());
    }

    #[test]
    fn resolve_pane_close_prunes_rejected_older_entries() {
        // Serial 10 was rejected (its Error frame carries no PaneClosed);
        // the serial-12 success must return its own pane and sweep the
        // stale entry without ever returning pane 5.
        let mut q = close_queue(&[(10, 5), (12, 7)]);
        assert_eq!(resolve_pane_close(&mut q, 12), Some(7));
        assert!(q.is_empty());
    }

    #[test]
    fn resolve_pane_close_unknown_serial_prunes_but_returns_none() {
        let mut q = close_queue(&[(10, 5), (14, 7)]);
        assert_eq!(resolve_pane_close(&mut q, 12), None);
        assert_eq!(q, close_queue(&[(14, 7)]), "newer in-flight entry survives");
    }

    #[test]
    fn check_focus_reports_live_dead_and_stranded() {
        let geo = geometry_of(&[(3, 100, 100), (7, 100, 100)]);
        assert_eq!(check_focus(Some(&geo), 7), FocusHealth::Live);
        assert_eq!(check_focus(Some(&geo), 99), FocusHealth::Refocus(3));
        let empty = geometry_of(&[]);
        assert_eq!(check_focus(Some(&empty), 7), FocusHealth::Stranded);
        assert_eq!(check_focus(None, 7), FocusHealth::Stranded);
    }

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
        assert_eq!((view.grid().cols, view.grid().rows), (80, 10));
        assert_eq!(view.last_sent_dims, (40, 10), "plan must not commit");
    }

    /// A background pane in scrollback must return to live when a layout
    /// resize forces a grid resize, or its stale offset drives `GetScrollback`
    /// at a bad `start_row` — the D2/TREK-139 desync class via `plan_pane_syncs`.
    #[test]
    fn plan_pane_syncs_resize_returns_scrolled_pane_to_live() {
        let geometry = geometry_of(&[(7, 400, 200)]);
        let mut panes = HashMap::new();
        plan_pane_syncs(&geometry, CELL, &mut panes);
        panes.get_mut(&7).unwrap().scroll_up(5);
        assert!(panes[&7].is_scrolled());

        let grown = geometry_of(&[(7, 800, 200)]);
        plan_pane_syncs(&grown, CELL, &mut panes);
        let view = &panes[&7];
        assert_eq!(view.viewport_offset(), 0, "resize must return to live");
        assert!(!view.is_scrolled(), "offset and snapshot both reset");
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

    #[test]
    fn action_desc_of_id_maps_every_catalog_action_to_its_effect() {
        // The palette's only execution bridge; a transposed arm would make
        // a selected action silently run the wrong effect.
        use crate::layout::FocusDirection;
        use oakterm_config::ActionId;
        use oakterm_protocol::message::SplitDirection as WireSplitDirection;

        let desc = App::action_desc_of_id;
        assert!(matches!(
            desc(ActionId::SplitPaneRight),
            ActionDesc::SplitPane(WireSplitDirection::Horizontal)
        ));
        assert!(matches!(
            desc(ActionId::SplitPaneDown),
            ActionDesc::SplitPane(WireSplitDirection::Vertical)
        ));
        assert!(matches!(desc(ActionId::ClosePane), ActionDesc::ClosePane));
        assert!(matches!(
            desc(ActionId::FocusPaneLeft),
            ActionDesc::FocusPane(FocusDirection::Left)
        ));
        assert!(matches!(
            desc(ActionId::FocusPaneRight),
            ActionDesc::FocusPane(FocusDirection::Right)
        ));
        assert!(matches!(
            desc(ActionId::FocusPaneUp),
            ActionDesc::FocusPane(FocusDirection::Up)
        ));
        assert!(matches!(
            desc(ActionId::FocusPaneDown),
            ActionDesc::FocusPane(FocusDirection::Down)
        ));
        assert!(matches!(desc(ActionId::NewTab), ActionDesc::NewTab));
        assert!(matches!(desc(ActionId::CloseTab), ActionDesc::CloseTab));
        assert!(matches!(desc(ActionId::NextTab), ActionDesc::NextTab));
        assert!(matches!(
            desc(ActionId::PreviousTab),
            ActionDesc::PreviousTab
        ));
        assert!(matches!(
            desc(ActionId::ToggleFullscreen),
            ActionDesc::ToggleFullscreen
        ));
        assert!(matches!(
            desc(ActionId::ShowCommandPalette),
            ActionDesc::ShowCommandPalette
        ));
        assert!(matches!(
            desc(ActionId::ReloadConfig),
            ActionDesc::ReloadConfig
        ));
    }
}
