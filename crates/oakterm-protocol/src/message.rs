//! Protocol message types per Spec-0001.

use crate::frame::Frame;
use std::io;

// Infrastructure (0x00-0x09).
pub const MSG_CLIENT_HELLO: u16 = 0x01;
pub const MSG_SERVER_HELLO: u16 = 0x02;
pub const MSG_PING: u16 = 0x03;
pub const MSG_PONG: u16 = 0x04;
pub const MSG_ERROR: u16 = 0x05;
pub const MSG_SHUTDOWN: u16 = 0x06;

// GUI — input (0x64-0x6F).
pub const MSG_KEY_INPUT: u16 = 0x64;
pub const MSG_MOUSE_INPUT: u16 = 0x65;
pub const MSG_RESIZE: u16 = 0x66;
pub const MSG_DETACH: u16 = 0x67;

// GUI — rendering (0x70-0x7F).
pub const MSG_DIRTY_NOTIFY: u16 = 0x70;
pub const MSG_GET_RENDER_UPDATE: u16 = 0x71;
pub const MSG_RENDER_UPDATE: u16 = 0x72;
pub const MSG_GET_SCROLLBACK: u16 = 0x73;
pub const MSG_SCROLLBACK_DATA: u16 = 0x74;
pub const MSG_FIND_PROMPT: u16 = 0x75;
pub const MSG_PROMPT_POSITION: u16 = 0x76;

// GUI — search (0x77-0x7B).
pub const MSG_SEARCH_SCROLLBACK: u16 = 0x77;
pub const MSG_SEARCH_RESULTS: u16 = 0x78;
pub const MSG_SEARCH_NEXT: u16 = 0x79;
pub const MSG_SEARCH_PREV: u16 = 0x7A;
pub const MSG_SEARCH_CLOSE: u16 = 0x7B;

// GUI — notifications (0x80-0x8F).
pub const MSG_TITLE_CHANGED: u16 = 0x80;
pub const MSG_SET_CLIPBOARD: u16 = 0x81;
pub const MSG_BELL: u16 = 0x82;
pub const MSG_PANE_EXITED: u16 = 0x83;
pub const MSG_CONFIG_CHANGED: u16 = 0x84;

// GUI — pane management (0x90-0x9F).
pub const MSG_CREATE_PANE: u16 = 0x90;
pub const MSG_CREATE_PANE_RESPONSE: u16 = 0x91;
pub const MSG_CLOSE_PANE: u16 = 0x92;
pub const MSG_CLOSE_PANE_RESPONSE: u16 = 0x93;
pub const MSG_FOCUS_PANE: u16 = 0x94;
pub const MSG_LIST_PANES: u16 = 0x95;
pub const MSG_LIST_PANES_RESPONSE: u16 = 0x96;

// Control protocol (0xC8-0xDF).
pub const MSG_CTL_COMMAND: u16 = 0xC8;
pub const MSG_CTL_RESPONSE: u16 = 0xC9;

/// Client type sent in `ClientHello`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ClientType {
    Gui = 0,
    Control = 1,
    ThirdParty = 2,
}

impl TryFrom<u8> for ClientType {
    type Error = io::Error;
    fn try_from(v: u8) -> io::Result<Self> {
        match v {
            0 => Ok(Self::Gui),
            1 => Ok(Self::Control),
            2 => Ok(Self::ThirdParty),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown client type: {v}"),
            )),
        }
    }
}

/// Handshake status from `ServerHello`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HandshakeStatus {
    Accepted = 0,
    VersionMismatch = 1,
    AuthRejected = 2,
    ServerFull = 3,
}

impl TryFrom<u8> for HandshakeStatus {
    type Error = io::Error;
    fn try_from(v: u8) -> io::Result<Self> {
        match v {
            0 => Ok(Self::Accepted),
            1 => Ok(Self::VersionMismatch),
            2 => Ok(Self::AuthRejected),
            3 => Ok(Self::ServerFull),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown handshake status: {v}"),
            )),
        }
    }
}

/// Error codes for the Error message (0x05).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ErrorCode {
    UnknownPane = 1,
    InvalidMessage = 2,
    MalformedPayload = 3,
    InternalError = 4,
    PaneExited = 5,
    PermissionDenied = 6,
}

impl TryFrom<u32> for ErrorCode {
    type Error = io::Error;
    fn try_from(v: u32) -> io::Result<Self> {
        match v {
            1 => Ok(Self::UnknownPane),
            2 => Ok(Self::InvalidMessage),
            3 => Ok(Self::MalformedPayload),
            4 => Ok(Self::InternalError),
            5 => Ok(Self::PaneExited),
            6 => Ok(Self::PermissionDenied),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown error code: {v}"),
            )),
        }
    }
}

/// Shutdown reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ShutdownReason {
    Clean = 0,
    Crash = 1,
    Upgrade = 2,
}

impl TryFrom<u8> for ShutdownReason {
    type Error = io::Error;
    fn try_from(v: u8) -> io::Result<Self> {
        match v {
            0 => Ok(Self::Clean),
            1 => Ok(Self::Crash),
            2 => Ok(Self::Upgrade),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown shutdown reason: {v}"),
            )),
        }
    }
}

/// Encode a length-prefixed UTF-8 string (u16 LE length + bytes).
fn encode_str(buf: &mut Vec<u8>, s: &str) -> io::Result<()> {
    let bytes = s.as_bytes();
    let len: u16 = bytes.len().try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("string too long: {} bytes (max {})", bytes.len(), u16::MAX),
        )
    })?;
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(bytes);
    Ok(())
}

/// Decode a length-prefixed UTF-8 string from a byte slice at an offset.
/// Returns the string and bytes consumed (2 + string length).
fn decode_str(data: &[u8], offset: usize, field: &str) -> io::Result<(String, usize)> {
    if data.len() < offset + 2 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("{field} length truncated"),
        ));
    }
    let len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
    if data.len() < offset + 2 + len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("{field} data truncated"),
        ));
    }
    let s = String::from_utf8(data[offset + 2..offset + 2 + len].to_vec())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("{field} not UTF-8")))?;
    Ok((s, 2 + len))
}

/// Client handshake message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientHello {
    pub protocol_version_major: u16,
    pub protocol_version_minor: u16,
    pub client_type: ClientType,
    pub client_name: String,
}

impl ClientHello {
    pub const VERSION_MAJOR: u16 = 1;
    pub const VERSION_MINOR: u16 = 0;

    /// # Errors
    /// Returns an error if the client name exceeds u16 max length.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(7 + self.client_name.len());
        buf.extend_from_slice(&self.protocol_version_major.to_le_bytes());
        buf.extend_from_slice(&self.protocol_version_minor.to_le_bytes());
        buf.push(self.client_type as u8);
        encode_str(&mut buf, &self.client_name)?;
        Ok(buf)
    }

    /// # Errors
    /// Returns an error if the payload is malformed.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 5 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "ClientHello too short",
            ));
        }
        let major = u16::from_le_bytes([data[0], data[1]]);
        let minor = u16::from_le_bytes([data[2], data[3]]);
        let client_type = ClientType::try_from(data[4])?;
        let (name, _) = decode_str(data, 5, "client_name")?;

        Ok(Self {
            protocol_version_major: major,
            protocol_version_minor: minor,
            client_type,
            client_name: name,
        })
    }

    /// # Errors
    /// Returns an error if encoding fails.
    pub fn to_frame(&self, serial: u32) -> io::Result<Frame> {
        Frame::new(MSG_CLIENT_HELLO, serial, self.encode()?)
    }
}

/// Server handshake response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerHello {
    pub status: HandshakeStatus,
    pub protocol_version_major: u16,
    pub protocol_version_minor: u16,
    pub server_version: String,
}

impl ServerHello {
    /// # Errors
    /// Returns an error if the server version exceeds u16 max length.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(7 + self.server_version.len());
        buf.push(self.status as u8);
        buf.extend_from_slice(&self.protocol_version_major.to_le_bytes());
        buf.extend_from_slice(&self.protocol_version_minor.to_le_bytes());
        encode_str(&mut buf, &self.server_version)?;
        Ok(buf)
    }

    /// # Errors
    /// Returns an error if the payload is malformed.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 5 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "ServerHello too short",
            ));
        }
        let status = HandshakeStatus::try_from(data[0])?;
        let major = u16::from_le_bytes([data[1], data[2]]);
        let minor = u16::from_le_bytes([data[3], data[4]]);
        let (version, _) = decode_str(data, 5, "server_version")?;

        Ok(Self {
            status,
            protocol_version_major: major,
            protocol_version_minor: minor,
            server_version: version,
        })
    }

    /// # Errors
    /// Returns an error if encoding fails.
    pub fn to_frame(&self, serial: u32) -> io::Result<Frame> {
        Frame::new(MSG_SERVER_HELLO, serial, self.encode()?)
    }
}

/// Error response message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorMessage {
    pub code: u32,
    pub message: String,
}

impl ErrorMessage {
    /// # Errors
    /// Returns an error if the message exceeds u16 max length.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(6 + self.message.len());
        buf.extend_from_slice(&self.code.to_le_bytes());
        encode_str(&mut buf, &self.message)?;
        Ok(buf)
    }

    /// # Errors
    /// Returns an error if the payload is malformed.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Error message too short",
            ));
        }
        let code = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let (message, _) = decode_str(data, 4, "error_message")?;

        Ok(Self { code, message })
    }

    /// # Errors
    /// Returns an error if encoding fails.
    pub fn to_frame(&self, serial: u32) -> io::Result<Frame> {
        Frame::new(MSG_ERROR, serial, self.encode()?)
    }
}

/// `TitleChanged` (0x80): daemon notifies GUI of title change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleChanged {
    pub pane_id: u32,
    pub title: String,
}

impl TitleChanged {
    /// # Errors
    /// Returns an error if the title exceeds u16 max length.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(6 + self.title.len());
        buf.extend_from_slice(&self.pane_id.to_le_bytes());
        encode_str(&mut buf, &self.title)?;
        Ok(buf)
    }

    /// # Errors
    /// Returns an error if the payload is malformed.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "TitleChanged too short",
            ));
        }
        let pane_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let (title, _) = decode_str(data, 4, "title")?;
        Ok(Self { pane_id, title })
    }

    /// Wrap as a push frame (serial 0).
    ///
    /// # Errors
    /// Returns an error if encoding fails.
    pub fn to_frame(&self) -> io::Result<Frame> {
        Frame::new(MSG_TITLE_CHANGED, 0, self.encode()?)
    }
}

/// `PaneExited` (0x83): daemon notifies GUI that a pane's process exited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneExited {
    pub pane_id: u32,
    pub exit_code: i32,
}

impl PaneExited {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8);
        buf.extend_from_slice(&self.pane_id.to_le_bytes());
        buf.extend_from_slice(&self.exit_code.to_le_bytes());
        buf
    }

    /// # Errors
    /// Returns an error if the payload is too short.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 8 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "PaneExited too short",
            ));
        }
        Ok(Self {
            pane_id: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            exit_code: i32::from_le_bytes([data[4], data[5], data[6], data[7]]),
        })
    }

    /// Wrap as a push frame (serial 0).
    ///
    /// # Errors
    /// Returns an error if frame construction fails.
    pub fn to_frame(&self) -> io::Result<Frame> {
        Frame::new(MSG_PANE_EXITED, 0, self.encode())
    }
}

/// Bell (0x82): daemon notifies GUI of bell event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bell {
    pub pane_id: u32,
}

impl Bell {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.pane_id.to_le_bytes().to_vec()
    }

    /// # Errors
    /// Returns an error if the payload is too short.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Bell too short",
            ));
        }
        Ok(Self {
            pane_id: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        })
    }

    /// Wrap as a push frame (serial 0).
    ///
    /// # Errors
    /// Returns an error if frame construction fails.
    pub fn to_frame(&self) -> io::Result<Frame> {
        Frame::new(MSG_BELL, 0, self.encode())
    }
}

/// `GetScrollback` (0x73): client requests scrollback rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetScrollback {
    pub pane_id: u32,
    /// Negative offset from the viewport top. -1 = most recent scrollback row.
    pub start_row: i64,
    pub count: u32,
}

impl GetScrollback {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(16);
        buf.extend_from_slice(&self.pane_id.to_le_bytes());
        buf.extend_from_slice(&self.start_row.to_le_bytes());
        buf.extend_from_slice(&self.count.to_le_bytes());
        buf
    }

    /// # Errors
    /// Returns an error if the payload is too short.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 16 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "GetScrollback too short",
            ));
        }
        Ok(Self {
            pane_id: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            start_row: i64::from_le_bytes([
                data[4], data[5], data[6], data[7], data[8], data[9], data[10], data[11],
            ]),
            count: u32::from_le_bytes([data[12], data[13], data[14], data[15]]),
        })
    }
}

/// `ScrollbackData` (0x74): daemon responds with scrollback rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrollbackData {
    pub pane_id: u32,
    pub start_row: i64,
    pub has_more: bool,
    /// Total number of rows currently in the daemon's hot scrollback buffer.
    /// Lets the client clamp `viewport_offset` to a valid range.
    pub total_rows: u32,
    pub rows: Vec<crate::render::DirtyRow>,
}

impl ScrollbackData {
    /// # Errors
    /// Returns an error if row count exceeds u32 or row encoding fails.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let row_count: u32 =
            self.rows.len().try_into().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "too many scrollback rows")
            })?;
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.pane_id.to_le_bytes());
        buf.extend_from_slice(&self.start_row.to_le_bytes());
        buf.push(u8::from(self.has_more));
        buf.extend_from_slice(&self.total_rows.to_le_bytes());
        buf.extend_from_slice(&row_count.to_le_bytes());
        for row in &self.rows {
            buf.extend_from_slice(&row.encode()?);
        }
        Ok(buf)
    }

    /// # Errors
    /// Returns an error if the payload is malformed.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 21 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "ScrollbackData too short",
            ));
        }
        let pane_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let start_row = i64::from_le_bytes([
            data[4], data[5], data[6], data[7], data[8], data[9], data[10], data[11],
        ]);
        let has_more = data[12] != 0;
        let total_rows = u32::from_le_bytes([data[13], data[14], data[15], data[16]]);
        let row_count_raw = u32::from_le_bytes([data[17], data[18], data[19], data[20]]);
        // Cap allocation to prevent OOM from malicious wire data.
        if row_count_raw > 10_000 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("ScrollbackData row_count too large: {row_count_raw}"),
            ));
        }
        let row_count = row_count_raw as usize;

        let mut offset = 21;
        let mut rows = Vec::with_capacity(row_count);
        for _ in 0..row_count {
            let (row, consumed) = crate::render::DirtyRow::decode(&data[offset..])?;
            rows.push(row);
            offset += consumed;
        }

        Ok(Self {
            pane_id,
            start_row,
            has_more,
            total_rows,
            rows,
        })
    }
}

/// Direction for prompt search. Wire encoding: 0xFF (-1) = older, 0x01 = newer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SearchDirection {
    /// Search toward older rows (up / toward index 0).
    Older = 0xFF,
    /// Search toward newer rows (down / toward live view).
    Newer = 0x01,
}

impl TryFrom<u8> for SearchDirection {
    type Error = io::Error;
    fn try_from(v: u8) -> io::Result<Self> {
        match v {
            0xFF => Ok(Self::Older),
            0x01 => Ok(Self::Newer),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown search direction: 0x{v:02X}"),
            )),
        }
    }
}

/// `FindPrompt` (0x75): client requests the position of the next/previous
/// shell prompt mark (`SemanticMark::PromptStart`) relative to the current
/// scrollback offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindPrompt {
    pub pane_id: u32,
    /// Negative offset from the viewport bottom, same semantics as
    /// `GetScrollback.start_row`.
    pub from_offset: i64,
    pub direction: SearchDirection,
}

impl FindPrompt {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(13);
        buf.extend_from_slice(&self.pane_id.to_le_bytes());
        buf.extend_from_slice(&self.from_offset.to_le_bytes());
        buf.push(self.direction as u8);
        buf
    }

    /// # Errors
    /// Returns an error if the payload is too short or direction is invalid.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 13 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "FindPrompt too short",
            ));
        }
        Ok(Self {
            pane_id: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            from_offset: i64::from_le_bytes([
                data[4], data[5], data[6], data[7], data[8], data[9], data[10], data[11],
            ]),
            direction: SearchDirection::try_from(data[12])?,
        })
    }
}

/// `PromptPosition` (0x76): daemon responds with the scrollback offset of
/// the found prompt. `offset` is `None` when no prompt exists in the search
/// direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptPosition {
    pub pane_id: u32,
    /// Negative offset for the found prompt (same coordinate space as
    /// `FindPrompt.from_offset`), or `None` if not found.
    pub offset: Option<i64>,
}

impl PromptPosition {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(13);
        buf.extend_from_slice(&self.pane_id.to_le_bytes());
        if let Some(v) = self.offset {
            buf.extend_from_slice(&v.to_le_bytes());
            buf.push(1);
        } else {
            buf.extend_from_slice(&0_i64.to_le_bytes());
            buf.push(0);
        }
        buf
    }

    /// # Errors
    /// Returns an error if the payload is too short.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 13 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "PromptPosition too short",
            ));
        }
        let raw_offset = i64::from_le_bytes([
            data[4], data[5], data[6], data[7], data[8], data[9], data[10], data[11],
        ]);
        let found = data[12] != 0;
        Ok(Self {
            pane_id: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            offset: if found { Some(raw_offset) } else { None },
        })
    }

    /// Wrap as a response frame.
    ///
    /// # Errors
    /// Returns an error if frame construction fails.
    pub fn to_frame(&self, serial: u32) -> io::Result<Frame> {
        Frame::new(MSG_PROMPT_POSITION, serial, self.encode())
    }
}

// ---------------------------------------------------------------------------
// Search messages
// ---------------------------------------------------------------------------

/// Search mode flags packed into a single byte.
///
/// Bit 0: regex mode. Bit 1: force case-sensitive (overrides smart case).
/// When both bits are 0, smart case is used for literal search.
/// When both are set, regex takes precedence; case sensitivity is
/// controlled by the pattern itself (e.g. `(?i)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchFlags(pub u8);

impl SearchFlags {
    pub const REGEX: u8 = 1 << 0;
    pub const CASE_SENSITIVE: u8 = 1 << 1;

    #[must_use]
    pub const fn regex(self) -> bool {
        self.0 & Self::REGEX != 0
    }

    #[must_use]
    pub const fn case_sensitive(self) -> bool {
        self.0 & Self::CASE_SENSITIVE != 0
    }
}

/// `SearchScrollback` (0x77): client requests a scrollback search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchScrollback {
    pub pane_id: u32,
    pub flags: SearchFlags,
    pub query: String,
}

impl SearchScrollback {
    /// # Errors
    /// Returns an error if the query string is too long.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(7 + self.query.len());
        buf.extend_from_slice(&self.pane_id.to_le_bytes());
        buf.push(self.flags.0);
        encode_str(&mut buf, &self.query)?;
        Ok(buf)
    }

    /// # Errors
    /// Returns an error if the payload is malformed.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 5 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "SearchScrollback too short",
            ));
        }
        let pane_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let flags = SearchFlags(data[4]);
        let (query, _) = decode_str(data, 5, "search query")?;
        Ok(Self {
            pane_id,
            flags,
            query,
        })
    }
}

/// A match visible in the current viewport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleMatch {
    /// Viewport row (0 = top of viewport).
    pub row: u16,
    /// Column of match start.
    pub col_start: u16,
    /// Column of match end (exclusive).
    pub col_end: u16,
    /// Whether this is the currently active (selected) match.
    pub is_active: bool,
}

/// `SearchResults` (0x78): daemon responds with search results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResults {
    pub pane_id: u32,
    pub total_matches: u32,
    /// Index of the active match, or `None` if no matches.
    pub active_index: Option<u32>,
    /// Scroll offset to show the active match (negative = scrolled up).
    pub active_row_offset: i64,
    /// True if the search hit the match cap and stopped early.
    pub capped: bool,
    pub visible_matches: Vec<VisibleMatch>,
}

impl SearchResults {
    /// # Errors
    ///
    /// Returns an error if `visible_matches` exceeds `u16::MAX` entries.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let count: u16 = self.visible_matches.len().try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "visible_matches exceeds u16")
        })?;
        let mut buf = Vec::with_capacity(23 + self.visible_matches.len() * 7);
        buf.extend_from_slice(&self.pane_id.to_le_bytes());
        buf.extend_from_slice(&self.total_matches.to_le_bytes());
        buf.extend_from_slice(&self.active_index.unwrap_or(u32::MAX).to_le_bytes());
        buf.extend_from_slice(&self.active_row_offset.to_le_bytes());
        buf.push(u8::from(self.capped));
        buf.extend_from_slice(&count.to_le_bytes());
        for m in &self.visible_matches {
            buf.extend_from_slice(&m.row.to_le_bytes());
            buf.extend_from_slice(&m.col_start.to_le_bytes());
            buf.extend_from_slice(&m.col_end.to_le_bytes());
            buf.push(u8::from(m.is_active));
        }
        Ok(buf)
    }

    /// # Errors
    /// Returns an error if the payload is malformed.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 23 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "SearchResults too short",
            ));
        }
        let pane_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let total_matches = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let raw_active = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);
        let active_index = if raw_active == u32::MAX {
            None
        } else {
            Some(raw_active)
        };
        let active_row_offset = i64::from_le_bytes([
            data[12], data[13], data[14], data[15], data[16], data[17], data[18], data[19],
        ]);
        let capped = data[20] != 0;
        let match_count = u16::from_le_bytes([data[21], data[22]]) as usize;
        let mut visible_matches = Vec::with_capacity(match_count);
        let mut offset = 23;
        for _ in 0..match_count {
            if data.len() < offset + 7 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "SearchResults visible match truncated",
                ));
            }
            visible_matches.push(VisibleMatch {
                row: u16::from_le_bytes([data[offset], data[offset + 1]]),
                col_start: u16::from_le_bytes([data[offset + 2], data[offset + 3]]),
                col_end: u16::from_le_bytes([data[offset + 4], data[offset + 5]]),
                is_active: data[offset + 6] != 0,
            });
            offset += 7;
        }
        Ok(Self {
            pane_id,
            total_matches,
            active_index,
            active_row_offset,
            capped,
            visible_matches,
        })
    }

    /// Wrap as a response frame.
    ///
    /// # Errors
    /// Returns an error if frame construction fails.
    pub fn to_frame(&self, serial: u32) -> io::Result<Frame> {
        Frame::new(MSG_SEARCH_RESULTS, serial, self.encode()?)
    }
}

/// Payload for `SearchNext` (0x79) and `SearchPrev` (0x7A).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchNav {
    pub pane_id: u32,
}

impl SearchNav {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.pane_id.to_le_bytes().to_vec()
    }

    /// # Errors
    /// Returns an error if the payload is too short.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "SearchNav too short",
            ));
        }
        Ok(Self {
            pane_id: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        })
    }
}

// --- Pane management messages (0x90-0x93) ---

/// `CreatePane` (0x90): client requests a new pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatePane {
    /// Shell command to run. Empty = default shell.
    pub command: String,
    /// Working directory. Empty = inherit from daemon.
    pub cwd: String,
}

impl CreatePane {
    /// # Errors
    /// Returns an error if command or cwd exceed u16 length.
    #[allow(clippy::similar_names)]
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let cmd = self.command.as_bytes();
        let cwd = self.cwd.as_bytes();
        let cmd_len: u16 = cmd
            .len()
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "command too long"))?;
        let cwd_len: u16 = cwd
            .len()
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "cwd too long"))?;
        let mut buf = Vec::with_capacity(4 + cmd.len() + cwd.len());
        buf.extend_from_slice(&cmd_len.to_le_bytes());
        buf.extend_from_slice(cmd);
        buf.extend_from_slice(&cwd_len.to_le_bytes());
        buf.extend_from_slice(cwd);
        Ok(buf)
    }

    /// # Errors
    /// Returns an error if the payload is malformed.
    #[allow(clippy::similar_names)]
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "CreatePane too short",
            ));
        }
        let cmd_len = u16::from_le_bytes([data[0], data[1]]) as usize;
        if data.len() < 2 + cmd_len + 2 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "CreatePane command truncated",
            ));
        }
        let command = String::from_utf8(data[2..2 + cmd_len].to_vec())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("command: {e}")))?;
        let off = 2 + cmd_len;
        let cwd_len = u16::from_le_bytes([data[off], data[off + 1]]) as usize;
        if data.len() < off + 2 + cwd_len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "CreatePane cwd truncated",
            ));
        }
        let cwd = String::from_utf8(data[off + 2..off + 2 + cwd_len].to_vec())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("cwd: {e}")))?;
        Ok(Self { command, cwd })
    }
}

/// `CreatePaneResponse` (0x91): daemon returns the new pane's ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatePaneResponse {
    pub pane_id: u32,
}

impl CreatePaneResponse {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.pane_id.to_le_bytes().to_vec()
    }

    /// # Errors
    /// Returns an error if the payload is too short.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "CreatePaneResponse too short",
            ));
        }
        Ok(Self {
            pane_id: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        })
    }

    /// Wrap as a response frame.
    ///
    /// # Errors
    /// Returns an error if frame construction fails.
    pub fn to_frame(&self, serial: u32) -> io::Result<Frame> {
        Frame::new(MSG_CREATE_PANE_RESPONSE, serial, self.encode())
    }
}

/// `ClosePane` (0x92): client requests pane closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosePane {
    pub pane_id: u32,
}

impl ClosePane {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.pane_id.to_le_bytes().to_vec()
    }

    /// # Errors
    /// Returns an error if the payload is too short.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "ClosePane too short",
            ));
        }
        Ok(Self {
            pane_id: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        })
    }
}

/// `FocusPane` (0x94): client sets the focused pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusPane {
    pub pane_id: u32,
}

impl FocusPane {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.pane_id.to_le_bytes().to_vec()
    }

    /// # Errors
    /// Returns an error if the payload is too short.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "FocusPane too short",
            ));
        }
        Ok(Self {
            pane_id: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        })
    }
}

/// Per-pane metadata returned by `ListPanesResponse`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneInfo {
    pub pane_id: u32,
    pub title: String,
    pub cols: u16,
    pub rows: u16,
    pub pid: u32,
    pub exit_code: i32,
    pub cwd: String,
}

impl PaneInfo {
    /// # Errors
    /// Returns an error if strings exceed u16 length.
    #[allow(clippy::similar_names)]
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let title_bytes = self.title.as_bytes();
        let cwd_bytes = self.cwd.as_bytes();
        let tlen: u16 = title_bytes
            .len()
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "title too long"))?;
        let cwdlen: u16 = cwd_bytes
            .len()
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "cwd too long"))?;
        let mut buf = Vec::with_capacity(18 + title_bytes.len() + cwd_bytes.len());
        buf.extend_from_slice(&self.pane_id.to_le_bytes());
        buf.extend_from_slice(&tlen.to_le_bytes());
        buf.extend_from_slice(title_bytes);
        buf.extend_from_slice(&self.cols.to_le_bytes());
        buf.extend_from_slice(&self.rows.to_le_bytes());
        buf.extend_from_slice(&self.pid.to_le_bytes());
        buf.extend_from_slice(&self.exit_code.to_le_bytes());
        buf.extend_from_slice(&cwdlen.to_le_bytes());
        buf.extend_from_slice(cwd_bytes);
        Ok(buf)
    }

    /// Decode a `PaneInfo` from `data`, returning the struct and bytes consumed.
    ///
    /// # Errors
    /// Returns an error if the payload is malformed.
    #[allow(clippy::similar_names)]
    pub fn decode(data: &[u8]) -> io::Result<(Self, usize)> {
        if data.len() < 6 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "PaneInfo too short",
            ));
        }
        let pane_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let tlen = u16::from_le_bytes([data[4], data[5]]) as usize;
        let fixed = 6 + tlen;
        if data.len() < fixed + 12 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "PaneInfo truncated",
            ));
        }
        let title = String::from_utf8(data[6..6 + tlen].to_vec())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("title: {e}")))?;
        let cols = u16::from_le_bytes([data[fixed], data[fixed + 1]]);
        let rows = u16::from_le_bytes([data[fixed + 2], data[fixed + 3]]);
        let pid = u32::from_le_bytes([
            data[fixed + 4],
            data[fixed + 5],
            data[fixed + 6],
            data[fixed + 7],
        ]);
        let exit_code = i32::from_le_bytes([
            data[fixed + 8],
            data[fixed + 9],
            data[fixed + 10],
            data[fixed + 11],
        ]);
        let cwd_off = fixed + 12;
        if data.len() < cwd_off + 2 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "PaneInfo cwd length truncated",
            ));
        }
        let cwdlen = u16::from_le_bytes([data[cwd_off], data[cwd_off + 1]]) as usize;
        let cwd_start = cwd_off + 2;
        if data.len() < cwd_start + cwdlen {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "PaneInfo cwd truncated",
            ));
        }
        let cwd = String::from_utf8(data[cwd_start..cwd_start + cwdlen].to_vec())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("cwd: {e}")))?;
        let total = cwd_start + cwdlen;
        Ok((
            Self {
                pane_id,
                title,
                cols,
                rows,
                pid,
                exit_code,
                cwd,
            },
            total,
        ))
    }
}

/// `ListPanesResponse` (0x96): daemon returns all pane metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListPanesResponse {
    pub panes: Vec<PaneInfo>,
}

impl ListPanesResponse {
    /// # Errors
    /// Returns an error if encoding fails.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let count: u16 = self
            .panes
            .len()
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many panes"))?;
        let mut buf = Vec::new();
        buf.extend_from_slice(&count.to_le_bytes());
        for pane in &self.panes {
            buf.extend_from_slice(&pane.encode()?);
        }
        Ok(buf)
    }

    /// # Errors
    /// Returns an error if the payload is malformed.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 2 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "ListPanesResponse too short",
            ));
        }
        let count = u16::from_le_bytes([data[0], data[1]]) as usize;
        let mut panes = Vec::with_capacity(count);
        let mut offset = 2;
        for _ in 0..count {
            let (info, consumed) = PaneInfo::decode(&data[offset..])?;
            panes.push(info);
            offset += consumed;
        }
        Ok(Self { panes })
    }

    /// Wrap as a response frame.
    ///
    /// # Errors
    /// Returns an error if frame construction fails.
    pub fn to_frame(&self, serial: u32) -> io::Result<Frame> {
        Frame::new(MSG_LIST_PANES_RESPONSE, serial, self.encode()?)
    }
}
