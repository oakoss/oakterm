/// Per-row base text direction. Reserved for `BiDi` (ADR-0009).
/// Phase 0 always uses `Ltr`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum Direction {
    #[default]
    Ltr,
    Rtl,
    Auto,
}

/// Maps between logical (PTY order) and visual (display order) column positions.
///
/// Phase 0: identity implementation. `BiDi` implementation replaces this with
/// a UBA-based mapper without changing cursor or selection code.
pub trait CoordinateMapper {
    fn logical_to_visual(&self, logical_col: u16, row: u16) -> u16;
    fn visual_to_logical(&self, visual_col: u16, row: u16) -> u16;
}

/// Identity mapper: logical == visual. Used until `BiDi` is implemented.
pub struct IdentityMapper;

impl CoordinateMapper for IdentityMapper {
    fn logical_to_visual(&self, logical_col: u16, _row: u16) -> u16 {
        logical_col
    }

    fn visual_to_logical(&self, visual_col: u16, _row: u16) -> u16 {
        visual_col
    }
}
