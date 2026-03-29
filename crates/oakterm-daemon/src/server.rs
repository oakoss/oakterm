//! Daemon server: PTY read loop, Unix socket listener, client connections.

use crate::socket::socket_path;
use bytes::BytesMut;
use oakterm_protocol::frame::{Frame, FrameCodec};
use oakterm_protocol::input::{KeyInput, MouseInput, Resize};
use oakterm_protocol::message::{
    Bell, ClientHello, HandshakeStatus, MSG_CLIENT_HELLO, MSG_DETACH, MSG_DIRTY_NOTIFY,
    MSG_GET_RENDER_UPDATE, MSG_KEY_INPUT, MSG_MOUSE_INPUT, MSG_PING, MSG_PONG, MSG_RENDER_UPDATE,
    MSG_RESIZE, ServerHello, TitleChanged,
};
use oakterm_protocol::render::{DirtyNotify, DirtyRow, GetRenderUpdate, RenderUpdate, WireCell};
use oakterm_terminal::grid::ScreenSet;
use oakterm_terminal::grid::cell::{Color, Rgb};
use oakterm_terminal::handler;
use std::io;
use std::os::unix::io::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, watch};
use tokio_util::codec::{Decoder, Encoder};

/// Handshake timeout per Spec-0001.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// PTY lifecycle: `NotSpawned` until the first client Resize triggers spawn.
enum PtyState {
    /// Waiting for first client Resize to determine dimensions.
    NotSpawned,
    /// Master fd for writes and resizes. The `Pty` struct is owned by the read loop.
    Running(RawFd),
}

/// Daemon state shared across tasks.
pub struct Daemon {
    screens: Arc<Mutex<ScreenSet>>,
    dirty_tx: watch::Sender<u64>,
    dirty_rx: watch::Receiver<u64>,
    socket_path: std::path::PathBuf,
    /// When false (default), the daemon exits after the last client disconnects.
    /// When true, the daemon stays running with zero clients (headless/persist mode).
    persist: bool,
}

impl Daemon {
    /// Create a new daemon. `cols` and `rows` set the initial grid size;
    /// actual PTY dimensions come from the first client Resize.
    ///
    /// # Errors
    /// Returns an error if the socket path cannot be resolved.
    pub fn new(cols: u16, rows: u16) -> io::Result<Self> {
        let path = socket_path()?;
        let (dirty_tx, dirty_rx) = watch::channel(0u64);
        Ok(Self {
            screens: Arc::new(Mutex::new(ScreenSet::new(cols, rows))),
            dirty_tx,
            dirty_rx,
            socket_path: path,
            persist: false,
        })
    }

    /// Enable persist mode: daemon stays running with zero clients.
    pub fn set_persist(&mut self, persist: bool) {
        self.persist = persist;
    }

    /// Listen for connections. The PTY spawns on the first client Resize
    /// so the shell starts at the correct dimensions.
    ///
    /// # Errors
    /// Returns an error if the listener fails to start.
    pub async fn run(&self) -> io::Result<()> {
        if self.socket_path.exists() {
            std::fs::remove_file(&self.socket_path)?;
        }

        let listener = UnixListener::bind(&self.socket_path)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.socket_path, std::fs::Permissions::from_mode(0o700))?;
        }

        let pty_state = Arc::new(Mutex::new(PtyState::NotSpawned));

        // Phase 0: counts all clients. ADR-0007 says "last window closes" —
        // when control clients exist, filter by ClientType::Gui.
        let client_count = Arc::new(AtomicUsize::new(0));
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let persist = self.persist;

        loop {
            tokio::select! {
                result = listener.accept() => {
                    let (stream, _) = result?;
                    let screens = Arc::clone(&self.screens);
                    let dirty_rx = self.dirty_rx.clone();
                    let dirty_tx = self.dirty_tx.clone();
                    let pty = Arc::clone(&pty_state);
                    let count = Arc::clone(&client_count);
                    let tx = shutdown_tx.clone();

                    count.fetch_add(1, Ordering::AcqRel);

                    tokio::spawn(async move {
                        handle_client(stream, screens, dirty_rx, dirty_tx, pty).await;
                        let remaining = count.fetch_sub(1, Ordering::AcqRel) - 1;
                        if remaining == 0 && !persist {
                            let _ = tx.send(true);
                        }
                    });
                }
                _ = shutdown_rx.wait_for(|&v| v) => {
                    break;
                }
            }
        }

        Ok(())
    }

    /// Get the socket path.
    #[must_use]
    pub fn socket_path(&self) -> &std::path::Path {
        &self.socket_path
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

/// Read PTY output, feed to VT parser, update Grid.
async fn pty_read_loop(
    pty: oakterm_pty::Pty,
    screens: Arc<Mutex<ScreenSet>>,
    dirty_tx: watch::Sender<u64>,
) {
    use tokio::io::unix::AsyncFd;

    let raw_fd = pty.master_raw_fd();

    // Set non-blocking for tokio `AsyncFd`.
    let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(raw_fd) };
    if let Ok(flags) = rustix::fs::fcntl_getfl(borrowed) {
        let _ = rustix::fs::fcntl_setfl(borrowed, flags | rustix::fs::OFlags::NONBLOCK);
    }

    let Ok(async_fd) = AsyncFd::new(raw_fd) else {
        return;
    };

    let mut buf = [0u8; 4096];

    loop {
        let Ok(mut guard) = async_fd.readable().await else {
            break;
        };

        match guard.try_io(|inner| {
            let fd = inner.get_ref();
            let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(*fd) };
            rustix::io::read(borrowed, &mut buf)
                .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
        }) {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                let mut s = screens.lock().await;
                let borrowed_wr = unsafe { rustix::fd::BorrowedFd::borrow_raw(raw_fd) };
                let mut pty_writer = FdWriter(borrowed_wr);
                handler::process_bytes(&mut *s, &buf[..n], &mut pty_writer);
                let seqno = s.active_grid().seqno;
                drop(s);
                let _ = dirty_tx.send(seqno);
            }
            Ok(Err(e)) if e.kind() == io::ErrorKind::WouldBlock => {}
            Ok(Err(_)) => break,
            Err(_would_block) => {}
        }
    }
}

/// Handle a single client connection.
#[allow(clippy::too_many_lines)]
async fn handle_client(
    mut stream: UnixStream,
    screens: Arc<Mutex<ScreenSet>>,
    mut dirty_rx: watch::Receiver<u64>,
    dirty_tx: watch::Sender<u64>,
    pty_state: Arc<Mutex<PtyState>>,
) {
    let mut codec = FrameCodec;
    let mut read_buf = BytesMut::with_capacity(4096);
    let mut write_buf = BytesMut::with_capacity(4096);

    // Handshake with timeout per Spec-0001.
    let handshake = async {
        read_frame(&mut stream, &mut read_buf).await?;
        let Ok(Some(frame)) = codec.decode(&mut read_buf) else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "no frame"));
        };
        if frame.msg_type != MSG_CLIENT_HELLO {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected ClientHello",
            ));
        }

        // Validate version per Spec-0001.
        let client_hello = ClientHello::decode(&frame.payload)?;
        if client_hello.protocol_version_major != ClientHello::VERSION_MAJOR {
            let hello = ServerHello {
                status: HandshakeStatus::VersionMismatch,
                protocol_version_major: ClientHello::VERSION_MAJOR,
                protocol_version_minor: ClientHello::VERSION_MINOR,
                server_version: env!("CARGO_PKG_VERSION").to_string(),
            };
            if let Ok(resp) = hello.to_frame(frame.serial) {
                let _ = write_frame(&mut stream, &mut codec, &mut write_buf, resp).await;
            }
            return Err(io::Error::other("version mismatch"));
        }

        let hello = ServerHello {
            status: HandshakeStatus::Accepted,
            protocol_version_major: ClientHello::VERSION_MAJOR,
            protocol_version_minor: ClientHello::VERSION_MINOR,
            server_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let Ok(response) = hello.to_frame(frame.serial) else {
            return Err(io::Error::other("encode failed"));
        };
        write_frame(&mut stream, &mut codec, &mut write_buf, response).await
    };

    let Ok(Ok(())) = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await else {
        return;
    };

    // Main client loop.
    'outer: loop {
        tokio::select! {
            result = dirty_rx.changed() => {
                if result.is_err() {
                    break;
                }

                // Collect title/bell notifications while holding the lock,
                // then send after releasing. Both can fire in the same cycle.
                // NOTE: Phase 0 single-client only. With multiple clients,
                // the first to lock clears the flags; others miss the event.
                // Phase 1 needs per-client notification queues.
                let (title_msg, bell_msg) = {
                    let mut s = screens.lock().await;
                    let g = s.active_grid_mut();
                    let t = if g.title_dirty {
                        g.title_dirty = false;
                        Some(TitleChanged {
                            pane_id: 0,
                            title: g.title.clone().unwrap_or_default(),
                        })
                    } else {
                        None
                    };
                    let b = if g.bell_pending {
                        g.bell_pending = false;
                        Some(Bell { pane_id: 0 })
                    } else {
                        None
                    };
                    (t, b)
                };
                if let Some(msg) = title_msg {
                    if let Ok(f) = msg.to_frame() {
                        if write_frame(&mut stream, &mut codec, &mut write_buf, f).await.is_err() {
                            break;
                        }
                    }
                }
                if let Some(msg) = bell_msg {
                    if let Ok(f) = msg.to_frame() {
                        if write_frame(&mut stream, &mut codec, &mut write_buf, f).await.is_err() {
                            break;
                        }
                    }
                }

                let notify = DirtyNotify { pane_id: 0 };
                let Ok(frame) = Frame::new(MSG_DIRTY_NOTIFY, 0, notify.encode()) else {
                    eprintln!("failed to create DirtyNotify frame");
                    continue;
                };
                if write_frame(&mut stream, &mut codec, &mut write_buf, frame).await.is_err() {
                    break;
                }
            }
            result = read_frame(&mut stream, &mut read_buf) => {
                if result.is_err() {
                    break;
                }
                while let Ok(Some(frame)) = codec.decode(&mut read_buf) {
                    match handle_request(&frame, &screens, &pty_state, &dirty_tx).await {
                        RequestResult::Response(response) => {
                            if write_frame(&mut stream, &mut codec, &mut write_buf, response).await.is_err() {
                                break 'outer;
                            }
                        }
                        RequestResult::Detach => break 'outer,
                        RequestResult::NoResponse => {}
                    }
                }
            }
        }
    }
}

/// Result of processing a client request.
enum RequestResult {
    Response(Frame),
    Detach,
    NoResponse,
}

/// Handle a single client request frame.
#[allow(clippy::too_many_lines)]
async fn handle_request(
    frame: &Frame,
    screens: &Arc<Mutex<ScreenSet>>,
    pty_state: &Mutex<PtyState>,
    dirty_tx: &watch::Sender<u64>,
) -> RequestResult {
    match frame.msg_type {
        MSG_KEY_INPUT => {
            let state = pty_state.lock().await;
            if let PtyState::Running(fd) = *state {
                drop(state);
                if let Ok(msg) = KeyInput::decode(&frame.payload) {
                    if !msg.key_data.is_empty() {
                        let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(fd) };
                        let _ = rustix::io::write(borrowed, &msg.key_data);
                    }
                }
            }
            RequestResult::NoResponse
        }
        MSG_MOUSE_INPUT => {
            let state = pty_state.lock().await;
            if let PtyState::Running(fd) = *state {
                drop(state);
                if let Ok(msg) = MouseInput::decode(&frame.payload) {
                    let s = screens.lock().await;
                    let g = s.active_grid();
                    // Only encode mouse if a mouse mode is active.
                    let sgr = g.modes.get(1006);
                    let click = g.modes.get(1000);
                    let cell_motion = g.modes.get(1002);
                    let all_motion = g.modes.get(1003);
                    drop(s);

                    let should_send = match msg.event_type {
                        0 | 1 | 3 | 4 => click || cell_motion || all_motion,
                        2 => cell_motion || all_motion,
                        _ => false,
                    };

                    if should_send {
                        let seq = encode_mouse_sgr(&msg, sgr);
                        if !seq.is_empty() {
                            let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(fd) };
                            let _ = rustix::io::write(borrowed, seq.as_bytes());
                        }
                    }
                }
            }
            RequestResult::NoResponse
        }
        MSG_RESIZE => {
            if let Ok(msg) = Resize::decode(&frame.payload) {
                let mut state = pty_state.lock().await;
                match *state {
                    PtyState::NotSpawned => {
                        // First Resize from any client: spawn PTY at these dimensions.
                        // WinSize omits pixel dimensions (set_winsize uses 0); fine
                        // until sixel/kitty graphics need them (Phase 0.4).
                        // Note: spawn_shell blocks briefly (~1-5ms for fork/exec)
                        // while holding the async Mutex. Acceptable for Phase 0;
                        // use spawn_blocking if this becomes a contention issue.
                        match oakterm_pty::spawn_shell(oakterm_pty::WinSize {
                            cols: msg.cols,
                            rows: msg.rows,
                        }) {
                            Ok(pty) => {
                                let fd = pty.master_raw_fd();
                                *state = PtyState::Running(fd);
                                drop(state);

                                {
                                    let mut s = screens.lock().await;
                                    s.resize_all(msg.cols, msg.rows);
                                }

                                let screens_clone = Arc::clone(screens);
                                let dtx = dirty_tx.clone();
                                tokio::spawn(pty_read_loop(pty, screens_clone, dtx));
                            }
                            Err(e) => {
                                // State stays NotSpawned; next Resize retries.
                                // TREK-21: add Failed variant and MSG_ERROR response.
                                eprintln!("failed to spawn PTY: {e}");
                            }
                        }
                    }
                    PtyState::Running(fd) => {
                        drop(state);
                        let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(fd) };
                        if oakterm_pty::resize_fd(
                            borrowed,
                            msg.cols,
                            msg.rows,
                            msg.pixel_width,
                            msg.pixel_height,
                        )
                        .is_ok()
                        {
                            let mut s = screens.lock().await;
                            s.resize_all(msg.cols, msg.rows);
                        }
                    }
                }
            }
            RequestResult::NoResponse
        }
        MSG_DETACH => RequestResult::Detach,
        MSG_GET_RENDER_UPDATE => {
            let Ok(req) = GetRenderUpdate::decode(&frame.payload) else {
                return RequestResult::NoResponse;
            };
            let s = screens.lock().await;
            let g = s.active_grid();
            let dirty_indices = g.dirty_rows(req.since_seqno);

            let dirty_rows: Vec<DirtyRow> = dirty_indices
                .iter()
                .filter_map(|&idx| {
                    let row = g.lines.get(idx as usize)?;
                    let cells: Vec<WireCell> = row
                        .cells
                        .iter()
                        .map(|c| {
                            let (fg_r, fg_g, fg_b, fg_type) =
                                resolve_color(c.fg, &g.palette, 255, 255, 255);
                            let (bg_r, bg_g, bg_b, bg_type) =
                                resolve_color(c.bg, &g.palette, 0, 0, 0);
                            WireCell {
                                codepoint: c.codepoint as u32,
                                fg_r,
                                fg_g,
                                fg_b,
                                fg_type,
                                bg_r,
                                bg_g,
                                bg_b,
                                bg_type,
                                flags: c.flags.bits(),
                                extra: vec![],
                            }
                        })
                        .collect();
                    Some(DirtyRow {
                        row_index: idx,
                        cells,
                        semantic_mark: 0,
                        mark_metadata: vec![],
                    })
                })
                .collect();

            let (bg_r, bg_g, bg_b) = match g.dynamic_bg {
                Some(rgb) => (rgb.r, rgb.g, rgb.b),
                None => (0, 0, 0),
            };
            let update = RenderUpdate {
                pane_id: req.pane_id,
                seqno: g.seqno,
                cursor_x: g.cursor.col,
                cursor_y: g.cursor.row,
                cursor_style: 0,
                cursor_visible: g.cursor.visible,
                bg_r,
                bg_g,
                bg_b,
                dirty_rows,
            };

            match update.encode() {
                Ok(payload) => match Frame::new(MSG_RENDER_UPDATE, frame.serial, payload) {
                    Ok(f) => RequestResult::Response(f),
                    Err(e) => {
                        eprintln!("failed to create RenderUpdate frame: {e}");
                        RequestResult::NoResponse
                    }
                },
                Err(e) => {
                    eprintln!("failed to encode RenderUpdate: {e}");
                    RequestResult::NoResponse
                }
            }
        }
        MSG_PING => match Frame::new(MSG_PONG, frame.serial, vec![]) {
            Ok(f) => RequestResult::Response(f),
            Err(_) => RequestResult::NoResponse,
        },
        _ => RequestResult::NoResponse,
    }
}

async fn read_frame(stream: &mut UnixStream, buf: &mut BytesMut) -> io::Result<()> {
    let n = stream.read_buf(buf).await?;
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "client disconnected",
        ));
    }
    Ok(())
}

async fn write_frame(
    stream: &mut UnixStream,
    codec: &mut FrameCodec,
    buf: &mut BytesMut,
    frame: Frame,
) -> io::Result<()> {
    buf.clear();
    codec.encode(frame, buf)?;
    stream.write_all(buf).await?;
    Ok(())
}

/// Encode a mouse event as an SGR escape sequence.
#[allow(clippy::match_same_arms)] // press/release intentionally share button encoding
fn encode_mouse_sgr(msg: &MouseInput, sgr: bool) -> String {
    // SGR button encoding: 0=left, 1=middle, 2=right, 64+=scroll
    let button = match msg.event_type {
        0 => msg.button,      // press
        1 => msg.button,      // release
        2 => 32 + msg.button, // motion (add 32)
        3 => 64,              // scroll up
        4 => 65,              // scroll down
        _ => return String::new(),
    };
    // Encode modifier bits: shift=4, alt=8, ctrl=16.
    let button = button | (msg.modifiers & 0x1C);
    // 1-based coordinates.
    let x = msg.x.saturating_add(1);
    let y = msg.y.saturating_add(1);

    if sgr {
        // SGR format: CSI < button ; x ; y M/m
        let suffix = if msg.event_type == 1 { 'm' } else { 'M' };
        format!("\x1b[<{button};{x};{y}{suffix}")
    } else {
        // Legacy X10 format (limited to 223 cols/rows).
        // Release is signaled by button=3 (no M/m distinction in X10).
        let legacy_button = if msg.event_type == 1 { 3 } else { button };
        let cx = ((x + 32).min(255)) as u8;
        let cy = ((y + 32).min(255)) as u8;
        let cb = legacy_button.saturating_add(32);
        format!("\x1b[M{}{}{}", cb as char, cx as char, cy as char)
    }
}

/// Thin Write adapter for a borrowed file descriptor.
/// Retries on `WouldBlock` since the PTY fd is non-blocking for async reads.
struct FdWriter<'a>(rustix::fd::BorrowedFd<'a>);

impl std::io::Write for FdWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            match rustix::io::write(self.0, buf) {
                Ok(n) => return Ok(n),
                Err(e) if e == rustix::io::Errno::AGAIN => {
                    std::thread::yield_now();
                }
                Err(e) => return Err(io::Error::from_raw_os_error(e.raw_os_error())),
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Resolve a terminal `Color` to RGB bytes using the palette.
fn resolve_color(
    color: Color,
    palette: &[Rgb; 256],
    def_r: u8,
    def_g: u8,
    def_b: u8,
) -> (u8, u8, u8, u8) {
    match color {
        Color::Default => (def_r, def_g, def_b, 0),
        Color::Named(n) => {
            let rgb = palette[n as u8 as usize];
            (rgb.r, rgb.g, rgb.b, 1)
        }
        Color::Indexed(i) => {
            let rgb = palette[usize::from(i)];
            (rgb.r, rgb.g, rgb.b, 2)
        }
        Color::Rgb(r, g, b) => (r, g, b, 3),
    }
}
