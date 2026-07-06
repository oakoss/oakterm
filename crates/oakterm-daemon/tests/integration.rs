//! Integration tests: daemon lifecycle, handshake, and error responses.
//!
//! Each test gets its own tempdir + socket path for parallel execution.

use bytes::BytesMut;
use oakterm_protocol::frame::{Frame, FrameCodec};
use oakterm_protocol::input::Resize;
use oakterm_protocol::message::{
    ClientHello, ClientType, ClosePane, CreatePane, CreatePaneResponse, ErrorCode, ErrorMessage,
    GetLayoutTree, HandshakeStatus, LayoutTree, LayoutTreeNode, ListPanesResponse, MSG_CLOSE_PANE,
    MSG_CLOSE_PANE_RESPONSE, MSG_CREATE_PANE, MSG_CREATE_PANE_RESPONSE, MSG_ERROR,
    MSG_GET_LAYOUT_TREE, MSG_LAYOUT_TREE, MSG_LIST_PANES, MSG_LIST_PANES_RESPONSE, MSG_PANE_EXITED,
    MSG_PING, MSG_PONG, MSG_REQUEST_SHUTDOWN, MSG_RESIZE_PANE, MSG_SERVER_HELLO, MSG_SHUTDOWN,
    MSG_SHUTDOWN_ACK, MSG_SPLIT_PANE, MSG_SPLIT_PANE_RESPONSE, MSG_SWAP_PANE,
    MSG_SWAP_PANE_RESPONSE, PaneExited, RequestShutdown, RequestShutdownReason, ResizePane,
    ServerHello, Shutdown, ShutdownAck, ShutdownAckStatus, ShutdownReason, SplitDirection,
    SplitPane, SplitPaneResponse, SwapPane,
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

/// Spec-0001: an unknown `msg_type` is ignored, not errored. A subsequent
/// Ping must still get a Pong, with no Error frame in between and the
/// connection left open.
#[tokio::test]
async fn unknown_message_type_is_ignored() {
    let (mut stream, mut codec, _handle) = connect_and_handshake().await;

    // Non-empty payload: the frame must be skipped whole, payload included.
    let frame = Frame::new(0xFFFF, 42, vec![0xDE, 0xAD, 0xBE, 0xEF]).expect("create frame");
    let mut buf = BytesMut::new();
    codec.encode(frame, &mut buf).expect("encode frame");
    stream.write_all(&buf).await.expect("write unknown msg");

    let ping = Frame::new(MSG_PING, 43, vec![]).expect("create ping");
    let mut buf = BytesMut::new();
    codec.encode(ping, &mut buf).expect("encode ping");
    stream.write_all(&buf).await.expect("write ping");

    // Read frames in order, skipping pushes (serial 0). The first
    // serial-carrying frame must be the Pong — a buggy daemon would send
    // an Error for serial 42 first.
    let read_ordered = async {
        let mut read_buf = BytesMut::with_capacity(256);
        loop {
            if let Some(response) = codec.decode(&mut read_buf).expect("decode") {
                if response.serial == 0 {
                    continue;
                }
                assert_ne!(
                    response.msg_type, MSG_ERROR,
                    "daemon must ignore unknown msg_type, not error"
                );
                assert_eq!(response.msg_type, MSG_PONG);
                assert_eq!(response.serial, 43);
                break;
            }
            let n = stream.read_buf(&mut read_buf).await.expect("read response");
            assert!(n > 0, "daemon closed connection after unknown msg_type");
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), read_ordered)
        .await
        .expect("daemon did not answer the Ping within 5s");
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

#[tokio::test]
async fn oversized_resize_is_rejected() {
    let (mut stream, mut codec, _handle) = connect_and_handshake().await;

    // Create a pane; it stays NotSpawned until a Resize arrives.
    let create = CreatePane {
        command: "/bin/sleep 60".to_string(),
        cwd: String::new(),
    };
    let frame = Frame::new(
        MSG_CREATE_PANE,
        10,
        create.encode().expect("encode CreatePane"),
    )
    .expect("create-pane frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 10).await;
    let pane_id = CreatePaneResponse::decode(&resp.payload)
        .expect("decode CreatePaneResponse")
        .pane_id;

    // A resize whose dimensions exceed the maximum must be rejected before any
    // grid allocation or PTY spawn, rather than driving a multi-terabyte alloc.
    let resize = Resize {
        pane_id,
        cols: u16::MAX,
        rows: u16::MAX,
        pixel_width: 0,
        pixel_height: 0,
    };
    let frame = Frame::new(0x66, 11, resize.encode()).expect("resize frame");
    write_frame(&mut stream, &mut codec, frame).await;

    let response = read_response_with_serial(&mut stream, &mut codec, 11).await;
    assert_eq!(response.msg_type, MSG_ERROR);
    let err = ErrorMessage::decode(&response.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::MalformedPayload as u32);
}

#[tokio::test]
async fn corrupt_framing_closes_connection() {
    let (mut stream, _codec, _handle) = connect_and_handshake().await;

    // Bytes that fail frame decoding (bad magic): the daemon must close the
    // connection — corrupt framing can never resync, and leaving the bytes
    // buffered would retry the same error forever.
    stream
        .write_all(&[0xFF; 16])
        .await
        .expect("write corrupt bytes");

    let mut read_buf = BytesMut::with_capacity(256);
    let eof = async {
        loop {
            let n = stream.read_buf(&mut read_buf).await.expect("read");
            if n == 0 {
                break;
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), eof)
        .await
        .expect("daemon did not close the connection on corrupt framing");
}

/// Connect to a daemon, complete the handshake, and return the stream.
/// The returned `TestDaemon` must be held alive for the socket to remain valid.
async fn connect_and_handshake() -> (UnixStream, FrameCodec, TestDaemon) {
    let td = TestDaemon::start().await;
    let (stream, codec) = connect_to(&td).await;
    (stream, codec, td)
}

/// Open an additional handshaken connection to a running test daemon.
async fn connect_to(td: &TestDaemon) -> (UnixStream, FrameCodec) {
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

    (stream, codec)
}

/// Holds a daemon task and its tempdir alive for the duration of a test.
struct TestDaemon {
    socket: std::path::PathBuf,
    dir: tempfile::TempDir,
    handle: tokio::task::JoinHandle<()>,
}

impl TestDaemon {
    async fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("sock");
        let mut daemon = oakterm_daemon::server::Daemon::with_socket_path(80, 24, socket.clone());
        // Session saves land in the test's tempdir, never the real one.
        daemon.set_state_dir(dir.path().join("state"));

        let handle = tokio::spawn(async move {
            let _ = daemon.run().await;
        });

        wait_for_socket(&socket).await;

        Self {
            socket,
            dir,
            handle,
        }
    }

    fn state_dir(&self) -> std::path::PathBuf {
        self.dir.path().join("state")
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

// Multi-thread flavor: the firehose child keeps the PTY read-loop task
// permanently ready, which starves a current_thread runtime's timers.
// The production daemon runs multi-threaded, so that starvation is a
// test-environment artifact, not the interleaving under test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_pane_kills_streaming_child_promptly() {
    use rustix::process::{Pid, test_kill_process};

    let (mut stream, mut codec, _td) = connect_and_handshake_as(ClientType::Control).await;

    // A chatty child keeps the read loop mid-burst, exercising the
    // remove-while-processing interleaving: ClosePane's tombstone write
    // contends with the read loop's per-read pane lock.
    let create = CreatePane {
        command: "/bin/sh -c 'while :; do echo x; done'".to_string(),
        cwd: String::new(),
    };
    let frame = Frame::new(
        MSG_CREATE_PANE,
        150,
        create.encode().expect("encode CreatePane"),
    )
    .expect("create-pane frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 150).await;
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

    let pid = poll_for_pid(&mut stream, &mut codec, pane_id).await;
    let pid_i32 = i32::try_from(pid).expect("PID fits in i32");
    let live_pid = Pid::from_raw(pid_i32).expect("daemon-reported PID is positive");

    // Let output flow so the close lands mid-stream.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let close = ClosePane { pane_id };
    let frame = Frame::new(MSG_CLOSE_PANE, 250, close.encode()).expect("close-pane frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 250).await;
    assert_eq!(resp.msg_type, MSG_CLOSE_PANE_RESPONSE);

    // A hang here (hold-and-wait between ClosePane and the read loop)
    // presents as the child surviving the window.
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
        "streaming child {pid} should have been killed within 500ms of ClosePane"
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

/// Send `SplitPane` for `target` and return the new pane's ID, asserting
/// the split was accepted.
async fn split_pane_ok(
    stream: &mut UnixStream,
    codec: &mut FrameCodec,
    target: u32,
    direction: SplitDirection,
    serial: u32,
) -> u32 {
    let split = SplitPane {
        pane_id: target,
        direction,
        command: String::new(),
        cwd: String::new(),
    };
    let frame = Frame::new(
        MSG_SPLIT_PANE,
        serial,
        split.encode().expect("encode SplitPane"),
    )
    .expect("split frame");
    write_frame(stream, codec, frame).await;
    let resp = read_response_with_serial(stream, codec, serial).await;
    assert_eq!(resp.msg_type, MSG_SPLIT_PANE_RESPONSE, "split rejected");
    SplitPaneResponse::decode(&resp.payload)
        .expect("decode SplitPaneResponse")
        .new_pane_id
}

#[tokio::test]
async fn split_pane_creates_pane_and_swap_round_trips() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let new_pane = split_pane_ok(&mut stream, &mut codec, 0, SplitDirection::Horizontal, 300).await;
    assert_ne!(new_pane, 0);

    // The new pane is listed alongside the default pane.
    let frame = Frame::new(MSG_LIST_PANES, 301, vec![]).expect("list-panes frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 301).await;
    let list = ListPanesResponse::decode(&resp.payload).expect("decode ListPanesResponse");
    let ids: Vec<u32> = list.panes.iter().map(|p| p.pane_id).collect();
    assert!(
        ids.contains(&0) && ids.contains(&new_pane),
        "panes: {ids:?}"
    );

    let swap = SwapPane {
        pane_id_a: 0,
        pane_id_b: new_pane,
    };
    let frame = Frame::new(MSG_SWAP_PANE, 302, swap.encode()).expect("swap frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 302).await;
    assert_eq!(resp.msg_type, MSG_SWAP_PANE_RESPONSE);
}

#[tokio::test]
async fn get_layout_tree_returns_split_topology() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let new_pane = split_pane_ok(&mut stream, &mut codec, 0, SplitDirection::Vertical, 310).await;

    let req = GetLayoutTree {
        workspace_id: 0,
        tab_id: 0,
    };
    let frame = Frame::new(MSG_GET_LAYOUT_TREE, 311, req.encode()).expect("get-layout frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 311).await;
    assert_eq!(resp.msg_type, MSG_LAYOUT_TREE);

    let tree = LayoutTree::decode(&resp.payload)
        .expect("decode LayoutTree")
        .tree;
    let LayoutTreeNode::Container {
        children, weights, ..
    } = tree
    else {
        panic!("expected container root after split, got {tree:?}");
    };
    assert_eq!(children.len(), 2);
    assert_eq!(weights.len(), 2);
    let leaf_ids: Vec<u32> = children
        .iter()
        .map(|c| match c {
            LayoutTreeNode::Leaf { pane_id } => *pane_id,
            LayoutTreeNode::Container { .. } => panic!("unexpected nested container"),
        })
        .collect();
    assert!(
        leaf_ids.contains(&0) && leaf_ids.contains(&new_pane),
        "leaves: {leaf_ids:?}"
    );
}

#[tokio::test]
async fn get_layout_tree_malformed_payload_errors() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let frame = Frame::new(MSG_GET_LAYOUT_TREE, 320, vec![0xFF; 3]).expect("short frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 320).await;
    assert_eq!(resp.msg_type, MSG_ERROR);
}

#[tokio::test]
async fn split_pane_unknown_target_errors() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let split = SplitPane {
        pane_id: 999,
        direction: SplitDirection::Horizontal,
        command: String::new(),
        cwd: String::new(),
    };
    let frame = Frame::new(
        MSG_SPLIT_PANE,
        310,
        split.encode().expect("encode SplitPane"),
    )
    .expect("split frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 310).await;
    assert_eq!(resp.msg_type, MSG_ERROR);
    let err = ErrorMessage::decode(&resp.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::UnknownPane as u32);
}

/// Spec-0007 Constraints: a split whose resulting pane would be under
/// 2 cols x 1 row is rejected — but only along the split axis.
#[tokio::test]
async fn split_pane_below_minimum_size_rejected() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    // Shrink pane 0 to 3 columns (this also spawns the default shell).
    // Frames on one connection are handled in order, so the split below
    // sees the new dimensions.
    let resize = Resize {
        pane_id: 0,
        cols: 3,
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

    // Horizontal split would leave 1.5 columns per pane: rejected.
    let split = SplitPane {
        pane_id: 0,
        direction: SplitDirection::Horizontal,
        command: String::new(),
        cwd: String::new(),
    };
    let frame = Frame::new(
        MSG_SPLIT_PANE,
        320,
        split.encode().expect("encode SplitPane"),
    )
    .expect("split frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 320).await;
    assert_eq!(resp.msg_type, MSG_ERROR);
    let err = ErrorMessage::decode(&resp.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::LayoutRejected as u32);

    // A vertical split of the same pane still fits (12 rows each).
    let new_pane = split_pane_ok(&mut stream, &mut codec, 0, SplitDirection::Vertical, 321).await;
    assert_ne!(new_pane, 0);
}

/// A same-direction insert shrinks every sibling by N/(N+1); a sibling
/// already at the minimum blocks the split even when the target is large.
#[tokio::test]
async fn split_pane_shrinking_sibling_below_minimum_rejected() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let b = split_pane_ok(&mut stream, &mut codec, 0, SplitDirection::Horizontal, 328).await;

    // Shrink the sibling to the 2-column floor (spawns its shell).
    let resize = Resize {
        pane_id: b,
        cols: 2,
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

    // Splitting the 80-col pane 0 would scale b by 2/3, below minimum.
    let split = SplitPane {
        pane_id: 0,
        direction: SplitDirection::Horizontal,
        command: String::new(),
        cwd: String::new(),
    };
    let frame = Frame::new(
        MSG_SPLIT_PANE,
        329,
        split.encode().expect("encode SplitPane"),
    )
    .expect("split frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 329).await;
    assert_eq!(resp.msg_type, MSG_ERROR);
    let err = ErrorMessage::decode(&resp.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::LayoutRejected as u32);
}

/// The minimum is inclusive: a 4-column pane splits into exactly 2+2.
#[tokio::test]
async fn split_pane_at_exact_minimum_accepted() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let resize = Resize {
        pane_id: 0,
        cols: 4,
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

    let new_pane = split_pane_ok(&mut stream, &mut codec, 0, SplitDirection::Horizontal, 325).await;
    assert_ne!(new_pane, 0);
}

#[tokio::test]
async fn resize_pane_unknown_neighbor_pushes_error() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;
    split_pane_ok(&mut stream, &mut codec, 0, SplitDirection::Horizontal, 330).await;

    let resize = ResizePane {
        pane_id: 0,
        neighbor_pane_id: 999,
        delta: 5,
    };
    let frame = Frame::new(MSG_RESIZE_PANE, 0, resize.encode()).expect("resize frame");
    write_frame(&mut stream, &mut codec, frame).await;

    // ResizePane is a push; its error arrives with serial 0.
    let resp = read_push_with_msg_type(&mut stream, &mut codec, MSG_ERROR).await;
    let err = ErrorMessage::decode(&resp.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::UnknownPane as u32);
}

#[tokio::test]
async fn resize_pane_corner_pair_rejected() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    // Build H[V[0,c], V[b,d]]: 0 top-left, d bottom-right — corner only.
    let b = split_pane_ok(&mut stream, &mut codec, 0, SplitDirection::Horizontal, 340).await;
    let _c = split_pane_ok(&mut stream, &mut codec, 0, SplitDirection::Vertical, 341).await;
    let d = split_pane_ok(&mut stream, &mut codec, b, SplitDirection::Vertical, 342).await;

    let resize = ResizePane {
        pane_id: 0,
        neighbor_pane_id: d,
        delta: 5,
    };
    let frame = Frame::new(MSG_RESIZE_PANE, 0, resize.encode()).expect("resize frame");
    write_frame(&mut stream, &mut codec, frame).await;

    let resp = read_push_with_msg_type(&mut stream, &mut codec, MSG_ERROR).await;
    let err = ErrorMessage::decode(&resp.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::LayoutRejected as u32);
}

/// A valid sibling resize is silent (push, no response): the next Pong
/// must arrive with no Error frame before it.
#[tokio::test]
async fn resize_pane_between_siblings_is_silent() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;
    let new_pane = split_pane_ok(&mut stream, &mut codec, 0, SplitDirection::Horizontal, 350).await;

    let resize = ResizePane {
        pane_id: 0,
        neighbor_pane_id: new_pane,
        delta: 5,
    };
    let frame = Frame::new(MSG_RESIZE_PANE, 0, resize.encode()).expect("resize frame");
    write_frame(&mut stream, &mut codec, frame).await;

    let ping = Frame::new(MSG_PING, 351, vec![]).expect("ping frame");
    write_frame(&mut stream, &mut codec, ping).await;

    let mut buf = BytesMut::with_capacity(4096);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        while let Some(frame) = codec.decode(&mut buf).expect("decode") {
            assert_ne!(
                frame.msg_type, MSG_ERROR,
                "sibling resize must not produce an error"
            );
            if frame.serial == 351 {
                assert_eq!(frame.msg_type, MSG_PONG);
                return;
            }
        }
        let timeout = deadline.saturating_duration_since(std::time::Instant::now());
        let n = tokio::time::timeout(timeout, stream.read_buf(&mut buf))
            .await
            .expect("timed out waiting for Pong")
            .expect("read error");
        assert!(n > 0, "daemon closed connection");
    }
}

#[tokio::test]
async fn swap_pane_unknown_pane_errors() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let swap = SwapPane {
        pane_id_a: 0,
        pane_id_b: 999,
    };
    let frame = Frame::new(MSG_SWAP_PANE, 360, swap.encode()).expect("swap frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 360).await;
    assert_eq!(resp.msg_type, MSG_ERROR);
    let err = ErrorMessage::decode(&resp.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::UnknownPane as u32);
}

async fn send_request_shutdown(
    stream: &mut UnixStream,
    codec: &mut FrameCodec,
    reason: RequestShutdownReason,
    serial: u32,
) -> ShutdownAck {
    let req = RequestShutdown { reason };
    let frame = Frame::new(MSG_REQUEST_SHUTDOWN, serial, req.encode()).expect("shutdown frame");
    write_frame(stream, codec, frame).await;
    let resp = read_response_with_serial(stream, codec, serial).await;
    assert_eq!(resp.msg_type, MSG_SHUTDOWN_ACK);
    ShutdownAck::decode(&resp.payload).expect("decode ShutdownAck")
}

/// Request a shutdown and assert the full accepted sequence on one buffer
/// (ack and Shutdown push can arrive in a single read): ack first, then
/// the Shutdown push with `expected`, then EOF.
async fn shutdown_and_expect_exit(
    stream: &mut UnixStream,
    codec: &mut FrameCodec,
    serial: u32,
    reason: RequestShutdownReason,
    expected: ShutdownReason,
) {
    let req = RequestShutdown { reason };
    let frame = Frame::new(MSG_REQUEST_SHUTDOWN, serial, req.encode()).expect("shutdown frame");
    write_frame(stream, codec, frame).await;

    let mut buf = BytesMut::with_capacity(4096);
    let mut got_ack = false;
    let mut got_shutdown = false;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        while let Some(frame) = codec.decode(&mut buf).expect("decode") {
            if frame.serial == serial {
                assert_eq!(frame.msg_type, MSG_SHUTDOWN_ACK);
                let ack = ShutdownAck::decode(&frame.payload).expect("decode ShutdownAck");
                assert_eq!(ack.status, ShutdownAckStatus::Accepted);
                assert!(!got_shutdown, "ack must precede the Shutdown push");
                got_ack = true;
            } else if frame.serial == 0 && frame.msg_type == MSG_SHUTDOWN {
                let msg = Shutdown::decode(&frame.payload).expect("decode Shutdown");
                assert_eq!(msg.reason, expected);
                assert!(got_ack, "Shutdown push must follow the ack");
                got_shutdown = true;
            }
        }
        let timeout = deadline.saturating_duration_since(std::time::Instant::now());
        let n = tokio::time::timeout(timeout, stream.read_buf(&mut buf))
            .await
            .expect("timed out waiting for shutdown sequence")
            .expect("read error during shutdown sequence");
        if n == 0 {
            assert!(got_ack, "connection closed before the ack");
            assert!(got_shutdown, "connection closed before the Shutdown push");
            return;
        }
    }
}

#[tokio::test]
async fn request_shutdown_quit_saves_session_and_exits() {
    let (mut stream, mut codec, mut td) = connect_and_handshake().await;

    shutdown_and_expect_exit(
        &mut stream,
        &mut codec,
        400,
        RequestShutdownReason::Quit,
        ShutdownReason::Clean,
    )
    .await;

    // EOF only proves handle_client returned; the daemon task itself must
    // finish (drain + archive teardown included) or `oakterm quit` leaves
    // a zombie holding the socket.
    tokio::time::timeout(std::time::Duration::from_secs(5), &mut td.handle)
        .await
        .expect("daemon task did not exit")
        .expect("daemon task panicked");

    let session = td.state_dir().join("session.json");
    let json = std::fs::read_to_string(&session).expect("session file written");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(parsed["version"], 1);
    assert_eq!(parsed["workspaces"].as_array().map(Vec::len), Some(1));
}

/// The 0x06 broadcast must reach clients other than the requester — an
/// idle client parked in the daemon's select loop.
#[tokio::test]
async fn request_shutdown_notifies_other_clients() {
    let (mut requester, mut req_codec, td) = connect_and_handshake().await;
    let (mut observer, mut obs_codec) = connect_to(&td).await;

    shutdown_and_expect_exit(
        &mut requester,
        &mut req_codec,
        440,
        RequestShutdownReason::Quit,
        ShutdownReason::Clean,
    )
    .await;

    // The observer never requested anything: it gets the push, then EOF.
    let push = read_push_with_msg_type(&mut observer, &mut obs_codec, MSG_SHUTDOWN).await;
    let msg = Shutdown::decode(&push.payload).expect("decode Shutdown");
    assert_eq!(msg.reason, ShutdownReason::Clean);

    let mut buf = BytesMut::with_capacity(256);
    let eof = async {
        loop {
            if observer.read_buf(&mut buf).await.expect("read") == 0 {
                return;
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), eof)
        .await
        .expect("daemon did not close the observer connection");
}

/// A minor-version-0 client stays accepted after the bump to 1 — minor
/// skew is additive per Spec-0001.
#[tokio::test]
async fn handshake_accepts_older_minor_version() {
    let td = TestDaemon::start().await;
    let mut stream = UnixStream::connect(&td.socket).await.expect("connect");
    let mut codec = FrameCodec;

    let hello = ClientHello {
        protocol_version_major: ClientHello::VERSION_MAJOR,
        protocol_version_minor: 0,
        client_type: ClientType::Gui,
        client_name: "old-minor-client".to_string(),
    };
    write_frame(&mut stream, &mut codec, hello.to_frame(1).expect("encode")).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 1).await;
    assert_eq!(resp.msg_type, MSG_SERVER_HELLO);
    let server_hello = ServerHello::decode(&resp.payload).expect("decode ServerHello");
    assert_eq!(server_hello.status, HandshakeStatus::Accepted);
    assert_eq!(server_hello.protocol_version_minor, 1);
}

#[tokio::test]
async fn handshake_rejects_newer_major_version() {
    let td = TestDaemon::start().await;
    let mut stream = UnixStream::connect(&td.socket).await.expect("connect");
    let mut codec = FrameCodec;

    let hello = ClientHello {
        protocol_version_major: ClientHello::VERSION_MAJOR + 1,
        protocol_version_minor: 0,
        client_type: ClientType::Gui,
        client_name: "future-client".to_string(),
    };
    write_frame(&mut stream, &mut codec, hello.to_frame(1).expect("encode")).await;

    let resp = read_response_with_serial(&mut stream, &mut codec, 1).await;
    assert_eq!(resp.msg_type, MSG_SERVER_HELLO);
    let server_hello = ServerHello::decode(&resp.payload).expect("decode ServerHello");
    assert_eq!(server_hello.status, HandshakeStatus::VersionMismatch);
    assert_eq!(
        server_hello.protocol_version_major,
        ClientHello::VERSION_MAJOR
    );

    // The daemon closes the connection after rejecting the version.
    let mut read_buf = BytesMut::with_capacity(64);
    let eof = async {
        loop {
            let n = stream.read_buf(&mut read_buf).await.expect("read");
            if n == 0 {
                break;
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), eof)
        .await
        .expect("daemon did not close the connection after a version mismatch");
}

#[tokio::test]
async fn oversized_handshake_frame_is_rejected() {
    let td = TestDaemon::start().await;
    let mut stream = UnixStream::connect(&td.socket).await.expect("connect");

    // A frame header advertising a payload far larger than a real ClientHello.
    // The daemon must reject by advertised length (before reserving the payload)
    // and close the connection, rather than buffer megabytes for an
    // unauthenticated client.
    let mut header = Vec::new();
    header.extend_from_slice(&[0x4F, 0x54]); // MAGIC "OT"
    header.push(0); // flags
    header.extend_from_slice(&1u16.to_le_bytes()); // msg_type
    header.extend_from_slice(&1u32.to_le_bytes()); // serial
    header.extend_from_slice(&5000u32.to_le_bytes()); // payload_length > handshake cap
    stream
        .write_all(&header)
        .await
        .expect("write oversized handshake header");

    let mut read_buf = BytesMut::with_capacity(64);
    let eof = async {
        loop {
            let n = stream.read_buf(&mut read_buf).await.expect("read");
            if n == 0 {
                break;
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), eof)
        .await
        .expect("daemon did not close the connection on an oversized handshake frame");
}

#[tokio::test]
async fn request_shutdown_upgrade_broadcasts_upgrade_reason() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    shutdown_and_expect_exit(
        &mut stream,
        &mut codec,
        410,
        RequestShutdownReason::Upgrade,
        ShutdownReason::Upgrade,
    )
    .await;
}

/// ADR-0020: a failed save aborts the shutdown — the daemon stays up.
#[tokio::test]
async fn request_shutdown_save_failure_aborts() {
    let (mut stream, mut codec, td) = connect_and_handshake().await;

    // A file where the state dir should be makes the save fail.
    std::fs::write(td.state_dir(), b"blocked").expect("block state dir");

    let ack =
        send_request_shutdown(&mut stream, &mut codec, RequestShutdownReason::Quit, 420).await;
    assert_eq!(ack.status, ShutdownAckStatus::SaveFailed);

    // Daemon still serves requests, and no Shutdown push precedes the Pong.
    let ping = Frame::new(MSG_PING, 421, vec![]).expect("ping frame");
    write_frame(&mut stream, &mut codec, ping).await;
    let mut buf = BytesMut::with_capacity(4096);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        while let Some(frame) = codec.decode(&mut buf).expect("decode") {
            assert_ne!(
                frame.msg_type, MSG_SHUTDOWN,
                "failed save must not shut the daemon down"
            );
            if frame.serial == 421 {
                assert_eq!(frame.msg_type, MSG_PONG);
                return;
            }
        }
        let timeout = deadline.saturating_duration_since(std::time::Instant::now());
        let n = tokio::time::timeout(timeout, stream.read_buf(&mut buf))
            .await
            .expect("timed out waiting for Pong")
            .expect("read error");
        assert!(n > 0, "daemon closed connection after failed save");
    }
}

/// Spec-0001 error case: unknown reason gets `MALFORMED_PAYLOAD` and the
/// daemon keeps running.
#[tokio::test]
async fn request_shutdown_unknown_reason_rejected() {
    let (mut stream, mut codec, td) = connect_and_handshake().await;

    let frame = Frame::new(MSG_REQUEST_SHUTDOWN, 430, vec![7]).expect("shutdown frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 430).await;
    assert_eq!(resp.msg_type, MSG_ERROR);
    let err = ErrorMessage::decode(&resp.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::MalformedPayload as u32);

    let ping = Frame::new(MSG_PING, 431, vec![]).expect("ping frame");
    write_frame(&mut stream, &mut codec, ping).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 431).await;
    assert_eq!(resp.msg_type, MSG_PONG);
    assert!(
        !td.state_dir().join("session.json").exists(),
        "rejected request must not save a session"
    );
}
