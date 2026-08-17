//! Daemon server: Unix socket listener, handshake, and client connections.

mod shutdown;

use crate::framing::{read_frame, write_frame};
use crate::pane::{PaneManager, PtyState, SharedPane, lock_live_pane};
use crate::requests::{RequestResult, handle_request, release_client_pins};
use crate::session::default_state_dir;
use bytes::BytesMut;
use oakterm_protocol::frame::{Frame, FrameCodec, HEADER_SIZE};
use oakterm_protocol::message::{
    Bell, ClientHello, HandshakeStatus, MSG_CLIENT_HELLO, MSG_DIRTY_NOTIFY, MSG_REQUEST_SHUTDOWN,
    PaneExited, ServerHello, Shutdown, ShutdownReason, TitleChanged,
};
use oakterm_protocol::render::DirtyNotify;
use oakterm_protocol::socket::socket_path;
use shutdown::{ShutdownCtx, ShutdownGate, ShutdownOutcome, request_shutdown};
use std::collections::{HashMap, HashSet};
use std::io;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, watch};
use tokio_util::codec::Decoder;
use tracing::{debug, error, info, warn};

/// Handshake timeout per Spec-0001.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Upper bound on the buffered bytes of an in-progress `ClientHello`. A real
/// hello is a few dozen bytes; this caps pre-auth buffering so a client can't
/// make the daemon reserve up to `MAX_PAYLOAD` before it has authenticated.
const MAX_HANDSHAKE_BYTES: usize = 4096;

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
            gate: ShutdownGate::new(term_tx),
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

/// Borrowed framed I/O for one connection, bundling the stream, codec, and
/// buffers so per-frame helpers take one argument instead of four. The
/// connection loop keeps the parts as separate locals (the `select!`
/// scrutinees borrow them individually) and hands out a `FrameIo` per arm.
struct FrameIo<'a> {
    stream: &'a mut UnixStream,
    codec: &'a mut FrameCodec,
    read_buf: &'a mut BytesMut,
    write_buf: &'a mut BytesMut,
}

impl FrameIo<'_> {
    async fn write(&mut self, frame: Frame) -> io::Result<()> {
        write_frame(self.stream, self.codec, self.write_buf, frame).await
    }
}

/// Handle a single client connection.
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
    let mut term_rx = shutdown.gate.subscribe();
    // A subscription marks the current value as seen, so a termination
    // signalled before this connection was accepted would never fire
    // `changed()` — check it directly and refuse the connection.
    if term_rx.borrow_and_update().is_some() {
        debug!(conn_id, "connection arrived during shutdown, closing");
        return;
    }

    let handshake = perform_handshake(
        conn_id,
        io(&mut stream, &mut codec, &mut read_buf, &mut write_buf),
    );
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

    // Per-pane last-seen seqno for this client, to avoid redundant DirtyNotify.
    let mut pane_exit_sent = HashSet::new();
    let mut last_seen: HashMap<u32, u64> = HashMap::new();
    'outer: loop {
        tokio::select! {
            result = dirty_rx.changed() => {
                if result.is_err() {
                    break;
                }
                let io = io(&mut stream, &mut codec, &mut read_buf, &mut write_buf);
                if send_dirty_notifications(conn_id, io, &panes, &mut pane_exit_sent, &mut last_seen)
                    .await
                    .is_break()
                {
                    break 'outer;
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
                let io = io(&mut stream, &mut codec, &mut read_buf, &mut write_buf);
                if dispatch_incoming(conn_id, io, &panes, &dirty_tx, &shutdown)
                    .await
                    .is_break()
                {
                    break 'outer;
                }
            }
        }
    }

    release_client_pins(conn_id, &panes).await;
}

fn io<'a>(
    stream: &'a mut UnixStream,
    codec: &'a mut FrameCodec,
    read_buf: &'a mut BytesMut,
    write_buf: &'a mut BytesMut,
) -> FrameIo<'a> {
    FrameIo {
        stream,
        codec,
        read_buf,
        write_buf,
    }
}

/// Decode and dispatch every buffered request frame. `RequestShutdown` is
/// handled here (session save plus termination signal) rather than in the
/// request dispatcher. Returns `Break` when the connection should close.
async fn dispatch_incoming(
    conn_id: u64,
    mut io: FrameIo<'_>,
    panes: &Arc<Mutex<PaneManager>>,
    dirty_tx: &watch::Sender<u64>,
    shutdown: &ShutdownCtx,
) -> ControlFlow<()> {
    loop {
        let frame = match io.codec.decode(io.read_buf) {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            // A stream that fails framing can never resync; leaving the
            // corrupt bytes buffered would re-hit this error forever.
            Err(e) => {
                error!(conn_id, error = %e, "frame decode failed, closing connection");
                return ControlFlow::Break(());
            }
        };
        if frame.msg_type == MSG_REQUEST_SHUTDOWN {
            match request_shutdown(conn_id, &frame, panes, shutdown).await {
                ShutdownOutcome::Accepted { ack } => {
                    if let Some(ack) = ack {
                        if io.write(ack).await.is_err() {
                            return ControlFlow::Break(());
                        }
                    }
                    // Pipelined frames after an accepted shutdown would
                    // mutate state the saved session no longer reflects; drop
                    // them and let the term arm close the connection.
                    return ControlFlow::Continue(());
                }
                ShutdownOutcome::Declined { response } => {
                    if let Some(response) = response {
                        if io.write(response).await.is_err() {
                            return ControlFlow::Break(());
                        }
                    }
                    continue;
                }
            }
        }
        // Wait out any in-flight shutdown save; if it was accepted, drop the
        // frame instead of dispatching (a mutation dispatched now would be
        // acked and then lost on exit). The permit covers only the dispatch,
        // not the response write, so a stalled reader can't block a later
        // shutdown save — the mutation is committed once the permit drops.
        let dispatch = {
            let Some(_permit) = shutdown.gate.dispatch_permit().await else {
                debug!(
                    conn_id,
                    msg_type = frame.msg_type,
                    "dropping frame during shutdown"
                );
                continue;
            };
            handle_request(conn_id, &frame, panes, dirty_tx).await
        };
        match dispatch {
            RequestResult::Response(response) => {
                if io.write(response).await.is_err() {
                    return ControlFlow::Break(());
                }
            }
            RequestResult::Detach => {
                debug!(conn_id, "client detached");
                return ControlFlow::Break(());
            }
            RequestResult::NoResponse => {}
        }
    }
    ControlFlow::Continue(())
}

/// Read the `ClientHello`, validate the protocol version (Spec-0001), and
/// reply with `ServerHello`. On version mismatch it sends the mismatch
/// response and returns an error so the caller closes the connection.
async fn perform_handshake(conn_id: u64, mut io: FrameIo<'_>) -> io::Result<()> {
    // Accumulate reads until a full ClientHello decodes: on a loaded system or
    // with a large client_name the hello can arrive split across reads, where a
    // single read+decode would see a fragment and reject it. The outer
    // HANDSHAKE_TIMEOUT and EOF (read_frame errors on n==0) bound the loop.
    let frame = loop {
        read_frame(io.stream, io.read_buf).await?;
        // Reject an oversized hello by its advertised payload length before
        // decoding: FrameCodec::decode reserves the full payload capacity
        // before it returns Ok(None), so an unauthenticated client could
        // otherwise make the daemon reserve up to MAX_PAYLOAD. The length lives
        // at bytes 9..13 of the front frame's header; a real ClientHello is a
        // few dozen bytes. Checking only the front frame preserves a client
        // that pipelines the hello with a larger first request.
        if io.read_buf.len() >= HEADER_SIZE {
            let payload_len = u32::from_le_bytes([
                io.read_buf[9],
                io.read_buf[10],
                io.read_buf[11],
                io.read_buf[12],
            ]);
            if payload_len as usize > MAX_HANDSHAKE_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "handshake frame exceeds the maximum size",
                ));
            }
        }
        match io.codec.decode(io.read_buf) {
            Ok(Some(frame)) => break frame,
            // Ok(None) is a partial frame; loop to read the rest.
            Ok(None) => {}
            Err(e) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("handshake frame decode failed: {e}"),
                ));
            }
        }
    };
    if frame.msg_type != MSG_CLIENT_HELLO {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected ClientHello",
        ));
    }

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
                if let Err(e) = io.write(resp).await {
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
    io.write(response).await
}

/// Collect and push this client's per-pane notifications after a dirty
/// wake (`PaneExited`, `TitleChanged`, `Bell`, `DirtyNotify`), one pane
/// lock at a time. Returns `Break` when a frame write fails and the caller
/// should close the connection.
async fn send_dirty_notifications(
    conn_id: u64,
    mut io: FrameIo<'_>,
    panes: &Arc<Mutex<PaneManager>>,
    pane_exit_sent: &mut HashSet<u32>,
    last_seen: &mut HashMap<u32, u64>,
) -> ControlFlow<()> {
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
                exit_msgs.push(PaneExited {
                    pane_id: id,
                    exit_code,
                });
            }
        }
        // Title/bell flags are per-grid, not per-client. First client to
        // wake clears them; others miss the event. Phase 1 needs per-client
        // notification queues.
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
        debug!(
            conn_id,
            pane_id = msg.pane_id,
            exit_code = msg.exit_code,
            "sending PaneExited"
        );
        match msg.to_frame() {
            Ok(f) => {
                if io.write(f).await.is_err() {
                    return ControlFlow::Break(());
                }
            }
            Err(e) => {
                error!(conn_id, error = %e, "failed to encode PaneExited frame");
                return ControlFlow::Break(());
            }
        }
    }
    for msg in title_msgs {
        match msg.to_frame() {
            Ok(f) => {
                if io.write(f).await.is_err() {
                    return ControlFlow::Break(());
                }
            }
            Err(e) => warn!(conn_id, error = %e, "failed to encode TitleChanged frame"),
        }
    }
    for msg in bell_msgs {
        match msg.to_frame() {
            Ok(f) => {
                if io.write(f).await.is_err() {
                    return ControlFlow::Break(());
                }
            }
            Err(e) => warn!(conn_id, error = %e, "failed to encode Bell frame"),
        }
    }

    for pane_id in dirty_pane_ids {
        let notify = DirtyNotify { pane_id };
        let Ok(frame) = Frame::new(MSG_DIRTY_NOTIFY, 0, notify.encode()) else {
            error!(conn_id, pane_id, "failed to create DirtyNotify frame");
            continue;
        };
        if io.write(frame).await.is_err() {
            return ControlFlow::Break(());
        }
    }
    ControlFlow::Continue(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use oakterm_protocol::message::{ClientType, CopyMode};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_util::codec::Encoder;

    async fn handshake(stream: &mut UnixStream) {
        let mut codec = FrameCodec;
        let hello = ClientHello {
            protocol_version_major: ClientHello::VERSION_MAJOR,
            protocol_version_minor: ClientHello::VERSION_MINOR,
            client_type: ClientType::Gui,
            client_name: "pin-test".to_string(),
        };
        let mut buf = BytesMut::new();
        codec
            .encode(hello.to_frame(1).expect("encode hello"), &mut buf)
            .expect("encode frame");
        stream.write_all(&buf).await.expect("write hello");

        let mut read_buf = BytesMut::with_capacity(256);
        loop {
            stream.read_buf(&mut read_buf).await.expect("read hello");
            if codec.decode(&mut read_buf).expect("decode").is_some() {
                return;
            }
        }
    }

    /// The pin release lives on `handle_client`'s exit path rather than in
    /// any request handler, so only a real connection drop exercises it.
    #[tokio::test]
    async fn client_disconnect_releases_copy_mode_pins() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("sock");
        let mut daemon = Daemon::with_socket_path(80, 24, socket.clone());
        daemon.set_persist(true);
        daemon.set_state_dir(dir.path().join("state"));
        let panes = Arc::clone(&daemon.panes);
        let pane_id = panes.lock().await.snapshot()[0].0;

        let handle = tokio::spawn(async move {
            let _ = daemon.run().await;
        });
        for _ in 0..40 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        {
            let mut stream = UnixStream::connect(&socket).await.expect("connect");
            handshake(&mut stream).await;
            let mut codec = FrameCodec;
            let mut buf = BytesMut::new();
            codec
                .encode(
                    CopyMode { pane_id }.to_enter_frame().expect("enter"),
                    &mut buf,
                )
                .expect("encode enter");
            stream.write_all(&buf).await.expect("write enter");

            for _ in 0..40 {
                if !lock_live_pane(&panes, pane_id)
                    .await
                    .expect("pane")
                    .copy_mode_pins
                    .is_empty()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            assert_eq!(
                lock_live_pane(&panes, pane_id)
                    .await
                    .expect("pane")
                    .copy_mode_pins
                    .len(),
                1,
                "EnterCopyMode did not pin"
            );
        }

        for _ in 0..40 {
            if lock_live_pane(&panes, pane_id)
                .await
                .expect("pane")
                .copy_mode_pins
                .is_empty()
            {
                handle.abort();
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        handle.abort();
        panic!("pin outlived the connection");
    }
}
