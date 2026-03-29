/// 24-bit RGB color value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// Terminal color representation.
///
/// SGR 30-37 / 90-97 produce `Named`. SGR 38;5;N produces `Indexed` for all N
/// (including 0-15). `Named` and `Indexed` are distinct even when they resolve
/// to the same palette entry — implementations must not normalize between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Color {
    #[default]
    Default,
    Named(NamedColor),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum NamedColor {
    Black = 0,
    Red = 1,
    Green = 2,
    Yellow = 3,
    Blue = 4,
    Magenta = 5,
    Cyan = 6,
    White = 7,
    BrightBlack = 8,
    BrightRed = 9,
    BrightGreen = 10,
    BrightYellow = 11,
    BrightBlue = 12,
    BrightMagenta = 13,
    BrightCyan = 14,
    BrightWhite = 15,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum UnderlineStyle {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

/// Visual attributes for a cell. Stored as a bitfield for compact representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CellFlags(u16);

impl CellFlags {
    pub const BOLD: Self = Self(1 << 0);
    pub const DIM: Self = Self(1 << 1);
    pub const ITALIC: Self = Self(1 << 2);
    pub const BLINK: Self = Self(1 << 3);
    pub const INVERSE: Self = Self(1 << 4);
    pub const HIDDEN: Self = Self(1 << 5);
    pub const STRIKETHROUGH: Self = Self(1 << 6);
    pub const OVERLINE: Self = Self(1 << 7);

    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Wide character state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WideState {
    #[default]
    Narrow,
    /// First cell of a double-width character.
    Wide,
    /// Continuation cell (second cell of a wide character).
    WideCont,
}

/// Grapheme cluster overflow data. Holds combining marks and ZWJ sequences
/// beyond the base codepoint. Empty for most cells.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GraphemeData {
    extra: Vec<char>,
}

impl GraphemeData {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.extra.is_empty()
    }

    pub fn push(&mut self, c: char) {
        self.extra.push(c);
    }

    pub fn clear(&mut self) {
        self.extra.clear();
    }

    #[must_use]
    pub fn chars(&self) -> &[char] {
        &self.extra
    }
}

/// Opaque handle to a hyperlink URI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HyperlinkId(pub(crate) u32);

/// The atomic unit of terminal content. One cell = one column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub codepoint: char,
    pub extra_codepoints: GraphemeData,
    pub fg: Color,
    pub bg: Color,
    pub underline_color: Option<Color>,
    pub underline_style: UnderlineStyle,
    pub flags: CellFlags,
    pub wide: WideState,
    pub hyperlink: Option<HyperlinkId>,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            codepoint: '\0',
            extra_codepoints: GraphemeData::default(),
            fg: Color::Default,
            bg: Color::Default,
            underline_color: None,
            underline_style: UnderlineStyle::None,
            flags: CellFlags::empty(),
            wide: WideState::Narrow,
            hyperlink: None,
        }
    }
}

impl Cell {
    /// Whether this cell has non-default styling.
    #[must_use]
    pub fn has_style(&self) -> bool {
        !self.flags.is_empty()
            || self.fg != Color::Default
            || self.bg != Color::Default
            || self.underline_color.is_some()
            || self.underline_style != UnderlineStyle::None
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Erase this cell using Background Color Erase (BCE).
    /// Clears content but keeps the given background color.
    pub fn erase_with_bg(&mut self, bg: Color) {
        *self = Self {
            bg,
            ..Self::default()
        };
    }
}
