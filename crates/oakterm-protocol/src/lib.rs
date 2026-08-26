pub mod frame;
pub mod input;
pub mod message;
pub mod render;
pub mod socket;

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

    /// A capability gate must never name a minor this build's own daemon
    /// does not advertise, or the feature is dead against a matched pair
    /// — the gate would be indistinguishable from the feature being
    /// broken, since both just fail to start.
    #[test]
    fn every_capability_gate_is_reachable_by_this_builds_daemon() {
        for (name, gate) in [
            ("LIST_TABS_MIN_MINOR", LIST_TABS_MIN_MINOR),
            ("TAB_OPS_MIN_MINOR", TAB_OPS_MIN_MINOR),
            ("COPY_MODE_MIN_MINOR", COPY_MODE_MIN_MINOR),
        ] {
            assert!(
                gate <= ClientHello::VERSION_MINOR,
                "{name} = {gate} exceeds the advertised minor {}",
                ClientHello::VERSION_MINOR
            );
        }
    }

    /// Copy mode is gated at the minor whose `EnterCopyMode` carries the
    /// client's base (ADR-0025); an older daemon would silently ignore it
    /// and pin at its own `history_len()`.
    #[test]
    fn copy_mode_is_gated_at_the_anchor_minor() {
        assert_eq!(COPY_MODE_MIN_MINOR, 5);
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
            input_flags: 0b0000_0101,
            kitty_kbd_flags: 3,
            history_len: 987_654,
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
            input_flags: 0,
            kitty_kbd_flags: 0,
            history_len: 0,
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
            base: 0,
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
            base: 4_200,
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

    // --- Copy mode (0x97-0x9C) ---

    #[test]
    fn enter_copy_mode_roundtrip() {
        let msg = EnterCopyMode {
            pane_id: 7,
            base: 12_345,
        };
        let encoded = msg.encode();
        assert_eq!(encoded.len(), 12);
        assert_eq!(EnterCopyMode::decode(&encoded).unwrap(), msg);
    }

    /// Entry is a correlated request (ADR-0025); exit stays a push.
    #[test]
    fn copy_mode_frames_carry_their_own_type_and_serial() {
        let enter = EnterCopyMode {
            pane_id: 7,
            base: 3,
        }
        .to_frame(41)
        .unwrap();
        assert_eq!(enter.msg_type, MSG_ENTER_COPY_MODE);
        assert_eq!(enter.serial, 41);

        let exit = ExitCopyMode { pane_id: 7 }.to_exit_frame().unwrap();
        assert_eq!(exit.msg_type, MSG_EXIT_COPY_MODE);
        assert_eq!(exit.serial, 0);
    }

    #[test]
    fn enter_copy_mode_ack_roundtrip() {
        let msg = EnterCopyModeAck {
            pane_id: 7,
            base: 12_345,
        };
        let encoded = msg.encode();
        assert_eq!(EnterCopyModeAck::decode(&encoded).unwrap(), msg);
        let frame = msg.to_frame(41).unwrap();
        assert_eq!(frame.msg_type, MSG_ENTER_COPY_MODE_ACK);
        assert_eq!(frame.serial, 41);
    }

    #[test]
    fn copy_mode_invalidated_roundtrip() {
        let msg = CopyModeInvalidated { pane_id: 7 };
        let encoded = msg.encode();
        assert_eq!(CopyModeInvalidated::decode(&encoded).unwrap(), msg);
        let frame = msg.to_frame().unwrap();
        assert_eq!(frame.msg_type, MSG_COPY_MODE_INVALIDATED);
        assert_eq!(frame.serial, 0, "invalidation is a push");
    }

    #[test]
    fn copy_mode_messages_too_short() {
        assert!(EnterCopyMode::decode(&[0; 11]).is_err());
        assert!(EnterCopyModeAck::decode(&[0; 11]).is_err());
        assert!(ExitCopyMode::decode(&[0; 3]).is_err());
        assert!(CopyModeInvalidated::decode(&[0; 3]).is_err());
    }

    #[test]
    fn yank_selection_roundtrip() {
        for ty in [
            CopySelectionType::Character,
            CopySelectionType::Line,
            CopySelectionType::Block,
        ] {
            let msg = YankSelection {
                pane_id: 2,
                start_row: -4_000_000_000,
                start_col: 3,
                end_row: 12,
                end_col: 79,
                selection_type: ty,
            };
            let encoded = msg.encode();
            assert_eq!(encoded.len(), 25);
            assert_eq!(YankSelection::decode(&encoded).unwrap(), msg);
        }
    }

    #[test]
    fn yank_selection_unknown_type_rejected() {
        let mut encoded = YankSelection {
            pane_id: 1,
            start_row: 0,
            start_col: 0,
            end_row: 0,
            end_col: 0,
            selection_type: CopySelectionType::Character,
        }
        .encode();
        *encoded.last_mut().unwrap() = 9;
        assert!(YankSelection::decode(&encoded).is_err());
    }

    #[test]
    fn yank_selection_too_short() {
        assert!(YankSelection::decode(&[0; 24]).is_err());
    }

    #[test]
    fn yank_response_roundtrip() {
        let resp = YankResponse {
            text: "café ☕\nsecond line".to_string(),
        };
        let encoded = resp.encode().unwrap();
        assert_eq!(YankResponse::decode(&encoded).unwrap(), resp);
    }

    #[test]
    fn yank_response_empty() {
        let resp = YankResponse {
            text: String::new(),
        };
        let encoded = resp.encode().unwrap();
        assert_eq!(encoded.len(), 4);
        assert!(YankResponse::decode(&encoded).unwrap().text.is_empty());
    }

    #[test]
    fn yank_response_truncated_text_rejected() {
        let mut encoded = YankResponse {
            text: "hello".to_string(),
        }
        .encode()
        .unwrap();
        encoded.pop();
        assert!(YankResponse::decode(&encoded).is_err());
    }

    #[test]
    fn yank_response_invalid_utf8_rejected() {
        let mut encoded = 2u32.to_le_bytes().to_vec();
        encoded.extend_from_slice(&[0xFF, 0xFE]);
        assert!(YankResponse::decode(&encoded).is_err());
    }

    #[test]
    fn yank_response_as_frame() {
        let resp = YankResponse {
            text: "x".to_string(),
        };
        let frame = resp.to_frame(11).unwrap();
        assert_eq!(frame.msg_type, MSG_YANK_RESPONSE);
        assert_eq!(frame.serial, 11);
    }

    // --- Split topology (0xA0-0xA4) ---

    #[test]
    fn split_pane_roundtrip() {
        let msg = SplitPane {
            pane_id: 3,
            direction: SplitDirection::Vertical,
            command: "htop --tree".into(),
            cwd: "/home/user".into(),
        };
        let encoded = msg.encode().unwrap();
        let decoded = SplitPane::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn split_pane_empty_strings_roundtrip() {
        let msg = SplitPane {
            pane_id: 0,
            direction: SplitDirection::Horizontal,
            command: String::new(),
            cwd: String::new(),
        };
        let encoded = msg.encode().unwrap();
        assert_eq!(encoded.len(), 9);
        let decoded = SplitPane::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn split_pane_unknown_direction_rejected() {
        let msg = SplitPane {
            pane_id: 1,
            direction: SplitDirection::Horizontal,
            command: String::new(),
            cwd: String::new(),
        };
        let mut encoded = msg.encode().unwrap();
        encoded[4] = 2;
        assert!(SplitPane::decode(&encoded).is_err());
    }

    #[test]
    fn split_pane_too_short() {
        assert!(SplitPane::decode(&[0; 4]).is_err());
    }

    #[test]
    fn split_pane_response_roundtrip_and_frame() {
        let resp = SplitPaneResponse { new_pane_id: 7 };
        let decoded = SplitPaneResponse::decode(&resp.encode()).unwrap();
        assert_eq!(decoded, resp);
        let frame = resp.to_frame(11).unwrap();
        assert_eq!(frame.msg_type, MSG_SPLIT_PANE_RESPONSE);
        assert_eq!(frame.serial, 11);
    }

    #[test]
    fn resize_pane_roundtrip() {
        let msg = ResizePane {
            pane_id: 2,
            neighbor_pane_id: 5,
            delta: -3,
        };
        let decoded = ResizePane::decode(&msg.encode()).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn resize_pane_too_short() {
        assert!(ResizePane::decode(&[0; 9]).is_err());
    }

    #[test]
    fn swap_pane_roundtrip() {
        let msg = SwapPane {
            pane_id_a: 1,
            pane_id_b: 9,
        };
        let decoded = SwapPane::decode(&msg.encode()).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn swap_pane_too_short() {
        assert!(SwapPane::decode(&[0; 7]).is_err());
    }

    #[test]
    fn shutdown_roundtrip_and_push_frame() {
        let msg = Shutdown {
            reason: ShutdownReason::Upgrade,
        };
        let decoded = Shutdown::decode(&msg.encode()).unwrap();
        assert_eq!(decoded, msg);
        let frame = msg.to_frame().unwrap();
        assert_eq!(frame.msg_type, MSG_SHUTDOWN);
        assert_eq!(frame.serial, 0, "Shutdown is a push");
    }

    #[test]
    fn request_shutdown_roundtrip() {
        for reason in [RequestShutdownReason::Quit, RequestShutdownReason::Upgrade] {
            let msg = RequestShutdown { reason };
            let decoded = RequestShutdown::decode(&msg.encode()).unwrap();
            assert_eq!(decoded, msg);
        }
        let frame = RequestShutdown {
            reason: RequestShutdownReason::Quit,
        }
        .to_frame(77)
        .unwrap();
        assert_eq!(frame.msg_type, MSG_REQUEST_SHUTDOWN);
        assert_eq!(frame.serial, 77);
    }

    #[test]
    fn request_shutdown_unknown_reason_rejected() {
        assert!(RequestShutdown::decode(&[9]).is_err());
        assert!(RequestShutdown::decode(&[]).is_err());
    }

    #[test]
    fn request_shutdown_reason_maps_to_broadcast_reason() {
        assert_eq!(
            RequestShutdownReason::Quit.broadcast_reason(),
            ShutdownReason::Clean
        );
        assert_eq!(
            RequestShutdownReason::Upgrade.broadcast_reason(),
            ShutdownReason::Upgrade
        );
    }

    #[test]
    fn shutdown_ack_roundtrip() {
        for status in [ShutdownAckStatus::Accepted, ShutdownAckStatus::SaveFailed] {
            let msg = ShutdownAck { status };
            let decoded = ShutdownAck::decode(&msg.encode()).unwrap();
            assert_eq!(decoded, msg);
        }
        let frame = ShutdownAck {
            status: ShutdownAckStatus::Accepted,
        }
        .to_frame(88)
        .unwrap();
        assert_eq!(frame.msg_type, MSG_SHUTDOWN_ACK);
        assert_eq!(frame.serial, 88);
    }

    #[test]
    fn error_code_roundtrip_all_variants() {
        use ErrorCode::{
            InternalError, InvalidMessage, LayoutRejected, MalformedPayload, PaneExited,
            PermissionDenied, UnknownPane, UnknownTab, UnknownWorkspace,
        };
        let all = [
            UnknownPane,
            InvalidMessage,
            MalformedPayload,
            InternalError,
            PaneExited,
            PermissionDenied,
            LayoutRejected,
            UnknownTab,
            UnknownWorkspace,
        ];
        for code in all {
            // Exhaustive match: adding a variant without extending `all`
            // stops compiling.
            match code {
                UnknownPane | InvalidMessage | MalformedPayload | InternalError | PaneExited
                | PermissionDenied | LayoutRejected | UnknownTab | UnknownWorkspace => {}
            }
            assert_eq!(ErrorCode::try_from(code as u32).unwrap(), code);
        }
    }

    #[test]
    fn get_layout_tree_roundtrip() {
        let msg = GetLayoutTree {
            workspace_id: 1,
            tab_id: 2,
        };
        let decoded = GetLayoutTree::decode(&msg.encode()).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn get_layout_tree_too_short() {
        assert!(GetLayoutTree::decode(&[0u8; 7]).is_err());
    }

    fn sample_tree() -> LayoutTreeNode {
        LayoutTreeNode::Container {
            direction: LayoutDirection::Horizontal,
            children: vec![
                LayoutTreeNode::Leaf { pane_id: 1 },
                LayoutTreeNode::Container {
                    direction: LayoutDirection::Vertical,
                    children: vec![
                        LayoutTreeNode::Leaf { pane_id: 2 },
                        LayoutTreeNode::Leaf { pane_id: 3 },
                    ],
                    weights: vec![0.5, 0.5],
                },
            ],
            weights: vec![0.3, 0.7],
        }
    }

    #[test]
    fn layout_tree_nested_roundtrip() {
        let msg = LayoutTree {
            tree: sample_tree(),
        };
        let decoded = LayoutTree::decode(&msg.encode().unwrap()).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn layout_tree_single_leaf_roundtrip() {
        let msg = LayoutTree {
            tree: LayoutTreeNode::Leaf { pane_id: 42 },
        };
        let decoded = LayoutTree::decode(&msg.encode().unwrap()).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn layout_tree_json_shape_pinned() {
        // Pins the Spec-0001 wire contract: externally tagged snake_case
        // variants, Spec-0010 lowercase direction strings, pane_id leaves.
        let json = serde_json::to_string(&LayoutTreeNode::Container {
            direction: LayoutDirection::Vertical,
            children: vec![
                LayoutTreeNode::Leaf { pane_id: 7 },
                LayoutTreeNode::Leaf { pane_id: 9 },
            ],
            weights: vec![0.5, 0.5],
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"container":{"direction":"vertical","children":[{"leaf":{"pane_id":7}},{"leaf":{"pane_id":9}}],"weights":[0.5,0.5]}}"#
        );
    }

    #[test]
    fn layout_tree_invalid_json_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&5u32.to_le_bytes());
        buf.extend_from_slice(b"nope!");
        assert!(LayoutTree::decode(&buf).is_err());
    }

    #[test]
    fn layout_tree_truncated_payload_rejected() {
        let msg = LayoutTree {
            tree: sample_tree(),
        };
        let encoded = msg.encode().unwrap();
        // tree_len claims more bytes than the payload carries.
        assert!(LayoutTree::decode(&encoded[..encoded.len() - 1]).is_err());
    }

    #[test]
    fn layout_tree_mismatched_weights_rejected() {
        let json = r#"{"container":{"direction":"horizontal","children":[{"leaf":{"pane_id":1}},{"leaf":{"pane_id":2}}],"weights":[1.0]}}"#;
        let mut buf = Vec::new();
        buf.extend_from_slice(&u32::try_from(json.len()).unwrap().to_le_bytes());
        buf.extend_from_slice(json.as_bytes());
        assert!(LayoutTree::decode(&buf).is_err());
    }

    #[test]
    fn layout_tree_single_child_container_rejected() {
        let json = r#"{"container":{"direction":"horizontal","children":[{"leaf":{"pane_id":1}}],"weights":[1.0]}}"#;
        let mut buf = Vec::new();
        buf.extend_from_slice(&u32::try_from(json.len()).unwrap().to_le_bytes());
        buf.extend_from_slice(json.as_bytes());
        assert!(LayoutTree::decode(&buf).is_err());
    }

    fn decode_tree_json(json: &str) -> std::io::Result<LayoutTree> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&u32::try_from(json.len()).unwrap().to_le_bytes());
        buf.extend_from_slice(json.as_bytes());
        LayoutTree::decode(&buf)
    }

    #[test]
    fn layout_tree_nonpositive_weight_rejected() {
        // Negative weights make the cumulative geometry walk non-monotonic
        // (u32 underflow); zero weights degenerate the sum.
        let negative = r#"{"container":{"direction":"horizontal","children":[{"leaf":{"pane_id":1}},{"leaf":{"pane_id":2}},{"leaf":{"pane_id":3}}],"weights":[1.0,-0.5,0.5]}}"#;
        assert!(decode_tree_json(negative).is_err());
        let zero = r#"{"container":{"direction":"horizontal","children":[{"leaf":{"pane_id":1}},{"leaf":{"pane_id":2}}],"weights":[0.0,0.0]}}"#;
        assert!(decode_tree_json(zero).is_err());
    }

    #[test]
    fn layout_tree_encode_rejects_invalid_tree() {
        let msg = LayoutTree {
            tree: LayoutTreeNode::Container {
                direction: LayoutDirection::Horizontal,
                children: vec![
                    LayoutTreeNode::Leaf { pane_id: 1 },
                    LayoutTreeNode::Leaf { pane_id: 2 },
                ],
                weights: vec![1.0],
            },
        };
        assert!(msg.encode().is_err());
    }

    #[test]
    fn layout_tree_max_length_prefix_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&u32::MAX.to_le_bytes());
        buf.extend_from_slice(b"{}");
        assert!(LayoutTree::decode(&buf).is_err());
    }

    #[test]
    fn new_tab_roundtrip() {
        let msg = NewTab {
            workspace_id: 3,
            command: "htop --tree".into(),
            cwd: "/home/user".into(),
        };
        let encoded = msg.encode().unwrap();
        let decoded = NewTab::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn new_tab_empty_strings_roundtrip() {
        let msg = NewTab {
            workspace_id: 0,
            command: String::new(),
            cwd: String::new(),
        };
        let encoded = msg.encode().unwrap();
        assert_eq!(encoded.len(), 8);
        let decoded = NewTab::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn new_tab_too_short() {
        assert!(NewTab::decode(&[0; 3]).is_err());
        // workspace_id present but the command length prefix is truncated.
        assert!(NewTab::decode(&[0; 5]).is_err());
    }

    #[test]
    fn new_tab_response_roundtrip_and_frame() {
        let resp = NewTabResponse {
            tab_id: 2,
            pane_id: 9,
        };
        let decoded = NewTabResponse::decode(&resp.encode()).unwrap();
        assert_eq!(decoded, resp);
        let frame = resp.to_frame(21).unwrap();
        assert_eq!(frame.msg_type, MSG_NEW_TAB_RESPONSE);
        assert_eq!(frame.serial, 21);
    }

    #[test]
    fn new_tab_response_too_short() {
        assert!(NewTabResponse::decode(&[0; 7]).is_err());
    }

    #[test]
    fn close_tab_roundtrip() {
        let msg = CloseTab { tab_id: 5 };
        let decoded = CloseTab::decode(&msg.encode()).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn close_tab_too_short() {
        assert!(CloseTab::decode(&[0; 3]).is_err());
    }

    #[test]
    fn close_tab_response_frame() {
        let frame = CloseTabResponse.to_frame(31).unwrap();
        assert_eq!(frame.msg_type, MSG_CLOSE_TAB_RESPONSE);
        assert_eq!(frame.serial, 31);
        assert!(frame.payload.is_empty());
        assert!(CloseTabResponse::decode(&frame.payload).is_ok());
    }

    #[test]
    fn switch_tab_roundtrip() {
        let msg = SwitchTab { tab_id: 4 };
        let decoded = SwitchTab::decode(&msg.encode()).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn switch_tab_too_short() {
        assert!(SwitchTab::decode(&[0; 3]).is_err());
    }

    #[test]
    fn new_workspace_roundtrip() {
        let msg = NewWorkspace { name: "dev".into() };
        let encoded = msg.encode().unwrap();
        let decoded = NewWorkspace::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn new_workspace_empty_name_roundtrip() {
        let msg = NewWorkspace {
            name: String::new(),
        };
        let encoded = msg.encode().unwrap();
        assert_eq!(encoded.len(), 2);
        let decoded = NewWorkspace::decode(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn new_workspace_too_short() {
        // Name length prefix truncated.
        assert!(NewWorkspace::decode(&[0; 1]).is_err());
        // Length prefix claims more bytes than the payload holds.
        assert!(NewWorkspace::decode(&[5, 0, b'a']).is_err());
    }

    #[test]
    fn new_workspace_response_roundtrip_and_frame() {
        let resp = NewWorkspaceResponse {
            workspace_id: 1,
            tab_id: 2,
            pane_id: 9,
        };
        let decoded = NewWorkspaceResponse::decode(&resp.encode()).unwrap();
        assert_eq!(decoded, resp);
        let frame = resp.to_frame(41).unwrap();
        assert_eq!(frame.msg_type, MSG_NEW_WORKSPACE_RESPONSE);
        assert_eq!(frame.serial, 41);
    }

    #[test]
    fn new_workspace_response_too_short() {
        assert!(NewWorkspaceResponse::decode(&[0; 11]).is_err());
    }

    #[test]
    fn switch_workspace_roundtrip() {
        let msg = SwitchWorkspace { workspace_id: 6 };
        let decoded = SwitchWorkspace::decode(&msg.encode()).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn switch_workspace_too_short() {
        assert!(SwitchWorkspace::decode(&[0; 3]).is_err());
    }

    #[test]
    fn move_tab_roundtrip() {
        let msg = MoveTab {
            tab_id: 7,
            new_index: 2,
        };
        let decoded = MoveTab::decode(&msg.encode()).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn move_tab_too_short() {
        assert!(MoveTab::decode(&[0; 7]).is_err());
    }

    #[test]
    fn rename_tab_roundtrip_and_empty_name() {
        let msg = RenameTab {
            tab_id: 4,
            name: "build".into(),
        };
        let decoded = RenameTab::decode(&msg.encode().unwrap()).unwrap();
        assert_eq!(decoded, msg);
        // Empty name (clear-the-pin) round-trips.
        let cleared = RenameTab {
            tab_id: 4,
            name: String::new(),
        };
        let decoded = RenameTab::decode(&cleared.encode().unwrap()).unwrap();
        assert_eq!(decoded, cleared);
    }

    #[test]
    fn rename_tab_too_short() {
        // Missing the name length prefix after the tab id.
        assert!(RenameTab::decode(&[0; 3]).is_err());
        // Length prefix claims more bytes than present.
        assert!(RenameTab::decode(&[1, 0, 0, 0, 5, 0, b'a']).is_err());
    }

    #[test]
    fn rename_tab_name_utf8_enforced() {
        let mut encoded = RenameTab {
            tab_id: 1,
            name: "ok".into(),
        }
        .encode()
        .unwrap();
        // Corrupt the name bytes into invalid UTF-8.
        *encoded.last_mut().unwrap() = 0xFF;
        assert!(RenameTab::decode(&encoded).is_err());
    }

    #[test]
    fn rename_workspace_roundtrip() {
        let msg = RenameWorkspace {
            workspace_id: 2,
            name: "ops".into(),
        };
        let decoded = RenameWorkspace::decode(&msg.encode().unwrap()).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn rename_workspace_too_short() {
        assert!(RenameWorkspace::decode(&[0; 3]).is_err());
    }

    #[test]
    fn close_workspace_roundtrip() {
        let msg = CloseWorkspace { workspace_id: 6 };
        let decoded = CloseWorkspace::decode(&msg.encode()).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn close_workspace_too_short() {
        assert!(CloseWorkspace::decode(&[0; 3]).is_err());
    }

    #[test]
    fn close_workspace_response_frame() {
        let frame = CloseWorkspaceResponse.to_frame(52).unwrap();
        assert_eq!(frame.msg_type, MSG_CLOSE_WORKSPACE_RESPONSE);
        assert_eq!(frame.serial, 52);
        assert!(frame.payload.is_empty());
        assert!(CloseWorkspaceResponse::decode(&frame.payload).is_ok());
    }

    fn sample_tab_list() -> TabList {
        TabList {
            workspace_id: 3,
            workspace_name: "default".to_string(),
            active_tab: 7,
            tabs: vec![
                TabEntry {
                    tab_id: 0,
                    focused_pane: 0,
                    name: "vim ~/notes".to_string(),
                },
                TabEntry {
                    tab_id: 7,
                    focused_pane: 12,
                    name: String::new(),
                },
            ],
        }
    }

    #[test]
    fn tab_list_roundtrip_and_frame() {
        let msg = sample_tab_list();
        let decoded = TabList::decode(&msg.encode().unwrap()).unwrap();
        assert_eq!(decoded, msg);
        let frame = msg.to_frame(90).unwrap();
        assert_eq!(frame.msg_type, MSG_TAB_LIST);
        assert_eq!(frame.serial, 90);
    }

    #[test]
    fn tab_list_empty_roundtrip() {
        // The transient empty multiplexer state: no workspace yet.
        let msg = TabList {
            workspace_id: 0,
            workspace_name: String::new(),
            active_tab: 0,
            tabs: vec![],
        };
        let decoded = TabList::decode(&msg.encode().unwrap()).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn tab_list_truncated_errors() {
        let encoded = sample_tab_list().encode().unwrap();
        for len in [3, 5, encoded.len() - 1] {
            assert!(TabList::decode(&encoded[..len]).is_err(), "len {len}");
        }
    }

    #[test]
    fn tab_list_workspace_name_utf8_enforced() {
        let mut encoded = sample_tab_list().encode().unwrap();
        // workspace_name starts after workspace_id (4) + name len (2).
        encoded[6] = 0xFF;
        assert!(TabList::decode(&encoded).is_err());
    }

    #[test]
    fn tab_entry_name_utf8_enforced() {
        let mut encoded = TabEntry {
            tab_id: 1,
            focused_pane: 2,
            name: "ab".to_string(),
        }
        .encode()
        .unwrap();
        let name_start = encoded.len() - 2;
        encoded[name_start] = 0xFF;
        assert!(TabEntry::decode(&encoded).is_err());
    }
}
