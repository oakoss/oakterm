//! Integration tests: daemon lifecycle, handshake, and error responses.
//!
//! Each test gets its own tempdir + socket path for parallel execution.

use bytes::BytesMut;
use oakterm_protocol::frame::{Frame, FrameCodec};
use oakterm_protocol::input::{KeyInput, Resize};
use oakterm_protocol::message::{
    ClientHello, ClientType, ClosePane, CloseTab, CloseWorkspace, CloseWorkspaceResponse, CopyMode,
    CopySelectionType, CreatePane, CreatePaneResponse, ErrorCode, ErrorMessage, GetLayoutTree,
    HandshakeStatus, LayoutTree, LayoutTreeNode, ListPanesResponse, MSG_CLOSE_PANE,
    MSG_CLOSE_PANE_RESPONSE, MSG_CLOSE_TAB, MSG_CLOSE_TAB_RESPONSE, MSG_CLOSE_WORKSPACE,
    MSG_CLOSE_WORKSPACE_RESPONSE, MSG_CREATE_PANE, MSG_CREATE_PANE_RESPONSE, MSG_ENTER_COPY_MODE,
    MSG_ERROR, MSG_GET_LAYOUT_TREE, MSG_KEY_INPUT, MSG_LAYOUT_TREE, MSG_LIST_PANES,
    MSG_LIST_PANES_RESPONSE, MSG_LIST_TABS, MSG_MOVE_TAB, MSG_NEW_TAB, MSG_NEW_TAB_RESPONSE,
    MSG_NEW_WORKSPACE, MSG_NEW_WORKSPACE_RESPONSE, MSG_PANE_EXITED, MSG_PING, MSG_PONG,
    MSG_RENAME_TAB, MSG_RENAME_WORKSPACE, MSG_REQUEST_SHUTDOWN, MSG_RESIZE_PANE, MSG_SERVER_HELLO,
    MSG_SHUTDOWN, MSG_SHUTDOWN_ACK, MSG_SPLIT_PANE, MSG_SPLIT_PANE_RESPONSE, MSG_SWAP_PANE,
    MSG_SWAP_PANE_RESPONSE, MSG_SWITCH_TAB, MSG_SWITCH_WORKSPACE, MSG_TAB_LIST, MSG_YANK_RESPONSE,
    MSG_YANK_SELECTION, MoveTab, NewTab, NewTabResponse, NewWorkspace, NewWorkspaceResponse,
    PaneExited, RenameTab, RenameWorkspace, RequestShutdown, RequestShutdownReason, ResizePane,
    ServerHello, Shutdown, ShutdownAck, ShutdownAckStatus, ShutdownReason, SplitDirection,
    SplitPane, SplitPaneResponse, SwapPane, SwitchTab, SwitchWorkspace, TabList, YankResponse,
    YankSelection,
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
async fn key_input_reaches_child() {
    let (mut stream, mut codec, _td) = connect_and_handshake_as(ClientType::Control).await;

    // The child blocks on stdin and exits with a distinctive code once a
    // line arrives, so PaneExited proves the KeyInput bytes traversed the
    // PTY write path end-to-end.
    let create = CreatePane {
        command: "/bin/sh -c 'read line; exit 9'".to_string(),
        cwd: String::new(),
    };
    let frame = Frame::new(
        MSG_CREATE_PANE,
        170,
        create.encode().expect("encode CreatePane"),
    )
    .expect("create-pane frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 170).await;
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

    // Wait for the spawn to complete so the write hits a Running pane.
    poll_for_pid(&mut stream, &mut codec, pane_id).await;

    let key = KeyInput {
        pane_id,
        key_data: b"go\n".to_vec(),
    };
    let frame = Frame::new(MSG_KEY_INPUT, 171, key.encode().expect("encode KeyInput"))
        .expect("key-input frame");
    write_frame(&mut stream, &mut codec, frame).await;

    let frame = read_push_with_msg_type(&mut stream, &mut codec, MSG_PANE_EXITED).await;
    let exited = PaneExited::decode(&frame.payload).expect("decode PaneExited");
    assert_eq!(exited.pane_id, pane_id);
    assert_eq!(exited.exit_code, 9);
}

#[tokio::test]
async fn resize_applies_to_running_pane() {
    let (mut stream, mut codec, _td) = connect_and_handshake_as(ClientType::Control).await;

    let create = CreatePane {
        command: "/bin/sh -c 'sleep 30'".to_string(),
        cwd: String::new(),
    };
    let frame = Frame::new(
        MSG_CREATE_PANE,
        175,
        create.encode().expect("encode CreatePane"),
    )
    .expect("create-pane frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 175).await;
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
    poll_for_pid(&mut stream, &mut codec, pane_id).await;

    // Second Resize hits the Running arm (resize_fd on the write handle)
    // rather than the spawn path the first Resize took.
    let resize = Resize {
        pane_id,
        cols: 100,
        rows: 30,
        pixel_width: 0,
        pixel_height: 0,
    };
    write_frame(
        &mut stream,
        &mut codec,
        resize.to_frame().expect("encode Resize"),
    )
    .await;

    let mut serial = 2000;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        serial += 1;
        let frame = Frame::new(MSG_LIST_PANES, serial, vec![]).expect("list-panes frame");
        write_frame(&mut stream, &mut codec, frame).await;
        let resp = read_response_with_serial(&mut stream, &mut codec, serial).await;
        let list = ListPanesResponse::decode(&resp.payload).expect("decode ListPanesResponse");
        let info = list
            .panes
            .iter()
            .find(|p| p.pane_id == pane_id)
            .expect("pane listed");
        if info.cols == 100 && info.rows == 30 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "pane never reached 100x30 (last saw {}x{})",
            info.cols,
            info.rows
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

// Multi-thread flavor so a regression to a blocking write stalls one worker
// and the ListPanes probe times out with a panic, instead of freezing the
// whole single-threaded runtime (timeout timer included) into a silent hang.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_key_input_does_not_wedge_daemon() {
    let (mut stream, mut codec, _td) = connect_and_handshake_as(ClientType::Control).await;

    // The daemon must stay responsive after large input bursts to a child
    // that never reads stdin. On Linux a full PTY queue backpressures the
    // writer (EAGAIN, exercising the drop path); macOS discards excess
    // canonical-mode input instead, so here the test only proves the write
    // path doesn't stall the pane lock.
    let create = CreatePane {
        command: "/bin/sh -c 'sleep 30'".to_string(),
        cwd: String::new(),
    };
    let frame = Frame::new(
        MSG_CREATE_PANE,
        185,
        create.encode().expect("encode CreatePane"),
    )
    .expect("create-pane frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 185).await;
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
    poll_for_pid(&mut stream, &mut codec, pane_id).await;

    for i in 0..4u32 {
        let key = KeyInput {
            pane_id,
            key_data: vec![b'x'; 60_000],
        };
        let frame = Frame::new(
            MSG_KEY_INPUT,
            186 + i,
            key.encode().expect("encode KeyInput"),
        )
        .expect("key-input frame");
        write_frame(&mut stream, &mut codec, frame).await;
    }

    let frame = Frame::new(MSG_LIST_PANES, 195, vec![]).expect("list-panes frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 195).await;
    assert_eq!(resp.msg_type, MSG_LIST_PANES_RESPONSE);
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
        if let Some(info) = list.panes.iter().find(|p| p.pane_id == target_pane)
            && info.pid != 0
        {
            return info.pid;
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

/// Send `NewTab` and return `(tab_id, pane_id)`, asserting acceptance.
async fn new_tab_ok(stream: &mut UnixStream, codec: &mut FrameCodec, serial: u32) -> (u32, u32) {
    let msg = NewTab {
        workspace_id: 0,
        command: String::new(),
        cwd: String::new(),
    };
    let frame = Frame::new(MSG_NEW_TAB, serial, msg.encode().expect("encode NewTab"))
        .expect("new-tab frame");
    write_frame(stream, codec, frame).await;
    let resp = read_response_with_serial(stream, codec, serial).await;
    assert_eq!(resp.msg_type, MSG_NEW_TAB_RESPONSE, "new tab rejected");
    let resp = NewTabResponse::decode(&resp.payload).expect("decode NewTabResponse");
    (resp.tab_id, resp.pane_id)
}

/// Fetch a tab's layout tree via `GetLayoutTree`. `tab_id` is literal;
/// the seeded default tab is 0.
async fn layout_tree(
    stream: &mut UnixStream,
    codec: &mut FrameCodec,
    serial: u32,
    tab_id: u32,
) -> LayoutTreeNode {
    let req = GetLayoutTree {
        workspace_id: 0,
        tab_id,
    };
    let frame = Frame::new(MSG_GET_LAYOUT_TREE, serial, req.encode()).expect("get-layout frame");
    write_frame(stream, codec, frame).await;
    let resp = read_response_with_serial(stream, codec, serial).await;
    assert_eq!(resp.msg_type, MSG_LAYOUT_TREE);
    LayoutTree::decode(&resp.payload)
        .expect("decode LayoutTree")
        .tree
}

/// Fetch the active workspace's tab list via `ListTabs`.
async fn list_tabs_ok(stream: &mut UnixStream, codec: &mut FrameCodec, serial: u32) -> TabList {
    let frame = Frame::new(MSG_LIST_TABS, serial, vec![]).expect("list-tabs frame");
    write_frame(stream, codec, frame).await;
    let resp = read_response_with_serial(stream, codec, serial).await;
    assert_eq!(resp.msg_type, MSG_TAB_LIST);
    TabList::decode(&resp.payload).expect("decode TabList")
}

#[tokio::test]
async fn new_tab_creates_tab_with_one_pane() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let (tab_id, pane_id) = new_tab_ok(&mut stream, &mut codec, 400).await;
    assert_ne!(tab_id, 0, "seeded default tab is 0");
    assert_ne!(pane_id, 0, "seeded default pane is 0");

    // The new tab is active and holds exactly the new pane.
    let tabs = list_tabs_ok(&mut stream, &mut codec, 401).await;
    assert_eq!(tabs.active_tab, tab_id);
    let tree = layout_tree(&mut stream, &mut codec, 403, tab_id).await;
    assert_eq!(tree, LayoutTreeNode::Leaf { pane_id });

    // The default pane survives in its background tab.
    let frame = Frame::new(MSG_LIST_PANES, 402, vec![]).expect("list-panes frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 402).await;
    let list = ListPanesResponse::decode(&resp.payload).expect("decode ListPanesResponse");
    let ids: Vec<u32> = list.panes.iter().map(|p| p.pane_id).collect();
    assert!(ids.contains(&0) && ids.contains(&pane_id), "panes: {ids:?}");
}

#[tokio::test]
async fn switch_tab_changes_active_tab() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let (tab_id, _pane_id) = new_tab_ok(&mut stream, &mut codec, 410).await;
    assert_eq!(
        list_tabs_ok(&mut stream, &mut codec, 411).await.active_tab,
        tab_id
    );

    // Frames on one connection are handled in order, so the query after
    // the push observes the switch.
    let switch = SwitchTab { tab_id: 0 };
    let frame = Frame::new(MSG_SWITCH_TAB, 0, switch.encode()).expect("switch frame");
    write_frame(&mut stream, &mut codec, frame).await;
    assert_eq!(
        list_tabs_ok(&mut stream, &mut codec, 412).await.active_tab,
        0
    );

    // And back to the new tab.
    let switch = SwitchTab { tab_id };
    let frame = Frame::new(MSG_SWITCH_TAB, 0, switch.encode()).expect("switch frame");
    write_frame(&mut stream, &mut codec, frame).await;
    assert_eq!(
        list_tabs_ok(&mut stream, &mut codec, 413).await.active_tab,
        tab_id
    );
}

/// Spec-0008 copy mode over the wire: `EnterCopyMode` and `ExitCopyMode`
/// are silent pushes, so a Ping after them must still get its Pong with no
/// Error in between.
#[tokio::test]
async fn copy_mode_enter_and_exit_are_silent_pushes() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let msg = CopyMode { pane_id: 0 };
    write_frame(
        &mut stream,
        &mut codec,
        msg.to_enter_frame().expect("enter"),
    )
    .await;
    write_frame(&mut stream, &mut codec, msg.to_exit_frame().expect("exit")).await;
    let ping = Frame::new(MSG_PING, 610, vec![]).expect("ping frame");
    write_frame(&mut stream, &mut codec, ping).await;

    // Drain every frame up to the Pong: an Error push would arrive with
    // serial 0, which the serial-matching reader would otherwise discard.
    let mut buf = BytesMut::with_capacity(4096);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        while let Some(frame) = codec.decode(&mut buf).expect("decode") {
            assert_ne!(
                frame.msg_type, MSG_ERROR,
                "copy mode pushes must not produce an Error"
            );
            if frame.msg_type == MSG_PONG {
                return;
            }
        }
        assert!(std::time::Instant::now() < deadline, "no Pong within 5s");
        let n = stream.read_buf(&mut buf).await.expect("read");
        assert!(n > 0, "daemon closed the connection");
    }
}

#[tokio::test]
async fn yank_selection_returns_pane_text() {
    let (mut stream, mut codec, _td) = connect_and_handshake_as(ClientType::Control).await;

    // The child prints one line then blocks, so the text stays on the grid
    // for the yank to resolve.
    // Multi-byte and wide glyphs, so the response exercises UTF-8 framing
    // rather than a pure-ASCII path.
    let create = CreatePane {
        command: "/bin/sh -c 'printf \"copymode-café-漢\\n\"; read line'".to_string(),
        cwd: String::new(),
    };
    let frame = Frame::new(MSG_CREATE_PANE, 620, create.encode().expect("encode")).expect("frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 620).await;
    let pane_id = CreatePaneResponse::decode(&resp.payload)
        .expect("decode CreatePaneResponse")
        .pane_id;

    let resize = Resize {
        pane_id,
        cols: 80,
        rows: 24,
        pixel_width: 0,
        pixel_height: 0,
    };
    write_frame(&mut stream, &mut codec, resize.to_frame().expect("resize")).await;
    poll_for_pid(&mut stream, &mut codec, pane_id).await;

    write_frame(
        &mut stream,
        &mut codec,
        CopyMode { pane_id }.to_enter_frame().expect("enter"),
    )
    .await;

    // Copy-mode row 0 is the top of the pinned viewport, where the child's
    // first line lands. Poll: the VT parse races the yank.
    let yank = YankSelection {
        pane_id,
        start_row: 0,
        start_col: 0,
        end_row: 0,
        end_col: 0,
        selection_type: CopySelectionType::Line,
    };
    let mut serial = 630;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        serial += 1;
        write_frame(
            &mut stream,
            &mut codec,
            yank.to_frame(serial).expect("yank frame"),
        )
        .await;
        let resp = read_response_with_serial(&mut stream, &mut codec, serial).await;
        assert_eq!(resp.msg_type, MSG_YANK_RESPONSE);
        let text = YankResponse::decode(&resp.payload)
            .expect("decode YankResponse")
            .text;
        if text == "copymode-café-漢" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "yank never returned the child's output, last saw {text:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn copy_mode_on_an_unknown_pane_reports_errors() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    // EnterCopyMode is a push, so the failure arrives as an Error push.
    write_frame(
        &mut stream,
        &mut codec,
        CopyMode { pane_id: 99 }.to_enter_frame().expect("enter"),
    )
    .await;
    let resp = read_push_with_msg_type(&mut stream, &mut codec, MSG_ERROR).await;
    let err = ErrorMessage::decode(&resp.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::UnknownPane as u32);

    let yank = YankSelection {
        pane_id: 99,
        start_row: -1,
        start_col: 0,
        end_row: -1,
        end_col: 0,
        selection_type: CopySelectionType::Line,
    };
    write_frame(&mut stream, &mut codec, yank.to_frame(640).expect("yank")).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 640).await;
    assert_eq!(resp.msg_type, MSG_ERROR);
    let err = ErrorMessage::decode(&resp.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::UnknownPane as u32);
}

#[tokio::test]
async fn malformed_copy_mode_payloads_error() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let frame = Frame::new(MSG_YANK_SELECTION, 650, vec![0x00]).expect("short yank");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 650).await;
    assert_eq!(resp.msg_type, MSG_ERROR);
    let err = ErrorMessage::decode(&resp.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::MalformedPayload as u32);

    let frame = Frame::new(MSG_ENTER_COPY_MODE, 0, vec![0x00]).expect("short enter");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_push_with_msg_type(&mut stream, &mut codec, MSG_ERROR).await;
    let err = ErrorMessage::decode(&resp.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::MalformedPayload as u32);
}

#[tokio::test]
async fn switch_tab_unknown_tab_pushes_error() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let switch = SwitchTab { tab_id: 99 };
    let frame = Frame::new(MSG_SWITCH_TAB, 0, switch.encode()).expect("switch frame");
    write_frame(&mut stream, &mut codec, frame).await;

    // SwitchTab is a push, so the failure arrives as an Error push.
    let resp = read_push_with_msg_type(&mut stream, &mut codec, MSG_ERROR).await;
    let err = ErrorMessage::decode(&resp.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::UnknownTab as u32);
}

#[tokio::test]
async fn close_tab_closes_all_panes_in_the_tab() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let (tab_id, pane_b) = new_tab_ok(&mut stream, &mut codec, 420).await;
    // A second pane in the new tab: CloseTab must remove both.
    let pane_c = split_pane_ok(
        &mut stream,
        &mut codec,
        pane_b,
        SplitDirection::Horizontal,
        421,
    )
    .await;

    let close = CloseTab { tab_id };
    let frame = Frame::new(MSG_CLOSE_TAB, 422, close.encode()).expect("close-tab frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 422).await;
    assert_eq!(resp.msg_type, MSG_CLOSE_TAB_RESPONSE);

    // Only the default pane remains, and the default tab is active again.
    let frame = Frame::new(MSG_LIST_PANES, 423, vec![]).expect("list-panes frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 423).await;
    let list = ListPanesResponse::decode(&resp.payload).expect("decode ListPanesResponse");
    let ids: Vec<u32> = list.panes.iter().map(|p| p.pane_id).collect();
    assert_eq!(ids, vec![0], "expected only the default pane, got {ids:?}");
    assert!(!ids.contains(&pane_b) && !ids.contains(&pane_c));
    assert_eq!(
        list_tabs_ok(&mut stream, &mut codec, 424).await.active_tab,
        0
    );
    assert_eq!(
        layout_tree(&mut stream, &mut codec, 425, 0).await,
        LayoutTreeNode::Leaf { pane_id: 0 }
    );
}

#[tokio::test]
async fn close_tab_unknown_tab_errors() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let close = CloseTab { tab_id: 99 };
    let frame = Frame::new(MSG_CLOSE_TAB, 430, close.encode()).expect("close-tab frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 430).await;
    assert_eq!(resp.msg_type, MSG_ERROR);
    let err = ErrorMessage::decode(&resp.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::UnknownTab as u32);
}

/// Closing the only tab would empty the daemon, mirroring the
/// `ClosePane` last-pane rule: refused, nothing removed.
#[tokio::test]
async fn close_tab_last_tab_refused() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let close = CloseTab { tab_id: 0 };
    let frame = Frame::new(MSG_CLOSE_TAB, 440, close.encode()).expect("close-tab frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 440).await;
    assert_eq!(resp.msg_type, MSG_ERROR);
    let err = ErrorMessage::decode(&resp.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::LayoutRejected as u32);

    // The default pane is untouched.
    let frame = Frame::new(MSG_LIST_PANES, 441, vec![]).expect("list-panes frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 441).await;
    let list = ListPanesResponse::decode(&resp.payload).expect("decode ListPanesResponse");
    assert_eq!(list.panes.len(), 1);
}

/// The last-tab guard is a pane-count totality check, not `tab_count == 1`:
/// a single tab holding multiple panes still refuses, and both survive.
#[tokio::test]
async fn close_tab_last_tab_multi_pane_refused() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    // Two panes, both in the only (default) tab.
    let pane_b = split_pane_ok(&mut stream, &mut codec, 0, SplitDirection::Horizontal, 450).await;

    let close = CloseTab { tab_id: 0 };
    let frame = Frame::new(MSG_CLOSE_TAB, 451, close.encode()).expect("close-tab frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 451).await;
    assert_eq!(resp.msg_type, MSG_ERROR);
    let err = ErrorMessage::decode(&resp.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::LayoutRejected as u32);

    let frame = Frame::new(MSG_LIST_PANES, 452, vec![]).expect("list-panes frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 452).await;
    let list = ListPanesResponse::decode(&resp.payload).expect("decode ListPanesResponse");
    let ids: Vec<u32> = list.panes.iter().map(|p| p.pane_id).collect();
    assert_eq!(ids.len(), 2, "both panes must survive: {ids:?}");
    assert!(ids.contains(&0) && ids.contains(&pane_b));
}

/// `NewTab` ignores `workspace_id` (routing to a non-active workspace
/// waits on a mux op that can target one); an unknown id is accepted, not
/// rejected. Locks the contract so the change is visible when routing lands.
#[tokio::test]
async fn new_tab_ignores_unknown_workspace_id() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let msg = NewTab {
        workspace_id: 999,
        command: String::new(),
        cwd: String::new(),
    };
    let frame =
        Frame::new(MSG_NEW_TAB, 460, msg.encode().expect("encode NewTab")).expect("new-tab frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 460).await;
    assert_eq!(
        resp.msg_type, MSG_NEW_TAB_RESPONSE,
        "unknown workspace_id must be accepted"
    );
}

/// Truncated tab payloads produce `MalformedPayload`: the request-shaped
/// `NewTab`/`CloseTab` reply on their serial; `SwitchTab` (a push) reports via
/// a serial-0 Error push.
#[tokio::test]
async fn malformed_tab_payloads_error() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let frame = Frame::new(MSG_NEW_TAB, 470, vec![0x00]).expect("new-tab frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 470).await;
    assert_eq!(resp.msg_type, MSG_ERROR);
    assert_eq!(
        ErrorMessage::decode(&resp.payload)
            .expect("decode ErrorMessage")
            .code,
        ErrorCode::MalformedPayload as u32
    );

    let frame = Frame::new(MSG_CLOSE_TAB, 471, vec![0x00]).expect("close-tab frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 471).await;
    assert_eq!(resp.msg_type, MSG_ERROR);
    assert_eq!(
        ErrorMessage::decode(&resp.payload)
            .expect("decode ErrorMessage")
            .code,
        ErrorCode::MalformedPayload as u32
    );

    let frame = Frame::new(MSG_SWITCH_TAB, 0, vec![0x00]).expect("switch frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_push_with_msg_type(&mut stream, &mut codec, MSG_ERROR).await;
    assert_eq!(
        ErrorMessage::decode(&resp.payload)
            .expect("decode ErrorMessage")
            .code,
        ErrorCode::MalformedPayload as u32
    );
}

/// `GetLayoutTree` resolves `tab_id` literally: a background tab's tree
/// is served as-is, not the active tab's.
#[tokio::test]
async fn get_layout_tree_honors_tab_id() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    // The new tab becomes active; the default tab (0) is background.
    let (tab_id, pane_id) = new_tab_ok(&mut stream, &mut codec, 510).await;

    assert_eq!(
        layout_tree(&mut stream, &mut codec, 511, 0).await,
        LayoutTreeNode::Leaf { pane_id: 0 },
        "background tab's own tree"
    );
    assert_eq!(
        layout_tree(&mut stream, &mut codec, 512, tab_id).await,
        LayoutTreeNode::Leaf { pane_id },
        "active tab's own tree"
    );
}

#[tokio::test]
async fn get_layout_tree_unknown_tab_errors() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let req = GetLayoutTree {
        workspace_id: 0,
        tab_id: 99,
    };
    let frame = Frame::new(MSG_GET_LAYOUT_TREE, 520, req.encode()).expect("get-layout frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 520).await;
    assert_eq!(resp.msg_type, MSG_ERROR);
    let err = ErrorMessage::decode(&resp.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::UnknownTab as u32);
}

#[tokio::test]
async fn list_tabs_returns_workspace_tabs_in_order() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let tabs = list_tabs_ok(&mut stream, &mut codec, 530).await;
    assert_eq!(tabs.workspace_name, "default");
    assert_eq!(tabs.active_tab, 0);
    assert_eq!(tabs.tabs.len(), 1);
    assert_eq!(tabs.tabs[0].tab_id, 0);
    assert_eq!(tabs.tabs[0].focused_pane, 0);
    // No explicit tab name and no OSC title yet: the fallback is empty.
    assert_eq!(tabs.tabs[0].name, "");

    let (tab_id, pane_id) = new_tab_ok(&mut stream, &mut codec, 531).await;
    let tabs = list_tabs_ok(&mut stream, &mut codec, 532).await;
    assert_eq!(tabs.active_tab, tab_id);
    let ids: Vec<u32> = tabs.tabs.iter().map(|t| t.tab_id).collect();
    assert_eq!(ids, vec![0, tab_id], "workspace tab order");
    assert_eq!(tabs.tabs[1].focused_pane, pane_id);
}

/// Send `NewWorkspace` and return `(workspace_id, tab_id, pane_id)`,
/// asserting acceptance.
async fn new_workspace_ok(
    stream: &mut UnixStream,
    codec: &mut FrameCodec,
    serial: u32,
) -> (u32, u32, u32) {
    let msg = NewWorkspace { name: "dev".into() };
    let frame = Frame::new(
        MSG_NEW_WORKSPACE,
        serial,
        msg.encode().expect("encode NewWorkspace"),
    )
    .expect("new-workspace frame");
    write_frame(stream, codec, frame).await;
    let resp = read_response_with_serial(stream, codec, serial).await;
    assert_eq!(
        resp.msg_type, MSG_NEW_WORKSPACE_RESPONSE,
        "new workspace rejected"
    );
    let resp = NewWorkspaceResponse::decode(&resp.payload).expect("decode NewWorkspaceResponse");
    (resp.workspace_id, resp.tab_id, resp.pane_id)
}

#[tokio::test]
async fn new_workspace_creates_workspace_with_one_tab_and_pane() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let (workspace_id, tab_id, pane_id) = new_workspace_ok(&mut stream, &mut codec, 480).await;
    assert_ne!(workspace_id, 0, "seeded default workspace is 0");
    assert_ne!(tab_id, 0, "seeded default tab is 0");
    assert_ne!(pane_id, 0, "seeded default pane is 0");

    // The new workspace is active and its tab holds exactly the new pane.
    let tabs = list_tabs_ok(&mut stream, &mut codec, 481).await;
    assert_eq!(tabs.workspace_id, workspace_id);
    assert_eq!(tabs.active_tab, tab_id);
    let tree = layout_tree(&mut stream, &mut codec, 483, tab_id).await;
    assert_eq!(tree, LayoutTreeNode::Leaf { pane_id });

    // The default pane survives in its background workspace.
    let frame = Frame::new(MSG_LIST_PANES, 482, vec![]).expect("list-panes frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 482).await;
    let list = ListPanesResponse::decode(&resp.payload).expect("decode ListPanesResponse");
    let ids: Vec<u32> = list.panes.iter().map(|p| p.pane_id).collect();
    assert!(ids.contains(&0) && ids.contains(&pane_id), "panes: {ids:?}");
}

#[tokio::test]
async fn switch_workspace_changes_active_workspace() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let (workspace_id, tab_id, _pane_id) = new_workspace_ok(&mut stream, &mut codec, 490).await;
    let tabs = list_tabs_ok(&mut stream, &mut codec, 491).await;
    assert_eq!(tabs.workspace_id, workspace_id);
    assert_eq!(tabs.active_tab, tab_id);

    // Frames on one connection are handled in order, so the query after
    // the push observes the switch.
    let switch = SwitchWorkspace { workspace_id: 0 };
    let frame = Frame::new(MSG_SWITCH_WORKSPACE, 0, switch.encode()).expect("switch frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let tabs = list_tabs_ok(&mut stream, &mut codec, 492).await;
    assert_eq!(tabs.workspace_id, 0);
    assert_eq!(tabs.active_tab, 0);

    // And back to the new workspace.
    let switch = SwitchWorkspace { workspace_id };
    let frame = Frame::new(MSG_SWITCH_WORKSPACE, 0, switch.encode()).expect("switch frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let tabs = list_tabs_ok(&mut stream, &mut codec, 493).await;
    assert_eq!(tabs.workspace_id, workspace_id);
    assert_eq!(tabs.active_tab, tab_id);
}

#[tokio::test]
async fn switch_workspace_unknown_workspace_pushes_error() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let switch = SwitchWorkspace { workspace_id: 99 };
    let frame = Frame::new(MSG_SWITCH_WORKSPACE, 0, switch.encode()).expect("switch frame");
    write_frame(&mut stream, &mut codec, frame).await;

    // SwitchWorkspace is a push, so the failure arrives as an Error push.
    let resp = read_push_with_msg_type(&mut stream, &mut codec, MSG_ERROR).await;
    let err = ErrorMessage::decode(&resp.payload).expect("decode ErrorMessage");
    assert_eq!(err.code, ErrorCode::UnknownWorkspace as u32);
}

/// Truncated workspace payloads produce `MalformedPayload`: the
/// request-shaped `NewWorkspace` replies on its serial; `SwitchWorkspace`
/// (a push) reports via a serial-0 Error push.
#[tokio::test]
async fn malformed_workspace_payloads_error() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    let frame = Frame::new(MSG_NEW_WORKSPACE, 500, vec![0x00]).expect("new-workspace frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 500).await;
    assert_eq!(resp.msg_type, MSG_ERROR);
    assert_eq!(
        ErrorMessage::decode(&resp.payload)
            .expect("decode ErrorMessage")
            .code,
        ErrorCode::MalformedPayload as u32
    );

    let frame = Frame::new(MSG_SWITCH_WORKSPACE, 0, vec![0x00]).expect("switch frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_push_with_msg_type(&mut stream, &mut codec, MSG_ERROR).await;
    assert_eq!(
        ErrorMessage::decode(&resp.payload)
            .expect("decode ErrorMessage")
            .code,
        ErrorCode::MalformedPayload as u32
    );
}

/// Send a push frame (serial 0) that expects no reply.
async fn push_frame(
    stream: &mut UnixStream,
    codec: &mut FrameCodec,
    msg_type: u16,
    payload: Vec<u8>,
) {
    let frame = Frame::new(msg_type, 0, payload).expect("push frame");
    write_frame(stream, codec, frame).await;
}

/// Poll `ListTabs` until `tab_id` reports a non-empty name, returning it.
/// Times out after 5s (the PTY must spawn and emit its OSC 2 title first).
async fn poll_tab_name(stream: &mut UnixStream, codec: &mut FrameCodec, tab_id: u32) -> String {
    let mut serial = 2000;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        serial += 1;
        let tabs = list_tabs_ok(stream, codec, serial).await;
        if let Some(t) = tabs.tabs.iter().find(|t| t.tab_id == tab_id)
            && !t.name.is_empty()
        {
            return t.name.clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("tab {tab_id} name stayed empty within the deadline");
}

/// Covers `RenameTab` precedence: a tab's name falls back to its pane's
/// live OSC 2 title, an explicit rename pins over it, and an empty rename
/// reverts to the title.
#[tokio::test]
async fn rename_tab_pins_name_over_the_osc_title() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    // A tab whose pane emits an OSC 2 title (BEL-terminated), then idles.
    let msg = NewTab {
        workspace_id: 0,
        command: "/bin/sh -c \"printf '\\033]2;osc-title\\007'; sleep 30\"".to_string(),
        cwd: String::new(),
    };
    let frame =
        Frame::new(MSG_NEW_TAB, 600, msg.encode().expect("encode NewTab")).expect("new-tab frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 600).await;
    assert_eq!(resp.msg_type, MSG_NEW_TAB_RESPONSE);
    let (tab_id, pane_id) = {
        let r = NewTabResponse::decode(&resp.payload).expect("decode NewTabResponse");
        (r.tab_id, r.pane_id)
    };

    // The first Resize spawns the PTY, which runs the command.
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

    // The fallback surfaces the live OSC 2 title.
    assert_eq!(
        poll_tab_name(&mut stream, &mut codec, tab_id).await,
        "osc-title"
    );

    // An explicit rename pins over the title (push, then observe).
    let rename = RenameTab {
        tab_id,
        name: "pinned".into(),
    };
    push_frame(
        &mut stream,
        &mut codec,
        MSG_RENAME_TAB,
        rename.encode().expect("encode"),
    )
    .await;
    let tabs = list_tabs_ok(&mut stream, &mut codec, 601).await;
    let entry = tabs.tabs.iter().find(|t| t.tab_id == tab_id).expect("tab");
    assert_eq!(entry.name, "pinned");

    // Renaming to empty clears the pin, reverting to the OSC title.
    let clear = RenameTab {
        tab_id,
        name: String::new(),
    };
    push_frame(
        &mut stream,
        &mut codec,
        MSG_RENAME_TAB,
        clear.encode().expect("encode"),
    )
    .await;
    let tabs = list_tabs_ok(&mut stream, &mut codec, 602).await;
    let entry = tabs.tabs.iter().find(|t| t.tab_id == tab_id).expect("tab");
    assert_eq!(entry.name, "osc-title");
}

#[tokio::test]
async fn rename_tab_unknown_tab_pushes_error() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;
    let rename = RenameTab {
        tab_id: 99,
        name: "x".into(),
    };
    push_frame(
        &mut stream,
        &mut codec,
        MSG_RENAME_TAB,
        rename.encode().expect("encode"),
    )
    .await;
    let resp = read_push_with_msg_type(&mut stream, &mut codec, MSG_ERROR).await;
    assert_eq!(
        ErrorMessage::decode(&resp.payload).expect("decode").code,
        ErrorCode::UnknownTab as u32
    );
}

#[tokio::test]
async fn move_tab_reorders_within_the_workspace() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    // Order becomes [0, tab_a, tab_b]; tab_b is active.
    let (tab_a, _) = new_tab_ok(&mut stream, &mut codec, 610).await;
    let (tab_b, _) = new_tab_ok(&mut stream, &mut codec, 611).await;
    let ids: Vec<u32> = list_tabs_ok(&mut stream, &mut codec, 612)
        .await
        .tabs
        .iter()
        .map(|t| t.tab_id)
        .collect();
    assert_eq!(ids, vec![0, tab_a, tab_b]);

    // Move the active tab_b to the front.
    let mv = MoveTab {
        tab_id: tab_b,
        new_index: 0,
    };
    push_frame(&mut stream, &mut codec, MSG_MOVE_TAB, mv.encode()).await;
    let tabs = list_tabs_ok(&mut stream, &mut codec, 613).await;
    let ids: Vec<u32> = tabs.tabs.iter().map(|t| t.tab_id).collect();
    assert_eq!(ids, vec![tab_b, 0, tab_a], "tab_b moved to the front");
    // The active tab still follows tab_b through the shuffle.
    assert_eq!(tabs.active_tab, tab_b);
}

#[tokio::test]
async fn move_tab_unknown_tab_pushes_error() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;
    let mv = MoveTab {
        tab_id: 99,
        new_index: 0,
    };
    push_frame(&mut stream, &mut codec, MSG_MOVE_TAB, mv.encode()).await;
    let resp = read_push_with_msg_type(&mut stream, &mut codec, MSG_ERROR).await;
    assert_eq!(
        ErrorMessage::decode(&resp.payload).expect("decode").code,
        ErrorCode::UnknownTab as u32
    );
}

#[tokio::test]
async fn rename_workspace_sets_the_workspace_name() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;
    assert_eq!(
        list_tabs_ok(&mut stream, &mut codec, 620)
            .await
            .workspace_name,
        "default"
    );
    let rename = RenameWorkspace {
        workspace_id: 0,
        name: "ops".into(),
    };
    push_frame(
        &mut stream,
        &mut codec,
        MSG_RENAME_WORKSPACE,
        rename.encode().expect("encode"),
    )
    .await;
    assert_eq!(
        list_tabs_ok(&mut stream, &mut codec, 621)
            .await
            .workspace_name,
        "ops"
    );
}

#[tokio::test]
async fn rename_workspace_unknown_pushes_error() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;
    let rename = RenameWorkspace {
        workspace_id: 99,
        name: "x".into(),
    };
    push_frame(
        &mut stream,
        &mut codec,
        MSG_RENAME_WORKSPACE,
        rename.encode().expect("encode"),
    )
    .await;
    let resp = read_push_with_msg_type(&mut stream, &mut codec, MSG_ERROR).await;
    assert_eq!(
        ErrorMessage::decode(&resp.payload).expect("decode").code,
        ErrorCode::UnknownWorkspace as u32
    );
}

/// Send `CloseWorkspace` and assert the ack, returning nothing.
async fn close_workspace_ok(
    stream: &mut UnixStream,
    codec: &mut FrameCodec,
    workspace_id: u32,
    serial: u32,
) {
    let msg = CloseWorkspace { workspace_id };
    let frame = Frame::new(MSG_CLOSE_WORKSPACE, serial, msg.encode()).expect("close-ws frame");
    write_frame(stream, codec, frame).await;
    let resp = read_response_with_serial(stream, codec, serial).await;
    assert_eq!(
        resp.msg_type, MSG_CLOSE_WORKSPACE_RESPONSE,
        "close workspace rejected"
    );
    CloseWorkspaceResponse::decode(&resp.payload).expect("decode CloseWorkspaceResponse");
}

#[tokio::test]
async fn close_workspace_removes_all_its_panes() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    // A second workspace, now active, with an extra pane in its tab.
    let (workspace_id, _tab_id, pane_id) = new_workspace_ok(&mut stream, &mut codec, 630).await;
    let extra = split_pane_ok(
        &mut stream,
        &mut codec,
        pane_id,
        SplitDirection::Horizontal,
        631,
    )
    .await;

    close_workspace_ok(&mut stream, &mut codec, workspace_id, 632).await;

    // Both of the closed workspace's panes are gone; the seeded pane 0 in
    // workspace 0 survives, and it is active again.
    let frame = Frame::new(MSG_LIST_PANES, 633, vec![]).expect("list-panes frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 633).await;
    let list = ListPanesResponse::decode(&resp.payload).expect("decode ListPanesResponse");
    let ids: Vec<u32> = list.panes.iter().map(|p| p.pane_id).collect();
    assert!(ids.contains(&0), "seeded pane survives: {ids:?}");
    assert!(
        !ids.contains(&pane_id) && !ids.contains(&extra),
        "closed panes gone: {ids:?}"
    );
    assert_eq!(
        list_tabs_ok(&mut stream, &mut codec, 634)
            .await
            .workspace_id,
        0
    );
}

#[tokio::test]
async fn close_workspace_last_workspace_refused() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;
    // Only the seeded workspace 0 exists.
    let msg = CloseWorkspace { workspace_id: 0 };
    let frame = Frame::new(MSG_CLOSE_WORKSPACE, 640, msg.encode()).expect("close-ws frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 640).await;
    assert_eq!(resp.msg_type, MSG_ERROR);
    assert_eq!(
        ErrorMessage::decode(&resp.payload).expect("decode").code,
        ErrorCode::LayoutRejected as u32
    );
}

#[tokio::test]
async fn close_workspace_unknown_errors() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;
    // Only the seeded workspace 0 exists. An unknown id must report
    // UnknownWorkspace even though the last-workspace guard would also
    // fire — existence is checked first.
    let msg = CloseWorkspace { workspace_id: 99 };
    let frame = Frame::new(MSG_CLOSE_WORKSPACE, 651, msg.encode()).expect("close-ws frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 651).await;
    assert_eq!(resp.msg_type, MSG_ERROR);
    assert_eq!(
        ErrorMessage::decode(&resp.payload).expect("decode").code,
        ErrorCode::UnknownWorkspace as u32
    );
}

/// Truncated tab-op payloads produce `MalformedPayload`: the request-shaped
/// `CloseWorkspace` replies on its serial; the push ops report via serial-0
/// Error pushes.
#[tokio::test]
async fn malformed_tab_op_payloads_error() {
    let (mut stream, mut codec, _td) = connect_and_handshake().await;

    for msg_type in [MSG_MOVE_TAB, MSG_RENAME_TAB, MSG_RENAME_WORKSPACE] {
        push_frame(&mut stream, &mut codec, msg_type, vec![0x00]).await;
        let resp = read_push_with_msg_type(&mut stream, &mut codec, MSG_ERROR).await;
        assert_eq!(
            ErrorMessage::decode(&resp.payload).expect("decode").code,
            ErrorCode::MalformedPayload as u32,
            "msg {msg_type:#x}"
        );
    }

    let frame = Frame::new(MSG_CLOSE_WORKSPACE, 660, vec![0x00]).expect("close-ws frame");
    write_frame(&mut stream, &mut codec, frame).await;
    let resp = read_response_with_serial(&mut stream, &mut codec, 660).await;
    assert_eq!(resp.msg_type, MSG_ERROR);
    assert_eq!(
        ErrorMessage::decode(&resp.payload).expect("decode").code,
        ErrorCode::MalformedPayload as u32
    );
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
    assert_eq!(
        server_hello.protocol_version_minor,
        ClientHello::VERSION_MINOR
    );
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
