//! Cold disk archive: zstd-compressed, AES-256-GCM encrypted segment files
//! with seek tables for random access per Spec-0004.

use crate::grid::row::Row;
use crate::scroll::row_codec;
use ring::aead::{self, AES_256_GCM, LessSafeKey, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use std::io::{self, Write};

/// Maximum frames per segment (~16 MB uncompressed at 64 KB/frame).
pub const MAX_FRAMES_PER_SEGMENT: u32 = 256;

/// Zstd compression level (fast compression, good ratio for terminal output).
const ZSTD_LEVEL: i32 = 3;

/// Size of each seek table entry in bytes.
const SEEK_ENTRY_SIZE: usize = 28;

/// Ephemeral encryption key with monotonic nonce counter.
pub struct ArchiveKey {
    key: LessSafeKey,
    nonce_counter: u64,
}

impl ArchiveKey {
    /// Generate a new ephemeral key from the system CSPRNG.
    ///
    /// # Errors
    ///
    /// Returns an error if the system RNG is unavailable.
    pub fn generate() -> io::Result<Self> {
        let rng = SystemRandom::new();
        let mut key_bytes = [0u8; 32];
        rng.fill(&mut key_bytes)
            .map_err(|_| io::Error::other("CSPRNG unavailable"))?;
        let unbound = UnboundKey::new(&AES_256_GCM, &key_bytes)
            .map_err(|_| io::Error::other("failed to create AES key"))?;
        Ok(Self {
            key: LessSafeKey::new(unbound),
            nonce_counter: 0,
        })
    }

    /// Current nonce counter value (for decryption).
    #[must_use]
    pub fn nonce_counter(&self) -> u64 {
        self.nonce_counter
    }

    /// Reference to the underlying key (for decryption).
    #[must_use]
    pub fn key(&self) -> &LessSafeKey {
        &self.key
    }
}

/// Build a 12-byte nonce from a counter value: 8 bytes LE + 4 zero bytes.
fn nonce_from_counter(counter: u64) -> aead::Nonce {
    let mut bytes = [0u8; 12];
    bytes[..8].copy_from_slice(&counter.to_le_bytes());
    aead::Nonce::assume_unique_for_key(bytes)
}

/// Encrypt data in place, appending a 16-byte GCM auth tag.
/// Returns the nonce counter used (caller needs it for decryption).
fn encrypt_frame(key: &mut ArchiveKey, plaintext: &[u8]) -> io::Result<Vec<u8>> {
    let nonce_val = key.nonce_counter;
    key.nonce_counter += 1;
    let nonce = nonce_from_counter(nonce_val);
    let mut buf = plaintext.to_vec();
    key.key
        .seal_in_place_append_tag(nonce, aead::Aad::empty(), &mut buf)
        .map_err(|_| io::Error::other("AES-GCM encryption failed"))?;
    Ok(buf)
}

/// Decrypt a frame using the given key and nonce counter value.
/// The input must include the 16-byte GCM auth tag at the end.
///
/// # Errors
///
/// Returns an error if decryption or authentication fails.
pub fn decrypt_frame(
    key: &LessSafeKey,
    nonce_counter: u64,
    ciphertext_with_tag: &[u8],
) -> io::Result<Vec<u8>> {
    let nonce = nonce_from_counter(nonce_counter);
    let mut buf = ciphertext_with_tag.to_vec();
    let plaintext = key
        .open_in_place(nonce, aead::Aad::empty(), &mut buf)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "AES-GCM decryption failed"))?;
    let len = plaintext.len();
    buf.truncate(len);
    Ok(buf)
}

fn compress_frame(data: &[u8]) -> io::Result<Vec<u8>> {
    zstd::encode_all(data, ZSTD_LEVEL)
}

/// Decompress a zstd-compressed frame.
///
/// # Errors
///
/// Returns an error if decompression fails.
pub fn decompress_frame(data: &[u8]) -> io::Result<Vec<u8>> {
    zstd::decode_all(data)
}

/// One entry in the segment seek table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeekTableEntry {
    /// Byte offset of this frame in the segment file.
    pub compressed_offset: u64,
    /// Size of the encrypted+tagged frame on disk.
    pub compressed_size: u32,
    /// Size of the plaintext compressed data (before encryption).
    pub decompressed_size: u32,
    /// Cumulative row count at frame start.
    pub first_row_index: u64,
    /// Number of rows in this frame.
    pub row_count: u32,
}

/// Serialize seek table entries as fixed-size LE binary.
#[must_use]
pub fn serialize_seek_table(entries: &[SeekTableEntry]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(entries.len() * SEEK_ENTRY_SIZE);
    for e in entries {
        buf.extend_from_slice(&e.compressed_offset.to_le_bytes());
        buf.extend_from_slice(&e.compressed_size.to_le_bytes());
        buf.extend_from_slice(&e.decompressed_size.to_le_bytes());
        buf.extend_from_slice(&e.first_row_index.to_le_bytes());
        buf.extend_from_slice(&e.row_count.to_le_bytes());
    }
    buf
}

/// Deserialize seek table entries from LE binary.
///
/// # Errors
///
/// Returns an error if the data length is not a multiple of the entry size.
///
/// # Panics
///
/// Cannot panic: the size check ensures each chunk is exactly 28 bytes.
pub fn deserialize_seek_table(data: &[u8]) -> io::Result<Vec<SeekTableEntry>> {
    if data.len() % SEEK_ENTRY_SIZE != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "seek table size {} is not a multiple of {SEEK_ENTRY_SIZE}",
                data.len()
            ),
        ));
    }
    let mut entries = Vec::with_capacity(data.len() / SEEK_ENTRY_SIZE);
    for chunk in data.chunks_exact(SEEK_ENTRY_SIZE) {
        // chunks_exact guarantees 28 bytes; array conversions are infallible.
        let u64_at =
            |off| u64::from_le_bytes(chunk[off..off + 8].try_into().expect("28-byte chunk"));
        let u32_at =
            |off| u32::from_le_bytes(chunk[off..off + 4].try_into().expect("28-byte chunk"));
        entries.push(SeekTableEntry {
            compressed_offset: u64_at(0),
            compressed_size: u32_at(8),
            decompressed_size: u32_at(12),
            first_row_index: u64_at(16),
            row_count: u32_at(24),
        });
    }
    Ok(entries)
}

/// Writes compressed, encrypted frames to a segment with a seek table footer.
pub struct SegmentWriter<W: Write> {
    writer: W,
    key: ArchiveKey,
    seek_table: Vec<SeekTableEntry>,
    written_bytes: u64,
    total_rows: u64,
    frame_count: u32,
}

impl<W: Write> SegmentWriter<W> {
    /// Create a new segment writer with a freshly generated ephemeral key.
    ///
    /// # Errors
    ///
    /// Returns an error if key generation fails.
    pub fn new(writer: W) -> io::Result<Self> {
        Ok(Self {
            writer,
            key: ArchiveKey::generate()?,
            seek_table: Vec::new(),
            written_bytes: 0,
            total_rows: 0,
            frame_count: 0,
        })
    }

    /// Create a segment writer with a provided key (for testing).
    pub fn with_key(writer: W, key: ArchiveKey) -> Self {
        Self {
            writer,
            key,
            seek_table: Vec::new(),
            written_bytes: 0,
            total_rows: 0,
            frame_count: 0,
        }
    }

    /// Write a batch of rows as one compressed+encrypted frame.
    ///
    /// # Errors
    ///
    /// Returns an error if the segment is full, or if serialization,
    /// compression, encryption, or I/O fails.
    pub fn write_frame(&mut self, rows: &[Row]) -> io::Result<()> {
        if self.is_full() {
            return Err(io::Error::other("segment is full (max frames reached)"));
        }
        let serialized = row_codec::serialize_rows(rows)?;
        let compressed = compress_frame(&serialized)?;

        let decompressed_size: u32 = compressed.len().try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "compressed frame exceeds u32")
        })?;

        let encrypted = encrypt_frame(&mut self.key, &compressed)?;

        let compressed_size: u32 = encrypted.len().try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "encrypted frame exceeds u32")
        })?;

        let row_count: u32 = rows
            .len()
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "row count exceeds u32"))?;

        self.seek_table.push(SeekTableEntry {
            compressed_offset: self.written_bytes,
            compressed_size,
            decompressed_size,
            first_row_index: self.total_rows,
            row_count,
        });

        self.writer.write_all(&encrypted)?;
        self.written_bytes += u64::from(compressed_size);
        self.total_rows += u64::from(row_count);
        self.frame_count += 1;

        Ok(())
    }

    /// Whether the segment has reached the maximum frame count.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.frame_count >= MAX_FRAMES_PER_SEGMENT
    }

    /// Number of frames written so far.
    #[must_use]
    pub fn frame_count(&self) -> u32 {
        self.frame_count
    }

    /// Total rows across all frames.
    #[must_use]
    pub fn total_rows(&self) -> u64 {
        self.total_rows
    }

    /// Reference to the seek table entries.
    #[must_use]
    pub fn seek_table(&self) -> &[SeekTableEntry] {
        &self.seek_table
    }

    /// Reference to the archive key (needed by reader for decryption).
    #[must_use]
    pub fn key(&self) -> &ArchiveKey {
        &self.key
    }

    /// Finalize the segment: write the seek table and its size footer.
    /// Returns the underlying writer.
    ///
    /// # Errors
    ///
    /// Returns an error if writing the seek table or footer fails.
    pub fn finalize(mut self) -> io::Result<(W, ArchiveKey)> {
        let table_bytes = serialize_seek_table(&self.seek_table);
        let table_len: u32 = table_bytes
            .len()
            .try_into()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "seek table exceeds u32"))?;
        self.writer.write_all(&table_bytes)?;
        self.writer.write_all(&table_len.to_le_bytes())?;
        self.writer.flush()?;
        Ok((self.writer, self.key))
    }
}

/// Read the seek table from a finalized segment stored in a byte buffer.
///
/// # Errors
///
/// Returns an error if the buffer is too small or the table is corrupt.
///
/// # Panics
///
/// Cannot panic: the length check ensures at least 4 bytes for the footer.
pub fn read_seek_table(data: &[u8]) -> io::Result<Vec<SeekTableEntry>> {
    if data.len() < 4 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "segment too small for seek table footer",
        ));
    }
    let footer: [u8; 4] = data[data.len() - 4..].try_into().expect("4-byte footer");
    let table_len = u32::from_le_bytes(footer) as usize;
    if data.len() < 4 + table_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("seek table length {table_len} exceeds segment size"),
        ));
    }
    let table_start = data.len() - 4 - table_len;
    deserialize_seek_table(&data[table_start..data.len() - 4])
}

/// Reads frames and individual rows from a finalized segment buffer.
pub struct SegmentReader<'a> {
    data: &'a [u8],
    seek_table: Vec<SeekTableEntry>,
    key: &'a LessSafeKey,
    nonce_start: u64,
}

impl<'a> SegmentReader<'a> {
    /// Open a finalized segment for reading.
    ///
    /// `nonce_start` is the key's nonce counter at the time this segment
    /// began writing. Frame N uses nonce `nonce_start + N`.
    ///
    /// # Errors
    ///
    /// Returns an error if the seek table cannot be parsed.
    pub fn open(data: &'a [u8], key: &'a LessSafeKey, nonce_start: u64) -> io::Result<Self> {
        let seek_table = read_seek_table(data)?;
        Ok(Self {
            data,
            seek_table,
            key,
            nonce_start,
        })
    }

    /// Number of frames in this segment.
    #[must_use]
    pub fn frame_count(&self) -> usize {
        self.seek_table.len()
    }

    /// Total rows across all frames.
    #[must_use]
    pub fn total_rows(&self) -> u64 {
        self.seek_table
            .last()
            .map_or(0, |e| e.first_row_index + u64::from(e.row_count))
    }

    /// Reference to the seek table.
    #[must_use]
    pub fn seek_table(&self) -> &[SeekTableEntry] {
        &self.seek_table
    }

    /// Whether this segment contains the given row index.
    #[must_use]
    pub fn contains_row(&self, row_index: u64) -> bool {
        if let (Some(first), Some(last)) = (self.seek_table.first(), self.seek_table.last()) {
            row_index >= first.first_row_index
                && row_index < last.first_row_index + u64::from(last.row_count)
        } else {
            false
        }
    }

    /// Read and decode all rows in a frame by frame index.
    ///
    /// # Errors
    ///
    /// Returns an error if the frame index is out of range, or if
    /// decryption, decompression, or deserialization fails.
    pub fn read_frame(&self, frame_index: usize) -> io::Result<Vec<Row>> {
        let entry = self.seek_table.get(frame_index).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "frame index {frame_index} out of range ({})",
                    self.seek_table.len()
                ),
            )
        })?;
        let frame_data = self.frame_bytes(entry)?;
        let compressed =
            decrypt_frame(self.key, self.nonce_start + frame_index as u64, frame_data)?;
        let serialized = decompress_frame(&compressed)?;
        row_codec::deserialize_rows(&serialized)
    }

    /// Read a single row by its absolute row index.
    ///
    /// # Errors
    ///
    /// Returns an error if the row index is not in this segment, or if
    /// decryption, decompression, or deserialization fails.
    #[allow(clippy::cast_possible_truncation)] // offset bounded by u32 row_count
    pub fn read_row(&self, row_index: u64) -> io::Result<Row> {
        let (frame_idx, entry) = self.find_frame(row_index)?;
        let rows = self.read_frame(frame_idx)?;
        let offset = (row_index - entry.first_row_index) as usize;
        rows.into_iter().nth(offset).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("row {row_index} not found in frame {frame_idx}"),
            )
        })
    }

    /// Read a range of rows by absolute row index.
    ///
    /// Returns up to `count` rows starting from `start`. May span
    /// multiple frames.
    ///
    /// # Errors
    ///
    /// Returns an error if `start` is not in this segment, or if any
    /// frame read fails.
    #[allow(clippy::cast_possible_truncation)] // within-frame offsets fit in usize (< u32 row_count)
    pub fn read_rows(&self, start: u64, count: usize) -> io::Result<Vec<Row>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        if !self.contains_row(start) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("start row {start} not in this segment"),
            ));
        }
        let mut result = Vec::with_capacity(count);
        let mut current = start;
        let end = start.saturating_add(count as u64);

        while current < end && self.contains_row(current) {
            let (frame_idx, entry) = self.find_frame(current)?;
            let rows = self.read_frame(frame_idx)?;
            let frame_start = entry.first_row_index;
            let skip = (current - frame_start) as usize;
            let remaining = rows.len().saturating_sub(skip);
            let take = remaining.min((end - current) as usize);
            if take == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("frame {frame_idx} has fewer rows than seek table claims"),
                ));
            }
            result.extend(rows.into_iter().skip(skip).take(take));
            current += take as u64;
        }

        Ok(result)
    }

    /// Binary search the seek table for the frame containing `row_index`.
    fn find_frame(&self, row_index: u64) -> io::Result<(usize, &SeekTableEntry)> {
        let idx = self
            .seek_table
            .partition_point(|e| e.first_row_index + u64::from(e.row_count) <= row_index);
        let entry = self.seek_table.get(idx).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("row {row_index} not in this segment"),
            )
        })?;
        if row_index < entry.first_row_index {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("row {row_index} not in this segment"),
            ));
        }
        Ok((idx, entry))
    }

    /// Extract frame bytes from the segment data using a seek table entry.
    fn frame_bytes(&self, entry: &SeekTableEntry) -> io::Result<&[u8]> {
        let off: usize = entry.compressed_offset.try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "compressed_offset exceeds usize",
            )
        })?;
        let len: usize = entry.compressed_size.try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "compressed_size exceeds usize")
        })?;
        if off + len > self.data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("frame at offset {off} + {len} exceeds segment size"),
            ));
        }
        Ok(&self.data[off..off + len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::cell::{CellFlags, Color, NamedColor};
    use crate::grid::row::{Row, SemanticMark};

    /// Extract frame bytes from a segment buffer using a seek table entry.
    #[allow(clippy::cast_possible_truncation)]
    fn frame_bytes<'a>(data: &'a [u8], entry: &SeekTableEntry) -> &'a [u8] {
        let off = entry.compressed_offset as usize;
        let len = entry.compressed_size as usize;
        &data[off..off + len]
    }

    #[test]
    fn encrypt_decrypt_round_trip() {
        let mut key = ArchiveKey::generate().unwrap();
        let plaintext = b"hello, terminal scrollback";
        let nonce_val = key.nonce_counter();
        let encrypted = encrypt_frame(&mut key, plaintext).unwrap();
        assert_ne!(&encrypted[..plaintext.len()], plaintext);
        let decrypted = decrypt_frame(key.key(), nonce_val, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_nonce_fails_decryption() {
        let mut key = ArchiveKey::generate().unwrap();
        let encrypted = encrypt_frame(&mut key, b"secret data").unwrap();
        let result = decrypt_frame(key.key(), 999, &encrypted);
        assert!(result.is_err());
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let mut key = ArchiveKey::generate().unwrap();
        let nonce_val = key.nonce_counter();
        let mut encrypted = encrypt_frame(&mut key, b"secret data").unwrap();
        encrypted[0] ^= 0xFF;
        let result = decrypt_frame(key.key(), nonce_val, &encrypted);
        assert!(result.is_err());
    }

    #[test]
    fn compress_decompress_round_trip() {
        let data = b"repeated repeated repeated repeated repeated";
        let compressed = compress_frame(data).unwrap();
        let decompressed = decompress_frame(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn compressed_is_different() {
        let data = vec![0u8; 1000];
        let compressed = compress_frame(&data).unwrap();
        assert!(compressed.len() < data.len());
    }

    #[test]
    fn seek_table_empty_round_trip() {
        let entries: Vec<SeekTableEntry> = vec![];
        let bytes = serialize_seek_table(&entries);
        assert!(bytes.is_empty());
        let decoded = deserialize_seek_table(&bytes).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn seek_table_round_trip() {
        let entries = vec![
            SeekTableEntry {
                compressed_offset: 0,
                compressed_size: 100,
                decompressed_size: 80,
                first_row_index: 0,
                row_count: 10,
            },
            SeekTableEntry {
                compressed_offset: 100,
                compressed_size: 200,
                decompressed_size: 150,
                first_row_index: 10,
                row_count: 20,
            },
        ];
        let bytes = serialize_seek_table(&entries);
        assert_eq!(bytes.len(), 2 * SEEK_ENTRY_SIZE);
        let decoded = deserialize_seek_table(&bytes).unwrap();
        assert_eq!(decoded, entries);
    }

    #[test]
    fn seek_table_invalid_size() {
        let result = deserialize_seek_table(&[0u8; 7]);
        assert!(result.is_err());
    }

    #[test]
    fn single_frame_write_and_finalize() {
        let rows = vec![Row::new(80)];
        let mut writer = SegmentWriter::new(Vec::new()).unwrap();
        writer.write_frame(&rows).unwrap();
        assert_eq!(writer.frame_count(), 1);
        assert_eq!(writer.total_rows(), 1);
        let (data, _key) = writer.finalize().unwrap();
        assert!(!data.is_empty());
        let table = read_seek_table(&data).unwrap();
        assert_eq!(table.len(), 1);
        assert_eq!(table[0].row_count, 1);
        assert_eq!(table[0].first_row_index, 0);
    }

    #[test]
    fn multi_frame_seek_table_tracks_rows() {
        let mut writer = SegmentWriter::new(Vec::new()).unwrap();
        writer.write_frame(&vec![Row::new(80); 5]).unwrap();
        writer.write_frame(&vec![Row::new(80); 3]).unwrap();
        writer.write_frame(&vec![Row::new(80); 7]).unwrap();
        assert_eq!(writer.frame_count(), 3);
        assert_eq!(writer.total_rows(), 15);
        let (data, _key) = writer.finalize().unwrap();
        let table = read_seek_table(&data).unwrap();
        assert_eq!(table.len(), 3);
        assert_eq!(table[0].first_row_index, 0);
        assert_eq!(table[0].row_count, 5);
        assert_eq!(table[1].first_row_index, 5);
        assert_eq!(table[1].row_count, 3);
        assert_eq!(table[2].first_row_index, 8);
        assert_eq!(table[2].row_count, 7);
    }

    #[test]
    fn is_full_at_max_frames() {
        let mut writer = SegmentWriter::new(Vec::new()).unwrap();
        for _ in 0..MAX_FRAMES_PER_SEGMENT {
            writer.write_frame(&[Row::new(10)]).unwrap();
        }
        assert!(writer.is_full());
        // Writing past max should fail.
        let err = writer.write_frame(&[Row::new(10)]);
        assert!(err.is_err());
    }

    #[test]
    fn full_round_trip_single_frame() {
        let mut rows = vec![Row::new(80), Row::new(80)];
        rows[0].cells[0].codepoint = 'A';
        rows[0].cells[0].fg = Color::Named(NamedColor::Red);
        rows[1].semantic_mark = SemanticMark::PromptStart;

        let mut writer = SegmentWriter::new(Vec::new()).unwrap();
        let nonce_start = writer.key().nonce_counter();
        writer.write_frame(&rows).unwrap();
        let (data, key) = writer.finalize().unwrap();

        // Read back
        let table = read_seek_table(&data).unwrap();
        assert_eq!(table.len(), 1);
        let entry = &table[0];
        let compressed = decrypt_frame(key.key(), nonce_start, frame_bytes(&data, entry)).unwrap();
        let decompressed = decompress_frame(&compressed).unwrap();
        let decoded = row_codec::deserialize_rows(&decompressed).unwrap();
        assert_eq!(decoded, rows);
    }

    #[test]
    fn full_round_trip_multi_frame() {
        let mut rows_a = vec![Row::new(40)];
        rows_a[0].cells[0].codepoint = 'X';
        rows_a[0].cells[0].flags = CellFlags::BOLD;

        let mut rows_b = vec![Row::new(40), Row::new(40)];
        rows_b[1].cells[0].codepoint = 'Y';

        let mut writer = SegmentWriter::new(Vec::new()).unwrap();
        let nonce_start = writer.key().nonce_counter();
        writer.write_frame(&rows_a).unwrap();
        writer.write_frame(&rows_b).unwrap();
        let (data, key) = writer.finalize().unwrap();

        let table = read_seek_table(&data).unwrap();
        assert_eq!(table.len(), 2);

        // Read frame 0
        let e0 = &table[0];
        let dec0 = decrypt_frame(key.key(), nonce_start, frame_bytes(&data, e0)).unwrap();
        let rows0 = row_codec::deserialize_rows(&decompress_frame(&dec0).unwrap()).unwrap();
        assert_eq!(rows0, rows_a);

        // Read frame 1
        let e1 = &table[1];
        let dec1 = decrypt_frame(key.key(), nonce_start + 1, frame_bytes(&data, e1)).unwrap();
        let rows1 = row_codec::deserialize_rows(&decompress_frame(&dec1).unwrap()).unwrap();
        assert_eq!(rows1, rows_b);
    }

    #[test]
    fn encrypted_data_has_no_plaintext() {
        let mut row = Row::new(80);
        row.cells[0].codepoint = 'S';
        row.cells[1].codepoint = 'E';
        row.cells[2].codepoint = 'C';
        row.cells[3].codepoint = 'R';
        row.cells[4].codepoint = 'E';
        row.cells[5].codepoint = 'T';

        let mut writer = SegmentWriter::new(Vec::new()).unwrap();
        writer.write_frame(&[row]).unwrap();
        let (data, _key) = writer.finalize().unwrap();

        // The word "SECRET" should not appear in the encrypted segment
        let data_str = String::from_utf8_lossy(&data);
        assert!(
            !data_str.contains("SECRET"),
            "plaintext leaked into encrypted segment"
        );
    }

    // --- SegmentReader tests ---

    /// Write rows to a segment, returning (data, key, `nonce_start`).
    fn write_segment(frames: &[Vec<Row>]) -> (Vec<u8>, ArchiveKey, u64) {
        let mut writer = SegmentWriter::new(Vec::new()).unwrap();
        let nonce_start = writer.key().nonce_counter();
        for rows in frames {
            writer.write_frame(rows).unwrap();
        }
        let (data, key) = writer.finalize().unwrap();
        (data, key, nonce_start)
    }

    #[test]
    fn reader_open_and_frame_count() {
        let (data, key, ns) = write_segment(&[vec![Row::new(80)], vec![Row::new(80)]]);
        let reader = SegmentReader::open(&data, key.key(), ns).unwrap();
        assert_eq!(reader.frame_count(), 2);
        assert_eq!(reader.total_rows(), 2);
    }

    #[test]
    fn reader_read_frame_round_trip() {
        let mut row = Row::new(40);
        row.cells[0].codepoint = 'Z';
        row.cells[0].fg = Color::Named(NamedColor::Magenta);
        let (data, key, ns) = write_segment(&[vec![row.clone()]]);
        let reader = SegmentReader::open(&data, key.key(), ns).unwrap();
        let rows = reader.read_frame(0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], row);
    }

    #[test]
    fn reader_read_frame_out_of_range() {
        let (data, key, ns) = write_segment(&[vec![Row::new(10)]]);
        let reader = SegmentReader::open(&data, key.key(), ns).unwrap();
        assert!(reader.read_frame(1).is_err());
    }

    #[test]
    fn reader_read_row_by_index() {
        let mut r0 = Row::new(20);
        r0.cells[0].codepoint = 'A';
        let mut r1 = Row::new(20);
        r1.cells[0].codepoint = 'B';
        let mut r2 = Row::new(20);
        r2.cells[0].codepoint = 'C';

        // Frame 0: [r0, r1], Frame 1: [r2]
        let (data, key, ns) = write_segment(&[vec![r0.clone(), r1.clone()], vec![r2.clone()]]);
        let reader = SegmentReader::open(&data, key.key(), ns).unwrap();

        assert_eq!(reader.read_row(0).unwrap(), r0);
        assert_eq!(reader.read_row(1).unwrap(), r1);
        assert_eq!(reader.read_row(2).unwrap(), r2);
    }

    #[test]
    fn reader_read_row_out_of_range() {
        let (data, key, ns) = write_segment(&[vec![Row::new(10)]]);
        let reader = SegmentReader::open(&data, key.key(), ns).unwrap();
        assert!(reader.read_row(1).is_err());
        assert!(reader.read_row(999).is_err());
    }

    #[test]
    fn reader_contains_row() {
        let (data, key, ns) = write_segment(&[vec![Row::new(10); 3], vec![Row::new(10); 2]]);
        let reader = SegmentReader::open(&data, key.key(), ns).unwrap();
        assert!(reader.contains_row(0));
        assert!(reader.contains_row(4));
        assert!(!reader.contains_row(5));
        assert!(!reader.contains_row(100));
    }

    #[test]
    fn reader_read_rows_within_frame() {
        let rows: Vec<Row> = (0..5)
            .map(|i| {
                let mut r = Row::new(10);
                r.cells[0].codepoint = char::from(b'A' + i);
                r
            })
            .collect();
        let (data, key, ns) = write_segment(std::slice::from_ref(&rows));
        let reader = SegmentReader::open(&data, key.key(), ns).unwrap();

        let result = reader.read_rows(1, 3).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], rows[1]);
        assert_eq!(result[2], rows[3]);
    }

    #[test]
    fn reader_read_rows_spanning_frames() {
        let mut r0 = Row::new(10);
        r0.cells[0].codepoint = 'X';
        let mut r1 = Row::new(10);
        r1.cells[0].codepoint = 'Y';
        let mut r2 = Row::new(10);
        r2.cells[0].codepoint = 'Z';

        // Frame 0: [r0, r1], Frame 1: [r2]
        let (data, key, ns) = write_segment(&[vec![r0.clone(), r1.clone()], vec![r2.clone()]]);
        let reader = SegmentReader::open(&data, key.key(), ns).unwrap();

        // Read rows 1..3 which spans both frames
        let result = reader.read_rows(1, 2).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], r1);
        assert_eq!(result[1], r2);
    }

    #[test]
    fn reader_read_rows_truncates_at_segment_end() {
        let (data, key, ns) = write_segment(&[vec![Row::new(10); 3]]);
        let reader = SegmentReader::open(&data, key.key(), ns).unwrap();
        // Request 10 rows starting at 1, but only 2 remain
        let result = reader.read_rows(1, 10).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn reader_full_round_trip_with_styles() {
        let mut r0 = Row::new(40);
        r0.cells[0].codepoint = 'H';
        r0.cells[0].fg = Color::Rgb(255, 0, 0);
        r0.cells[0].flags = CellFlags::BOLD.union(CellFlags::ITALIC);
        r0.semantic_mark = SemanticMark::PromptStart;

        let mut r1 = Row::new(40);
        r1.cells[0].codepoint = 'W';
        r1.cells[0].bg = Color::Named(NamedColor::Blue);

        let (data, key, ns) = write_segment(&[vec![r0.clone()], vec![r1.clone()]]);
        let reader = SegmentReader::open(&data, key.key(), ns).unwrap();

        assert_eq!(reader.read_row(0).unwrap(), r0);
        assert_eq!(reader.read_row(1).unwrap(), r1);
    }

    #[test]
    fn reader_read_rows_out_of_range_start() {
        let (data, key, ns) = write_segment(&[vec![Row::new(10); 3]]);
        let reader = SegmentReader::open(&data, key.key(), ns).unwrap();
        assert!(reader.read_rows(99, 1).is_err());
    }
}
