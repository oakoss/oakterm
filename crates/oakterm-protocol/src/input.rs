//! GUI input protocol messages: `KeyInput`, `Resize`, `Detach`.
//! Wire formats per Spec-0001 section "GUI Protocol — Input (0x64-0x6F)".

use crate::frame::Frame;
use crate::message::{MSG_DETACH, MSG_KEY_INPUT, MSG_RESIZE};
use std::io;

/// `KeyInput` (0x64): client sends keyboard input to a pane.
/// Payload: `pane_id: u32` + `key_data_len: u16` + `key_data: bytes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInput {
    pub pane_id: u32,
    pub key_data: Vec<u8>,
}

impl KeyInput {
    /// # Errors
    /// Returns an error if `key_data` exceeds `u16::MAX` bytes.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let len: u16 = self.key_data.len().try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "key_data too long: {} bytes (max {})",
                    self.key_data.len(),
                    u16::MAX
                ),
            )
        })?;
        let mut buf = Vec::with_capacity(6 + self.key_data.len());
        buf.extend_from_slice(&self.pane_id.to_le_bytes());
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(&self.key_data);
        Ok(buf)
    }

    /// # Errors
    /// Returns an error if the payload is malformed.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 6 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "KeyInput too short",
            ));
        }
        let pane_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let key_data_len = usize::from(u16::from_le_bytes([data[4], data[5]]));
        if data.len() < 6 + key_data_len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "KeyInput key_data truncated",
            ));
        }
        Ok(Self {
            pane_id,
            key_data: data[6..6 + key_data_len].to_vec(),
        })
    }

    /// Wrap as a push frame (serial 0).
    ///
    /// # Errors
    /// Returns an error if encoding fails.
    pub fn to_frame(&self) -> io::Result<Frame> {
        Frame::new(MSG_KEY_INPUT, 0, self.encode()?)
    }
}

/// `Resize` (0x66): client requests pane resize.
/// Payload: `pane_id: u32` + `cols: u16` + `rows: u16` + `pixel_width: u16` + `pixel_height: u16`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resize {
    pub pane_id: u32,
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl Resize {
    /// Encoded size is always 12 bytes.
    pub const SIZE: usize = 12;

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(Self::SIZE);
        buf.extend_from_slice(&self.pane_id.to_le_bytes());
        buf.extend_from_slice(&self.cols.to_le_bytes());
        buf.extend_from_slice(&self.rows.to_le_bytes());
        buf.extend_from_slice(&self.pixel_width.to_le_bytes());
        buf.extend_from_slice(&self.pixel_height.to_le_bytes());
        buf
    }

    /// # Errors
    /// Returns an error if the payload is too short.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < Self::SIZE {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Resize too short",
            ));
        }
        Ok(Self {
            pane_id: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            cols: u16::from_le_bytes([data[4], data[5]]),
            rows: u16::from_le_bytes([data[6], data[7]]),
            pixel_width: u16::from_le_bytes([data[8], data[9]]),
            pixel_height: u16::from_le_bytes([data[10], data[11]]),
        })
    }

    /// Wrap as a push frame (serial 0).
    ///
    /// # Errors
    /// Returns an error if frame construction fails.
    pub fn to_frame(&self) -> io::Result<Frame> {
        Frame::new(MSG_RESIZE, 0, self.encode())
    }
}

/// `Detach` (0x67): client is disconnecting cleanly. Empty payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detach;

impl Detach {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        vec![]
    }

    /// Decode from a payload. Always succeeds; payload is ignored.
    ///
    /// # Errors
    /// This function does not return errors.
    pub fn decode(_data: &[u8]) -> io::Result<Self> {
        Ok(Self)
    }

    /// Wrap as a push frame (serial 0).
    ///
    /// # Errors
    /// Returns an error if frame construction fails.
    pub fn to_frame(&self) -> io::Result<Frame> {
        Frame::new(MSG_DETACH, 0, self.encode())
    }
}
