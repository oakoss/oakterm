//! Render protocol messages: `DirtyNotify`, `GetRenderUpdate`, `RenderUpdate`.
//! Wire formats per Spec-0001.

use std::io;

/// `DirtyNotify` (0x70): daemon signals that pane content has changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyNotify {
    pub pane_id: u32,
}

impl DirtyNotify {
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
                "DirtyNotify too short",
            ));
        }
        Ok(Self {
            pane_id: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        })
    }
}

/// `GetRenderUpdate` (0x71): GUI requests dirty rows since a sequence number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetRenderUpdate {
    pub pane_id: u32,
    pub since_seqno: u64,
}

impl GetRenderUpdate {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(12);
        buf.extend_from_slice(&self.pane_id.to_le_bytes());
        buf.extend_from_slice(&self.since_seqno.to_le_bytes());
        buf
    }

    /// # Errors
    /// Returns an error if the payload is too short.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 12 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "GetRenderUpdate too short",
            ));
        }
        Ok(Self {
            pane_id: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            since_seqno: u64::from_le_bytes([
                data[4], data[5], data[6], data[7], data[8], data[9], data[10], data[11],
            ]),
        })
    }
}

/// Wire representation of a terminal cell (16 bytes fixed + variable extra).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireCell {
    pub codepoint: u32,
    pub fg_r: u8,
    pub fg_g: u8,
    pub fg_b: u8,
    pub fg_type: u8,
    pub bg_r: u8,
    pub bg_g: u8,
    pub bg_b: u8,
    pub bg_type: u8,
    pub flags: u16,
    pub extra: Vec<u8>,
}

impl WireCell {
    /// Fixed size of a wire cell (excluding variable extra data).
    pub const FIXED_SIZE: usize = 16;

    /// # Errors
    /// Returns an error if extra data exceeds u16 max length.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let extra_len: u16 = self.extra.len().try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "cell extra data exceeds u16")
        })?;
        let mut buf = Vec::with_capacity(Self::FIXED_SIZE + self.extra.len());
        buf.extend_from_slice(&self.codepoint.to_le_bytes());
        buf.push(self.fg_r);
        buf.push(self.fg_g);
        buf.push(self.fg_b);
        buf.push(self.fg_type);
        buf.push(self.bg_r);
        buf.push(self.bg_g);
        buf.push(self.bg_b);
        buf.push(self.bg_type);
        buf.extend_from_slice(&self.flags.to_le_bytes());
        buf.extend_from_slice(&extra_len.to_le_bytes());
        buf.extend_from_slice(&self.extra);
        Ok(buf)
    }

    /// Decode a wire cell from a byte slice. Returns the cell and bytes consumed.
    ///
    /// # Errors
    /// Returns an error if the data is too short.
    pub fn decode(data: &[u8]) -> io::Result<(Self, usize)> {
        if data.len() < Self::FIXED_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "WireCell too short",
            ));
        }
        let codepoint = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let flags = u16::from_le_bytes([data[12], data[13]]);
        let extra_len = u16::from_le_bytes([data[14], data[15]]) as usize;

        let total = Self::FIXED_SIZE + extra_len;
        if data.len() < total {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "WireCell extra data truncated",
            ));
        }

        Ok((
            Self {
                codepoint,
                fg_r: data[4],
                fg_g: data[5],
                fg_b: data[6],
                fg_type: data[7],
                bg_r: data[8],
                bg_g: data[9],
                bg_b: data[10],
                bg_type: data[11],
                flags,
                extra: data[Self::FIXED_SIZE..total].to_vec(),
            },
            total,
        ))
    }
}

/// A single dirty row in a `RenderUpdate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyRow {
    pub row_index: u16,
    pub cells: Vec<WireCell>,
    pub semantic_mark: u8,
    pub mark_metadata: Vec<u8>,
}

impl DirtyRow {
    /// # Errors
    /// Returns an error if cell count or metadata length exceeds u16.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let cell_count: u16 =
            self.cells.len().try_into().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "too many cells for u16")
            })?;
        let meta_len: u16 = self.mark_metadata.len().try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "mark_metadata exceeds u16")
        })?;
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.row_index.to_le_bytes());
        buf.extend_from_slice(&cell_count.to_le_bytes());
        for cell in &self.cells {
            buf.extend_from_slice(&cell.encode()?);
        }
        buf.push(self.semantic_mark);
        buf.extend_from_slice(&meta_len.to_le_bytes());
        buf.extend_from_slice(&self.mark_metadata);
        Ok(buf)
    }

    /// # Errors
    /// Returns an error if the data is malformed.
    pub fn decode(data: &[u8]) -> io::Result<(Self, usize)> {
        if data.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "DirtyRow too short",
            ));
        }
        let row_index = u16::from_le_bytes([data[0], data[1]]);
        let cell_count = u16::from_le_bytes([data[2], data[3]]) as usize;

        let mut offset = 4;
        let mut cells = Vec::with_capacity(cell_count);
        for _ in 0..cell_count {
            let (cell, consumed) = WireCell::decode(&data[offset..])?;
            cells.push(cell);
            offset += consumed;
        }

        if data.len() < offset + 3 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "DirtyRow metadata truncated",
            ));
        }
        let semantic_mark = data[offset];
        offset += 1;
        let meta_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2;

        if data.len() < offset + meta_len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "DirtyRow mark_metadata truncated",
            ));
        }
        let mark_metadata = data[offset..offset + meta_len].to_vec();
        offset += meta_len;

        Ok((
            Self {
                row_index,
                cells,
                semantic_mark,
                mark_metadata,
            },
            offset,
        ))
    }
}

/// `RenderUpdate` (0x72): daemon responds with dirty rows and cursor state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderUpdate {
    pub pane_id: u32,
    pub seqno: u64,
    pub cursor_x: u16,
    pub cursor_y: u16,
    pub cursor_style: u8,
    pub cursor_visible: bool,
    /// Dynamic background color (from OSC 11 or default).
    pub bg_r: u8,
    pub bg_g: u8,
    pub bg_b: u8,
    /// Whether the terminal has DECSET 2004 (bracketed paste) active.
    pub bracketed_paste: bool,
    pub dirty_rows: Vec<DirtyRow>,
}

impl RenderUpdate {
    /// # Errors
    /// Returns an error if dirty row count exceeds u16 or row encoding fails.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let row_count: u16 = self.dirty_rows.len().try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "too many dirty rows for u16")
        })?;
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.pane_id.to_le_bytes());
        buf.extend_from_slice(&self.seqno.to_le_bytes());
        buf.extend_from_slice(&self.cursor_x.to_le_bytes());
        buf.extend_from_slice(&self.cursor_y.to_le_bytes());
        buf.push(self.cursor_style);
        buf.push(u8::from(self.cursor_visible));
        buf.push(self.bg_r);
        buf.push(self.bg_g);
        buf.push(self.bg_b);
        buf.push(u8::from(self.bracketed_paste));
        buf.extend_from_slice(&row_count.to_le_bytes());
        for row in &self.dirty_rows {
            buf.extend_from_slice(&row.encode()?);
        }
        Ok(buf)
    }

    /// # Errors
    /// Returns an error if the payload is malformed.
    pub fn decode(data: &[u8]) -> io::Result<Self> {
        if data.len() < 24 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "RenderUpdate too short",
            ));
        }
        let pane_id = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let seqno = u64::from_le_bytes([
            data[4], data[5], data[6], data[7], data[8], data[9], data[10], data[11],
        ]);
        let cursor_x = u16::from_le_bytes([data[12], data[13]]);
        let cursor_y = u16::from_le_bytes([data[14], data[15]]);
        let cursor_style = data[16];
        let cursor_visible = data[17] != 0;
        let bg_r = data[18];
        let bg_g = data[19];
        let bg_b = data[20];
        let bracketed_paste = data[21] != 0;
        let dirty_row_count = u16::from_le_bytes([data[22], data[23]]) as usize;

        let mut offset = 24;
        let mut dirty_rows = Vec::with_capacity(dirty_row_count);
        for _ in 0..dirty_row_count {
            let (row, consumed) = DirtyRow::decode(&data[offset..])?;
            dirty_rows.push(row);
            offset += consumed;
        }

        Ok(Self {
            pane_id,
            seqno,
            cursor_x,
            cursor_y,
            cursor_style,
            cursor_visible,
            bg_r,
            bg_g,
            bg_b,
            bracketed_paste,
            dirty_rows,
        })
    }
}
