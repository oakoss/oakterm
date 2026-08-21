//! Daemon connection: socket setup, handshake, frame I/O, and the
//! background reader thread that turns daemon pushes into [`UserEvent`]s.

use crate::UserEvent;
use oakterm_protocol::frame::Frame;
use oakterm_protocol::message::{
    ClientHello, ClientType, ErrorCode, ErrorMessage, HandshakeStatus, LayoutTree, MSG_BELL,
    MSG_CLOSE_PANE_RESPONSE, MSG_CLOSE_TAB_RESPONSE, MSG_DIRTY_NOTIFY, MSG_ERROR,
    MSG_GET_RENDER_UPDATE, MSG_LAYOUT_TREE, MSG_NEW_TAB_RESPONSE, MSG_PROMPT_POSITION,
    MSG_RENDER_UPDATE, MSG_SCROLLBACK_DATA, MSG_SERVER_HELLO, MSG_SHUTDOWN,
    MSG_SPLIT_PANE_RESPONSE, MSG_TAB_LIST, MSG_TITLE_CHANGED, MSG_YANK_RESPONSE, NewTabResponse,
    PromptPosition, ScrollbackData, Shutdown, SplitPaneResponse, TabList, TitleChanged,
    YankResponse,
};
use oakterm_protocol::render::{DirtyNotify, GetRenderUpdate, RenderUpdate};
use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use tracing::{debug, error, info, warn};
use winit::event_loop::EventLoopProxy;

/// Thread-safe handle for writing frames to the daemon socket.
#[derive(Clone)]
pub(crate) struct DaemonWriter {
    stream: Arc<Mutex<UnixStream>>,
}

impl DaemonWriter {
    pub(crate) fn send_frame(&self, frame: &Frame) -> std::io::Result<()> {
        let data = frame.encode_to_vec();
        let mut stream = self.stream.lock().expect("daemon writer lock poisoned");
        stream.write_all(&data)
    }

    /// Shut the socket down both ways. The reader thread holds a clone of
    /// this handle, so dropping the App's copy alone leaves the fd open
    /// and the reader blocked in `read_exact` forever; shutdown makes its
    /// read fail immediately and drives the `Disconnected` exit path.
    pub(crate) fn shutdown(&self) {
        if let Ok(stream) = self.stream.lock() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// A connected daemon: the frame writer, the spawned child (when this
/// client started the daemon), and the server's advertised protocol
/// minor version for gating newer request types (Spec-0001).
pub(crate) struct DaemonConnection {
    pub(crate) writer: DaemonWriter,
    pub(crate) child: Option<std::process::Child>,
    pub(crate) server_minor: u16,
}

/// Connect to the daemon, spawning it if needed.
///
/// Uses tmux-style connect-and-check with a lock file to handle stale
/// sockets and prevent two clients from racing to start the daemon.
pub(crate) fn connect_to_daemon(
    proxy: &EventLoopProxy<UserEvent>,
) -> std::io::Result<DaemonConnection> {
    let socket_path = oakterm_protocol::socket::socket_path()?;

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
    let _lock = oakterm_protocol::socket::acquire_startup_lock()?;

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
    if let Err(e) = std::fs::remove_file(&socket_path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        return Err(std::io::Error::new(
            e.kind(),
            format!(
                "failed to remove stale socket at {}: {e}",
                socket_path.display()
            ),
        ));
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
) -> std::io::Result<DaemonConnection> {
    let mut read_stream = stream.try_clone()?;
    let write_stream = Arc::new(Mutex::new(stream));

    let writer = DaemonWriter {
        stream: Arc::clone(&write_stream),
    };
    let server_minor = handshake(&writer, &mut read_stream)?;

    let reader_writer = writer.clone();
    let proxy = proxy.clone();
    std::thread::spawn(move || {
        daemon_reader(read_stream, &reader_writer, &proxy);
    });

    Ok(DaemonConnection {
        writer,
        child,
        server_minor,
    })
}

/// Perform the protocol handshake per Spec-0001. Returns the server's
/// advertised protocol minor version.
fn handshake(writer: &DaemonWriter, read_stream: &mut UnixStream) -> std::io::Result<u16> {
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

    Ok(server_hello.protocol_version_minor)
}

/// Per-pane bookkeeping for the daemon read loop's request/response
/// debounce. I/O-free: every method mutates only `self` and returns the
/// action the caller must perform, so the transition table is testable
/// without sockets, threads, or the event-loop proxy.
#[derive(Debug, Default)]
struct ReaderState {
    /// Last `seqno` we observed in a `RenderUpdate` for each pane. Used as
    /// the `since_seqno` cursor on the next `GetRenderUpdate`.
    seqnos: HashMap<u32, u64>,
    /// Panes with an outstanding `GetRenderUpdate` request.
    in_flight: HashSet<u32>,
    /// Panes that received a `DirtyNotify` while a request was already in
    /// flight; the next `RenderUpdate` for each will fire one follow-up.
    pending: HashSet<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirtyOutcome {
    /// Caller should send a `GetRenderUpdate` with this `since_seqno`.
    Send(u64),
    /// A request is already in flight; this notify was coalesced.
    Coalesce,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateOutcome {
    /// No follow-up needed.
    Done,
    /// Caller should send a follow-up `GetRenderUpdate` with this seqno.
    SendFollowUp(u64),
}

impl ReaderState {
    fn on_dirty_notify(&mut self, pane_id: u32) -> DirtyOutcome {
        if self.in_flight.contains(&pane_id) {
            self.pending.insert(pane_id);
            DirtyOutcome::Coalesce
        } else {
            let since_seqno = self.seqnos.get(&pane_id).copied().unwrap_or(0);
            self.in_flight.insert(pane_id);
            DirtyOutcome::Send(since_seqno)
        }
    }

    fn on_render_update(&mut self, pane_id: u32, seqno: u64) -> UpdateOutcome {
        self.seqnos.insert(pane_id, seqno);
        self.in_flight.remove(&pane_id);
        if self.pending.remove(&pane_id) {
            // Re-mark in-flight for the follow-up. The daemon's
            // GetRenderUpdate handler returns rows with seqno > since_seqno,
            // so passing the freshly-bumped seqno here covers every row that
            // changed during the in-flight window — no matter how many
            // DirtyNotify arrivals we coalesced.
            self.in_flight.insert(pane_id);
            UpdateOutcome::SendFollowUp(self.seqnos.get(&pane_id).copied().unwrap_or(0))
        } else {
            UpdateOutcome::Done
        }
    }

    /// Drop all bookkeeping for `pane_id`. Currently unused — `daemon_reader`
    /// has no `MSG_PANE_EXITED` arm, so per-pane state leaks until the
    /// daemon connection closes. TREK-157 will add the arm and call this.
    #[allow(dead_code)] // TREK-157
    fn on_pane_exit(&mut self, pane_id: u32) {
        self.seqnos.remove(&pane_id);
        self.in_flight.remove(&pane_id);
        self.pending.remove(&pane_id);
    }
}

/// Send `UserEvent::Disconnected` to the GUI event loop. Logs at `warn` if
/// the event loop has already shut down (the reader is on its way out
/// either way; the user just won't see the disconnect notification).
fn notify_disconnected(proxy: &EventLoopProxy<UserEvent>) {
    if let Err(e) = proxy.send_event(UserEvent::Disconnected) {
        warn!(error = %e, "event loop closed before Disconnected delivered");
    }
}

/// Send a `GetRenderUpdate` for `pane_id`. Caller passes the `since_seqno`
/// from `ReaderState` so this helper has no map-lookup responsibility.
fn send_get_render_update(
    pane_id: u32,
    since_seqno: u64,
    writer: &DaemonWriter,
) -> std::io::Result<()> {
    let req = GetRenderUpdate {
        pane_id,
        since_seqno,
    };
    let frame = Frame::new(MSG_GET_RENDER_UPDATE, 1, req.encode())
        .expect("GetRenderUpdate payload fits in frame");
    writer.send_frame(&frame)
}

/// Failures land here both as serial-0 pushes (e.g. `ResizePane`
/// rejections, Spec-0001) and as serial-carrying error responses (e.g.
/// `GetRenderUpdate` for an unknown pane). There is no response routing
/// yet — nothing correlates a serial back to its request, so an errored
/// `GetRenderUpdate` leaves its pane's `in_flight` entry set (recovery is
/// TREK-156; routing arrives with the TREK-99 client wiring). Until a UI
/// surface exists, the log is the only signal.
fn log_daemon_error(frame: &Frame) {
    match ErrorMessage::decode(&frame.payload) {
        Ok(err) => {
            error!(
                serial = frame.serial,
                code = err.code,
                message = %err.message,
                "daemon reported an error"
            );
        }
        Err(e) => {
            error!(
                serial = frame.serial,
                payload_len = frame.payload.len(),
                error = %e,
                "failed to decode ErrorMessage"
            );
        }
    }
}

/// The daemon closes the socket right after this push; the read loop's
/// EOF then drives the existing disconnect path.
fn log_daemon_shutdown(frame: &Frame) {
    match Shutdown::decode(&frame.payload) {
        Ok(msg) => {
            info!(reason = ?msg.reason, "daemon announced shutdown");
        }
        Err(e) => {
            error!(error = %e, "failed to decode Shutdown");
        }
    }
}

/// Background thread: read frames, request render updates on `DirtyNotify`.
///
/// Per pane, at most one `GetRenderUpdate` is in flight at a time; subsequent
/// `DirtyNotify` arrivals collapse into a single follow-up after the response
/// lands. Without this, fast PTY output (e.g. `tree` flooding) produces one
/// round-trip per PTY chunk and the daemon's per-update serialization plus
/// the client's per-update decode/apply work pin the UI. See `ReaderState`
/// for the transition table.
fn daemon_reader(
    mut read_stream: UnixStream,
    writer: &DaemonWriter,
    proxy: &EventLoopProxy<UserEvent>,
) {
    let mut state = ReaderState::default();

    loop {
        match read_frame(&mut read_stream) {
            Ok(frame) => match frame.msg_type {
                MSG_DIRTY_NOTIFY => {
                    let pane_id = match DirtyNotify::decode(&frame.payload) {
                        Ok(n) => n.pane_id,
                        Err(e) => {
                            error!(
                                error = %e,
                                payload_len = frame.payload.len(),
                                "failed to decode DirtyNotify, dropping"
                            );
                            continue;
                        }
                    };
                    match state.on_dirty_notify(pane_id) {
                        DirtyOutcome::Send(since_seqno) => {
                            if let Err(e) = send_get_render_update(pane_id, since_seqno, writer) {
                                // state.in_flight has the phantom entry from
                                // on_dirty_notify above; safe only because we
                                // break and drop `state`. Any future retry
                                // path here must roll the state mutation back.
                                error!(error = %e, "daemon write error");
                                notify_disconnected(proxy);
                                break;
                            }
                        }
                        DirtyOutcome::Coalesce => {
                            debug!(pane_id, "render request coalesced");
                        }
                    }
                }
                MSG_RENDER_UPDATE => match RenderUpdate::decode(&frame.payload) {
                    Ok(update) => {
                        let pane_id = update.pane_id;
                        let seqno = update.seqno;
                        // Paint first; bookkeeping after. The reader thread is
                        // single-threaded, so the event loop can't race against
                        // the state mutation — but the user-visible repaint
                        // lands ASAP this way.
                        let _ = proxy.send_event(UserEvent::RenderUpdate(Box::new(update)));
                        if let UpdateOutcome::SendFollowUp(since_seqno) =
                            state.on_render_update(pane_id, seqno)
                            && let Err(e) = send_get_render_update(pane_id, since_seqno, writer)
                        {
                            // Same phantom-in_flight caveat as the
                            // DirtyNotify arm above; safe only because we
                            // break.
                            error!(error = %e, "daemon write error");
                            notify_disconnected(proxy);
                            break;
                        }
                    }
                    Err(e) => {
                        error!(
                            error = %e,
                            payload_len = frame.payload.len(),
                            "failed to decode RenderUpdate, disconnecting"
                        );
                        notify_disconnected(proxy);
                        break;
                    }
                },
                other => {
                    if !forward_event_frame(&frame, proxy) {
                        warn!(
                            msg_type = format_args!("0x{other:04x}"),
                            "unhandled daemon message"
                        );
                    }
                }
            },
            Err(e) => {
                error!(error = %e, "daemon read error");
                notify_disconnected(proxy);
                break;
            }
        }
    }
}

/// Decode-and-forward for frames that carry no reader state: each becomes
/// a `UserEvent` (or a log line). Returns false for message types this
/// helper doesn't own — `DirtyNotify`/`RenderUpdate` stay in
/// `daemon_reader` because they drive its in-flight state machine.
fn forward_event_frame(frame: &Frame, proxy: &EventLoopProxy<UserEvent>) -> bool {
    let Some(events) = events_for_frame(frame) else {
        return false;
    };
    for event in events {
        let _ = proxy.send_event(event);
    }
    true
}

/// Decode a frame into the `UserEvent`s it produces, `None` for message
/// types this helper doesn't own. Split from the send so the decisions
/// here — which serial travels with a reply, which errors ring the bell
/// — are reachable without an event loop.
fn events_for_frame(frame: &Frame) -> Option<Vec<UserEvent>> {
    let mut events = Vec::new();
    match frame.msg_type {
        MSG_TITLE_CHANGED => match TitleChanged::decode(&frame.payload) {
            Ok(msg) => {
                events.push(UserEvent::TitleChanged(msg.pane_id, msg.title));
            }
            Err(e) => {
                error!(error = %e, "failed to decode TitleChanged");
            }
        },
        MSG_SCROLLBACK_DATA => match ScrollbackData::decode(&frame.payload) {
            Ok(data) => {
                events.push(UserEvent::ScrollbackData {
                    serial: frame.serial,
                    data: Box::new(data),
                });
            }
            Err(e) => {
                error!(error = %e, "failed to decode ScrollbackData");
            }
        },
        MSG_YANK_RESPONSE => match YankResponse::decode(&frame.payload) {
            Ok(resp) => {
                events.push(UserEvent::YankResponse {
                    serial: frame.serial,
                    text: resp.text,
                });
            }
            Err(e) => {
                error!(error = %e, "failed to decode YankResponse");
                // Retire the yank through the ordinary failure path: a
                // dropped reply would leave it pending forever, and every
                // later response refused as answering a superseded one.
                events.push(UserEvent::RequestFailed {
                    serial: frame.serial,
                    code: None,
                });
            }
        },
        MSG_PROMPT_POSITION => match PromptPosition::decode(&frame.payload) {
            Ok(pos) => {
                events.push(UserEvent::PromptPosition(pos));
            }
            Err(e) => {
                error!(error = %e, "failed to decode PromptPosition");
            }
        },
        MSG_BELL => {
            events.push(UserEvent::Bell);
        }
        MSG_SPLIT_PANE_RESPONSE => match SplitPaneResponse::decode(&frame.payload) {
            Ok(resp) => {
                events.push(UserEvent::SplitCreated(resp.new_pane_id));
            }
            Err(e) => {
                error!(error = %e, "failed to decode SplitPaneResponse");
            }
        },
        MSG_LAYOUT_TREE => match LayoutTree::decode(&frame.payload) {
            Ok(msg) => {
                events.push(UserEvent::LayoutTree(Box::new(msg.tree)));
            }
            Err(e) => {
                error!(error = %e, "failed to decode LayoutTree");
            }
        },
        MSG_TAB_LIST => match TabList::decode(&frame.payload) {
            Ok(msg) => {
                events.push(UserEvent::TabList(Box::new(msg)));
            }
            Err(e) => {
                error!(error = %e, "failed to decode TabList");
            }
        },
        MSG_NEW_TAB_RESPONSE => match NewTabResponse::decode(&frame.payload) {
            Ok(resp) => {
                events.push(UserEvent::TabCreated {
                    tab_id: resp.tab_id,
                    pane_id: resp.pane_id,
                });
            }
            Err(e) => {
                error!(error = %e, "failed to decode NewTabResponse");
            }
        },
        MSG_CLOSE_TAB_RESPONSE => {
            events.push(UserEvent::TabClosed);
        }
        MSG_CLOSE_PANE_RESPONSE => {
            events.push(UserEvent::PaneClosed {
                serial: frame.serial,
            });
        }
        MSG_ERROR => events.extend(error_events(frame)),
        MSG_SHUTDOWN => log_daemon_shutdown(frame),
        _ => return None,
    }
    Some(events)
}

/// What an error frame reports: the failed serial, plus a bell for the
/// rejections that would otherwise look like a dead keybind.
fn error_events(frame: &Frame) -> Vec<UserEvent> {
    log_daemon_error(frame);
    let code = ErrorMessage::decode(&frame.payload)
        .ok()
        .and_then(|err| ErrorCode::try_from(err.code).ok());
    let mut events = vec![UserEvent::RequestFailed {
        serial: frame.serial,
        code,
    }];
    if error_rings_bell(code) {
        events.push(UserEvent::Bell);
    }
    events
}

/// Whether a rejected request should ring the bell. These are routine
/// user-triggered outcomes, and a silent one makes the keybind or click
/// look dead. Shared with the App's own failure paths so a rejection
/// that already rings here is not announced twice.
pub(crate) fn error_rings_bell(code: Option<ErrorCode>) -> bool {
    matches!(
        code,
        Some(
            ErrorCode::LayoutRejected
                | ErrorCode::UnknownPane
                | ErrorCode::UnknownTab
                | ErrorCode::UnknownWorkspace
        )
    )
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

#[cfg(test)]
mod tests {
    use super::{DirtyOutcome, ReaderState, UpdateOutcome, error_rings_bell, events_for_frame};
    use crate::UserEvent;
    use oakterm_protocol::frame::Frame;
    use oakterm_protocol::message::{
        ErrorCode, ErrorMessage, MSG_ERROR, MSG_SCROLLBACK_DATA, MSG_YANK_RESPONSE, ScrollbackData,
        YankResponse,
    };

    fn yank_frame(serial: u32, text: &str) -> Frame {
        let payload = YankResponse { text: text.into() }.encode().expect("encode");
        Frame::new(MSG_YANK_RESPONSE, serial, payload).expect("frame")
    }

    fn scrollback_frame(serial: u32) -> Frame {
        let payload = ScrollbackData {
            pane_id: 3,
            start_row: -8,
            has_more: true,
            total_rows: 100,
            rows: vec![],
        }
        .encode()
        .expect("encode");
        Frame::new(MSG_SCROLLBACK_DATA, serial, payload).expect("frame")
    }

    fn error_frame(serial: u32, code: ErrorCode) -> Frame {
        let payload = ErrorMessage {
            code: code as u32,
            message: "nope".into(),
        }
        .encode()
        .expect("encode");
        Frame::new(MSG_ERROR, serial, payload).expect("frame")
    }

    /// The reply's serial is the only thing correlating a copy-mode cache
    /// fill with the window it asked for. Forwarding 0 instead would leave
    /// every fill unclaimable, which no test above this layer can see.
    #[test]
    fn a_scrollback_reply_carries_the_frames_serial() {
        let events = events_for_frame(&scrollback_frame(37)).expect("owned type");

        match events.as_slice() {
            [UserEvent::ScrollbackData { serial, data }] => {
                assert_eq!(*serial, 37);
                assert_eq!(data.pane_id, 3);
            }
            other => panic!("expected one ScrollbackData, got {other:?}"),
        }
    }

    /// A yank reply correlates by serial alone — the daemon answers with
    /// text and nothing else — so forwarding 0 would strand every yank.
    #[test]
    fn a_yank_reply_carries_the_frames_serial_and_text() {
        let events = events_for_frame(&yank_frame(41, "hello")).expect("owned type");

        match events.as_slice() {
            [UserEvent::YankResponse { serial, text }] => {
                assert_eq!(*serial, 41);
                assert_eq!(text, "hello");
            }
            other => panic!("expected one YankResponse, got {other:?}"),
        }
    }

    /// A corrupt yank payload must still retire the pending yank. Logging
    /// and dropping it leaves `y` inert for the rest of copy mode, since
    /// the next reply answers a serial the client no longer expects.
    #[test]
    fn a_corrupt_yank_reply_fails_its_request() {
        let truncated = Frame::new(MSG_YANK_RESPONSE, 41, vec![0xff, 0x00]).expect("frame");

        let events = events_for_frame(&truncated).expect("owned type");

        match events.as_slice() {
            [UserEvent::RequestFailed { serial, code }] => {
                assert_eq!(*serial, 41);
                assert_eq!(*code, None, "no daemon error code to report");
            }
            other => panic!("expected one RequestFailed, got {other:?}"),
        }
    }

    /// The error code is what tells a rejected copy-mode entry from a
    /// transient read failure; decoding it to `None` would make every
    /// failure look retryable and silently disable the bell.
    #[test]
    fn an_error_frame_carries_its_decoded_code() {
        let events = events_for_frame(&error_frame(9, ErrorCode::UnknownPane)).expect("owned type");

        match events.as_slice() {
            [UserEvent::RequestFailed { serial, code }, UserEvent::Bell] => {
                assert_eq!(*serial, 9);
                assert_eq!(*code, Some(ErrorCode::UnknownPane));
            }
            other => panic!("expected RequestFailed + Bell, got {other:?}"),
        }
    }

    /// An internal error is not a user-triggered rejection, so it reports
    /// its code without the bell.
    #[test]
    fn an_internal_error_reports_its_code_without_ringing() {
        let events =
            events_for_frame(&error_frame(9, ErrorCode::InternalError)).expect("owned type");

        match events.as_slice() {
            [UserEvent::RequestFailed { code, .. }] => {
                assert_eq!(*code, Some(ErrorCode::InternalError));
            }
            other => panic!("expected a lone RequestFailed, got {other:?}"),
        }
    }

    #[test]
    fn only_user_triggered_rejections_ring_the_bell() {
        for code in [
            ErrorCode::LayoutRejected,
            ErrorCode::UnknownPane,
            ErrorCode::UnknownTab,
            ErrorCode::UnknownWorkspace,
        ] {
            assert!(error_rings_bell(Some(code)), "{code:?} should ring");
        }
        for code in [
            ErrorCode::InternalError,
            ErrorCode::MalformedPayload,
            ErrorCode::PaneExited,
        ] {
            assert!(!error_rings_bell(Some(code)), "{code:?} must not ring");
        }
        assert!(!error_rings_bell(None));
    }

    /// Message types this helper does not own must stay unclaimed, or
    /// `daemon_reader` stops logging them as unhandled.
    #[test]
    fn an_unowned_message_type_is_not_claimed() {
        let frame = Frame::new(0x7FFF, 1, vec![]).expect("frame");
        assert!(events_for_frame(&frame).is_none());
    }

    /// A malformed payload is logged and dropped, not turned into an
    /// event carrying garbage — but the type stays claimed.
    #[test]
    fn a_malformed_payload_yields_no_events() {
        let frame = Frame::new(MSG_SCROLLBACK_DATA, 5, vec![0x00]).expect("frame");
        assert_eq!(events_for_frame(&frame).expect("owned type").len(), 0);
    }

    #[test]
    fn dirty_notify_with_no_in_flight_sends() {
        let mut s = ReaderState::default();
        assert_eq!(s.on_dirty_notify(1), DirtyOutcome::Send(0));
        assert!(s.in_flight.contains(&1));
        assert!(!s.pending.contains(&1));
    }

    #[test]
    fn dirty_notify_uses_last_seen_seqno() {
        let mut s = ReaderState::default();
        s.seqnos.insert(1, 42);
        assert_eq!(s.on_dirty_notify(1), DirtyOutcome::Send(42));
    }

    #[test]
    fn dirty_notify_while_in_flight_coalesces() {
        let mut s = ReaderState::default();
        s.on_dirty_notify(1);
        assert_eq!(s.on_dirty_notify(1), DirtyOutcome::Coalesce);
        assert!(s.pending.contains(&1));
    }

    #[test]
    fn many_coalesced_dirty_collapse_to_single_followup() {
        let mut s = ReaderState::default();
        s.on_dirty_notify(1);
        for _ in 0..100 {
            assert_eq!(s.on_dirty_notify(1), DirtyOutcome::Coalesce);
        }
        assert_eq!(s.on_render_update(1, 5), UpdateOutcome::SendFollowUp(5));
        // Follow-up re-marks in_flight; pending was drained by the take.
        assert!(s.in_flight.contains(&1));
        assert!(!s.pending.contains(&1));
    }

    #[test]
    fn render_update_without_pending_is_done() {
        let mut s = ReaderState::default();
        s.on_dirty_notify(1);
        assert_eq!(s.on_render_update(1, 5), UpdateOutcome::Done);
        assert!(!s.in_flight.contains(&1));
    }

    #[test]
    fn render_update_with_pending_fires_followup_with_new_seqno() {
        let mut s = ReaderState::default();
        s.on_dirty_notify(1);
        s.on_dirty_notify(1); // sets pending
        // Follow-up uses the seqno from this update, not the previous one.
        assert_eq!(s.on_render_update(1, 99), UpdateOutcome::SendFollowUp(99));
        assert!(s.in_flight.contains(&1));
        assert!(!s.pending.contains(&1));
    }

    #[test]
    fn cross_pane_state_is_independent() {
        let mut s = ReaderState::default();
        s.on_dirty_notify(1);
        s.on_dirty_notify(2);
        assert!(s.in_flight.contains(&1));
        assert!(s.in_flight.contains(&2));
        // Render for pane 1 must not touch pane 2.
        s.on_render_update(1, 10);
        assert!(!s.in_flight.contains(&1));
        assert!(s.in_flight.contains(&2));
    }

    #[test]
    fn render_for_unknown_pane_is_safe() {
        // Spurious update for a pane we never requested. Server bug or
        // stale frame after disconnect; must not panic or fire follow-up.
        let mut s = ReaderState::default();
        assert_eq!(s.on_render_update(99, 1), UpdateOutcome::Done);
        assert!(!s.in_flight.contains(&99));
        assert!(!s.pending.contains(&99));
    }

    #[test]
    fn pane_exit_clears_all_per_pane_state() {
        let mut s = ReaderState::default();
        s.on_dirty_notify(1);
        s.on_dirty_notify(1); // sets pending
        s.on_pane_exit(1);
        assert!(!s.in_flight.contains(&1));
        assert!(!s.pending.contains(&1));
        assert!(!s.seqnos.contains_key(&1));
    }
}
