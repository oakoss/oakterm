//! Integration tests: daemon lifecycle, handshake, and error responses.
//!
//! Each test gets its own tempdir + socket path for parallel execution.

use bytes::BytesMut;
use oakterm_protocol::frame::{Frame, FrameCodec};
use oakterm_protocol::input::Resize;
use oakterm_protocol::message::{
    ClientHello, ClientType, ClosePane, CreatePane, CreatePaneResponse, ErrorCode, ErrorMessage,
    HandshakeStatus, ListPanesResponse, MSG_CLOSE_PANE, MSG_CLOSE_PANE_RESPONSE, MSG_CREATE_PANE,
    MSG_CREATE_PANE_RESPONSE, MSG_ERROR, MSG_LIST_PANES, MSG_LIST_PANES_RESPONSE, MSG_PANE_EXITED,
    MSG_SERVER_HELLO, PaneExited, ServerHello,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio_util::codec::{Decoder, Encoder};

#[tokio::test]
async fn daemon_handshake() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("sock");
    let daemon = oakterm_daemon::server::Daemon::with_socket_path(80, 24, socket.clone());

    let handle = tokio::spawn(async move {
        let _ = daemon.run().await;
    });

    wait_for_socket(&socket).await;

    let mut stream = UnixStream::connect(&socket).await.expect("connect");

    // Send `ClientHello`.
    let hello = ClientHello {
        protocol_version_major: ClientHello::VERSION_MAJOR,
        protocol_version_minor: ClientHello::VERSION_MINOR,
        client_type: ClientType::Gui,
        client_name: "test-client".to_string(),
    };
    let frame = hello.to_frame(1).expect("encode hello");
    let mut codec = FrameCodec;
    let mut buf = BytesMut::new();
    codec.encode(frame, &mut buf).expect("encode frame");
    stream.write_all(&buf).await.expect("write hello");

    // Read `ServerHello`.
    let mut read_buf = BytesMut::with_capacity(256);
    let n = stream.read_buf(&mut read_buf).await.expect("read response");
    assert!(n > 0, "should receive ServerHello");

    let response = codec.decode(&mut read_buf).expect("decode").expect("frame");
    assert_eq!(response.msg_type, MSG_SERVER_HELLO);
    assert_eq!(response.serial, 1);

    let server_hello = ServerHello::decode(&response.payload).expect("decode ServerHello");
    assert_eq!(server_hello.status, HandshakeStatus::Accepted);
    assert_eq!(
        server_hello.protocol_version_major,
        ClientHello::VERSION_MAJOR
    );

    handle.abort();
}

#[tokio::test]
async fn unknown_message_type_returns_error() {
    let (mut stream, mut codec, _handle) = connect_and_handshake().await;

    // Send a frame with an unknown message type.
    let frame = Frame::new(0xFFFF, 42, vec![]).expect("create frame");
    let mut buf = BytesMut::new();
    codec.encode(frame, &mut buf).expect("encode frame");
    stream.write_all(&buf).await.expect("write unknown msg");

    // Read error response.
    let mut read_buf = BytesMut::with_capacity(256);
    let n = stream.read_buf(&mut read_buf).await.expect("read response");
    assert!(n > 0, "should receive error response");

    let response = codec.decode(&mut read_buf).expect("decode").expect("frame");
    assert_eq!(response.msg_type, MSG_ERROR);
    assert_eq!(response.serial, 42);

    let err = ErrorMessage::decode(&response.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::InvalidMessage as u32);
}

#[tokio::test]
async fn malformed_payload_returns_error() {
    let (mut stream, mut codec, _handle) = connect_and_handshake().await;

    // Send a KeyInput (0x64) with a truncated payload (needs at least 6 bytes).
    let frame = Frame::new(0x64, 99, vec![0x00]).expect("create frame");
    let mut buf = BytesMut::new();
    codec.encode(frame, &mut buf).expect("encode frame");
    stream.write_all(&buf).await.expect("write malformed msg");

    // Read error response.
    let mut read_buf = BytesMut::with_capacity(256);
    let n = stream.read_buf(&mut read_buf).await.expect("read response");
    assert!(n > 0, "should receive error response");

    let response = codec.decode(&mut read_buf).expect("decode").expect("frame");
    assert_eq!(response.msg_type, MSG_ERROR);
    assert_eq!(response.serial, 99);

    let err = ErrorMessage::decode(&response.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::MalformedPayload as u32);
}

/// Connect to a daemon, complete the handshake, and return the stream.
/// The returned `TestDaemon` must be held alive for the socket to remain valid.
async fn connect_and_handshake() -> (UnixStream, FrameCodec, TestDaemon) {
    let td = TestDaemon::start().await;

    let mut stream = UnixStream::connect(&td.socket).await.expect("connect");
    let mut codec = FrameCodec;

    let hello = ClientHello {
        protocol_version_major: ClientHello::VERSION_MAJOR,
        protocol_version_minor: ClientHello::VERSION_MINOR,
        client_type: ClientType::Gui,
        client_name: "test-client".to_string(),
    };
    let frame = hello.to_frame(1).expect("encode hello");
    let mut buf = BytesMut::new();
    codec.encode(frame, &mut buf).expect("encode frame");
    stream.write_all(&buf).await.expect("write hello");

    let mut read_buf = BytesMut::with_capacity(256);
    let n = stream.read_buf(&mut read_buf).await.expect("read response");
    assert!(n > 0, "should receive ServerHello");

    let response = codec.decode(&mut read_buf).expect("decode").expect("frame");
    assert_eq!(response.msg_type, MSG_SERVER_HELLO);

    let server_hello = ServerHello::decode(&response.payload).expect("decode ServerHello");
    assert_eq!(server_hello.status, HandshakeStatus::Accepted);

    (stream, codec, td)
}

/// Holds a daemon task and its tempdir alive for the duration of a test.
struct TestDaemon {
    socket: std::path::PathBuf,
    _dir: tempfile::TempDir,
    _handle: tokio::task::JoinHandle<()>,
}

impl TestDaemon {
    async fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("sock");
        let daemon = oakterm_daemon::server::Daemon::with_socket_path(80, 24, socket.clone());

        let handle = tokio::spawn(async move {
            let _ = daemon.run().await;
        });

        wait_for_socket(&socket).await;

        Self {
            socket,
            _dir: dir,
            _handle: handle,
        }
    }
}

async fn wait_for_socket(path: &std::path::Path) {
    for i in 0..20 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50 * (i + 1))).await;
    }
    panic!("daemon did not bind socket in time");
}

/// Closing a pane must reap the child even when the shell produces no output
/// at all. The spawned `sleep` is silent for its entire lifetime, so the read
/// loop spends the whole test blocked in `readable().await` — the only path
/// that can wake it is the cancel channel from `MSG_CLOSE_PANE`.
///
/// Without active cancellation, the loop would stay parked, the `Pty` would
/// stay alive in the read-loop task, and the `kill+wait` in `Pty::Drop` would
/// never run. The reap-within-500ms assertion would fail.
#[tokio::test]
async fn close_pane_kills_idle_child_promptly() {
    use rustix::process::{Pid, test_kill_process};

    let (mut stream, mut codec, _td) = connect_and_handshake_as(ClientType::Control).await;

    // `sleep` produces no output and never exits on its own within the test
    // window. Only Pty::Drop (triggered by the read loop exiting via cancel)
    // can kill it.
    let create = CreatePane {
        command: "/bin/sleep 60".to_string(),
        cwd: String::new(),
    };
    let frame = Frame::new(
        MSG_CREATE_PANE,
        100,
        create.encode().expect("encode CreatePane"),
    )
    .expect("create-pane frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 100).await;
    assert_eq!(resp.msg_type, MSG_CREATE_PANE_RESPONSE);
    let create_resp = CreatePaneResponse::decode(&resp.payload).expect("decode CreatePaneResponse");
    let pane_id = create_resp.pane_id;
    assert!(pane_id > 0, "expected non-default pane, got {pane_id}");

    // Resize triggers spawn (push, no response).
    let resize = Resize {
        pane_id,
        cols: 80,
        rows: 24,
        pixel_width: 0,
        pixel_height: 0,
    };
    write_frame(
        &mut stream,
        &mut codec,
        resize.to_frame().expect("encode Resize"),
    )
    .await;

    // Poll ListPanes until the new pane reports a non-zero PID.
    let pid = poll_for_pid(&mut stream, &mut codec, pane_id).await;
    let pid_i32 = i32::try_from(pid).expect("PID fits in i32");
    let live_pid = Pid::from_raw(pid_i32).expect("daemon-reported PID is positive");

    // Close the pane. The cancel signal must reach the read loop so the
    // Pty drops and Pty::Drop kills + reaps the child. With no PTY output,
    // this is the only path that can free the child.
    let close = ClosePane { pane_id };
    let frame = Frame::new(MSG_CLOSE_PANE, 200, close.encode()).expect("close-pane frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 200).await;
    assert_eq!(resp.msg_type, MSG_CLOSE_PANE_RESPONSE);

    // Within 500ms the child must be reaped. test_kill_process(signal 0)
    // returns Err(Errno::SRCH) once the PID is no longer a valid live process.
    let mut alive = true;
    for _ in 0..50 {
        if test_kill_process(live_pid).is_err() {
            alive = false;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        !alive,
        "child {pid} should have been killed within 500ms of ClosePane"
    );
}

#[tokio::test]
async fn pane_exited_reports_non_zero_child_status() {
    let (mut stream, mut codec, _td) = connect_and_handshake_as(ClientType::Control).await;

    let create = CreatePane {
        command: "/bin/sh -c \"exit 7\"".to_string(),
        cwd: String::new(),
    };
    let frame = Frame::new(
        MSG_CREATE_PANE,
        300,
        create.encode().expect("encode CreatePane"),
    )
    .expect("create-pane frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 300).await;
    assert_eq!(resp.msg_type, MSG_CREATE_PANE_RESPONSE);
    let create_resp = CreatePaneResponse::decode(&resp.payload).expect("decode CreatePaneResponse");
    let pane_id = create_resp.pane_id;

    let resize = Resize {
        pane_id,
        cols: 80,
        rows: 24,
        pixel_width: 0,
        pixel_height: 0,
    };
    write_frame(
        &mut stream,
        &mut codec,
        resize.to_frame().expect("encode Resize"),
    )
    .await;

    let frame = read_push_with_msg_type(&mut stream, &mut codec, MSG_PANE_EXITED).await;
    let exited = PaneExited::decode(&frame.payload).expect("decode PaneExited");
    assert_eq!(exited.pane_id, pane_id);
    assert_eq!(exited.exit_code, 7);
}

#[tokio::test]
async fn pane_exited_reports_signal_killed_child() {
    use rustix::process::{Pid, Signal, kill_process};

    let (mut stream, mut codec, _td) = connect_and_handshake_as(ClientType::Control).await;

    // Long sleep so the child only exits via the SIGTERM we send.
    let create = CreatePane {
        command: "/bin/sh -c \"sleep 30\"".to_string(),
        cwd: String::new(),
    };
    let frame = Frame::new(
        MSG_CREATE_PANE,
        310,
        create.encode().expect("encode CreatePane"),
    )
    .expect("create-pane frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 310).await;
    assert_eq!(resp.msg_type, MSG_CREATE_PANE_RESPONSE);
    let create_resp = CreatePaneResponse::decode(&resp.payload).expect("decode CreatePaneResponse");
    let pane_id = create_resp.pane_id;

    let resize = Resize {
        pane_id,
        cols: 80,
        rows: 24,
        pixel_width: 0,
        pixel_height: 0,
    };
    write_frame(
        &mut stream,
        &mut codec,
        resize.to_frame().expect("encode Resize"),
    )
    .await;

    // Wait for the daemon to fork+exec, then signal the child directly so the
    // PTY EOFs and pty_read_loop captures status via wait().
    let pid = poll_for_pid(&mut stream, &mut codec, pane_id).await;
    #[allow(clippy::cast_possible_wrap)] // PID fits in i32
    let raw_pid = pid as i32;
    let target = Pid::from_raw(raw_pid).expect("non-zero PID");
    kill_process(target, Signal::TERM).expect("SIGTERM child");

    let frame = read_push_with_msg_type(&mut stream, &mut codec, MSG_PANE_EXITED).await;
    let exited = PaneExited::decode(&frame.payload).expect("decode PaneExited");
    assert_eq!(exited.pane_id, pane_id);
    // POSIX shell convention: signal-killed children report 128 + signal.
    assert_eq!(exited.exit_code, 128 + 15);
}

/// Connect + handshake with a chosen client type. Control clients don't
/// receive render-update pushes, which keeps the response stream clean.
async fn connect_and_handshake_as(client_type: ClientType) -> (UnixStream, FrameCodec, TestDaemon) {
    let td = TestDaemon::start().await;

    let mut stream = UnixStream::connect(&td.socket).await.expect("connect");
    let mut codec = FrameCodec;

    let hello = ClientHello {
        protocol_version_major: ClientHello::VERSION_MAJOR,
        protocol_version_minor: ClientHello::VERSION_MINOR,
        client_type,
        client_name: "test-client".to_string(),
    };
    let frame = hello.to_frame(1).expect("encode hello");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 1).await;
    assert_eq!(resp.msg_type, MSG_SERVER_HELLO);
    let server_hello = ServerHello::decode(&resp.payload).expect("decode ServerHello");
    assert_eq!(server_hello.status, HandshakeStatus::Accepted);

    (stream, codec, td)
}

async fn write_frame(stream: &mut UnixStream, codec: &mut FrameCodec, frame: Frame) {
    let mut buf = BytesMut::new();
    codec.encode(frame, &mut buf).expect("encode frame");
    stream.write_all(&buf).await.expect("write frame");
}

/// Read frames until one matches `serial`, ignoring any pushes. Times out
/// after ~3 seconds so a hung daemon doesn't hang the test forever.
async fn read_response_with_serial(
    stream: &mut UnixStream,
    codec: &mut FrameCodec,
    serial: u32,
) -> Frame {
    let mut buf = BytesMut::with_capacity(4096);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        // Drain any complete frames currently in the buffer.
        while let Some(frame) = codec.decode(&mut buf).expect("decode") {
            if frame.serial == serial {
                return frame;
            }
            // Otherwise it's a push (DirtyNotify, etc.) — ignore.
        }
        // saturating_duration_since returns Duration::ZERO past the deadline,
        // and tokio::time::timeout(ZERO, _) immediately yields Err — so the
        // timeout arm covers the past-deadline case without a separate guard.
        let timeout = deadline.saturating_duration_since(std::time::Instant::now());
        let result = tokio::time::timeout(timeout, stream.read_buf(&mut buf))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for serial {serial}"))
            .unwrap_or_else(|e| panic!("read error waiting for serial {serial}: {e}"));
        assert!(
            result > 0,
            "daemon closed connection while waiting for serial {serial}"
        );
    }
}

/// Read pushes until one matches `msg_type`, ignoring request/response frames.
async fn read_push_with_msg_type(
    stream: &mut UnixStream,
    codec: &mut FrameCodec,
    msg_type: u16,
) -> Frame {
    let mut buf = BytesMut::with_capacity(4096);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        while let Some(frame) = codec.decode(&mut buf).expect("decode") {
            if frame.serial == 0 && frame.msg_type == msg_type {
                return frame;
            }
        }
        let timeout = deadline.saturating_duration_since(std::time::Instant::now());
        let result = tokio::time::timeout(timeout, stream.read_buf(&mut buf))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for push type {msg_type:#x}"))
            .unwrap_or_else(|e| panic!("read error waiting for push type {msg_type:#x}: {e}"));
        assert!(
            result > 0,
            "daemon closed connection while waiting for push type {msg_type:#x}"
        );
    }
}

/// Poll `ListPanes` until `target_pane` reports a non-zero PID. Times out
/// after 5 seconds (fork+exec under the `PaneManager` mutex can spike on a
/// loaded CI runner). Helper owns serials in the 1001+ range.
async fn poll_for_pid(stream: &mut UnixStream, codec: &mut FrameCodec, target_pane: u32) -> u32 {
    let mut serial = 1000;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        serial += 1;
        let frame = Frame::new(MSG_LIST_PANES, serial, vec![]).expect("list-panes frame");
        write_frame(stream, codec, frame).await;
        let resp = read_response_with_serial(stream, codec, serial).await;
        assert_eq!(resp.msg_type, MSG_LIST_PANES_RESPONSE);
        let list = ListPanesResponse::decode(&resp.payload).expect("decode ListPanesResponse");
        if let Some(info) = list.panes.iter().find(|p| p.pane_id == target_pane) {
            if info.pid != 0 {
                return info.pid;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("PTY for pane {target_pane} did not report a PID within 1s");
}
