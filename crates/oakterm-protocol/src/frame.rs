//! Wire frame header and codec per Spec-0001.

use bytes::{BufMut, BytesMut};
use std::io;
use tokio_util::codec::{Decoder, Encoder};

/// Protocol magic bytes: "OT" (0x4F54 big-endian).
pub const MAGIC: [u8; 2] = [0x4F, 0x54];

/// Maximum payload size: 16 MiB.
pub const MAX_PAYLOAD: u32 = 16 * 1024 * 1024;

/// Frame header size in bytes.
pub const HEADER_SIZE: usize = 13;

// Compile-time check: MAX_PAYLOAD + HEADER_SIZE must fit in usize (32-bit safety).
const _: () = assert!(MAX_PAYLOAD as u64 + HEADER_SIZE as u64 <= u32::MAX as u64);

/// A wire frame: 13-byte header + payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub flags: u8,
    pub msg_type: u16,
    pub serial: u32,
    pub payload: Vec<u8>,
}

impl Frame {
    /// Create a new frame. Returns an error if the payload exceeds `MAX_PAYLOAD`.
    ///
    /// # Errors
    /// Returns an error if `payload.len() > MAX_PAYLOAD`.
    pub fn new(msg_type: u16, serial: u32, payload: Vec<u8>) -> io::Result<Self> {
        if payload.len() > MAX_PAYLOAD as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "payload exceeds MAX_PAYLOAD",
            ));
        }
        Ok(Self {
            flags: 0,
            msg_type,
            serial,
            payload,
        })
    }

    /// Encode the frame header + payload into bytes.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)] // payload bounded by MAX_PAYLOAD in constructor
    pub fn encode_to_vec(&self) -> Vec<u8> {
        debug_assert!(self.payload.len() <= MAX_PAYLOAD as usize);
        let mut buf = Vec::with_capacity(HEADER_SIZE + self.payload.len());
        buf.extend_from_slice(&MAGIC);
        buf.push(self.flags);
        buf.extend_from_slice(&self.msg_type.to_le_bytes());
        buf.extend_from_slice(&self.serial.to_le_bytes());
        buf.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Decode a frame from a byte slice. Returns the frame and bytes consumed.
    ///
    /// # Errors
    /// Returns an error if the data is too short, magic doesn't match,
    /// or payload exceeds the maximum size.
    pub fn decode_from_slice(data: &[u8]) -> io::Result<(Self, usize)> {
        if data.len() < HEADER_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete frame header",
            ));
        }

        if data[0..2] != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid magic bytes",
            ));
        }

        let flags = data[2];
        let msg_type = u16::from_le_bytes([data[3], data[4]]);
        let serial = u32::from_le_bytes([data[5], data[6], data[7], data[8]]);
        let payload_length = u32::from_le_bytes([data[9], data[10], data[11], data[12]]);

        if payload_length > MAX_PAYLOAD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("payload too large: {payload_length} bytes"),
            ));
        }

        // Safe: payload_length <= MAX_PAYLOAD (16 MiB), fits in usize on all targets.
        let total = HEADER_SIZE + payload_length as usize;
        if data.len() < total {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete payload",
            ));
        }

        let payload = data[HEADER_SIZE..total].to_vec();

        Ok((
            Self {
                flags,
                msg_type,
                serial,
                payload,
            },
            total,
        ))
    }
}

/// Tokio codec for framed protocol I/O.
#[derive(Debug, Default)]
pub struct FrameCodec;

impl Decoder for FrameCodec {
    type Item = Frame;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> io::Result<Option<Frame>> {
        if src.len() < HEADER_SIZE {
            return Ok(None);
        }

        if src[0..2] != MAGIC {
            // Drain the buffer — this connection is irrecoverable.
            src.clear();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid magic bytes — connection must be closed",
            ));
        }

        let payload_length = u32::from_le_bytes([src[9], src[10], src[11], src[12]]) as usize;

        if payload_length > MAX_PAYLOAD as usize {
            src.clear();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "payload too large",
            ));
        }

        let total = HEADER_SIZE + payload_length;
        if src.len() < total {
            src.reserve(total - src.len());
            return Ok(None);
        }

        let header_bytes = src.split_to(HEADER_SIZE);
        let payload = src.split_to(payload_length).to_vec();

        let flags = header_bytes[2];
        let msg_type = u16::from_le_bytes([header_bytes[3], header_bytes[4]]);
        let serial = u32::from_le_bytes([
            header_bytes[5],
            header_bytes[6],
            header_bytes[7],
            header_bytes[8],
        ]);

        Ok(Some(Frame {
            flags,
            msg_type,
            serial,
            payload,
        }))
    }
}

impl Encoder<Frame> for FrameCodec {
    type Error = io::Error;

    #[allow(clippy::cast_possible_truncation)] // bounded by MAX_PAYLOAD check below
    fn encode(&mut self, frame: Frame, dst: &mut BytesMut) -> io::Result<()> {
        let payload_len = frame.payload.len();
        if payload_len > MAX_PAYLOAD as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "payload too large",
            ));
        }

        dst.reserve(HEADER_SIZE + payload_len);
        dst.put_slice(&MAGIC);
        dst.put_u8(frame.flags);
        dst.put_u16_le(frame.msg_type);
        dst.put_u32_le(frame.serial);
        dst.put_u32_le(payload_len as u32);
        dst.put_slice(&frame.payload);

        Ok(())
    }
}
