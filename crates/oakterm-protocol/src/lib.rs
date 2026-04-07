pub mod frame;
pub mod input;
pub mod message;
pub mod render;

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
    fn pane_exited_roundtrip() {
        let msg = PaneExited {
            pane_id: 1,
            exit_code: 137,
        };
        let encoded = msg.encode();
        assert_eq!(encoded.len(), 8);
        let decoded = PaneExited::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn pane_exited_as_frame() {
        let msg = PaneExited {
            pane_id: 0,
            exit_code: 0,
        };
        let frame = msg.to_frame().unwrap();
        assert_eq!(frame.msg_type, MSG_PANE_EXITED);
        assert_eq!(frame.serial, 0); // Push.
        let decoded = PaneExited::decode(&frame.payload).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn pane_exited_too_short() {
        assert!(PaneExited::decode(&[0; 4]).is_err());
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

    // --- Render protocol tests ---

    use crate::render::{DirtyNotify, DirtyRow, GetRenderUpdate, RenderUpdate, WireCell};

    #[test]
    fn dirty_notify_roundtrip() {
        let msg = DirtyNotify { pane_id: 42 };
        let encoded = msg.encode();
        let decoded = DirtyNotify::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn get_render_update_roundtrip() {
        let msg = GetRenderUpdate {
            pane_id: 1,
            since_seqno: 12345,
        };
        let encoded = msg.encode();
        let decoded = GetRenderUpdate::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn wire_cell_roundtrip() {
        let cell = WireCell {
            codepoint: 'A' as u32,
            fg_r: 255,
            fg_g: 0,
            fg_b: 0,
            fg_type: 1,
            bg_r: 0,
            bg_g: 0,
            bg_b: 0,
            bg_type: 0,
            flags: 0x0001, // bold
            extra: vec![],
        };
        let encoded = cell.encode().unwrap();
        assert_eq!(encoded.len(), WireCell::FIXED_SIZE);
        let (decoded, consumed) = WireCell::decode(&encoded).unwrap();
        assert_eq!(consumed, WireCell::FIXED_SIZE);
        assert_eq!(decoded, cell);
    }

    #[test]
    fn wire_cell_with_extra_data() {
        let cell = WireCell {
            codepoint: 'X' as u32,
            fg_r: 0,
            fg_g: 0,
            fg_b: 0,
            fg_type: 0,
            bg_r: 0,
            bg_g: 0,
            bg_b: 0,
            bg_type: 0,
            flags: 0,
            extra: vec![0x68, 0x69], // some extra data
        };
        let encoded = cell.encode().unwrap();
        assert_eq!(encoded.len(), WireCell::FIXED_SIZE + 2);
        let (decoded, consumed) = WireCell::decode(&encoded).unwrap();
        assert_eq!(consumed, WireCell::FIXED_SIZE + 2);
        assert_eq!(decoded, cell);
    }

    #[test]
    fn render_update_roundtrip() {
        let update = RenderUpdate {
            pane_id: 1,
            seqno: 99,
            cursor_x: 5,
            cursor_y: 10,
            cursor_style: 0,
            cursor_visible: true,
            bg_r: 0,
            bg_g: 0,
            bg_b: 0,
            bracketed_paste: true,
            alt_screen: true,
            dirty_rows: vec![DirtyRow {
                row_index: 0,
                cells: vec![WireCell {
                    codepoint: 'H' as u32,
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
                }],
                semantic_mark: 0,
                mark_metadata: vec![],
            }],
        };
        let encoded = update.encode().unwrap();
        let decoded = RenderUpdate::decode(&encoded).unwrap();
        assert_eq!(decoded, update);
    }

    #[test]
    fn render_update_empty_rows() {
        let update = RenderUpdate {
            pane_id: 5,
            seqno: 0,
            cursor_x: 0,
            cursor_y: 0,
            cursor_style: 0,
            cursor_visible: false,
            bg_r: 0,
            bg_g: 0,
            bg_b: 0,
            bracketed_paste: false,
            alt_screen: false,
            dirty_rows: vec![],
        };
        let encoded = update.encode().unwrap();
        let decoded = RenderUpdate::decode(&encoded).unwrap();
        assert_eq!(decoded, update);
    }

    #[test]
    fn dirty_notify_as_frame() {
        let msg = DirtyNotify { pane_id: 7 };
        let frame = Frame::new(MSG_DIRTY_NOTIFY, 0, msg.encode()).unwrap();
        assert_eq!(frame.msg_type, MSG_DIRTY_NOTIFY);
        assert_eq!(frame.serial, 0); // Push — serial 0.
        let decoded = DirtyNotify::decode(&frame.payload).unwrap();
        assert_eq!(decoded.pane_id, 7);
    }

    // --- Input protocol tests ---

    use crate::input::{Detach, KeyInput, Resize};

    #[test]
    fn key_input_roundtrip() {
        let msg = KeyInput {
            pane_id: 1,
            key_data: b"hello".to_vec(),
        };
        let encoded = msg.encode().unwrap();
        let decoded = KeyInput::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn key_input_empty_data() {
        let msg = KeyInput {
            pane_id: 0,
            key_data: vec![],
        };
        let encoded = msg.encode().unwrap();
        let decoded = KeyInput::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
        assert!(decoded.key_data.is_empty());
    }

    #[test]
    fn key_input_single_byte() {
        let msg = KeyInput {
            pane_id: 42,
            key_data: vec![0x1B], // ESC
        };
        let encoded = msg.encode().unwrap();
        let decoded = KeyInput::decode(&encoded).unwrap();
        assert_eq!(decoded.key_data, vec![0x1B]);
    }

    #[test]
    fn key_input_too_short() {
        assert!(KeyInput::decode(&[0, 0]).is_err());
    }

    #[test]
    fn key_input_as_frame() {
        let msg = KeyInput {
            pane_id: 3,
            key_data: b"x".to_vec(),
        };
        let frame = Frame::new(MSG_KEY_INPUT, 0, msg.encode().unwrap()).unwrap();
        assert_eq!(frame.msg_type, MSG_KEY_INPUT);
        assert_eq!(frame.serial, 0); // Push.
        let decoded = KeyInput::decode(&frame.payload).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn resize_roundtrip() {
        let msg = Resize {
            pane_id: 1,
            cols: 120,
            rows: 40,
            pixel_width: 960,
            pixel_height: 640,
        };
        let encoded = msg.encode();
        let decoded = Resize::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn resize_too_short() {
        assert!(Resize::decode(&[0; 4]).is_err());
    }

    #[test]
    fn resize_as_frame() {
        let msg = Resize {
            pane_id: 0,
            cols: 80,
            rows: 24,
            pixel_width: 640,
            pixel_height: 480,
        };
        let frame = Frame::new(MSG_RESIZE, 0, msg.encode()).unwrap();
        assert_eq!(frame.msg_type, MSG_RESIZE);
        let decoded = Resize::decode(&frame.payload).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn detach_roundtrip() {
        let msg = Detach;
        let encoded = msg.encode();
        assert!(encoded.is_empty());
        Detach::decode(&encoded).unwrap();
    }

    #[test]
    fn detach_as_frame() {
        let frame = Frame::new(MSG_DETACH, 0, Detach.encode()).unwrap();
        assert_eq!(frame.msg_type, MSG_DETACH);
        assert!(frame.payload.is_empty());
    }

    #[test]
    fn key_input_oversized_data_rejected() {
        let msg = KeyInput {
            pane_id: 0,
            key_data: vec![0u8; u16::MAX as usize + 1],
        };
        assert!(msg.encode().is_err());
    }

    #[test]
    fn key_input_truncated_key_data() {
        let msg = KeyInput {
            pane_id: 1,
            key_data: b"abcd".to_vec(),
        };
        let mut encoded = msg.encode().unwrap();
        encoded.truncate(encoded.len() - 2); // chop off 2 bytes of key_data
        assert!(KeyInput::decode(&encoded).is_err());
    }

    // --- Scrollback protocol tests ---

    #[test]
    fn get_scrollback_roundtrip() {
        let msg = GetScrollback {
            pane_id: 1,
            start_row: -50,
            count: 25,
        };
        let encoded = msg.encode();
        assert_eq!(encoded.len(), 16);
        let decoded = GetScrollback::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn get_scrollback_too_short() {
        assert!(GetScrollback::decode(&[0; 8]).is_err());
    }

    #[test]
    fn scrollback_data_roundtrip_empty() {
        let msg = ScrollbackData {
            pane_id: 0,
            start_row: -10,
            has_more: false,
            total_rows: 0,
            rows: vec![],
        };
        let encoded = msg.encode().unwrap();
        let decoded = ScrollbackData::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn scrollback_data_roundtrip_with_rows() {
        let msg = ScrollbackData {
            pane_id: 1,
            start_row: -5,
            has_more: true,
            total_rows: 100,
            rows: vec![DirtyRow {
                row_index: 0,
                cells: vec![WireCell {
                    codepoint: 'A' as u32,
                    fg_r: 255,
                    fg_g: 255,
                    fg_b: 255,
                    fg_type: 0,
                    bg_r: 0,
                    bg_g: 0,
                    bg_b: 0,
                    bg_type: 0,
                    flags: 0,
                    extra: vec![],
                }],
                semantic_mark: 0,
                mark_metadata: vec![],
            }],
        };
        let encoded = msg.encode().unwrap();
        let decoded = ScrollbackData::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn scrollback_data_too_short() {
        assert!(ScrollbackData::decode(&[0; 10]).is_err());
    }

    // --- FindPrompt / PromptPosition protocol tests ---

    use crate::message::SearchDirection;

    #[test]
    fn find_prompt_roundtrip() {
        let msg = FindPrompt {
            pane_id: 1,
            from_offset: -42,
            direction: SearchDirection::Older,
        };
        let encoded = msg.encode();
        assert_eq!(encoded.len(), 13);
        let decoded = FindPrompt::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn find_prompt_forward() {
        let msg = FindPrompt {
            pane_id: 0,
            from_offset: -10,
            direction: SearchDirection::Newer,
        };
        let decoded = FindPrompt::decode(&msg.encode()).unwrap();
        assert_eq!(decoded.direction, SearchDirection::Newer);
    }

    #[test]
    fn find_prompt_invalid_direction() {
        let mut data = FindPrompt {
            pane_id: 0,
            from_offset: -1,
            direction: SearchDirection::Older,
        }
        .encode();
        data[12] = 0x00; // invalid direction byte
        assert!(FindPrompt::decode(&data).is_err());
    }

    #[test]
    fn find_prompt_too_short() {
        assert!(FindPrompt::decode(&[0; 8]).is_err());
    }

    #[test]
    fn search_direction_try_from() {
        assert_eq!(
            SearchDirection::try_from(0xFF).unwrap(),
            SearchDirection::Older
        );
        assert_eq!(
            SearchDirection::try_from(0x01).unwrap(),
            SearchDirection::Newer
        );
        assert!(SearchDirection::try_from(0x00).is_err());
        assert!(SearchDirection::try_from(0x02).is_err());
    }

    #[test]
    fn prompt_position_found() {
        let msg = PromptPosition {
            pane_id: 1,
            offset: Some(-25),
        };
        let encoded = msg.encode();
        assert_eq!(encoded.len(), 13);
        let decoded = PromptPosition::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn prompt_position_not_found() {
        let msg = PromptPosition {
            pane_id: 0,
            offset: None,
        };
        let decoded = PromptPosition::decode(&msg.encode()).unwrap();
        assert!(decoded.offset.is_none());
    }

    #[test]
    fn prompt_position_too_short() {
        assert!(PromptPosition::decode(&[0; 8]).is_err());
    }

    #[test]
    fn prompt_position_as_frame() {
        let msg = PromptPosition {
            pane_id: 1,
            offset: Some(-15),
        };
        let frame = msg.to_frame(7).unwrap();
        assert_eq!(frame.msg_type, MSG_PROMPT_POSITION);
        assert_eq!(frame.serial, 7);
        let decoded = PromptPosition::decode(&frame.payload).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn search_scrollback_roundtrip() {
        let msg = SearchScrollback {
            pane_id: 3,
            flags: SearchFlags(SearchFlags::REGEX),
            query: "error.*timeout".into(),
        };
        let encoded = msg.encode().unwrap();
        let decoded = SearchScrollback::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn search_scrollback_empty_query() {
        let msg = SearchScrollback {
            pane_id: 0,
            flags: SearchFlags(0),
            query: String::new(),
        };
        let encoded = msg.encode().unwrap();
        let decoded = SearchScrollback::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn search_scrollback_too_short() {
        assert!(SearchScrollback::decode(&[0; 3]).is_err());
    }

    #[test]
    fn search_results_roundtrip() {
        let msg = SearchResults {
            pane_id: 1,
            total_matches: 42,
            active_index: Some(7),
            active_row_offset: -100,
            capped: false,
            visible_matches: vec![
                VisibleMatch {
                    row: 5,
                    col_start: 10,
                    col_end: 15,
                    is_active: true,
                },
                VisibleMatch {
                    row: 8,
                    col_start: 0,
                    col_end: 3,
                    is_active: false,
                },
            ],
        };
        let encoded = msg.encode().unwrap();
        let decoded = SearchResults::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn search_results_no_matches() {
        let msg = SearchResults {
            pane_id: 0,
            total_matches: 0,
            active_index: None,
            active_row_offset: 0,
            capped: false,
            visible_matches: vec![],
        };
        let encoded = msg.encode().unwrap();
        let decoded = SearchResults::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn search_results_too_short() {
        assert!(SearchResults::decode(&[0; 10]).is_err());
    }

    #[test]
    fn search_nav_roundtrip() {
        let msg = SearchNav { pane_id: 99 };
        let encoded = msg.encode();
        let decoded = SearchNav::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn search_nav_too_short() {
        assert!(SearchNav::decode(&[0; 2]).is_err());
    }

    // --- CreatePane / ClosePane ---

    #[test]
    fn create_pane_roundtrip() {
        let msg = CreatePane {
            command: "bash".into(),
            cwd: "/home/user".into(),
        };
        let encoded = msg.encode().unwrap();
        let decoded = CreatePane::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn create_pane_empty_fields() {
        let msg = CreatePane {
            command: String::new(),
            cwd: String::new(),
        };
        let encoded = msg.encode().unwrap();
        assert_eq!(encoded.len(), 4);
        let decoded = CreatePane::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn create_pane_too_short() {
        assert!(CreatePane::decode(&[0; 2]).is_err());
    }

    #[test]
    fn create_pane_response_roundtrip() {
        let msg = CreatePaneResponse { pane_id: 42 };
        let encoded = msg.encode();
        let decoded = CreatePaneResponse::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn create_pane_response_as_frame() {
        let msg = CreatePaneResponse { pane_id: 7 };
        let frame = msg.to_frame(123).unwrap();
        assert_eq!(frame.msg_type, MSG_CREATE_PANE_RESPONSE);
        assert_eq!(frame.serial, 123);
        let decoded = CreatePaneResponse::decode(&frame.payload).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn close_pane_roundtrip() {
        let msg = ClosePane { pane_id: 5 };
        let encoded = msg.encode();
        let decoded = ClosePane::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn close_pane_too_short() {
        assert!(ClosePane::decode(&[0; 2]).is_err());
    }

    // --- FocusPane / ListPanes ---

    #[test]
    fn focus_pane_roundtrip() {
        let msg = FocusPane { pane_id: 3 };
        let encoded = msg.encode();
        let decoded = FocusPane::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn pane_info_roundtrip() {
        let info = PaneInfo {
            pane_id: 1,
            title: "bash".into(),
            cols: 80,
            rows: 24,
            pid: 12345,
            exit_code: -1,
            cwd: "/home/user".into(),
        };
        let encoded = info.encode().unwrap();
        let (decoded, consumed) = PaneInfo::decode(&encoded).unwrap();
        assert_eq!(decoded, info);
        assert_eq!(consumed, encoded.len());
    }

    #[test]
    fn list_panes_response_roundtrip() {
        let resp = ListPanesResponse {
            panes: vec![
                PaneInfo {
                    pane_id: 0,
                    title: "zsh".into(),
                    cols: 120,
                    rows: 40,
                    pid: 100,
                    exit_code: -1,
                    cwd: String::new(),
                },
                PaneInfo {
                    pane_id: 1,
                    title: String::new(),
                    cols: 80,
                    rows: 24,
                    pid: 0,
                    exit_code: 0,
                    cwd: "/tmp".into(),
                },
            ],
        };
        let encoded = resp.encode().unwrap();
        let decoded = ListPanesResponse::decode(&encoded).unwrap();
        assert_eq!(decoded, resp);
    }

    #[test]
    fn list_panes_response_empty() {
        let resp = ListPanesResponse { panes: vec![] };
        let encoded = resp.encode().unwrap();
        assert_eq!(encoded.len(), 2);
        let decoded = ListPanesResponse::decode(&encoded).unwrap();
        assert_eq!(decoded.panes.len(), 0);
    }

    #[test]
    fn list_panes_response_as_frame() {
        let resp = ListPanesResponse { panes: vec![] };
        let frame = resp.to_frame(42).unwrap();
        assert_eq!(frame.msg_type, MSG_LIST_PANES_RESPONSE);
        assert_eq!(frame.serial, 42);
    }
}
