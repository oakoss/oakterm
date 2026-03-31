/// 24-bit RGB color value.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
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
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum Color {
    #[default]
    Default,
    Named(NamedColor),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
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
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
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
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum WideState {
    #[default]
    Narrow,
    /// First cell of a double-width character.
    Wide,
    /// Continuation cell (second cell of a wide character).
    WideCont,
}

/// Opaque handle to a hyperlink URI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct HyperlinkId(pub(crate) u32);

/// Heap-allocated storage for rare cell attributes. Only allocated when a cell
/// has grapheme clusters, underline color, or hyperlinks.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CellExtra {
    pub(super) graphemes: Vec<char>,
    pub(super) underline_color: Option<Color>,
    pub(super) hyperlink: Option<HyperlinkId>,
}

/// The atomic unit of terminal content. One cell = one column.
///
/// Common fields (`codepoint`, `fg`, `bg`, `flags`, `underline_style`, `wide`) are inline.
/// Rare fields (graphemes, `underline_color`, hyperlink) live in a heap-allocated
/// [`CellExtra`] behind `Option<Box<CellExtra>>` — null (8 bytes) when unused.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Cell {
    pub codepoint: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: CellFlags,
    pub underline_style: UnderlineStyle,
    pub wide: WideState,
    extra: Option<Box<CellExtra>>,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            codepoint: '\0',
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            underline_style: UnderlineStyle::None,
            wide: WideState::Narrow,
            extra: None,
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
            || self.underline_color().is_some()
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

    // --- Extra field accessors ---

    #[must_use]
    pub fn underline_color(&self) -> Option<Color> {
        self.extra.as_ref().and_then(|e| e.underline_color)
    }

    pub fn set_underline_color(&mut self, color: Option<Color>) {
        if color.is_some() {
            self.ensure_extra().underline_color = color;
        } else if let Some(e) = &mut self.extra {
            e.underline_color = None;
            self.drop_extra_if_empty();
        }
    }

    #[must_use]
    pub fn hyperlink(&self) -> Option<HyperlinkId> {
        self.extra.as_ref().and_then(|e| e.hyperlink)
    }

    pub fn set_hyperlink(&mut self, id: Option<HyperlinkId>) {
        if id.is_some() {
            self.ensure_extra().hyperlink = id;
        } else if let Some(e) = &mut self.extra {
            e.hyperlink = None;
            self.drop_extra_if_empty();
        }
    }

    #[must_use]
    pub fn graphemes(&self) -> &[char] {
        match &self.extra {
            Some(e) => &e.graphemes,
            None => &[],
        }
    }

    pub fn push_grapheme(&mut self, c: char) {
        self.ensure_extra().graphemes.push(c);
    }

    pub fn clear_graphemes(&mut self) {
        if let Some(e) = &mut self.extra {
            if !e.graphemes.is_empty() {
                e.graphemes.clear();
                self.drop_extra_if_empty();
            }
        }
    }

    #[must_use]
    pub fn has_graphemes(&self) -> bool {
        self.extra.as_ref().is_some_and(|e| !e.graphemes.is_empty())
    }

    fn ensure_extra(&mut self) -> &mut CellExtra {
        self.extra.get_or_insert_with(|| {
            Box::new(CellExtra {
                graphemes: Vec::new(),
                underline_color: None,
                hyperlink: None,
            })
        })
    }

    fn drop_extra_if_empty(&mut self) {
        if let Some(e) = &self.extra {
            if e.graphemes.is_empty() && e.underline_color.is_none() && e.hyperlink.is_none() {
                self.extra = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_size_within_target() {
        let size = std::mem::size_of::<Cell>();
        assert!(size <= 24, "Cell is {size} bytes, target is <= 24");
    }

    #[test]
    fn cell_default_has_no_extra() {
        let cell = Cell::default();
        assert!(cell.extra.is_none());
        assert!(cell.graphemes().is_empty());
        assert!(cell.underline_color().is_none());
        assert!(cell.hyperlink().is_none());
    }

    #[test]
    fn cell_grapheme_allocates_extra() {
        let mut cell = Cell::default();
        cell.push_grapheme('\u{0301}'); // combining acute
        assert!(cell.extra.is_some());
        assert_eq!(cell.graphemes(), &['\u{0301}']);
    }

    #[test]
    fn cell_underline_color_allocates_extra() {
        let mut cell = Cell::default();
        cell.set_underline_color(Some(Color::Rgb(255, 0, 0)));
        assert_eq!(cell.underline_color(), Some(Color::Rgb(255, 0, 0)));
    }

    #[test]
    fn cell_clear_underline_color_drops_extra() {
        let mut cell = Cell::default();
        cell.set_underline_color(Some(Color::Rgb(255, 0, 0)));
        cell.set_underline_color(None);
        assert!(cell.extra.is_none());
    }

    #[test]
    fn cell_reset_clears_extra() {
        let mut cell = Cell::default();
        cell.push_grapheme('x');
        cell.set_underline_color(Some(Color::Indexed(1)));
        cell.reset();
        assert!(cell.extra.is_none());
    }

    #[test]
    fn cell_erase_with_bg_clears_extra() {
        let mut cell = Cell::default();
        cell.push_grapheme('x');
        cell.erase_with_bg(Color::Rgb(0, 0, 0));
        assert!(cell.extra.is_none());
        assert_eq!(cell.bg, Color::Rgb(0, 0, 0));
    }
}
