/// Text selection type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionType {
    Normal,
    Block,
    Semantic,
    Line,
}

/// Half-cell precision for selection boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorSide {
    Left,
    Right,
}

/// Selection anchor point. Row is signed: negative values reference scrollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionAnchor {
    pub row: i64,
    pub col: u16,
    pub side: AnchorSide,
}

/// Text selection state, tracked separately from the grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub ty: SelectionType,
    pub start: SelectionAnchor,
    pub end: SelectionAnchor,
}
