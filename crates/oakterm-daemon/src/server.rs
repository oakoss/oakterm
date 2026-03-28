//! Daemon server: PTY read loop, Unix socket listener, client connections.

use crate::socket::socket_path;
use bytes::BytesMut;
use oakterm_protocol::frame::{Frame, FrameCodec};
use oakterm_protocol::input::{KeyInput, Resize};
use oakterm_protocol::message::{
    ClientHello, HandshakeStatus, MSG_CLIENT_HELLO, MSG_DETACH, MSG_DIRTY_NOTIFY,
    MSG_GET_RENDER_UPDATE, MSG_KEY_INPUT, MSG_PING, MSG_PONG, MSG_RENDER_UPDATE, MSG_RESIZE,
    ServerHello,
};
use oakterm_protocol::render::{DirtyNotify, DirtyRow, GetRenderUpdate, RenderUpdate, WireCell};
use oakterm_terminal::grid::Grid;
use oakterm_terminal::handler;
use std::io;
use std::os::unix::io::RawFd;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, watch};
use tokio_util::codec::{Decoder, Encoder};

/// Handshake timeout per Spec-0001.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Daemon state shared across tasks.
pub struct Daemon {
    grid: Arc<Mutex<Grid>>,
    dirty_tx: watch::Sender<u64>,
    dirty_rx: watch::Receiver<u64>,
    socket_path: std::path::PathBuf,
    cols: u16,
    rows: u16,
}

impl Daemon {
    /// Create a new daemon with the given terminal dimensions.
    ///
    /// # Errors
    /// Returns an error if the socket path cannot be resolved.
    pub fn new(cols: u16, rows: u16) -> io::Result<Self> {
        let path = socket_path()?;
        let (dirty_tx, dirty_rx) = watch::channel(0u64);
        Ok(Self {
            grid: Arc::new(Mutex::new(Grid::new(cols, rows))),
            dirty_tx,
            dirty_rx,
            socket_path: path,
            cols,
            rows,
        })
    }

    /// Run the daemon: start PTY, listen for connections.
    ///
    /// # Errors
    /// Returns an error if the listener or PTY fails to start.
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

        let pty = oakterm_pty::spawn_shell(oakterm_pty::WinSize {
            cols: self.cols,
            rows: self.rows,
        })?;
        let master_fd = pty.master_raw_fd();
        let grid = Arc::clone(&self.grid);
        let dirty_tx = self.dirty_tx.clone();
        tokio::spawn(pty_read_loop(pty, grid, dirty_tx));

        loop {
            let (stream, _) = listener.accept().await?;
            let grid = Arc::clone(&self.grid);
            let dirty_rx = self.dirty_rx.clone();
            tokio::spawn(handle_client(stream, grid, dirty_rx, master_fd));
        }
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
    grid: Arc<Mutex<Grid>>,
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
                let mut g = grid.lock().await;
                handler::process_bytes(&mut g, &buf[..n]);
                let seqno = g.seqno;
                drop(g);
                let _ = dirty_tx.send(seqno);
            }
            Ok(Err(e)) if e.kind() == io::ErrorKind::WouldBlock => {}
            Ok(Err(_)) => break,
            Err(_would_block) => {}
        }
    }
}

/// Handle a single client connection.
async fn handle_client(
    mut stream: UnixStream,
    grid: Arc<Mutex<Grid>>,
    mut dirty_rx: watch::Receiver<u64>,
    master_fd: RawFd,
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
                let notify = DirtyNotify { pane_id: 0 };
                let Ok(frame) = Frame::new(MSG_DIRTY_NOTIFY, 0, notify.encode()) else {
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
                    match handle_request(&frame, &grid, master_fd).await {
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
async fn handle_request(frame: &Frame, grid: &Arc<Mutex<Grid>>, master_fd: RawFd) -> RequestResult {
    match frame.msg_type {
        MSG_KEY_INPUT => {
            if let Ok(msg) = KeyInput::decode(&frame.payload) {
                if !msg.key_data.is_empty() {
                    let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(master_fd) };
                    let _ = rustix::io::write(borrowed, &msg.key_data);
                }
            }
            RequestResult::NoResponse
        }
        MSG_RESIZE => {
            if let Ok(msg) = Resize::decode(&frame.payload) {
                let borrowed = unsafe { rustix::fd::BorrowedFd::borrow_raw(master_fd) };
                if oakterm_pty::resize_fd(
                    borrowed,
                    msg.cols,
                    msg.rows,
                    msg.pixel_width,
                    msg.pixel_height,
                )
                .is_ok()
                {
                    let mut g = grid.lock().await;
                    g.resize(msg.cols, msg.rows);
                }
            }
            RequestResult::NoResponse
        }
        MSG_DETACH => RequestResult::Detach,
        MSG_GET_RENDER_UPDATE => {
            let Ok(req) = GetRenderUpdate::decode(&frame.payload) else {
                return RequestResult::NoResponse;
            };
            let g = grid.lock().await;
            let dirty_indices = g.dirty_rows(req.since_seqno);

            let dirty_rows: Vec<DirtyRow> = dirty_indices
                .iter()
                .filter_map(|&idx| {
                    let row = g.lines.get(idx as usize)?;
                    let cells: Vec<WireCell> = row
                        .cells
                        .iter()
                        .map(|c| WireCell {
                            codepoint: c.codepoint as u32,
                            fg_r: 0,
                            fg_g: 0,
                            fg_b: 0,
                            fg_type: 0,
                            bg_r: 0,
                            bg_g: 0,
                            bg_b: 0,
                            bg_type: 0,
                            flags: 0,
                            extra: vec![],
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

            let update = RenderUpdate {
                pane_id: req.pane_id,
                seqno: g.seqno,
                cursor_x: g.cursor.col,
                cursor_y: g.cursor.row,
                cursor_style: 0,
                cursor_visible: g.cursor.visible,
                dirty_rows,
            };

            match update.encode() {
                Ok(payload) => match Frame::new(MSG_RENDER_UPDATE, frame.serial, payload) {
                    Ok(f) => RequestResult::Response(f),
                    Err(_) => RequestResult::NoResponse,
                },
                Err(_) => RequestResult::NoResponse,
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
