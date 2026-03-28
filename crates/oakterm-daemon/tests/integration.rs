//! Integration test: daemon starts, client connects, handshake succeeds.

use bytes::BytesMut;
use oakterm_protocol::frame::FrameCodec;
use oakterm_protocol::message::{
    ClientHello, ClientType, HandshakeStatus, MSG_SERVER_HELLO, ServerHello,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio_util::codec::{Decoder, Encoder};

#[tokio::test]
async fn daemon_handshake() {
    let daemon = oakterm_daemon::server::Daemon::new(80, 24).expect("create daemon");
    let socket = daemon.socket_path().to_path_buf();

    let handle = tokio::spawn(async move {
        let _ = daemon.run().await;
    });

    // Poll for socket file instead of fixed sleep.
    for i in 0..20 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50 * (i + 1))).await;
    }
    assert!(socket.exists(), "daemon did not bind socket in time");

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
