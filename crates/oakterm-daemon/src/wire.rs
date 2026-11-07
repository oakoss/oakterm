//! Row-to-wire encoding shared by the render and scrollback paths.

use oakterm_protocol::render::{DirtyRow, FLAG_WIDE, FLAG_WIDE_CONT, WireCell};
use oakterm_terminal::grid::cell::{Color, Rgb, WideState};
use oakterm_terminal::grid::row::{MarkMetadata, Row};

/// Resolve a terminal `Color` to RGB bytes using the palette.
fn resolve_color(
    color: Color,
    palette: &[Rgb; 256],
    def_r: u8,
    def_g: u8,
    def_b: u8,
) -> (u8, u8, u8, u8) {
    match color {
        Color::Default => (def_r, def_g, def_b, 0),
        Color::Named(n) => {
            let rgb = palette[n as u8 as usize];
            (rgb.r, rgb.g, rgb.b, 1)
        }
        Color::Indexed(i) => {
            let rgb = palette[usize::from(i)];
            (rgb.r, rgb.g, rgb.b, 2)
        }
        Color::Rgb(r, g, b) => (r, g, b, 3),
    }
}

/// Convert a terminal `Row` to a wire `DirtyRow` using the given palette.
pub(crate) fn row_to_wire(row: &Row, row_index: u16, palette: &[Rgb; 256]) -> DirtyRow {
    let cells: Vec<WireCell> = row
        .cells
        .iter()
        .map(|c| {
            let (fg_r, fg_g, fg_b, fg_type) = resolve_color(c.fg, palette, 255, 255, 255);
            let (bg_r, bg_g, bg_b, bg_type) = resolve_color(c.bg, palette, 0, 0, 0);
            WireCell {
                codepoint: c.codepoint as u32,
                fg_r,
                fg_g,
                fg_b,
                fg_type,
                bg_r,
                bg_g,
                bg_b,
                bg_type,
                flags: c.flags.bits()
                    | match c.wide {
                        WideState::Narrow => 0,
                        WideState::Wide => FLAG_WIDE,
                        WideState::WideCont => FLAG_WIDE_CONT,
                    },
                extra: vec![],
            }
        })
        .collect();
    let mark_metadata = row
        .mark_metadata
        .as_ref()
        .map_or_else(Vec::new, MarkMetadata::to_wire_bytes);
    DirtyRow {
        row_index,
        cells,
        semantic_mark: row.semantic_mark.to_wire(),
        mark_metadata,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oakterm_terminal::grid::cell::CellFlags;

    #[test]
    fn wide_state_rides_in_the_wire_flags() {
        let mut row = Row::new(4);
        row.cells[0].codepoint = '中';
        row.cells[0].wide = WideState::Wide;
        row.cells[0].flags = CellFlags::BOLD;
        row.cells[1].wide = WideState::WideCont;
        row.cells[2].codepoint = 'x';
        let wire = row_to_wire(&row, 0, &[Rgb::default(); 256]);
        assert_eq!(wire.cells[0].flags, FLAG_WIDE | CellFlags::BOLD.bits());
        assert_eq!(wire.cells[1].flags, FLAG_WIDE_CONT);
        assert_eq!(wire.cells[2].flags, 0);
    }
}
