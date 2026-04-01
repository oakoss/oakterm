//! Daemon server: PTY read loop, Unix socket listener, client connections.

use crate::socket::socket_path;
use bytes::BytesMut;
use oakterm_protocol::frame::{Frame, FrameCodec};
use oakterm_protocol::input::{KeyInput, MouseInput, Resize};
use oakterm_protocol::message::{
    Bell, ClientHello, ErrorCode, ErrorMessage, FindPrompt, GetScrollback, HandshakeStatus,
    MSG_CLIENT_HELLO, MSG_DETACH, MSG_DIRTY_NOTIFY, MSG_FIND_PROMPT, MSG_GET_RENDER_UPDATE,
    MSG_GET_SCROLLBACK, MSG_KEY_INPUT, MSG_MOUSE_INPUT, MSG_PING, MSG_PONG, MSG_RENDER_UPDATE,
    MSG_RESIZE, MSG_SCROLLBACK_DATA, MSG_SEARCH_CLOSE, MSG_SEARCH_NEXT, MSG_SEARCH_PREV,
    MSG_SEARCH_SCROLLBACK, PaneExited, PromptPosition, ScrollbackData, SearchDirection, SearchNav,
    SearchResults, SearchScrollback, ServerHello, TitleChanged,
};
use oakterm_protocol::render::{DirtyNotify, DirtyRow, GetRenderUpdate, RenderUpdate, WireCell};
use oakterm_terminal::grid::cell::{Color, Rgb};
use oakterm_terminal::grid::row::{MarkMetadata, SemanticMark};
use oakterm_terminal::grid::{ScreenId, ScreenSet};
use oakterm_terminal::handler;
use std::io;
use std::os::unix::io::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tracing::{debug, error, info, warn};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, watch};
use tokio_util::codec::{Decoder, Encoder};

/// Handshake timeout per Spec-0001.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Arrow key repeats per wheel tick for mode 1007 alt-screen scroll.
const ALT_SCROLL_LINES: usize = 3;

/// PTY lifecycle state machine.
///
/// Transitions: `NotSpawned` -> `Running` | `Failed` (terminal);
/// `Running` -> `Exited` (terminal). First client Resize triggers spawn.
enum PtyState {
    /// Waiting for first client Resize to determine dimensions.
    NotSpawned,
    /// Master fd for writes and resizes. The `Pty` struct is owned by the read loop.
    Running(RawFd),
    /// PTY spawn failed; terminal state. The error string is returned to any
    /// client that sends a subsequent Resize.
    Failed(String),
    /// PTY read loop exited (master fd EOF or error).
    Exited { exit_code: i32 },
}

/// Configuration for the cold disk scrollback archive.
pub struct ArchiveConfig {
    /// Maximum archive size in bytes.
    pub max_bytes: u64,
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
    /// When `Some`, cold disk archiving is enabled with the given limits.
    archive_config: Option<ArchiveConfig>,
}

impl Daemon {
    /// Create a new daemon with the default socket path.
    ///
    /// # Errors
    /// Returns an error if the socket path cannot be resolved.
    pub fn new(cols: u16, rows: u16) -> io::Result<Self> {
        Ok(Self::with_socket_path(cols, rows, socket_path()?))
    }

    /// Create a new daemon bound to a specific socket path.
    #[must_use]
    pub fn with_socket_path(cols: u16, rows: u16, socket_path: std::path::PathBuf) -> Self {
        let (dirty_tx, dirty_rx) = watch::channel(0u64);
        Self {
            screens: Arc::new(Mutex::new(ScreenSet::new(cols, rows))),
            dirty_tx,
            dirty_rx,
            socket_path,
            persist: false,
            archive_config: None,
        }
    }

    /// Enable persist mode: daemon stays running with zero clients.
    pub fn set_persist(&mut self, persist: bool) {
        self.persist = persist;
    }

    pub fn set_archive_config(&mut self, config: ArchiveConfig) {
        self.archive_config = Some(config);
    }

    /// Listen for connections. The PTY spawns on the first client Resize
    /// so the shell starts at the correct dimensions.
    ///
    /// # Errors
    /// Returns an error if the listener fails to start.
    pub async fn run(&self) -> io::Result<()> {
        // Set up cold disk archive if configured.
        if let Some(config) = &self.archive_config {
            let pid = std::process::id();
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let session_id = format!("{pid}-{ts}");
            let base_dir = archive_base_dir();
            if let Err(e) =
                oakterm_terminal::scroll::archive_manager::ArchiveManager::cleanup_orphans(
                    &base_dir,
                    &session_id,
                )
            {
                warn!(error = %e, "failed to clean up orphaned archive dirs");
            }
            let session_dir = base_dir.join(&session_id).join("scrollback-0");
            match oakterm_terminal::scroll::archive_manager::ArchiveManager::new(
                session_dir,
                config.max_bytes,
            ) {
                Ok(mgr) => {
                    self.screens.lock().await.set_archive(mgr);
                    info!("scrollback archive enabled");
                }
                Err(e) => {
                    warn!(error = %e, "failed to create scrollback archive, continuing without");
                }
            }
        }

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
        let mut next_conn_id: u64 = 0;

        loop {
            tokio::select! {
                result = listener.accept() => {
                    let (stream, _) = result?;
                    let conn_id = next_conn_id;
                    next_conn_id += 1;
                    let screens = Arc::clone(&self.screens);
                    let dirty_rx = self.dirty_rx.clone();
                    let dirty_tx = self.dirty_tx.clone();
                    let pty = Arc::clone(&pty_state);
                    let count = Arc::clone(&client_count);
                    let tx = shutdown_tx.clone();

                    count.fetch_add(1, Ordering::AcqRel);
                    info!(conn_id, "client connected");

                    tokio::spawn(async move {
                        handle_client(conn_id, stream, screens, dirty_rx, dirty_tx, pty).await;
                        let remaining = count.fetch_sub(1, Ordering::AcqRel) - 1;
                        info!(conn_id, remaining, "client disconnected");
                        if remaining == 0 && !persist {
                            let _ = tx.send(true);
                        }
                    });
                }
                _ = shutdown_rx.wait_for(|&v| v) => {
                    info!("last client disconnected, shutting down");
                    break;
                }
            }
        }

        // Shut down the archive if configured.
        if let Some(archive) = self.screens.lock().await.archive_mut() {
            let parent = archive
                .session_dir()
                .parent()
                .map(std::path::Path::to_path_buf);
            if let Err(e) = archive.shutdown() {
                warn!(error = %e, "archive shutdown failed");
            }
            // Parent should be empty after shutdown removed the scrollback subdirectory.
            if let Some(p) = parent {
                if let Err(e) = std::fs::remove_dir(&p) {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        warn!(error = %e, path = %p.display(), "failed to remove session directory");
                    }
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
        if let Err(e) = std::fs::remove_file(&self.socket_path) {
            if e.kind() != io::ErrorKind::NotFound {
                warn!(error = %e, path = %self.socket_path.display(), "failed to remove socket on drop");
            }
        }
    }
}

/// Read PTY output, feed to VT parser, update Grid.
async fn pty_read_loop(
    pty: oakterm_pty::Pty,
    screens: Arc<Mutex<ScreenSet>>,
    dirty_tx: watch::Sender<u64>,
    pty_state: Arc<Mutex<PtyState>>,
) {
    use tokio::io::unix::AsyncFd;

    let raw_fd = pty.master_raw_fd();
    let pid = pty.child_pid();

    // Set non-blocking for tokio `AsyncFd`.
    let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(raw_fd) };
    match rustix::fs::fcntl_getfl(borrowed) {
        Ok(flags) => {
            if let Err(e) = rustix::fs::fcntl_setfl(borrowed, flags | rustix::fs::OFlags::NONBLOCK)
            {
                error!(error = %e, "failed to set PTY non-blocking");
                return;
            }
        }
        Err(e) => {
            error!(error = %e, "failed to get PTY fd flags");
            return;
        }
    }

    let Ok(async_fd) = AsyncFd::new(raw_fd) else {
        error!("failed to create AsyncFd for PTY");
        return;
    };

    debug!(pid, "PTY read loop started");
    let mut buf = [0u8; 4096];

    let exit_reason = loop {
        let Ok(mut guard) = async_fd.readable().await else {
            break "readable poll failed";
        };

        match guard.try_io(|inner| {
            let fd = inner.get_ref();
            let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(*fd) };
            rustix::io::read(borrowed, &mut buf)
                .map_err(|e| io::Error::from_raw_os_error(e.raw_os_error()))
        }) {
            Ok(Ok(0)) => break "EOF",
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
            Ok(Err(e)) => {
                warn!(error = %e, "PTY read error");
                break "read error";
            }
            Err(_would_block) => {}
        }
    };

    // Transition to Exited state. Exit code 0 is a placeholder — the real
    // status is lost in Pty::drop which calls child.kill() + child.wait()
    // but discards the result. Phase 1 should wait(2) before drop for the
    // real exit code.
    let exit_code = 0;
    info!(pid, exit_reason, exit_code, "PTY read loop ended");
    *pty_state.lock().await = PtyState::Exited { exit_code };

    // Bump dirty seqno so handle_client wakes, detects the Exited state,
    // and sends a PaneExited frame to the connected client.
    let _ = dirty_tx.send(u64::MAX);
}

/// Handle a single client connection.
#[allow(clippy::too_many_lines)]
async fn handle_client(
    conn_id: u64,
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
        debug!(
            conn_id,
            client = %client_hello.client_name,
            version = %format!("{}.{}", client_hello.protocol_version_major, client_hello.protocol_version_minor),
            "handshake received",
        );

        if client_hello.protocol_version_major != ClientHello::VERSION_MAJOR {
            warn!(
                conn_id,
                client_version = client_hello.protocol_version_major,
                server_version = ClientHello::VERSION_MAJOR,
                "version mismatch",
            );
            let hello = ServerHello {
                status: HandshakeStatus::VersionMismatch,
                protocol_version_major: ClientHello::VERSION_MAJOR,
                protocol_version_minor: ClientHello::VERSION_MINOR,
                server_version: env!("CARGO_PKG_VERSION").to_string(),
            };
            match hello.to_frame(frame.serial) {
                Ok(resp) => {
                    if let Err(e) = write_frame(&mut stream, &mut codec, &mut write_buf, resp).await
                    {
                        debug!(conn_id, error = %e, "failed to send version mismatch response");
                    }
                }
                Err(e) => {
                    warn!(conn_id, error = %e, "failed to encode version mismatch response");
                }
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

    match tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await {
        Ok(Ok(())) => debug!(conn_id, "handshake completed"),
        Ok(Err(e)) => {
            warn!(conn_id, error = %e, "handshake failed");
            return;
        }
        Err(_) => {
            warn!(conn_id, "handshake timed out");
            return;
        }
    }

    // Main client loop.
    let mut pane_exit_sent = false;
    'outer: loop {
        tokio::select! {
            result = dirty_rx.changed() => {
                if result.is_err() {
                    break;
                }

                // Check if PTY exited and send PaneExited once.
                if !pane_exit_sent {
                    let state = pty_state.lock().await;
                    if let PtyState::Exited { exit_code } = *state {
                        drop(state);
                        pane_exit_sent = true;
                        debug!(conn_id, exit_code, "sending PaneExited to client");
                        let msg = PaneExited { pane_id: 0, exit_code };
                        match msg.to_frame() {
                            Ok(f) => {
                                if write_frame(&mut stream, &mut codec, &mut write_buf, f).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                error!(conn_id, error = %e, "failed to encode PaneExited frame");
                                break;
                            }
                        }
                    }
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
                    match msg.to_frame() {
                        Ok(f) => {
                            if write_frame(&mut stream, &mut codec, &mut write_buf, f).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => warn!(conn_id, error = %e, "failed to encode TitleChanged frame"),
                    }
                }
                if let Some(msg) = bell_msg {
                    match msg.to_frame() {
                        Ok(f) => {
                            if write_frame(&mut stream, &mut codec, &mut write_buf, f).await.is_err() {
                                break;
                            }
                        }
                        Err(e) => warn!(conn_id, error = %e, "failed to encode Bell frame"),
                    }
                }

                let notify = DirtyNotify { pane_id: 0 };
                let Ok(frame) = Frame::new(MSG_DIRTY_NOTIFY, 0, notify.encode()) else {
                    error!(conn_id, "failed to create DirtyNotify frame");
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
                    match handle_request(conn_id, &frame, &screens, &pty_state, &dirty_tx).await {
                        RequestResult::Response(response) => {
                            if write_frame(&mut stream, &mut codec, &mut write_buf, response).await.is_err() {
                                break 'outer;
                            }
                        }
                        RequestResult::Detach => {
                            debug!(conn_id, "client detached");
                            break 'outer;
                        }
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
    conn_id: u64,
    frame: &Frame,
    screens: &Arc<Mutex<ScreenSet>>,
    pty_state: &Arc<Mutex<PtyState>>,
    dirty_tx: &watch::Sender<u64>,
) -> RequestResult {
    match frame.msg_type {
        MSG_KEY_INPUT => {
            let Ok(msg) = KeyInput::decode(&frame.payload) else {
                warn!(conn_id, "malformed KeyInput payload");
                return make_error_response(
                    conn_id,
                    frame.serial,
                    ErrorCode::MalformedPayload,
                    "malformed KeyInput",
                );
            };
            let state = pty_state.lock().await;
            match *state {
                PtyState::Running(fd) => {
                    drop(state);
                    if !msg.key_data.is_empty() {
                        let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(fd) };
                        if let Err(e) = rustix::io::write(borrowed, &msg.key_data) {
                            warn!(conn_id, error = %e, "PTY write failed");
                        }
                    }
                }
                PtyState::Exited { .. } | PtyState::Failed(_) => {
                    debug!(conn_id, "KeyInput ignored: PTY not running");
                }
                PtyState::NotSpawned => {
                    debug!(conn_id, "KeyInput ignored: PTY not spawned");
                }
            }
            RequestResult::NoResponse
        }
        MSG_MOUSE_INPUT => {
            let Ok(msg) = MouseInput::decode(&frame.payload) else {
                warn!(conn_id, "malformed MouseInput payload");
                return make_error_response(
                    conn_id,
                    frame.serial,
                    ErrorCode::MalformedPayload,
                    "malformed MouseInput",
                );
            };
            let state = pty_state.lock().await;
            if let PtyState::Running(fd) = *state {
                drop(state);

                // Read all needed mode/screen state while holding the lock.
                let s = screens.lock().await;
                let g = s.active_grid();
                let sgr = g.modes.get(1006);
                let click = g.modes.get(1000);
                let cell_motion = g.modes.get(1002);
                let all_motion = g.modes.get(1003);
                let alt_scroll = g.modes.get(1007);
                let decckm = g.modes.get(1);
                let on_alt = s.active_screen() == ScreenId::Alternate;
                drop(s);

                let mouse_reporting = click || cell_motion || all_motion;
                // Shift (bit 2) bypasses mouse tracking — don't forward to PTY.
                // Defense-in-depth: the GUI also filters Shift events.
                let shift_held = msg.modifiers & 4 != 0;
                let should_send = if shift_held {
                    false
                } else {
                    match msg.event_type {
                        0 | 1 | 3 | 4 => mouse_reporting,
                        2 => cell_motion || all_motion,
                        _ => false,
                    }
                };

                if should_send {
                    let seq = encode_mouse_sgr(&msg, sgr);
                    if !seq.is_empty() {
                        let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(fd) };
                        if let Err(e) = rustix::io::write(borrowed, seq.as_bytes()) {
                            warn!(conn_id, error = %e, "PTY mouse write failed");
                        }
                    }
                } else if (msg.event_type == 3 || msg.event_type == 4) && on_alt && alt_scroll {
                    // Mode 1007: convert wheel to arrow keys on alternate screen.
                    let arrow: &[u8] = match (msg.event_type, decckm) {
                        (3, true) => b"\x1bOA",
                        (3, false) => b"\x1b[A",
                        (4, true) => b"\x1bOB",
                        (_, _) => b"\x1b[B",
                    };
                    let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(fd) };
                    for _ in 0..ALT_SCROLL_LINES {
                        if let Err(e) = rustix::io::write(borrowed, arrow) {
                            warn!(conn_id, error = %e, "PTY alt-scroll write failed");
                            break;
                        }
                    }
                }
            }
            RequestResult::NoResponse
        }
        MSG_RESIZE => {
            let Ok(msg) = Resize::decode(&frame.payload) else {
                warn!(conn_id, "malformed Resize payload");
                return make_error_response(
                    conn_id,
                    frame.serial,
                    ErrorCode::MalformedPayload,
                    "malformed Resize",
                );
            };
            let mut state = pty_state.lock().await;
            match *state {
                PtyState::NotSpawned => {
                    info!(conn_id, cols = msg.cols, rows = msg.rows, "spawning PTY");
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
                            let pid = pty.child_pid();
                            *state = PtyState::Running(fd);
                            drop(state);

                            info!(pid, "PTY spawned");

                            {
                                let mut s = screens.lock().await;
                                s.resize_all(msg.cols, msg.rows);
                            }

                            let screens_clone = Arc::clone(screens);
                            let dtx = dirty_tx.clone();
                            let pty_clone = Arc::clone(pty_state);
                            tokio::spawn(pty_read_loop(pty, screens_clone, dtx, pty_clone));
                        }
                        Err(e) => {
                            error!(conn_id, error = %e, "failed to spawn PTY");
                            *state = PtyState::Failed(e.to_string());
                            drop(state);
                            return make_error_response(
                                conn_id,
                                frame.serial,
                                ErrorCode::InternalError,
                                &format!("PTY spawn failed: {e}"),
                            );
                        }
                    }
                }
                PtyState::Running(fd) => {
                    drop(state);
                    let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(fd) };
                    if let Err(e) = oakterm_pty::resize_fd(
                        borrowed,
                        msg.cols,
                        msg.rows,
                        msg.pixel_width,
                        msg.pixel_height,
                    ) {
                        warn!(conn_id, error = %e, "PTY resize failed");
                    } else {
                        let mut s = screens.lock().await;
                        s.resize_all(msg.cols, msg.rows);
                    }
                }
                PtyState::Failed(ref reason) => {
                    warn!(conn_id, reason, "Resize ignored: PTY previously failed");
                    return make_error_response(
                        conn_id,
                        frame.serial,
                        ErrorCode::InternalError,
                        &format!("PTY failed: {reason}"),
                    );
                }
                PtyState::Exited { exit_code } => {
                    debug!(conn_id, exit_code, "Resize ignored: PTY exited");
                    return make_error_response(
                        conn_id,
                        frame.serial,
                        ErrorCode::PaneExited,
                        "PTY has exited",
                    );
                }
            }
            RequestResult::NoResponse
        }
        MSG_DETACH => RequestResult::Detach,
        MSG_GET_RENDER_UPDATE => {
            let Ok(req) = GetRenderUpdate::decode(&frame.payload) else {
                warn!(conn_id, "malformed GetRenderUpdate payload");
                return make_error_response(
                    conn_id,
                    frame.serial,
                    ErrorCode::MalformedPayload,
                    "malformed GetRenderUpdate",
                );
            };
            let s = screens.lock().await;
            let g = s.active_grid();
            let dirty_indices = g.dirty_rows(req.since_seqno);

            let dirty_rows: Vec<DirtyRow> = dirty_indices
                .iter()
                .filter_map(|&idx| {
                    let row = g.lines.get(idx as usize)?;
                    Some(row_to_wire(row, idx, &g.palette))
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
                cursor_style: g.cursor.style.to_wire(),
                cursor_visible: g.cursor.visible,
                bg_r,
                bg_g,
                bg_b,
                bracketed_paste: g.modes.get(2004),
                dirty_rows,
            };

            match update.encode() {
                Ok(payload) => match Frame::new(MSG_RENDER_UPDATE, frame.serial, payload) {
                    Ok(f) => RequestResult::Response(f),
                    Err(e) => {
                        error!(conn_id, error = %e, "failed to create RenderUpdate frame");
                        make_error_response(
                            conn_id,
                            frame.serial,
                            ErrorCode::InternalError,
                            "RenderUpdate frame error",
                        )
                    }
                },
                Err(e) => {
                    error!(conn_id, error = %e, "failed to encode RenderUpdate");
                    make_error_response(
                        conn_id,
                        frame.serial,
                        ErrorCode::InternalError,
                        "RenderUpdate encode error",
                    )
                }
            }
        }
        MSG_GET_SCROLLBACK => {
            let Ok(req) = GetScrollback::decode(&frame.payload) else {
                warn!(conn_id, "malformed GetScrollback payload");
                return make_error_response(
                    conn_id,
                    frame.serial,
                    ErrorCode::MalformedPayload,
                    "malformed GetScrollback",
                );
            };
            let s = screens.lock().await;
            let buf = s.scrollback();
            // Convert negative start_row to buffer index.
            // SAFETY: buf.len() fits in i64 — HotBuffer is capped at 50MB (~250K rows).
            #[allow(clippy::cast_possible_wrap)]
            let buf_len = buf.len() as i64;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let start_idx = (buf_len + req.start_row).max(0) as usize;
            let end_idx = (start_idx + req.count as usize).min(buf.len());
            let has_more = start_idx > 0;

            let rows: Vec<DirtyRow> = (start_idx..end_idx)
                .filter_map(|i| {
                    let row = buf.get(i)?;
                    // row_index is 0 for scrollback; client uses positional order.
                    Some(row_to_wire(row, 0, &s.active_grid().palette))
                })
                .collect();

            let data = ScrollbackData {
                pane_id: req.pane_id,
                start_row: req.start_row,
                has_more,
                rows,
            };

            match data.encode() {
                Ok(payload) => match Frame::new(MSG_SCROLLBACK_DATA, frame.serial, payload) {
                    Ok(f) => RequestResult::Response(f),
                    Err(e) => {
                        error!(conn_id, error = %e, "failed to create ScrollbackData frame");
                        make_error_response(
                            conn_id,
                            frame.serial,
                            ErrorCode::InternalError,
                            "ScrollbackData frame error",
                        )
                    }
                },
                Err(e) => {
                    error!(conn_id, error = %e, "failed to encode ScrollbackData");
                    make_error_response(
                        conn_id,
                        frame.serial,
                        ErrorCode::InternalError,
                        "ScrollbackData encode error",
                    )
                }
            }
        }
        MSG_FIND_PROMPT => {
            let Ok(req) = FindPrompt::decode(&frame.payload) else {
                warn!(conn_id, "malformed FindPrompt payload");
                return make_error_response(
                    conn_id,
                    frame.serial,
                    ErrorCode::MalformedPayload,
                    "malformed FindPrompt",
                );
            };
            let s = screens.lock().await;
            let found_offset =
                find_prompt_in_buffer(s.scrollback(), req.from_offset, req.direction);
            let response = PromptPosition {
                pane_id: req.pane_id,
                offset: found_offset,
            };

            match response.to_frame(frame.serial) {
                Ok(f) => RequestResult::Response(f),
                Err(e) => {
                    error!(conn_id, error = %e, "failed to create PromptPosition frame");
                    make_error_response(
                        conn_id,
                        frame.serial,
                        ErrorCode::InternalError,
                        "PromptPosition frame error",
                    )
                }
            }
        }
        MSG_SEARCH_SCROLLBACK => {
            let Ok(req) = SearchScrollback::decode(&frame.payload) else {
                warn!(conn_id, "malformed SearchScrollback payload");
                return make_error_response(
                    conn_id,
                    frame.serial,
                    ErrorCode::MalformedPayload,
                    "malformed SearchScrollback",
                );
            };
            let mode = if req.flags.regex() {
                oakterm_terminal::search::SearchMode::Regex
            } else if req.flags.case_sensitive() {
                oakterm_terminal::search::SearchMode::CaseSensitive
            } else {
                oakterm_terminal::search::SearchMode::SmartCase
            };
            let engine = match oakterm_terminal::search::SearchEngine::new(&req.query, mode) {
                Ok(e) => e,
                Err(e) => {
                    return make_error_response(
                        conn_id,
                        frame.serial,
                        ErrorCode::MalformedPayload,
                        &format!("invalid search pattern: {e}"),
                    );
                }
            };
            let mut s = screens.lock().await;
            s.set_search(engine);
            s.run_search();
            build_search_response(conn_id, &s, req.pane_id, frame.serial)
        }
        MSG_SEARCH_NEXT => {
            let Ok(req) = SearchNav::decode(&frame.payload) else {
                warn!(conn_id, "malformed SearchNext payload");
                return make_error_response(
                    conn_id,
                    frame.serial,
                    ErrorCode::MalformedPayload,
                    "malformed SearchNext",
                );
            };
            let mut s = screens.lock().await;
            if let Some(engine) = s.search_mut() {
                engine.next();
            } else {
                warn!(conn_id, "SearchNext with no active search");
            }
            build_search_response(conn_id, &s, req.pane_id, frame.serial)
        }
        MSG_SEARCH_PREV => {
            let Ok(req) = SearchNav::decode(&frame.payload) else {
                warn!(conn_id, "malformed SearchPrev payload");
                return make_error_response(
                    conn_id,
                    frame.serial,
                    ErrorCode::MalformedPayload,
                    "malformed SearchPrev",
                );
            };
            let mut s = screens.lock().await;
            if let Some(engine) = s.search_mut() {
                engine.prev();
            } else {
                warn!(conn_id, "SearchPrev with no active search");
            }
            build_search_response(conn_id, &s, req.pane_id, frame.serial)
        }
        MSG_SEARCH_CLOSE => {
            // Idempotent, no payload — fire and forget.
            screens.lock().await.clear_search();
            RequestResult::NoResponse
        }
        MSG_PING => match Frame::new(MSG_PONG, frame.serial, vec![]) {
            Ok(f) => RequestResult::Response(f),
            Err(e) => {
                error!(conn_id, error = %e, "failed to create Pong frame");
                make_error_response(
                    conn_id,
                    frame.serial,
                    ErrorCode::InternalError,
                    "Pong frame error",
                )
            }
        },
        unknown => {
            warn!(conn_id, msg_type = unknown, "unknown message type");
            make_error_response(
                conn_id,
                frame.serial,
                ErrorCode::InvalidMessage,
                &format!("unknown message type: 0x{unknown:04X}"),
            )
        }
    }
}

/// Build an error response frame, falling back to `NoResponse` if encoding fails.
fn make_error_response(conn_id: u64, serial: u32, code: ErrorCode, message: &str) -> RequestResult {
    let err = ErrorMessage {
        code: code as u32,
        message: message.to_string(),
    };
    match err.to_frame(serial) {
        Ok(f) => RequestResult::Response(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to encode error response");
            RequestResult::NoResponse
        }
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

/// Resolve the base directory for scrollback archive files.
///
/// macOS: `$TMPDIR/oakterm-{uid}`. Linux: `$XDG_RUNTIME_DIR/oakterm`
/// (falls back to `$TMPDIR/oakterm-{uid}`).
fn archive_base_dir() -> std::path::PathBuf {
    #[cfg(target_os = "macos")]
    {
        let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let uid = rustix::process::getuid().as_raw();
        std::path::PathBuf::from(tmpdir).join(format!("oakterm-{uid}"))
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
            std::path::PathBuf::from(xdg).join("oakterm")
        } else {
            let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
            let uid = rustix::process::getuid().as_raw();
            std::path::PathBuf::from(tmpdir).join(format!("oakterm-{uid}"))
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        // No per-user isolation — unsupported platform, exists for compilation only.
        std::env::temp_dir().join("oakterm")
    }
}

#[allow(clippy::cast_possible_wrap)]
fn build_search_response(
    conn_id: u64,
    screens: &oakterm_terminal::grid::ScreenSet,
    pane_id: u32,
    serial: u32,
) -> RequestResult {
    let (total_matches, active_index, active_row_offset, capped) = match screens.search() {
        Some(engine) => {
            let total = u32::try_from(engine.match_count()).unwrap_or(u32::MAX);
            let (idx, offset) = match engine.active_match() {
                Some(m) => {
                    let buf_len = screens.scrollback().len();
                    let neg_offset = m.row as i64 - buf_len as i64;
                    (
                        engine
                            .active_index()
                            .map(|i| u32::try_from(i).unwrap_or(u32::MAX)),
                        neg_offset,
                    )
                }
                None => (None, 0),
            };
            (total, idx, offset, engine.is_capped())
        }
        None => (0, None, 0, false),
    };

    let response = SearchResults {
        pane_id,
        total_matches,
        active_index,
        active_row_offset,
        capped,
        visible_matches: Vec::new(),
    };

    match response.to_frame(serial) {
        Ok(f) => RequestResult::Response(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to create SearchResults frame");
            make_error_response(
                conn_id,
                serial,
                ErrorCode::InternalError,
                "SearchResults frame error",
            )
        }
    }
}

/// Returns `Some(negative_offset)` if found, `None` otherwise. The offset
/// uses the same coordinate space as `GetScrollback.start_row`.
fn find_prompt_in_buffer(
    buf: &oakterm_terminal::scroll::HotBuffer,
    from_offset: i64,
    direction: SearchDirection,
) -> Option<i64> {
    // SAFETY: buf.len() fits in i64 — HotBuffer is capped at 50MB (~250K rows).
    #[allow(clippy::cast_possible_wrap)]
    let buf_len = buf.len() as i64;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let from_idx = (buf_len + from_offset).max(0) as usize;

    let found_idx = match direction {
        SearchDirection::Older => (0..from_idx).rev().find(|&i| {
            buf.get(i)
                .is_some_and(|r| r.semantic_mark == SemanticMark::PromptStart)
        }),
        SearchDirection::Newer => {
            let start = (from_idx + 1).min(buf.len());
            (start..buf.len()).find(|&i| {
                buf.get(i)
                    .is_some_and(|r| r.semantic_mark == SemanticMark::PromptStart)
            })
        }
    };

    found_idx.map(|idx| {
        #[allow(clippy::cast_possible_wrap)]
        let offset = idx as i64 - buf_len;
        offset
    })
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

/// Convert a terminal `Row` to a wire `DirtyRow` using the given palette.
fn row_to_wire(
    row: &oakterm_terminal::grid::row::Row,
    row_index: u16,
    palette: &[Rgb; 256],
) -> DirtyRow {
    let cells: Vec<WireCell> = row
        .cells
        .iter()
        .map(|c| {
            let (fg_r, fg_g, fg_b, fg_type) = resolve_color(c.fg, palette, 255, 255, 255);
            let (bg_r, bg_g, bg_b, bg_type) = resolve_color(c.bg, palette, 0, 0, 0);
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
    let mark_metadata = row
        .mark_metadata
        .as_ref()
        .map_or_else(Vec::new, MarkMetadata::to_wire_bytes);
    DirtyRow {
        row_index,
        cells,
        semantic_mark: row.semantic_mark.to_wire(),
        mark_metadata,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oakterm_terminal::grid::row::Row;
    use oakterm_terminal::scroll::HotBuffer;

    /// Push rows into a buffer, marking specific indices as `PromptStart`.
    fn buffer_with_prompts(total: usize, prompt_indices: &[usize]) -> HotBuffer {
        let mut buf = HotBuffer::new(10 * 1024 * 1024);
        for i in 0..total {
            let mut row = Row::new(80);
            if prompt_indices.contains(&i) {
                row.semantic_mark = SemanticMark::PromptStart;
            }
            buf.push(row);
        }
        buf
    }

    #[test]
    fn find_prompt_backward_finds_nearest() {
        // Rows: [P, _, _, P, _, _, _, _, _, _]  (P at 0 and 3)
        let buf = buffer_with_prompts(10, &[0, 3]);
        // Search backward from offset -5 (index 5)
        let result = find_prompt_in_buffer(&buf, -5, SearchDirection::Older);
        // Nearest prompt before index 5 is at index 3 → offset = 3 - 10 = -7
        assert_eq!(result, Some(-7));
    }

    #[test]
    fn find_prompt_backward_skips_current() {
        // Rows: [_, _, _, P, _, _]  (P at 3)
        let buf = buffer_with_prompts(6, &[3]);
        // Search backward from index 3 (offset -3): should skip index 3 itself
        let result = find_prompt_in_buffer(&buf, -3, SearchDirection::Older);
        assert_eq!(result, None);
    }

    #[test]
    fn find_prompt_forward_finds_nearest() {
        // Rows: [_, _, _, _, P, _, _, P, _, _]  (P at 4 and 7)
        let buf = buffer_with_prompts(10, &[4, 7]);
        // Search forward from offset -8 (index 2)
        let result = find_prompt_in_buffer(&buf, -8, SearchDirection::Newer);
        // Nearest prompt after index 2 is at index 4 → offset = 4 - 10 = -6
        assert_eq!(result, Some(-6));
    }

    #[test]
    fn find_prompt_forward_skips_current() {
        // Rows: [_, _, _, P, _, _]  (P at 3)
        let buf = buffer_with_prompts(6, &[3]);
        // Search forward from index 3 (offset -3): should skip index 3
        let result = find_prompt_in_buffer(&buf, -3, SearchDirection::Newer);
        assert_eq!(result, None);
    }

    #[test]
    fn find_prompt_empty_buffer() {
        let buf = HotBuffer::new(1024);
        assert_eq!(find_prompt_in_buffer(&buf, 0, SearchDirection::Older), None);
        assert_eq!(find_prompt_in_buffer(&buf, 0, SearchDirection::Newer), None);
    }

    #[test]
    fn find_prompt_no_prompts_in_buffer() {
        let buf = buffer_with_prompts(10, &[]);
        assert_eq!(
            find_prompt_in_buffer(&buf, -5, SearchDirection::Older),
            None
        );
        assert_eq!(
            find_prompt_in_buffer(&buf, -5, SearchDirection::Newer),
            None
        );
    }

    #[test]
    fn find_prompt_offset_clamped_to_zero() {
        // offset more negative than buffer length → clamped to index 0
        let buf = buffer_with_prompts(5, &[2]);
        let result = find_prompt_in_buffer(&buf, -100, SearchDirection::Newer);
        assert_eq!(result, Some(-3)); // index 2 → 2 - 5 = -3
    }

    #[test]
    fn find_prompt_at_live_view() {
        // offset 0 means live view (from_idx = buf.len())
        let buf = buffer_with_prompts(5, &[1, 3]);
        // Backward from live should find the last prompt (index 3)
        let result = find_prompt_in_buffer(&buf, 0, SearchDirection::Older);
        assert_eq!(result, Some(-2)); // index 3 → 3 - 5 = -2
        // Forward from live: nothing after buf.len()
        let result = find_prompt_in_buffer(&buf, 0, SearchDirection::Newer);
        assert_eq!(result, None);
    }

    #[test]
    fn find_prompt_offset_roundtrip() {
        // Verify the offset produced by find_prompt_in_buffer converts back
        // to the correct viewport_offset via checked_neg + u32::try_from.
        let buf = buffer_with_prompts(100, &[25, 50, 75]);
        let offset = find_prompt_in_buffer(&buf, -30, SearchDirection::Older)
            .expect("should find prompt at index 50");
        // from_idx = 100 + (-30) = 70; nearest prompt before 70 is at index 50
        assert_eq!(offset, -50); // 50 - 100 = -50
        // Client conversion: negate to get positive viewport_offset
        let viewport = offset
            .checked_neg()
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        assert_eq!(viewport, 50);
    }
}
