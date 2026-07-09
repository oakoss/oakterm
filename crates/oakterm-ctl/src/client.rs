//! Blocking Unix-socket client for the oakterm daemon.
//!
//! Single-shot request/response over `std::os::unix::net::UnixStream`. The
//! daemon interleaves unsolicited pushes (dirty/bell/title, serial 0) with
//! responses, so [`DaemonClient::request`] drains until the frame matching its
//! own serial arrives.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use oakterm_protocol::frame::{Frame, HEADER_SIZE, MAGIC, MAX_PAYLOAD};
use oakterm_protocol::input::KeyInput;
use oakterm_protocol::message::{
    ClientHello, ClientType, ErrorMessage, GetScrollback, HandshakeStatus, ListPanesResponse,
    MSG_ERROR, MSG_GET_RENDER_UPDATE, MSG_GET_SCROLLBACK, MSG_LIST_PANES, MSG_LIST_PANES_RESPONSE,
    MSG_PING, MSG_PONG, MSG_RENDER_UPDATE, MSG_SCROLLBACK_DATA, MSG_SERVER_HELLO, PaneInfo,
    ScrollbackData, ServerHello,
};
use oakterm_protocol::render::{GetRenderUpdate, RenderUpdate};

/// Bound on any single blocking read, so a wedged daemon can't hang the CLI.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct DaemonClient {
    stream: UnixStream,
    next_serial: u32,
}

impl DaemonClient {
    /// Connect to the running daemon (path from `$TMPDIR`/`$XDG_RUNTIME_DIR`)
    /// and complete the handshake.
    pub(crate) fn connect() -> io::Result<Self> {
        let path = oakterm_protocol::socket::socket_path()?;
        let mut stream = UnixStream::connect(&path).map_err(|e| {
            let hint = if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) {
                " (is oakterm running?)"
            } else {
                ""
            };
            io::Error::new(
                e.kind(),
                format!("cannot reach daemon at {}: {e}{hint}", path.display()),
            )
        })?;
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        handshake(&mut stream)?;
        // Serial 1 was spent on the handshake; requests start at 2.
        Ok(Self {
            stream,
            next_serial: 2,
        })
    }

    pub(crate) fn list_panes(&mut self) -> io::Result<Vec<PaneInfo>> {
        let payload = self.request(MSG_LIST_PANES, Vec::new(), MSG_LIST_PANES_RESPONSE)?;
        Ok(ListPanesResponse::decode(&payload)?.panes)
    }

    /// Write raw key bytes to a pane's PTY.
    ///
    /// `KeyInput` is a fire-and-forget push: the daemon silently drops input to
    /// an unknown or exited pane (its error, if any, rides serial 0 and is
    /// indistinguishable from a push), so validate the target first rather than
    /// report a no-op as success. Then a `Ping` round-trip guarantees in-order
    /// delivery before this short-lived process exits and closes the socket.
    pub(crate) fn send_input(&mut self, pane_id: u32, key_data: Vec<u8>) -> io::Result<()> {
        let panes = self.list_panes()?;
        let pane = panes
            .iter()
            .find(|p| p.pane_id == pane_id)
            .ok_or_else(|| io::Error::other(format!("unknown pane {pane_id}")))?;
        if !crate::pane_running(pane) {
            return Err(io::Error::other(format!(
                "pane {pane_id} is not running; cannot send input"
            )));
        }
        // A narrow race remains if the pane exits between this check and the
        // write; the daemon logs and drops the input in that case.
        let frame = KeyInput { pane_id, key_data }.to_frame()?;
        self.write_frame(&frame)?;
        self.request(MSG_PING, Vec::new(), MSG_PONG)?;
        Ok(())
    }

    /// Fetch the pane's current visible screen (`since_seqno = 0` = all rows).
    pub(crate) fn visible_screen(&mut self, pane_id: u32) -> io::Result<RenderUpdate> {
        let req = GetRenderUpdate {
            pane_id,
            since_seqno: 0,
        };
        let payload = self.request(MSG_GET_RENDER_UPDATE, req.encode(), MSG_RENDER_UPDATE)?;
        RenderUpdate::decode(&payload)
    }

    pub(crate) fn scrollback(&mut self, pane_id: u32, lines: u32) -> io::Result<ScrollbackData> {
        let req = GetScrollback {
            pane_id,
            start_row: -i64::from(lines),
            count: lines,
        };
        let payload = self.request(MSG_GET_SCROLLBACK, req.encode(), MSG_SCROLLBACK_DATA)?;
        ScrollbackData::decode(&payload)
    }

    fn next_serial(&mut self) -> u32 {
        let serial = self.next_serial;
        // Never wrap to 0 — serial 0 marks unsolicited daemon pushes.
        self.next_serial = self.next_serial.checked_add(1).unwrap_or(1);
        serial
    }

    /// Send a request, then drain frames until the one echoing our serial
    /// arrives. Serial-0 pushes and any stale frames are discarded.
    fn request(&mut self, msg_type: u16, payload: Vec<u8>, expect: u16) -> io::Result<Vec<u8>> {
        let serial = self.next_serial();
        self.write_frame(&Frame::new(msg_type, serial, payload)?)?;
        loop {
            let frame = read_frame(&mut self.stream)?;
            if frame.serial != serial {
                continue;
            }
            if frame.msg_type == expect {
                return Ok(frame.payload);
            }
            if frame.msg_type == MSG_ERROR {
                let err = ErrorMessage::decode(&frame.payload)?;
                return Err(io::Error::other(format!(
                    "daemon error {}: {}",
                    err.code, err.message
                )));
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected response msg_type {:#x}", frame.msg_type),
            ));
        }
    }

    fn write_frame(&mut self, frame: &Frame) -> io::Result<()> {
        self.stream.write_all(&frame.encode_to_vec())
    }
}

fn handshake(stream: &mut UnixStream) -> io::Result<()> {
    let hello = ClientHello {
        protocol_version_major: ClientHello::VERSION_MAJOR,
        protocol_version_minor: ClientHello::VERSION_MINOR,
        client_type: ClientType::Control,
        client_name: "oakterm-ctl".to_string(),
    };
    stream.write_all(&hello.to_frame(1)?.encode_to_vec())?;

    let frame = read_frame(stream)?;
    if frame.msg_type != MSG_SERVER_HELLO {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected ServerHello, got msg_type {:#x}", frame.msg_type),
        ));
    }
    let server = ServerHello::decode(&frame.payload)?;
    if server.status != HandshakeStatus::Accepted {
        return Err(io::Error::other(format!(
            "daemon rejected handshake: {:?}",
            server.status
        )));
    }
    Ok(())
}

/// Mirror of the GUI's reader (`oakterm/src/daemon_conn.rs`); the protocol
/// crate exposes no blocking reader, only the async `FrameCodec`.
fn read_frame(stream: &mut impl Read) -> io::Result<Frame> {
    let mut header = [0u8; HEADER_SIZE];
    read_exact(stream, &mut header)?;
    if header[0..2] != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid magic bytes",
        ));
    }
    let msg_type = u16::from_le_bytes([header[3], header[4]]);
    let serial = u32::from_le_bytes([header[5], header[6], header[7], header[8]]);
    let payload_len = u32::from_le_bytes([header[9], header[10], header[11], header[12]]);
    if payload_len > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("payload too large: {payload_len}"),
        ));
    }
    let mut payload = vec![0u8; payload_len as usize];
    if !payload.is_empty() {
        read_exact(stream, &mut payload)?;
    }
    Frame::new(msg_type, serial, payload)
}

/// Maps a mid-frame EOF or a read timeout (set in `connect`) to an actionable
/// message instead of the opaque stock text.
fn read_exact(stream: &mut impl Read, buf: &mut [u8]) -> io::Result<()> {
    stream.read_exact(buf).map_err(|e| match e.kind() {
        io::ErrorKind::UnexpectedEof => {
            io::Error::new(e.kind(), "daemon closed the connection unexpectedly")
        }
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
            io::Error::new(io::ErrorKind::TimedOut, "daemon did not respond in time")
        }
        _ => e,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use oakterm_protocol::message::MSG_DIRTY_NOTIFY;
    use std::io::Cursor;

    fn frame_bytes(msg_type: u16, serial: u32, payload: Vec<u8>) -> Vec<u8> {
        Frame::new(msg_type, serial, payload)
            .unwrap()
            .encode_to_vec()
    }

    /// A client wired to one end of a socketpair; the returned stream is the
    /// "daemon" end to preload with frames. The read timeout keeps a
    /// mis-written test from hanging on `read_exact`.
    fn paired_client() -> (DaemonClient, UnixStream) {
        let (a, b) = UnixStream::pair().unwrap();
        a.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        (
            DaemonClient {
                stream: a,
                next_serial: 2,
            },
            b,
        )
    }

    #[test]
    fn request_drains_serial_zero_push_then_returns_response() {
        let (mut client, mut daemon) = paired_client();
        // A serial-0 push precedes the real (serial-2) response.
        daemon
            .write_all(&frame_bytes(MSG_DIRTY_NOTIFY, 0, vec![]))
            .unwrap();
        daemon
            .write_all(&frame_bytes(MSG_PONG, 2, vec![1, 2, 3]))
            .unwrap();
        let payload = client.request(MSG_PING, Vec::new(), MSG_PONG).unwrap();
        assert_eq!(payload, vec![1, 2, 3], "push drained, response returned");
    }

    #[test]
    fn request_maps_daemon_error_on_matching_serial() {
        let (mut client, mut daemon) = paired_client();
        let err = ErrorMessage {
            code: 1,
            message: "unknown pane".into(),
        }
        .encode()
        .unwrap();
        daemon.write_all(&frame_bytes(MSG_ERROR, 2, err)).unwrap();
        let e = client
            .request(MSG_LIST_PANES, Vec::new(), MSG_LIST_PANES_RESPONSE)
            .unwrap_err();
        assert!(e.to_string().contains("unknown pane"), "got: {e}");
    }

    #[test]
    fn request_rejects_matching_serial_with_unexpected_type() {
        let (mut client, mut daemon) = paired_client();
        daemon
            .write_all(&frame_bytes(MSG_DIRTY_NOTIFY, 2, vec![]))
            .unwrap();
        let e = client.request(MSG_PING, Vec::new(), MSG_PONG).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    fn server_hello(status: HandshakeStatus) -> Vec<u8> {
        ServerHello {
            status,
            protocol_version_major: 1,
            protocol_version_minor: 3,
            server_version: "test".into(),
        }
        .to_frame(1)
        .unwrap()
        .encode_to_vec()
    }

    #[test]
    fn handshake_accepts_server_hello() {
        let (mut a, mut daemon) = UnixStream::pair().unwrap();
        a.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        daemon
            .write_all(&server_hello(HandshakeStatus::Accepted))
            .unwrap();
        assert!(handshake(&mut a).is_ok());
    }

    #[test]
    fn handshake_rejects_non_accepted_status() {
        let (mut a, mut daemon) = UnixStream::pair().unwrap();
        a.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        daemon
            .write_all(&server_hello(HandshakeStatus::VersionMismatch))
            .unwrap();
        assert!(handshake(&mut a).is_err());
    }

    #[test]
    fn read_frame_rejects_bad_magic() {
        let mut bad = Cursor::new(vec![0u8; HEADER_SIZE]);
        assert!(read_frame(&mut bad).is_err());
    }

    #[test]
    fn read_frame_rejects_oversized_payload() {
        // Valid magic, then payload_len = MAX_PAYLOAD + 1.
        let mut header = vec![MAGIC[0], MAGIC[1], 0, 0, 0, 0, 0, 0, 0];
        header.extend_from_slice(&(MAX_PAYLOAD + 1).to_le_bytes());
        let mut cur = Cursor::new(header);
        let e = read_frame(&mut cur).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn next_serial_wraps_past_zero() {
        let (a, _b) = UnixStream::pair().unwrap();
        let mut client = DaemonClient {
            stream: a,
            next_serial: u32::MAX,
        };
        assert_eq!(client.next_serial(), u32::MAX);
        assert_eq!(
            client.next_serial(),
            1,
            "wraps to 1, skipping push serial 0"
        );
    }
}
