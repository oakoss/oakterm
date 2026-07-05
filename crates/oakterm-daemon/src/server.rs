//! Daemon server: Unix socket listener, handshake, and client connections.

use crate::framing::{read_frame, write_frame};
use crate::pane::{PaneManager, PtyState, SharedPane, lock_live_pane};
use crate::requests::{RequestResult, handle_request};
use crate::session::{default_state_dir, save_session};
use crate::socket::socket_path;
use bytes::BytesMut;
use oakterm_protocol::frame::{Frame, FrameCodec};
use oakterm_protocol::message::{
    Bell, ClientHello, ErrorCode, ErrorMessage, HandshakeStatus, MSG_CLIENT_HELLO,
    MSG_DIRTY_NOTIFY, MSG_REQUEST_SHUTDOWN, PaneExited, RequestShutdown, ServerHello, Shutdown,
    ShutdownAck, ShutdownAckStatus, ShutdownReason, TitleChanged,
};
use oakterm_protocol::render::DirtyNotify;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, watch};
use tokio_util::codec::Decoder;
use tracing::{debug, error, info, warn};

/// Handshake timeout per Spec-0001.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Configuration for the cold disk scrollback archive.
pub struct ArchiveConfig {
    /// Maximum archive size in bytes.
    pub max_bytes: u64,
}

/// Daemon state shared across tasks.
pub struct Daemon {
    panes: Arc<Mutex<PaneManager>>,
    dirty_tx: watch::Sender<u64>,
    dirty_rx: watch::Receiver<u64>,
    socket_path: std::path::PathBuf,
    /// When false (default), the daemon exits after the last client disconnects.
    /// When true, the daemon stays running with zero clients (headless/persist mode).
    persist: bool,
    /// When `Some`, cold disk archiving is enabled with the given limits.
    archive_config: Option<ArchiveConfig>,
    /// Pane created at construction; `run` attaches the archive to it.
    default_pane_id: u32,
    /// Directory for the Spec-0010 session file.
    state_dir: std::path::PathBuf,
}

/// Per-connection handle for client-requested shutdown (Spec-0001 0x07):
/// signals daemon termination and carries the session state dir.
#[derive(Clone)]
struct ShutdownCtx {
    term_tx: watch::Sender<Option<ShutdownReason>>,
    /// Request-dispatch barrier: frame handlers hold `read`, a shutdown
    /// save holds `write`, so the snapshot waits out in-flight mutations
    /// and none can be acknowledged after it and then lost on exit. A
    /// reader blocked during a save re-checks `term_tx` on wake — accepted:
    /// drop the frame (connection closing); aborted: dispatch normally.
    /// The write guard also serializes concurrent `RequestShutdown`s.
    gate: Arc<tokio::sync::RwLock<()>>,
    state_dir: Arc<std::path::PathBuf>,
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
        let mut mgr = PaneManager::new();
        let default_pane_id = mgr.create(cols, rows, String::new(), String::new());
        Self {
            panes: Arc::new(Mutex::new(mgr)),
            dirty_tx,
            dirty_rx,
            socket_path,
            persist: false,
            archive_config: None,
            default_pane_id,
            state_dir: default_state_dir(),
        }
    }

    /// Enable persist mode: daemon stays running with zero clients.
    pub fn set_persist(&mut self, persist: bool) {
        self.persist = persist;
    }

    /// Override the Spec-0010 session directory (tests; defaults to
    /// `$OAKTERM_STATE_DIR` or the platform state dir).
    pub fn set_state_dir(&mut self, dir: std::path::PathBuf) {
        self.state_dir = dir;
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
        if let Some(config) = &self.archive_config {
            self.setup_archive(config).await;
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

        // Phase 0: counts all clients. ADR-0007 says "last window closes" —
        // when control clients exist, filter by ClientType::Gui.
        let client_count = Arc::new(AtomicUsize::new(0));
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let (term_tx, mut term_rx) = watch::channel::<Option<ShutdownReason>>(None);
        let shutdown_ctx = ShutdownCtx {
            term_tx,
            gate: Arc::new(tokio::sync::RwLock::new(())),
            state_dir: Arc::new(self.state_dir.clone()),
        };
        let persist = self.persist;
        let mut next_conn_id: u64 = 0;
        let mut drain_clients = false;

        loop {
            tokio::select! {
                result = listener.accept() => {
                    let (stream, _) = result?;
                    let conn_id = next_conn_id;
                    next_conn_id += 1;
                    let panes = Arc::clone(&self.panes);
                    let dirty_rx = self.dirty_rx.clone();
                    let dirty_tx = self.dirty_tx.clone();
                    let count = Arc::clone(&client_count);
                    let tx = shutdown_tx.clone();
                    let shutdown = shutdown_ctx.clone();

                    count.fetch_add(1, Ordering::AcqRel);
                    info!(conn_id, "client connected");

                    tokio::spawn(async move {
                        handle_client(conn_id, stream, panes, dirty_rx, dirty_tx, shutdown).await;
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
                // Awaiting inside an arm would hold the select's output
                // (the watch guard) across the await; drain after the loop.
                _ = term_rx.wait_for(Option::is_some) => {
                    info!("client-requested shutdown, draining connections");
                    drain_clients = true;
                    break;
                }
            }
        }

        // Fail new connects fast: a client arriving now would otherwise
        // block on ServerHello until the process exits, seeing a raw reset
        // instead of the "daemon not running" path it already handles.
        drop(listener);
        if let Err(e) = std::fs::remove_file(&self.socket_path) {
            if e.kind() != io::ErrorKind::NotFound {
                warn!(error = %e, "failed to remove socket during shutdown");
            }
        }

        if drain_clients {
            // Spec-0001: wait up to 1 second for clients to close after
            // the Shutdown broadcast, then exit regardless.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            while client_count.load(Ordering::Acquire) > 0 && tokio::time::Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }

        // Shut down archives for all panes, one pane lock at a time.
        let all_panes = self.panes.lock().await.snapshot();
        for (_, pane) in all_panes {
            let mut pane = pane.lock().await;
            if let Some(archive) = pane.screens.archive_mut() {
                let parent = archive
                    .session_dir()
                    .parent()
                    .map(std::path::Path::to_path_buf);
                if let Err(e) = archive.shutdown() {
                    warn!(error = %e, "archive shutdown failed");
                }
                if let Some(p) = parent {
                    if let Err(e) = std::fs::remove_dir(&p) {
                        if e.kind() != std::io::ErrorKind::NotFound {
                            warn!(error = %e, path = %p.display(), "failed to remove session directory");
                        }
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

    /// Create the cold disk archive and attach it to the default pane.
    /// Failures are logged and the daemon continues without archiving.
    async fn setup_archive(&self, config: &ArchiveConfig) {
        let pid = std::process::id();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let session_id = format!("{pid}-{ts}");
        let base_dir = archive_base_dir();
        if let Err(e) = oakterm_terminal::scroll::archive_manager::ArchiveManager::cleanup_orphans(
            &base_dir,
            &session_id,
        ) {
            warn!(error = %e, "failed to clean up orphaned archive dirs");
        }
        let session_dir = base_dir
            .join(&session_id)
            .join(format!("scrollback-{}", self.default_pane_id));
        match oakterm_terminal::scroll::archive_manager::ArchiveManager::new(
            session_dir,
            config.max_bytes,
        ) {
            Ok(mgr) => {
                if let Some(mut pane) = lock_live_pane(&self.panes, self.default_pane_id).await {
                    pane.screens.set_archive(mgr);
                    info!("scrollback archive enabled");
                } else {
                    error!(
                        pane_id = self.default_pane_id,
                        "scrollback archive created but default pane missing"
                    );
                }
            }
            Err(e) => {
                warn!(error = %e, "failed to create scrollback archive, continuing without");
            }
        }
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

/// Handle a single client connection.
#[allow(clippy::too_many_lines)]
async fn handle_client(
    conn_id: u64,
    mut stream: UnixStream,
    panes: Arc<Mutex<PaneManager>>,
    mut dirty_rx: watch::Receiver<u64>,
    dirty_tx: watch::Sender<u64>,
    shutdown: ShutdownCtx,
) {
    let mut codec = FrameCodec;
    let mut read_buf = BytesMut::with_capacity(4096);
    let mut write_buf = BytesMut::with_capacity(4096);
    let mut term_rx = shutdown.term_tx.subscribe();
    // A subscription marks the current value as seen, so a termination
    // signalled before this connection was accepted would never fire
    // `changed()` — check it directly and refuse the connection.
    if term_rx.borrow_and_update().is_some() {
        debug!(conn_id, "connection arrived during shutdown, closing");
        return;
    }

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
    // Per-pane last-seen seqno for this client, to avoid redundant DirtyNotify.
    let mut pane_exit_sent = std::collections::HashSet::new();
    let mut last_seen: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    'outer: loop {
        tokio::select! {
            result = dirty_rx.changed() => {
                if result.is_err() {
                    break;
                }

                // Collect per-pane notifications, one pane lock at a time.
                let mut exit_msgs = Vec::new();
                let mut title_msgs = Vec::new();
                let mut bell_msgs = Vec::new();
                let mut dirty_pane_ids = Vec::new();
                let pane_list: Vec<(u32, SharedPane)> = panes.lock().await.snapshot();
                for (id, pane) in pane_list {
                    let mut pane = pane.lock().await;
                    if pane.closed {
                        continue;
                    }
                    // PaneExited (once per pane).
                    if !pane_exit_sent.contains(&id) {
                        if let PtyState::Exited { exit_code } = pane.pty_state {
                            pane_exit_sent.insert(id);
                            exit_msgs.push(PaneExited { pane_id: id, exit_code });
                        }
                    }
                    // Title/bell.
                    // NOTE: flags are per-grid, not per-client. First client
                    // to wake clears them; others miss the event. Phase 1
                    // needs per-client notification queues.
                    let g = pane.screens.active_grid_mut();
                    if g.title_dirty {
                        g.title_dirty = false;
                        title_msgs.push(TitleChanged {
                            pane_id: id,
                            title: g.title.clone().unwrap_or_default(),
                        });
                    }
                    if g.bell_pending {
                        g.bell_pending = false;
                        bell_msgs.push(Bell { pane_id: id });
                    }
                    // Only notify if this pane's seqno advanced since last seen.
                    let prev = last_seen.entry(id).or_insert(0);
                    if pane.dirty_seqno > *prev {
                        *prev = pane.dirty_seqno;
                        dirty_pane_ids.push(id);
                    }
                }

                for msg in exit_msgs {
                    debug!(conn_id, pane_id = msg.pane_id, exit_code = msg.exit_code, "sending PaneExited");
                    match msg.to_frame() {
                        Ok(f) => {
                            if write_frame(&mut stream, &mut codec, &mut write_buf, f).await.is_err() {
                                break 'outer;
                            }
                        }
                        Err(e) => {
                            error!(conn_id, error = %e, "failed to encode PaneExited frame");
                            break 'outer;
                        }
                    }
                }
                for msg in title_msgs {
                    match msg.to_frame() {
                        Ok(f) => {
                            if write_frame(&mut stream, &mut codec, &mut write_buf, f).await.is_err() {
                                break 'outer;
                            }
                        }
                        Err(e) => warn!(conn_id, error = %e, "failed to encode TitleChanged frame"),
                    }
                }
                for msg in bell_msgs {
                    match msg.to_frame() {
                        Ok(f) => {
                            if write_frame(&mut stream, &mut codec, &mut write_buf, f).await.is_err() {
                                break 'outer;
                            }
                        }
                        Err(e) => warn!(conn_id, error = %e, "failed to encode Bell frame"),
                    }
                }

                // Send DirtyNotify for each pane.
                for pane_id in dirty_pane_ids {
                    let notify = DirtyNotify { pane_id };
                    let Ok(frame) = Frame::new(MSG_DIRTY_NOTIFY, 0, notify.encode()) else {
                        error!(conn_id, pane_id, "failed to create DirtyNotify frame");
                        continue;
                    };
                    if write_frame(&mut stream, &mut codec, &mut write_buf, frame).await.is_err() {
                        break 'outer;
                    }
                }
            }
            result = term_rx.changed() => {
                if result.is_err() {
                    break;
                }
                let Some(reason) = *term_rx.borrow_and_update() else {
                    continue;
                };
                match (Shutdown { reason }).to_frame() {
                    Ok(f) => {
                        match write_frame(&mut stream, &mut codec, &mut write_buf, f).await {
                            Ok(()) => info!(conn_id, reason = ?reason, "shutdown broadcast sent, closing connection"),
                            Err(e) => warn!(conn_id, error = %e, "failed to deliver Shutdown broadcast; client sees a bare EOF"),
                        }
                    }
                    Err(e) => error!(conn_id, error = %e, "failed to encode Shutdown frame"),
                }
                break;
            }
            result = read_frame(&mut stream, &mut read_buf) => {
                if result.is_err() {
                    break;
                }
                while let Ok(Some(frame)) = codec.decode(&mut read_buf) {
                    // Infrastructure message needing daemon-level control
                    // (session save + termination signal), so it is handled
                    // here rather than in the request dispatch.
                    if frame.msg_type == MSG_REQUEST_SHUTDOWN {
                        let (response, accepted) = request_shutdown(conn_id, &frame, &panes, &shutdown).await;
                        if let Some(response) = response {
                            if write_frame(&mut stream, &mut codec, &mut write_buf, response).await.is_err() {
                                break 'outer;
                            }
                        }
                        if accepted {
                            // Pipelined frames after an accepted shutdown
                            // would mutate state the saved session no longer
                            // reflects; drop them and let the term arm close
                            // the connection.
                            break;
                        }
                        continue;
                    }
                    // Wait out any in-flight shutdown save; if it was
                    // accepted, drop the frame instead of dispatching (a
                    // mutation dispatched now would be acked and then lost
                    // on exit). An aborted save dispatches normally. The
                    // guard covers only the dispatch, not the response
                    // write, so a stalled reader can't block a later
                    // shutdown save — the mutation is already committed
                    // once the guard drops.
                    let dispatch = {
                        let _in_flight = shutdown.gate.read().await;
                        if shutdown.term_tx.borrow().is_some() {
                            debug!(conn_id, msg_type = frame.msg_type, "dropping frame during shutdown");
                            continue;
                        }
                        handle_request(conn_id, &frame, &panes, &dirty_tx).await
                    };
                    match dispatch {
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

/// Save-then-signal for `RequestShutdown` (Spec-0001 0x07). Returns the
/// response frame for the caller to write, and whether the shutdown was
/// accepted (termination signal sent — the requester's `Shutdown` push
/// follows the ack because writes on one connection are sequential). A
/// failed save aborts the shutdown regardless of the requested reason
/// (ADR-0020): the daemon replies `save_failed` and keeps running. The
/// daemon never exits ack-less — an ack that fails to encode also
/// suppresses the signal.
async fn request_shutdown(
    conn_id: u64,
    frame: &Frame,
    panes: &Arc<Mutex<PaneManager>>,
    shutdown: &ShutdownCtx,
) -> (Option<Frame>, bool) {
    let msg = match RequestShutdown::decode(&frame.payload) {
        Ok(m) => m,
        Err(e) => {
            // Spec-0001 error case: unknown reason or malformed payload —
            // do not shut down.
            warn!(conn_id, error = %e, "malformed RequestShutdown payload");
            let err = ErrorMessage {
                code: ErrorCode::MalformedPayload as u32,
                message: "malformed RequestShutdown".to_string(),
            };
            let response = match err.to_frame(frame.serial) {
                Ok(f) => Some(f),
                Err(e) => {
                    error!(conn_id, error = %e, "failed to encode error response");
                    None
                }
            };
            return (response, false);
        }
    };
    // Drain in-flight request handlers (they hold the gate's read side)
    // before snapshotting, and block new dispatch until the outcome is
    // known. Also serializes concurrent RequestShutdowns.
    let _barrier = shutdown.gate.write().await;
    if shutdown.term_tx.borrow().is_some() {
        // A concurrent request already saved and signalled termination;
        // acknowledge idempotently without saving again.
        info!(conn_id, "shutdown already in progress, acknowledging");
        let ack = match (ShutdownAck {
            status: ShutdownAckStatus::Accepted,
        })
        .to_frame(frame.serial)
        {
            Ok(f) => Some(f),
            Err(e) => {
                error!(conn_id, error = %e, "failed to encode ShutdownAck");
                None
            }
        };
        return (ack, true);
    }
    let status = match save_session(panes, &shutdown.state_dir).await {
        Ok(path) => {
            info!(
                conn_id,
                reason = ?msg.reason,
                path = %path.display(),
                "shutdown requested, session saved"
            );
            ShutdownAckStatus::Accepted
        }
        Err(e) => {
            error!(conn_id, error = %e, "session save failed, aborting shutdown");
            ShutdownAckStatus::SaveFailed
        }
    };
    let ack = match (ShutdownAck { status }).to_frame(frame.serial) {
        Ok(f) => Some(f),
        Err(e) => {
            error!(conn_id, error = %e, "failed to encode ShutdownAck");
            None
        }
    };
    let accepted = status == ShutdownAckStatus::Accepted && ack.is_some();
    if accepted {
        let _ = shutdown.term_tx.send(Some(msg.reason.broadcast_reason()));
    }
    (ack, accepted)
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
