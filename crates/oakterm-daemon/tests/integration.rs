//! Integration tests: daemon lifecycle, handshake, and error responses.
//!
//! Each test gets its own tempdir + socket path for parallel execution.

use bytes::BytesMut;
use oakterm_protocol::frame::{Frame, FrameCodec};
use oakterm_protocol::message::{
    ClientHello, ClientType, ErrorCode, ErrorMessage, HandshakeStatus, MSG_ERROR, MSG_SERVER_HELLO,
    ServerHello,
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
