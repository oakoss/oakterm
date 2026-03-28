/// Cursor visual style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CursorStyle {
    #[default]
    BlinkingBlock,
    SteadyBlock,
    BlinkingUnderline,
    SteadyUnderline,
    BlinkingBar,
    SteadyBar,
}

/// Terminal cursor state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub row: u16,
    pub col: u16,
    pub style: CursorStyle,
    pub visible: bool,
    /// DEC mode 12 blink override. `None` = use style's blink state.
    pub blink_override: Option<bool>,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            row: 0,
            col: 0,
            style: CursorStyle::BlinkingBlock,
            visible: true,
            blink_override: None,
        }
    }
}

/// Scroll region defined by DECSTBM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollRegion {
    pub top: u16,
    pub bottom: u16,
}

/// Index into the G0-G3 character set designations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(u8)]
pub enum CharsetIndex {
    #[default]
    G0 = 0,
    G1 = 1,
    G2 = 2,
    G3 = 3,
}

/// Standard character set designation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum StandardCharset {
    #[default]
    Ascii,
    SpecialGraphics,
}
