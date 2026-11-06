---
spec: '0008'
title: Copy Mode
status: accepted
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
    /// Row 0 = the live grid's top row when copy mode was entered — the
    /// row the daemon pins. Entering unscrolled, that is also the top of
    /// the visible area; entering from scrollback at offset N, the
    /// visible area is `[-N, -N + visible_rows)` and the cursor seeds at
    /// its bottom, so the client tracks the entry offset rather than
    /// assuming 0.
    /// Positive values = further down the live grid.
    /// Negative values = scrollback above it.
    cursor_row: i64,
    cursor_col: u16,

    /// Top row of the display window (see Display below), in the same
    /// row space. Starts at the entry page's top and never exceeds it.
    display_top: i64,

    /// Active selection, if any.
    selection: Option<CopySelection>,

    /// Cached scrollback rows from the daemon.
    cache: ViewportCache,

    /// Active search state, if any.
    search: Option<SearchState>,

    /// The painted viewport cells captured at entry (ADR-0025 clause 6):
    /// a dedicated copy, not the still-mutating saved live snapshot.
    /// Rows `[viewport_top, viewport_top + rows)` resolve here; rows
    /// below are daemon-served.
    snapshot: Vec<Vec<char>>,
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
    /// Cached rows, keyed by daemon row index. Every served row is
    /// stored, blank gap fills included, so the key range IS the cache
    /// window — deriving `start`/`end` from the keys keeps them from
    /// drifting out of step with the rows they describe. The pin is row
    /// 0 by definition, so it needs no field either.
    rows: BTreeMap<i64, CachedRow>,
}

struct SearchState {
    /// The search query.
    query: String,

    /// Direction `n` follows: the direction the search started in, or
    /// the last repeat key's.
    direction: SearchDirection,

    /// Whether the search bar is open and keys edit the query.
    editing: bool,

    /// Cursor position when the search started; `Escape` returns here.
    origin: (i64, u16),
}

/// A match on one row, end exclusive, found by the GUI in the row text
/// yank and motions read (see Search).
struct SearchMatch {
    row: i64,
    start_col: u16,
    end_col: u16,
}

enum SearchDirection {
    /// Toward newer rows (down).
    Forward,
    /// Toward older rows (up).
    Backward,
}
```

The preset enum (`Vim`, `Emacs`, `Basic`) lives with the config surface as the value of `copy_mode_keybinds` (Spec-0005), not in per-pane state.

### Key Tables

Copy mode activates a key table (ADR-0011). Unmatched keys are dropped, not forwarded to the PTY. The active preset is `oakterm.config.copy_mode_keybinds` (Spec-0005), read at each key press rather than frozen into per-pane state at entry, so a config reload switches tables immediately.

A configured leader chord outranks every preset row it collides with — ADR-0011 resolves the leader layer before the copy-mode table. With the template's suggested `leader = { key = "ctrl+b" }`, the vim preset's `Ctrl+b` (page up) and the emacs preset's `Ctrl+b` (cursor backward) arm the leader instead.

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

| Key          | Action                                                |
| ------------ | ----------------------------------------------------- |
| `Ctrl+n`     | Cursor down                                           |
| `Ctrl+p`     | Cursor up                                             |
| `Ctrl+f`     | Cursor forward one character                          |
| `Ctrl+b`     | Cursor backward one character                         |
| `Alt+f`      | Cursor to next word end                               |
| `Alt+b`      | Cursor to previous word start                         |
| `Ctrl+a`     | Cursor to start of line                               |
| `Ctrl+e`     | Cursor to end of line                                 |
| `Alt+<`      | Cursor to top of scrollback                           |
| `Alt+>`      | Cursor to bottom                                      |
| `Ctrl+v`     | Page down                                             |
| `Alt+v`      | Page up                                               |
| `Ctrl+Space` | Start/toggle selection                                |
| `Alt+w`      | Yank selection to clipboard, exit copy mode           |
| `Ctrl+g`     | Exit copy mode                                        |
| `Escape`     | Exit copy mode (clear selection if active, else exit) |
| `Ctrl+s`     | Start forward search                                  |
| `Ctrl+r`     | Start backward search                                 |

`Alt+f` lands on the last character of the word, matching tmux's emacs
copy mode (`next-word-end`) rather than emacs's point-after-word. `Escape`
follows the vim row's clear-then-exit shape so every preset keeps a
universal way out.

#### Basic Preset

| Key                                                      | Action                                                |
| -------------------------------------------------------- | ----------------------------------------------------- |
| `Up` / `Down` / `Left` / `Right`                         | Cursor movement                                       |
| `Page Up` / `Page Down`                                  | Page movement                                         |
| `Home`                                                   | Top of scrollback                                     |
| `End`                                                    | Bottom                                                |
| `Shift+Up` / `Shift+Down` / `Shift+Left` / `Shift+Right` | Extend selection                                      |
| `Ctrl+c`                                                 | Copy selection to clipboard, exit copy mode           |
| `Escape`                                                 | Exit copy mode (clear selection if active, else exit) |
| `Ctrl+f`                                                 | Start backward search                                 |

Unshifted movement clears an active selection before moving; any shifted movement extends one, anchoring at the cursor if none is active. This is the GUI convention the preset mirrors — a plain arrow deselects, `Shift+Page Down` selects a page from a bare caret.

### Protocol Messages

Copy mode uses these wire protocol messages (see Spec-0001 for framing):

| msg_type | Name                | Direction | Serial   | Payload                                                                                                                                 |
| -------- | ------------------- | --------- | -------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `0x97`   | EnterCopyMode       | C→D       | Request  | `pane_id: u32`, `base: u64` — the daemon-published base bound to the painted content (ADR-0025)                                         |
| `0x9B`   | EnterCopyModeAck    | D→C       | Response | `pane_id: u32`, `base: u64` — the accepted base echoed back; an out-of-range base gets `Error` (`InvalidMessage`) at the serial instead  |
| `0x98`   | ExitCopyMode        | C→D       | Push (0) | `pane_id: u32`                                                                                                                          |
| `0x9C`   | CopyModeInvalidated | D→C       | Push (0) | `pane_id: u32` — a resize dropped this client's pin; exit and discard in-flight fills                                                   |
| `0x99`   | YankSelection       | C→D       | Request  | `pane_id: u32`, `start_row: i64`, `start_col: u16`, `end_row: i64`, `end_col: u16`, `selection_type: u8` (0=character, 1=line, 2=block) |
| `0x9A`   | YankResponse        | D→C       | Response | `text_len: u32`, `text: UTF-8`                                                                                                          |

`GetScrollback` (0x73) keeps its payload; `ScrollbackData` (0x74) gains `base: u64`, the history length the served rows were resolved against — a pinned client discards a fill whose `base` differs from its acknowledged pin (ADR-0025 clause 5).

Existing messages reused without modification to their payloads:

- `FindPrompt` (0x75) / `PromptPosition` (0x76): prompt jumps.

Copy mode does not use `SearchScrollback` (0x77) or its companions: search runs in the GUI over fetched rows (see Search).

`GetScrollback` and `FindPrompt` resolve their `i64` offsets against the sending client's pin while it holds one, so all three of cache fills, prompt jumps, and yanks share one origin. See Spec-0001's copy-mode addendum.

**The client's viewport offset is never authoritative for daemon coordinates.** It is a client-side concept the daemon never receives, it names the page a client _requested_ rather than the one it has painted, and treating it as an origin has produced the same defect at three layers: a pin that ignored it, a spec sentence claiming the daemon records it, and coordinates seeded from a page not yet on screen. A client entering copy mode from scrollback translates its own offset into the daemon's space; it never assumes the daemon shares it.

### Row Coordinate Space

Every row that has ever scrolled off the live grid holds a stable absolute index for the life of the pane, counted from the first row ever pushed. The daemon derives it from a monotonic push counter, not from the archive's row count: panes without a disk archive discard pruned rows, and an index derived from `archived + hot_buffer_len` would slide backwards as they were dropped, silently re-pointing a pinned row at whatever later occupied that slot.

A pinned client's copy-mode row `n` is absolute row `pin + n`. Absolute indices resolve, in ascending order, to:

1. **Disk archive** — rows the archive captured.
2. **Gap** — rows pruned from the hot buffer while no archive was attached, or dropped by the archive under load (Spec-0004 overload policy). These are permanently gone and read as blank rows; they keep their indices so later rows do not shift.
3. **Hot buffer** — retained scrollback rows.
4. **Live grid** — rows still on screen, at `pushed + grid_row`.

A `Resize` that shrinks the grid moves rows between the live grid and scrollback. Rows already in scrollback keep their absolute indices across it; rows still on the live grid do not, because the grid captures its trailing rows into scrollback. The daemon tracks this as the **resize watermark**: the history length after the last resize that advanced it, updated whether or not pins existed at the time, and monotonic because grow never reclaims rows from scrollback. An `EnterCopyMode` base below the watermark names coordinates a resize capture invalidated and is refused. When a resize drops existing pins, the daemon pushes `CopyModeInvalidated` to each affected client, which exits copy mode (the exit is what closes the refusal window below), discards in-flight fills, and rings — the initiating client's local teardown is unchanged; clients re-enter copy mode after a resize they initiated. From the drop until that exit lands, every read the client could issue against its dead pin (`GetScrollback`, `FindPrompt`, search operations, `YankSelection`) is refused with `Error` (`InvalidMessage`) rather than resolved against live coordinates — see Spec-0001's copy-mode section.

## Behavior

### Entry

1. User presses `oak_mod + [` (or configured keybind). Entry on the alternate screen is refused client-side (ADR-0025 clause 9): splicing primary history beneath alt content has no coherent display.
2. GUI activates the copy mode key table for the focused pane and takes the **entry snapshot**: a dedicated copy of the painted viewport cells — not the still-mutating saved live snapshot.
3. GUI sends `EnterCopyMode { pane_id, base }` as a serial-correlated request. `base` is the daemon-published history length bound to the painted content: the last applied `RenderUpdate.history_len` at offset 0, the serving `ScrollbackData.base` when the page is entirely scrollback. A partially scrolled page mixes both sources; entry requires the two bases to coincide, otherwise the client refetches the scrollback page (one round trip) and enters once the page is single-instant. The daemon validates `resize_watermark <= base <= history_len()` before mutating anything, pins the client at the echoed base on success, and answers with the ack — or an `Error` (`InvalidMessage`) that leaves any prior pin untouched, after which the client abandons entry and may retry after the next painted update. `EnterCopyMode` from a client that is already pinned on that pane re-pins at the newly echoed base — an implicit exit plus enter — since the re-entering client refills its cache anyway.
4. Only after the ack does the GUI send `GetScrollback { pane_id, start_row, count }` to fill the initial cache (the frozen viewport plus one screen either side, capped at row 0).
5. Cursor starts at the bottom-left of the visible area: `(cursor_row = viewport_top + rows - 1, cursor_col = 0)`, where `rows` is the pane's visible row count and `viewport_top` is `-offset` for a client entering at scroll offset `offset`. Entering unscrolled this is `rows - 1`. Copy mode addresses exactly the painted page and the history above it: rows `[viewport_top, viewport_top + rows)` resolve from the entry snapshot, rows below `viewport_top` are daemon-served, and nothing below the page is addressable (ADR-0025 clause 6).

### Cursor Movement

All cursor movement is local to the GUI. No IPC per keystroke.

- Cursor position is clamped to valid row/col ranges within the cached rows.
- When the cursor moves past the top or bottom of the cache, the GUI sends `GetScrollback` to fetch the next chunk.
- **Prefetch:** When the display window (see Display) enters the top or bottom 25% of the cache window, the GUI starts fetching the next chunk in the background to hide latency.
- Word boundaries for `w`/`b`/`e` are defined as transitions between alphanumeric/underscore characters and everything else (matching vim's `iskeyword` default).

**As built (TREK-112).** `gg` and `Ctrl+b` clamp to the oldest row the cache currently holds rather than jumping to the true top of scrollback; reaching rows the GUI has not fetched needs a fetch-then-jump, which is deferred to a follow-up task. Word motions stop at the same edge for the same reason: they resolve against text the GUI holds, so an unfetched row ends the scan.

### Display

Copy mode draws the rows `[display_top, display_top + rows)`, where `rows` is the pane's visible row count.

- **The window follows the cursor.** It starts at `viewport_top` and moves only as far as needed to keep the cursor inside it: a cursor above the window pulls `display_top` up to the cursor's row, and a cursor below it pulls the window down. The window never moves below the entry page (`display_top <= viewport_top`), because nothing below the page is addressable.
- **Row sources.** A row the cache holds draws from it, with its colors, the same source yank and motions read. Other rows on the entry page draw from the frozen page, and a history row not yet cached draws blank until its fill lands. The frozen page can carry a scroll indicator over history rows, and the cache keeps it from showing.
- **Cursor.** The live cursor is hidden. A non-blinking block cursor marks the copy-mode cursor.
- **Selection.** The selection highlights exactly the cells a yank would take, for character, line and block selections alike. It stays drawn while another pane has focus, since it is state the user built rather than a transient drag.
- **Prefetch** keys off the display window rather than the cursor, so scrolling the window fetches ahead the same way cursor movement does.

### Mouse

While copy mode is active, mouse clicks and pointer motion reach neither the application nor the host selection, Shift+click included: the copy-mode selection is the pane's only selection. Entering copy mode drops any host selection and ends a drag in progress. The wheel scrolls the display window by its usual step, clamped as above, and the cursor clamps into the window it leaves on screen. Leaving copy mode is a key action, never a side effect of scrolling.

### Selection

- Starting a selection sets the anchor at the current cursor position.
- Moving the cursor extends the selection from anchor to cursor.
- **Character selection:** All cells between anchor and cursor in reading order (row-major).
- **Line selection:** All complete rows between anchor row and cursor row (inclusive).
- **Block selection:** The rectangular region defined by anchor and cursor corners.
- Toggling the same selection type cancels the selection. Toggling a different type switches the selection type.

### Search

Search runs in the GUI over the rows copy mode can address, reading each row the way yank and motions do (the cache first, then the entry page), so a match is always the text a yank would take. The daemon's search messages are not used: their state is shared by every client on the pane and their results carry no columns.

1. **Start.** `/` (vim) and `Ctrl+s` (emacs) start a forward search, toward newer rows; `?` (vim), `Ctrl+r` (emacs) and `Ctrl+f` (basic) start a backward one, toward older rows. Starting opens the search bar and records where the cursor stood.
2. **The bar.** A one-row bar floats at the top right of the pane, over the display window, showing the query and the position among matches found so far (`3/17`, with `+` while history in the search direction is still unscanned). It moves to the bottom right while the cursor is on the window's top row. The bar is drawn over the display, not part of it: yanks, motions and the a11y rows never read its text.
3. **Typing.** Printable keys edit the query and `Backspace` deletes; the preset's key table is bypassed while the bar is open. After each edit the cursor jumps to the nearest match from where the search started, in its direction. Matching is literal and smart-case, the same rule the daemon's search uses: case-insensitive unless the query has an uppercase letter.
4. **Closing.** `Enter` closes the bar and leaves the cursor on the match; a scan still reading history finishes and moves the cursor when it finds one. `Escape` closes the bar, stops any scan, and returns the cursor to where the search started, refetching that row's window when the cache no longer holds it. Either way the query stays: matches remain highlighted until copy mode exits or a new search starts.
5. **Next and previous.** `n` moves to the next match in the search's direction and `N` to the previous one (vim); with the bar open, the preset's start keys search on in their own direction and the search keeps that direction (`Ctrl+s`/`Ctrl+r` in emacs, `Ctrl+f` in basic). When no held row has a further match, the GUI fetches the next chunk of history in that direction (`GetScrollback`, up to the request limit) and scans it without caching it; the cursor stays put meanwhile. A chunk holding a match joins the cache, replacing it when the two do not touch, and the cursor moves there; otherwise the next chunk is fetched. Rows the display window shows but the cache lacks are fetched before the cache grows at its edges. The search ends at the oldest history row going back, or at the bottom of the entry page going forward. A miss from `n`, `N` or a repeat key rings the bell; a miss while typing does not, since the query is still changing.
6. **Highlights.** Every match in the display window is highlighted; the selection paints over a match where they overlap.

### Yank

1. User presses `y` (vim), `Alt+w` (emacs), or `Ctrl+c` (basic) with an active selection.
2. The selection resolves under the ownership split (ADR-0025 clause 7). Wholly at or above 0: the text extracts from the entry snapshot, lands on the clipboard, and copy mode exits — no daemon round trip. Wholly below 0: the GUI sends `YankSelection { pane_id, start, end, type }` as before. Spanning the boundary: the range splits at row −1/0 — the daemon half runs with its end at (−1, full row width) for character and line selections while the client applies the true end column on its half, block selections apply the same column rectangle to both halves, and the halves join with `\n` matching the daemon's row join.
3. The daemon extracts text from the selection range across the disk archive and the hot buffer (see Row Coordinate Space). The daemon-side live-grid tier remains reachable for non-copy-mode reads, but a 1.5 copy-mode client's daemon half never reaches row 0 — its painted-page rows come from the entry snapshot. For character and line selections, text is extracted in reading order with newlines between rows. For block selections, each row's selected columns are extracted with newlines between rows. Each row's text is trimmed of trailing spaces; rows falling in a gap contribute an empty line so line N of the result is always row N of the range.
4. Daemon responds with `YankResponse { text }`.
5. GUI writes the text to the system clipboard and exits copy mode.

**Selection range semantics.** Both endpoints are inclusive cell coordinates, matching Spec-0003's `Selection`. A range whose endpoints arrive out of order is normalized rather than rejected: character and line selections normalize in reading order, while a block normalizes each axis independently, so any drag direction yields the same rectangle. An endpoint outside available history opens the selection to that row's edge — a start before the oldest retained row begins at column 0, and an end past the last live grid row runs to the end of the last real row — because the column it named belongs to a row that does not exist. Block selections keep their column range when the row span is trimmed; the columns define the rectangle on every row, not an endpoint on the last one.

**Row `>= 0` asymmetry.** A well-behaved 1.5 client never sends the daemon a `YankSelection` reaching row 0 or above — those rows resolve from its entry snapshot. The daemon-side live-grid tier remains for non-copy-mode reads, and `GetScrollback` still clamps its window to history and never returns grid rows.

### Exit

1. User presses `Escape`/`q` (vim), `Ctrl+g` (emacs), or `Escape` (basic).
2. GUI deactivates the copy mode key table.
3. GUI sends `ExitCopyMode { pane_id }` to the daemon. The daemon removes the client from the pane's pinned viewport set and resumes normal scroll-on-output for that client.
4. GUI discards the `CopyModeState` for this pane. The viewport snaps to follow live output.

## Constraints

- **Cache size:** Default 3x visible rows. Configurable. Larger caches use more GUI memory but reduce `GetScrollback` requests.
- **Search latency:** A search that runs past the held rows costs one `GetScrollback` round trip per chunk scanned, so a match N rows away costs about N / chunk size round trips, and a miss scans all history in its direction. Chunks reaching into the disk archive pay its read cost (Spec-0004). The cursor jumps as soon as a fill holds a match, so a near match never waits on the rest of history.
- **Yank size:** Maximum yank size is bounded by the wire protocol's 16 MiB frame limit. At ~100 bytes per line, this supports yanking ~160K lines in a single response.

## References

- [ADR 0012: Copy Mode Scrollback Access](../adrs/0012-copy-mode-scrollback-access.md)
- [ADR 0011: Keybind Dispatch](../adrs/0011-keybind-dispatch.md) — key tables
- [Spec 0001: Daemon Wire Protocol](0001-daemon-wire-protocol.md) — framing, scrollback and search messages
- [Spec 0003: Screen Buffer](0003-screen-buffer.md) — Selection struct, `i64` row coordinates
- [Spec 0004: Scroll Buffer & Archive](0004-scroll-buffer.md) — hot buffer, disk archive, seek table
- [03-multiplexer.md](../ideas/03-multiplexer.md) — copy mode keybinds and presets
