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
