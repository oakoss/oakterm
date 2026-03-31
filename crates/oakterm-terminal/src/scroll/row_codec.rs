//! Row serialization for the cold disk archive.
//!
//! Encodes/decodes `Row` with full fidelity using postcard (serde).
//! The output feeds into zstd compression and AES-256-GCM encryption
//! in the archive writer (TREK-58).

use crate::grid::row::Row;
use std::io;

/// Serialize a single row to bytes.
///
/// # Errors
///
/// Returns an error if serialization fails.
pub fn serialize_row(row: &Row) -> io::Result<Vec<u8>> {
    postcard::to_allocvec(row)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("serialize row: {e}")))
}

/// Deserialize a single row from bytes.
///
/// # Errors
///
/// Returns an error if deserialization fails.
pub fn deserialize_row(data: &[u8]) -> io::Result<Row> {
    postcard::from_bytes(data).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("deserialize row ({} bytes): {e}", data.len()),
        )
    })
}

/// Serialize multiple rows with u32 LE length-prefixed framing.
///
/// Format: `[u32 LE length][row bytes]` repeated for each row.
///
/// # Errors
///
/// Returns an error if serialization of any row fails.
pub fn serialize_rows(rows: &[Row]) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    for row in rows {
        let row_bytes = serialize_row(row)?;
        let len: u32 = row_bytes.len().try_into().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "serialized row exceeds u32")
        })?;
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(&row_bytes);
    }
    Ok(buf)
}

/// Deserialize multiple rows from length-prefixed framing.
///
/// # Errors
///
/// Returns an error if any frame is truncated or deserialization fails.
pub fn deserialize_rows(data: &[u8]) -> io::Result<Vec<Row>> {
    let mut rows = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        if pos + 4 > data.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated row length prefix",
            ));
        }
        let len: usize =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                .try_into()
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "row length exceeds usize")
                })?;
        pos += 4;
        if len == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "row {} at offset {}: zero-length frame",
                    rows.len(),
                    pos - 4
                ),
            ));
        }
        if pos + len > data.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "row {} at offset {}: need {} bytes but only {} remain",
                    rows.len(),
                    pos,
                    len,
                    data.len() - pos
                ),
            ));
        }
        rows.push(deserialize_row(&data[pos..pos + len]).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("row {} at offset {} ({len} bytes): {e}", rows.len(), pos),
            )
        })?);
        pos += len;
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::cell::{CellFlags, Color, HyperlinkId, NamedColor, UnderlineStyle, WideState};
    use crate::grid::row::{MarkMetadata, Row, SemanticMark};
    use oakterm_common::bidi::Direction;

    fn round_trip(row: &Row) -> Row {
        let bytes = serialize_row(row).expect("serialize failed");
        deserialize_row(&bytes).expect("deserialize failed")
    }

    #[test]
    fn default_cell_round_trip() {
        let row = Row::new(1);
        assert_eq!(round_trip(&row), row);
    }

    #[test]
    fn default_row_80_cols() {
        let row = Row::new(80);
        assert_eq!(round_trip(&row), row);
    }

    #[test]
    fn cell_with_named_color() {
        let mut row = Row::new(1);
        row.cells[0].fg = Color::Named(NamedColor::Red);
        row.cells[0].bg = Color::Named(NamedColor::BrightCyan);
        assert_eq!(round_trip(&row), row);
    }

    #[test]
    fn cell_with_indexed_color() {
        let mut row = Row::new(1);
        row.cells[0].fg = Color::Indexed(42);
        row.cells[0].bg = Color::Indexed(200);
        assert_eq!(round_trip(&row), row);
    }

    #[test]
    fn cell_with_rgb_color() {
        let mut row = Row::new(1);
        row.cells[0].fg = Color::Rgb(255, 128, 0);
        row.cells[0].bg = Color::Rgb(0, 0, 0);
        assert_eq!(round_trip(&row), row);
    }

    #[test]
    fn cell_flags_each_bit() {
        for flag in [
            CellFlags::BOLD,
            CellFlags::DIM,
            CellFlags::ITALIC,
            CellFlags::BLINK,
            CellFlags::INVERSE,
            CellFlags::HIDDEN,
            CellFlags::STRIKETHROUGH,
            CellFlags::OVERLINE,
        ] {
            let mut row = Row::new(1);
            row.cells[0].flags = flag;
            assert_eq!(round_trip(&row), row, "flag {flag:?} failed round-trip");
        }
    }

    #[test]
    fn cell_all_flags_combined() {
        let mut row = Row::new(1);
        row.cells[0].flags = CellFlags::BOLD
            .union(CellFlags::ITALIC)
            .union(CellFlags::STRIKETHROUGH)
            .union(CellFlags::OVERLINE);
        assert_eq!(round_trip(&row), row);
    }

    #[test]
    fn cell_underline_styles() {
        for style in [
            UnderlineStyle::None,
            UnderlineStyle::Single,
            UnderlineStyle::Double,
            UnderlineStyle::Curly,
            UnderlineStyle::Dotted,
            UnderlineStyle::Dashed,
        ] {
            let mut row = Row::new(1);
            row.cells[0].underline_style = style;
            assert_eq!(round_trip(&row), row, "style {style:?} failed round-trip");
        }
    }

    #[test]
    fn wide_cell_pair() {
        let mut row = Row::new(2);
        row.cells[0].codepoint = '漢';
        row.cells[0].wide = WideState::Wide;
        row.cells[1].wide = WideState::WideCont;
        assert_eq!(round_trip(&row), row);
    }

    #[test]
    fn cell_with_grapheme_cluster() {
        let mut row = Row::new(1);
        row.cells[0].codepoint = 'e';
        row.cells[0].push_grapheme('\u{0301}'); // combining acute accent
        assert_eq!(round_trip(&row), row);
    }

    #[test]
    fn cell_with_underline_color() {
        let mut row = Row::new(1);
        row.cells[0].set_underline_color(Some(Color::Rgb(255, 0, 0)));
        assert_eq!(round_trip(&row), row);
    }

    #[test]
    fn cell_with_hyperlink() {
        let mut row = Row::new(1);
        row.cells[0].set_hyperlink(Some(HyperlinkId(42)));
        assert_eq!(round_trip(&row), row);
    }

    #[test]
    fn row_semantic_marks() {
        for mark in [
            SemanticMark::PromptStart,
            SemanticMark::InputStart,
            SemanticMark::OutputStart,
            SemanticMark::OutputEnd,
        ] {
            let mut row = Row::new(1);
            row.semantic_mark = mark;
            assert_eq!(round_trip(&row), row, "mark {mark:?} failed round-trip");
        }
    }

    #[test]
    fn row_mark_metadata_exit_code() {
        let mut row = Row::new(1);
        row.semantic_mark = SemanticMark::OutputEnd;
        row.mark_metadata = Some(MarkMetadata::ExitCode(1));
        assert_eq!(round_trip(&row), row);
    }

    #[test]
    fn row_mark_metadata_working_directory() {
        let mut row = Row::new(1);
        row.semantic_mark = SemanticMark::PromptStart;
        row.mark_metadata = Some(MarkMetadata::WorkingDirectory("/home/user".into()));
        assert_eq!(round_trip(&row), row);
    }

    #[test]
    fn row_wrapped_flag() {
        let mut row = Row::new(80);
        row.flags.set_wrapped(true);
        assert_eq!(round_trip(&row), row);
        assert!(round_trip(&row).flags.wrapped());
    }

    #[test]
    fn row_rtl_direction() {
        let mut row = Row::new(10);
        row.direction = Direction::Rtl;
        assert_eq!(round_trip(&row), row);
    }

    #[test]
    fn multi_row_round_trip() {
        let mut rows = vec![Row::new(80), Row::new(80)];
        rows[0].cells[0].codepoint = 'A';
        rows[0].cells[0].fg = Color::Named(NamedColor::Green);
        rows[1].cells[0].codepoint = 'B';
        rows[1].semantic_mark = SemanticMark::PromptStart;

        let bytes = serialize_rows(&rows).expect("serialize failed");
        let decoded = deserialize_rows(&bytes).expect("deserialize failed");
        assert_eq!(decoded, rows);
    }

    #[test]
    fn empty_rows_round_trip() {
        let rows: Vec<Row> = vec![];
        let bytes = serialize_rows(&rows).expect("serialize failed");
        assert!(bytes.is_empty());
        let decoded = deserialize_rows(&bytes).expect("deserialize failed");
        assert!(decoded.is_empty());
    }

    #[test]
    fn serialized_size_80_col_default_row() {
        let row = Row::new(80);
        let bytes = serialize_row(&row).expect("serialize failed");
        assert!(
            bytes.len() < 700,
            "80-col default row serialized to {} bytes, expected < 700",
            bytes.len()
        );
    }
}
