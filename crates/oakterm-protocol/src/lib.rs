pub mod frame;
pub mod message;

#[cfg(test)]
mod tests {
    use crate::frame::{Frame, FrameCodec, MAX_PAYLOAD};
    use crate::message::*;
    use bytes::BytesMut;
    use tokio_util::codec::{Decoder, Encoder};

    #[test]
    fn frame_roundtrip() {
        let frame = Frame::new(0x42, 7, b"hello".to_vec()).unwrap();
        let encoded = frame.encode_to_vec();
        let (decoded, consumed) = Frame::decode_from_slice(&encoded).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, frame);
    }

    #[test]
    fn frame_empty_payload() {
        let frame = Frame::new(MSG_PING, 1, vec![]).unwrap();
        let encoded = frame.encode_to_vec();
        let (decoded, _) = Frame::decode_from_slice(&encoded).unwrap();
        assert_eq!(decoded.msg_type, MSG_PING);
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn frame_bad_magic() {
        let mut data = Frame::new(0x01, 1, vec![]).unwrap().encode_to_vec();
        data[0] = 0xFF;
        assert!(Frame::decode_from_slice(&data).is_err());
    }

    #[test]
    fn frame_too_short() {
        assert!(Frame::decode_from_slice(&[0x4F, 0x54]).is_err());
    }

    #[test]
    fn frame_oversized_payload_rejected() {
        let big = vec![0u8; MAX_PAYLOAD as usize + 1];
        assert!(Frame::new(0x01, 1, big).is_err());
    }

    #[test]
    fn codec_roundtrip() {
        let mut codec = FrameCodec;
        let frame = Frame::new(0x10, 42, b"test payload".to_vec()).unwrap();

        let mut buf = BytesMut::new();
        codec.encode(frame.clone(), &mut buf).unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn codec_partial_header() {
        let mut codec = FrameCodec;
        let frame = Frame::new(0x01, 1, b"data".to_vec()).unwrap();
        let encoded = frame.encode_to_vec();

        let mut buf = BytesMut::from(&encoded[..5]);
        assert!(codec.decode(&mut buf).unwrap().is_none());

        buf.extend_from_slice(&encoded[5..]);
        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn codec_multiple_frames() {
        let mut codec = FrameCodec;
        let f1 = Frame::new(MSG_PING, 1, vec![]).unwrap();
        let f2 = Frame::new(MSG_PONG, 1, vec![]).unwrap();

        let mut buf = BytesMut::new();
        codec.encode(f1.clone(), &mut buf).unwrap();
        codec.encode(f2.clone(), &mut buf).unwrap();

        assert_eq!(codec.decode(&mut buf).unwrap().unwrap(), f1);
        assert_eq!(codec.decode(&mut buf).unwrap().unwrap(), f2);
        assert!(codec.decode(&mut buf).unwrap().is_none());
    }

    #[test]
    fn codec_bad_magic_clears_buffer() {
        let mut codec = FrameCodec;
        let mut buf = BytesMut::from(&[0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0][..]);
        assert!(codec.decode(&mut buf).is_err());
        assert!(buf.is_empty()); // Buffer cleared on fatal error.
    }

    #[test]
    fn client_hello_roundtrip() {
        let hello = ClientHello {
            protocol_version_major: 1,
            protocol_version_minor: 0,
            client_type: ClientType::Gui,
            client_name: "oakterm-gui".to_string(),
        };
        let encoded = hello.encode().unwrap();
        let decoded = ClientHello::decode(&encoded).unwrap();
        assert_eq!(decoded, hello);
    }

    #[test]
    fn server_hello_roundtrip() {
        let hello = ServerHello {
            status: HandshakeStatus::Accepted,
            protocol_version_major: 1,
            protocol_version_minor: 0,
            server_version: "0.1.0".to_string(),
        };
        let encoded = hello.encode().unwrap();
        let decoded = ServerHello::decode(&encoded).unwrap();
        assert_eq!(decoded, hello);
    }

    #[test]
    fn server_hello_version_mismatch() {
        let hello = ServerHello {
            status: HandshakeStatus::VersionMismatch,
            protocol_version_major: 2,
            protocol_version_minor: 0,
            server_version: "2.0.0".to_string(),
        };
        let decoded = ServerHello::decode(&hello.encode().unwrap()).unwrap();
        assert_eq!(decoded.status, HandshakeStatus::VersionMismatch);
    }

    #[test]
    fn error_message_roundtrip() {
        let err = ErrorMessage {
            code: ErrorCode::UnknownPane as u32,
            message: "pane 42 not found".to_string(),
        };
        let encoded = err.encode().unwrap();
        let decoded = ErrorMessage::decode(&encoded).unwrap();
        assert_eq!(decoded, err);
    }

    #[test]
    fn ping_pong_as_frames() {
        let request = Frame::new(MSG_PING, 5, vec![]).unwrap();
        let response = Frame::new(MSG_PONG, 5, vec![]).unwrap();
        assert_eq!(request.serial, response.serial);
        assert!(request.payload.is_empty());
        assert!(response.payload.is_empty());
    }

    #[test]
    fn handshake_as_frames() {
        let client = ClientHello {
            protocol_version_major: 1,
            protocol_version_minor: 0,
            client_type: ClientType::Gui,
            client_name: "test".to_string(),
        };
        let frame = client.to_frame(1).unwrap();
        assert_eq!(frame.msg_type, MSG_CLIENT_HELLO);
        assert_eq!(frame.serial, 1);

        let decoded = ClientHello::decode(&frame.payload).unwrap();
        assert_eq!(decoded, client);
    }

    #[test]
    fn unknown_client_type_rejected() {
        assert!(ClientType::try_from(255).is_err());
    }

    #[test]
    fn unknown_shutdown_reason_rejected() {
        assert!(ShutdownReason::try_from(99).is_err());
    }

    #[test]
    fn client_hello_empty_payload_rejected() {
        assert!(ClientHello::decode(&[]).is_err());
    }
}
