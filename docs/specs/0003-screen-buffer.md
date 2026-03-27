---
spec: '0003'
title: Screen Buffer
status: draft
date: 2026-03-26
adrs: ['0006', '0001']
tags: [core]
---

# 0003. Screen Buffer

## Overview

Defines the in-memory data model for terminal screen content: cells, rows, grids, cursor state, selection, and dirty tracking. The VT handler (Spec-0002) mutates this model. The wire protocol (Spec-0001) serializes it for transmission to GUI clients. This spec defines the logical structure; implementations may optimize memory layout (style deduplication, packed structs, arena allocation) without changing the contract.

## Contract

### Cell

The atomic unit of terminal content. One cell occupies one column of the terminal grid.

```rust
struct Cell {
    /// Base codepoint. 0 = empty cell.
    codepoint: char,

    /// Additional codepoints for grapheme clusters (combining marks, ZWJ sequences).
    /// Empty for most cells. Storage mechanism is implementation-defined
    /// (heap, arena, interning table).
    extra_codepoints: GraphemeData,

    /// Foreground color.
    fg: Color,

    /// Background color.
    bg: Color,

    /// Underline color. None = use foreground.
    underline_color: Option<Color>,

    /// Visual attributes.
    flags: CellFlags,

    /// Wide character state.
    wide: WideState,

    /// Hyperlink URI. None for most cells.
    hyperlink: Option<HyperlinkId>,
}

enum Color {
    Default,
    Named(NamedColor),    // 0-7 standard, 8-15 bright
    Indexed(u8),          // 0-255 palette
    Rgb(u8, u8, u8),      // True color
}

enum NamedColor {
    Black, Red, Green, Yellow, Blue, Magenta, Cyan, White,
    BrightBlack, BrightRed, BrightGreen, BrightYellow,
    BrightBlue, BrightMagenta, BrightCyan, BrightWhite,
}
```

**Canonical color representation:** SGR 30-37 and 90-97 produce `Named`. SGR 38;5;N produces `Indexed` for all N (including 0-15). `Named` and `Indexed` are distinct representations even when they resolve to the same palette entry. Implementations must not normalize between them.

**Wire serialization mapping:** `Color::Named(c)` serializes as `fg_type=2` (indexed) with the palette index (0-15) in `fg_r`. `Color::Indexed(n)` also serializes as `fg_type=2`. `Color::Default` serializes as `fg_type=0`. `Color::Rgb(r,g,b)` serializes as `fg_type=1`. See Spec-0001 Cell wire format.

```rust
struct CellFlags {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: UnderlineStyle,
    blink: bool,
    inverse: bool,
    hidden: bool,
    strikethrough: bool,
    overline: bool,
}

enum UnderlineStyle { None, Single, Double, Curly, Dotted, Dashed }

enum WideState {
    Narrow,       // Normal single-width character
    Wide,         // First cell of a double-width character
    WideCont,     // Continuation cell (second cell of a wide character)
}

/// Opaque handle to grapheme overflow data.
/// Implementation may use Vec<char>, arena allocation, or interning.
struct GraphemeData { /* implementation-defined */ }

/// Opaque handle to a hyperlink. Implementation maps this to a URI string.
struct HyperlinkId(u32);
```

**Wide character invariant:** A cell with `wide: Wide` must be immediately followed by a cell with `wide: WideCont` in the same row. Writing to either cell clears both. A wide character at the last column wraps to the next row (the continuation cell is the first cell of the next row).

**Grapheme invariant:** `codepoint` holds the base character. `extra_codepoints` holds zero or more combining marks or ZWJ-joined characters. The full grapheme cluster is the base codepoint followed by the extra codepoints in order.

### Row

A horizontal sequence of cells with metadata.

```rust
struct Row {
    /// Cells in this row. Length equals the grid's column count.
    cells: [Cell],

    /// Row metadata.
    flags: RowFlags,

    /// Shell integration mark on this row (from OSC 133). See Spec-0002.
    semantic_mark: SemanticMark,

    /// Mark metadata. Exit code for OutputEnd, CWD for PromptStart.
    mark_metadata: Option<MarkMetadata>,

    /// Sequence number. Incremented on any visual mutation to this row.
    /// Used by dirty tracking and the wire protocol's `since_seqno`.
    seqno: u64,
}

struct RowFlags {
    /// This row soft-wraps to the next row (line continuation).
    wrapped: bool,

    /// This row is a continuation of the previous row's wrap.
    wrap_continuation: bool,

    /// Optimization hint: true if any cell has non-default style.
    /// May have false positives. Never false negatives.
    has_styles: bool,

    /// Optimization hint: true if any cell has a hyperlink.
    has_hyperlinks: bool,

    /// Optimization hint: true if any cell has extra grapheme codepoints.
    has_graphemes: bool,
}

enum SemanticMark {
    None,
    PromptStart,   // OSC 133;A
    InputStart,    // OSC 133;B
    OutputStart,   // OSC 133;C
    OutputEnd,     // OSC 133;D
}

enum MarkMetadata {
    ExitCode(i32),       // For OutputEnd
    WorkingDirectory(String),  // For PromptStart (from OSC 7)
}
```

**Row-flag optimization hints:** The `has_styles`, `has_hyperlinks`, and `has_graphemes` flags are set when a cell gains the property but never cleared (clearing would require scanning all cells). Consumers must handle false positives. These flags enable fast-path skipping: a row with `has_styles: false` is guaranteed to have all-default styles, so style-related processing can be skipped entirely.

### Grid

The visible terminal screen. A 2D array of rows.

```rust
struct Grid {
    /// Rows in the visible area. Length equals `rows`.
    lines: [Row],

    /// Grid dimensions.
    cols: u16,
    rows: u16,

    /// Current cursor state.
    cursor: Cursor,

    /// Saved cursor (DECSC / ESC 7). Restored with DECRC / ESC 8.
    saved_cursor: Cursor,

    /// Active character set indices.
    active_charset: CharsetIndex,
    charsets: [StandardCharset; 4],

    /// Current SGR attributes applied to new characters.
    current_attr: CellFlags,
    current_fg: Color,
    current_bg: Color,
    current_underline_color: Option<Color>,

    /// Active DEC private modes. See Spec-0002 for the mode table.
    modes: ModeFlags,

    /// Scroll region (DECSTBM). None = full screen.
    scroll_region: Option<ScrollRegion>,

    /// Tab stops. Indexed by column number.
    tab_stops: Vec<bool>,

    /// Global sequence number. Incremented on any mutation.
    /// Individual row seqnos are set to this value when modified.
    seqno: u64,

    /// Color palette (256 entries, mutable via OSC 4).
    palette: [Rgb; 256],

    /// Dynamic colors (foreground, background, cursor — mutable via OSC 10/11/12).
    dynamic_fg: Option<Rgb>,
    dynamic_bg: Option<Rgb>,
    dynamic_cursor: Option<Rgb>,
}

/// Bitset tracking active DEC private modes and ANSI modes.
/// See Spec-0002 for PrivateMode and AnsiMode enums.
/// Stored as a fixed-size bitfield indexed by mode number.
struct ModeFlags { /* bitfield indexed by mode number */ }

struct Cursor {
    row: u16,
    col: u16,
    style: CursorStyle,
    visible: bool,
    /// DEC mode 12 blink override. None = use style's blink state.
    /// Some(true) = force blinking. Some(false) = force steady.
    blink_override: Option<bool>,
}

enum CursorStyle {
    BlinkingBlock,
    SteadyBlock,
    BlinkingUnderline,
    SteadyUnderline,
    BlinkingBar,
    SteadyBar,
}

struct ScrollRegion {
    top: u16,    // First row (inclusive)
    bottom: u16, // Last row (inclusive)
}

struct Rgb { r: u8, g: u8, b: u8 }
```

### Screen Set

The terminal maintains two grids: primary and alternate. Only one is active at a time.

```rust
struct ScreenSet {
    active: ScreenId,
    primary: Grid,
    alternate: Grid,
}

enum ScreenId { Primary, Alternate }
```

**Alternate screen behavior:** DECSET 1049 saves the primary cursor, switches `active` to `Alternate`, and clears the alternate grid. DECRST 1049 switches back to `Primary` and restores the saved cursor. The primary grid's content is preserved during alternate screen use. The alternate grid has no scrollback. Per ADR-0006, lines that scroll off the top of the alternate grid are captured to the primary screen's scrollback if `save-alternate-scrollback` is enabled.

**Alternate screen lazy allocation:** The alternate grid is not allocated until first used (DECSET 1049). Once allocated, it persists for the lifetime of the terminal session to avoid repeated allocation/deallocation when applications enter and leave the alternate screen frequently.

### Dirty Tracking

Per-row sequence numbers enable efficient change detection.

**Mechanism:**

1. The grid maintains a global `seqno: u64`, incremented on every mutation.
2. When a row is visually modified, its `row.seqno` is set to the grid's current `seqno`.
3. To find rows changed since a previous observation, compare each `row.seqno > observed_seqno`.
4. The wire protocol's `GetRenderUpdate { since_seqno }` (Spec-0001) uses this: the daemon returns all rows where `row.seqno > since_seqno`.

**What counts as a visual mutation (increments row seqno):**

- Character written to a cell
- Cell erased or cleared
- SGR attribute change on a cell
- Row scrolled (enters or leaves the visible area)
- Cursor moves to or from a row (cursor is visual)
- Semantic mark added

**What does NOT increment row seqno:**

- Mode changes (DECSET/DECRST) that don't affect visible content
- Cursor save/restore (DECSC/DECRC)
- Tab stop changes

**Global operations** (setting `seqno` on all visible rows):

- Full-screen clear (ED 2, ED 3)
- Palette change (OSC 4) — all rows use palette colors
- Dynamic color change (OSC 10/11/12)
- Grid resize

### Selection

Text selection state, tracked separately from the grid.

```rust
struct Selection {
    /// Selection type.
    ty: SelectionType,

    /// Start anchor (where the user started selecting).
    start: SelectionAnchor,

    /// End anchor (where the user stopped selecting or current mouse position).
    end: SelectionAnchor,
}

enum SelectionType {
    Normal,    // Character-level selection
    Block,     // Rectangular block selection
    Semantic,  // Word-level (expand to word boundaries)
    Line,      // Full-line selection
}

struct SelectionAnchor {
    row: i64,    // Signed: negative values reference scrollback
    col: u16,
    side: AnchorSide,
}

enum AnchorSide {
    Left,   // Selection boundary is on the left edge of the cell
    Right,  // Selection boundary is on the right edge of the cell
}
```

**Anchor side:** Half-cell precision for selection. When clicking between two characters, `side` determines which cell is included. This prevents the common UX issue of needing pixel-perfect clicks.

**Scrollback-aware rows:** Selection uses `i64` row indices where row 0 is the first visible row and negative values reference scrollback lines. This allows selection to span from scrollback into the visible area.

**Selection invalidation:** Selection is cleared when:

- The underlying content is modified (characters written to selected cells)
- The grid is resized (reflow changes row boundaries)
- The alternate screen is entered or exited

Selection is NOT cleared when:

- Content scrolls (the selection tracks with the content via row indices)
- The viewport scrolls (the user scrolls through scrollback)

## Behavior

### Grid Resize

When the terminal window is resized:

1. New grid with updated dimensions is created.
2. Content from the old grid is reflowed into the new grid:
   - **Column grow:** Soft-wrapped lines are unwrapped (content pulled from continuation rows).
   - **Column shrink:** Lines exceeding the new width are re-wrapped (excess content pushed to new continuation rows).
   - **Row grow:** Empty rows added at the bottom. Scrollback lines may be pulled into the visible area.
   - **Row shrink:** Bottom rows pushed into scrollback. If the cursor is in the trimmed region, scrollback is not created — the cursor stays in the visible area.
3. Cursor position is clamped to the new dimensions.
4. Scroll region is reset to full screen.
5. All rows are marked dirty (full redraw).
6. Selection is cleared.

### Cell Write

When the VT handler writes a character at the cursor position:

1. If the cursor is past the last column and auto-wrap is enabled (DECAWM), the current row is marked wrapped, and the cursor moves to column 0 of the next row (scrolling if necessary).
2. If the character is wide (East Asian Width W or F) and the cursor is at the last column, the last cell is cleared and the cursor wraps to the next row.
3. The cell at the cursor position is written: `codepoint`, `fg`, `bg`, `flags` from current attributes.
4. If the character is wide, the next cell is set to `wide: WideCont`.
5. The row's `seqno` is updated.
6. The cursor advances by 1 (narrow) or 2 (wide) columns.

### Scroll

When the VT handler scrolls (SU, SD, LF at bottom of scroll region):

1. Rows within the scroll region shift up (SU) or down (SD).
2. New blank rows are inserted at the bottom (SU) or top (SD) of the scroll region.
3. Rows shifted out of the scroll region are sent to scrollback (SU) or discarded (SD). On the alternate grid, scroll-up rows are discarded unless `save-alternate-scrollback` is enabled, in which case they are appended to the primary grid's scrollback (per ADR-0006).
4. All rows in the scroll region have their `seqno` updated.

## Constraints

- **Cell logical size:** Implementations should target 8-24 bytes per cell. Ghostty achieves 8 bytes with style deduplication; Alacritty and WezTerm use 24 bytes with inline colors. The logical model does not mandate a specific size.
- **Row metadata:** Row flags and seqno should fit within 16 bytes. Ghostty packs row metadata into 8 bytes.
- **Grid memory:** A 200×50 grid at 24 bytes/cell = ~240 KB. At 8 bytes/cell = ~80 KB. Both are acceptable.
- **Sequence number overflow:** `u64` seqno overflows after ~18 quintillion mutations. Not a practical concern (at 1 billion mutations/sec, overflow takes ~584 years).
- **Dirty tracking cost:** Comparing `row.seqno > observed_seqno` is O(rows) per frame. For 50 visible rows, this is negligible.
- **Selection stability:** Selection anchors use `i64` row indices. At 1 billion lines of scrollback, row indices fit within i64 range.

## References

- [ADR 0006: Scroll Buffer Architecture](../adrs/0006-scroll-buffer-architecture.md)
- [ADR 0001: Accessibility in Phase 0](../adrs/0001-accessibility-in-phase-zero.md)
- [Spec 0001: Daemon Wire Protocol](0001-daemon-wire-protocol.md) — RenderUpdate and DirtyRow wire formats
- [Spec 0002: VT Parser & Terminal Handler](0002-vt-parser.md) — handler methods that mutate this model
- [02-renderer.md](../ideas/02-renderer.md)
- [15-memory-management.md](../ideas/15-memory-management.md)
