---
spec: '0008'
title: Copy Mode
status: draft
date: 2026-04-02
adrs: ['0012', '0011']
tags: [core]
---

# 0008. Copy Mode

## Overview

Defines the modal scrollback navigation system: entering and exiting copy mode, cursor movement, visual selection, search integration, and yank-to-clipboard. Copy mode is a GUI-side feature backed by daemon scrollback access. The daemon pins the pane's viewport on entry and provides scrollback chunks on demand (ADR-0012). Keybinds are dispatched via key tables (ADR-0011).

## Contract

### Copy Mode State

Per-pane state tracked by the GUI while copy mode is active.

```rust
struct CopyModeState {
    /// Cursor position in daemon row-index space (i64).
    /// Row 0 = top of the visible area at the time copy mode was entered.
    /// Positive values = further down the visible area.
    /// Negative values = scrollback above the visible area.
    cursor_row: i64,
    cursor_col: u16,

    /// Active selection, if any.
    selection: Option<CopySelection>,

    /// Cached scrollback rows from the daemon.
    cache: ViewportCache,

    /// Active search state, if any.
    search: Option<SearchState>,

    /// Which keybind preset is active.
    preset: CopyModePreset,
}

struct CopySelection {
    /// Selection type.
    ty: CopySelectionType,

    /// Anchor (where selection started).
    anchor_row: i64,
    anchor_col: u16,

    /// The cursor position is the other end of the selection.
}

/// Copy mode selection types. Spec-0003 defines a separate `SelectionType`
/// for mouse selection (with `Normal` and `Semantic` variants). Copy mode
/// does not support semantic (word-level) selection; use `w`/`b` motions instead.
enum CopySelectionType {
    /// Character-level selection (equivalent to Spec-0003's `Normal`).
    Character,
    /// Full-line selection (equivalent to Spec-0003's `Line`).
    Line,
    /// Rectangular block selection (equivalent to Spec-0003's `Block`).
    Block,
}

/// A cached row from the daemon. Wraps the Row type from Spec-0003.
/// Implementation may add render-side metadata (highlight state, etc.).
struct CachedRow {
    row: Row,
}

struct ViewportCache {
    /// Cached rows, keyed by daemon row index.
    rows: BTreeMap<i64, CachedRow>,

    /// The pinned viewport offset (row index of the first visible row
    /// when copy mode was entered).
    pinned_offset: i64,

    /// Cache window: rows from `start` to `start + count` are cached.
    start: i64,
    count: u32,
}

struct SearchState {
    /// The search query.
    query: String,

    /// Match positions returned by the daemon.
    matches: Vec<SearchMatch>,

    /// Index of the currently focused match.
    current_match: Option<usize>,

    /// Search direction.
    direction: SearchDirection,
}

struct SearchMatch {
    row: i64,
    start_col: u16,
    end_col: u16,
}

enum SearchDirection {
    Forward,
    Backward,
}

enum CopyModePreset {
    Vim,
    Emacs,
    Basic,
}
```

### Key Tables

Copy mode activates a key table (ADR-0011). Unmatched keys are dropped, not forwarded to the PTY.

#### Vim Preset (default)

| Key      | Action                                                |
| -------- | ----------------------------------------------------- |
| `j`      | Cursor down one line                                  |
| `k`      | Cursor up one line                                    |
| `h`      | Cursor left one character                             |
| `l`      | Cursor right one character                            |
| `w`      | Cursor to next word start                             |
| `b`      | Cursor to previous word start                         |
| `e`      | Cursor to end of word                                 |
| `0`      | Cursor to start of line                               |
| `$`      | Cursor to end of line                                 |
| `^`      | Cursor to first non-blank character                   |
| `gg`     | Cursor to top of scrollback                           |
| `G`      | Cursor to bottom (live output position)               |
| `Ctrl+d` | Half-page down                                        |
| `Ctrl+u` | Half-page up                                          |
| `Ctrl+f` | Full page down                                        |
| `Ctrl+b` | Full page up                                          |
| `v`      | Start/toggle character selection                      |
| `V`      | Start/toggle line selection                           |
| `Ctrl+v` | Start/toggle block selection                          |
| `y`      | Yank selection to clipboard, exit copy mode           |
| `Escape` | Exit copy mode (clear selection if active, else exit) |
| `q`      | Exit copy mode                                        |
| `/`      | Start forward search                                  |
| `?`      | Start backward search                                 |
| `n`      | Next search match                                     |
| `N`      | Previous search match                                 |

#### Emacs Preset

| Key          | Action                                      |
| ------------ | ------------------------------------------- |
| `Ctrl+n`     | Cursor down                                 |
| `Ctrl+p`     | Cursor up                                   |
| `Ctrl+f`     | Cursor forward one character                |
| `Ctrl+b`     | Cursor backward one character               |
| `Alt+f`      | Cursor to next word                         |
| `Alt+b`      | Cursor to previous word                     |
| `Ctrl+a`     | Cursor to start of line                     |
| `Ctrl+e`     | Cursor to end of line                       |
| `Alt+<`      | Cursor to top of scrollback                 |
| `Alt+>`      | Cursor to bottom                            |
| `Ctrl+v`     | Page down                                   |
| `Alt+v`      | Page up                                     |
| `Ctrl+Space` | Start/toggle selection                      |
| `Alt+w`      | Yank selection to clipboard, exit copy mode |
| `Ctrl+g`     | Exit copy mode                              |
| `Ctrl+s`     | Start forward search                        |
| `Ctrl+r`     | Start backward search                       |

#### Basic Preset

| Key                                                      | Action                                      |
| -------------------------------------------------------- | ------------------------------------------- |
| `Up` / `Down` / `Left` / `Right`                         | Cursor movement                             |
| `Page Up` / `Page Down`                                  | Page movement                               |
| `Home`                                                   | Top of scrollback                           |
| `End`                                                    | Bottom                                      |
| `Shift+Up` / `Shift+Down` / `Shift+Left` / `Shift+Right` | Extend selection                            |
| `Ctrl+c`                                                 | Copy selection to clipboard, exit copy mode |
| `Escape`                                                 | Exit copy mode                              |
| `Ctrl+f`                                                 | Start search                                |

### Protocol Messages

Copy mode uses these wire protocol messages (see Spec-0001 for framing):

| msg_type | Name          | Direction | Serial   | Payload                                                                                                                                 |
| -------- | ------------- | --------- | -------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `0x97`   | EnterCopyMode | C→D       | Push (0) | `pane_id: u32`                                                                                                                          |
| `0x98`   | ExitCopyMode  | C→D       | Push (0) | `pane_id: u32`                                                                                                                          |
| `0x99`   | YankSelection | C→D       | Request  | `pane_id: u32`, `start_row: i64`, `start_col: u16`, `end_row: i64`, `end_col: u16`, `selection_type: u8` (0=character, 1=line, 2=block) |
| `0x9A`   | YankResponse  | D→C       | Response | `text_len: u32`, `text: UTF-8`                                                                                                          |

Existing messages reused without modification:

- `GetScrollback` (0x73) / `ScrollbackData` (0x74): viewport cache fills.
- `SearchScrollback` (0x77) / `SearchResults` (0x78) / `SearchNext` (0x79) / `SearchPrev` (0x7A) / `SearchClose` (0x7B): search operations.

## Behavior

### Entry

1. User presses `oak_mod + [` (or configured keybind).
2. GUI activates the copy mode key table for the focused pane.
3. GUI sends `EnterCopyMode { pane_id }` to the daemon. The daemon records the client ID and the pane's current viewport offset as the pinned position.
4. GUI sends `GetScrollback { pane_id, start_row, count }` to fill the initial cache (visible rows plus one screen above and below).
5. Cursor starts at the bottom-left of the visible area: `(cursor_row = rows - 1, cursor_col = 0)` where `rows` is the pane's visible row count. Row 0 is the top of the visible area.

### Cursor Movement

All cursor movement is local to the GUI. No IPC per keystroke.

- Cursor position is clamped to valid row/col ranges within the cached rows.
- When the cursor moves past the top or bottom of the cache, the GUI sends `GetScrollback` to fetch the next chunk.
- **Prefetch:** When the cursor enters the top or bottom 25% of the cache window, the GUI starts fetching the next chunk in the background to hide latency.
- Word boundaries for `w`/`b`/`e` are defined as transitions between alphanumeric/underscore characters and everything else (matching vim's `iskeyword` default).

### Selection

- Starting a selection sets the anchor at the current cursor position.
- Moving the cursor extends the selection from anchor to cursor.
- **Character selection:** All cells between anchor and cursor in reading order (row-major).
- **Line selection:** All complete rows between anchor row and cursor row (inclusive).
- **Block selection:** The rectangular region defined by anchor and cursor corners.
- Toggling the same selection type cancels the selection. Toggling a different type switches the selection type.

### Search

1. User presses `/` (vim) or `Ctrl+s` (emacs) or `Ctrl+f` (basic).
2. GUI shows a search input overlay at the bottom of the pane.
3. As the user types, GUI sends `SearchScrollback { pane_id, query, direction }` to the daemon.
4. Daemon searches the full scrollback (hot buffer + disk archive) and responds with `SearchResults` containing match positions.
5. GUI highlights matches within the cached viewport.
6. `n`/`N` cycle through matches. The cursor jumps to the match position. If the match is outside the cache, the GUI fetches the surrounding rows.
7. Pressing `Enter` or `Escape` closes the search overlay. Matches remain highlighted until copy mode exits.

### Yank

1. User presses `y` (vim), `Alt+w` (emacs), or `Ctrl+c` (basic) with an active selection.
2. GUI sends `YankSelection { pane_id, start, end, type }` to the daemon.
3. The daemon extracts text from the selection range across hot buffer and disk archive. For character and line selections, text is extracted in reading order with newlines between rows. For block selections, each row's selected columns are extracted with newlines between rows.
4. Daemon responds with `YankResponse { text }`.
5. GUI writes the text to the system clipboard and exits copy mode.

### Exit

1. User presses `Escape`/`q` (vim), `Ctrl+g` (emacs), or `Escape` (basic).
2. GUI deactivates the copy mode key table.
3. GUI sends `ExitCopyMode { pane_id }` to the daemon. The daemon removes the client from the pane's pinned viewport set and resumes normal scroll-on-output for that client.
4. GUI discards the `CopyModeState` for this pane. The viewport snaps to follow live output.

## Constraints

- **Cache size:** Default 3x visible rows. Configurable. Larger caches use more GUI memory but reduce `GetScrollback` requests.
- **Search latency:** Daemon search over disk-archived scrollback depends on archive size. The seek table (Spec-0004) provides O(log N) frame lookup for point queries. Sequential scan (full-text search) must decompress every frame: ~63μs per frame × frame count. At 64 KB uncompressed per frame and ~4.8 KB per row (200 cols × 24 bytes/cell), each frame holds ~13 rows. For 1M lines of scrollback, that is ~77K frames, ~4.8 seconds worst case. Incremental results must stream back to the GUI as frames are searched; the GUI highlights matches as they arrive. This makes streaming a requirement, not an optimization.
- **Yank size:** Maximum yank size is bounded by the wire protocol's 16 MiB frame limit. At ~100 bytes per line, this supports yanking ~160K lines in a single response.

## References

- [ADR 0012: Copy Mode Scrollback Access](../adrs/0012-copy-mode-scrollback-access.md)
- [ADR 0011: Keybind Dispatch](../adrs/0011-keybind-dispatch.md) — key tables
- [Spec 0001: Daemon Wire Protocol](0001-daemon-wire-protocol.md) — framing, scrollback and search messages
- [Spec 0003: Screen Buffer](0003-screen-buffer.md) — Selection struct, `i64` row coordinates
- [Spec 0004: Scroll Buffer & Archive](0004-scroll-buffer.md) — hot buffer, disk archive, seek table
- [03-multiplexer.md](../ideas/03-multiplexer.md) — copy mode keybinds and presets
