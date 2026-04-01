---
spec: '0002'
title: VT Parser & Terminal Handler
status: implementing
date: 2026-03-26
adrs: ['0004', '0008']
tags: [core]
---

# 0002. VT Parser & Terminal Handler

## Overview

Defines the VT parser state machine, the terminal handler interface, and the supported escape sequence inventory for OakTerm Phase 0. The parser reads bytes from the PTY and dispatches parsed sequences to the handler. The handler interprets sequences and mutates the screen buffer. Together they implement the `xterm-256color` terminal type with Kitty graphics (ADR-0004) and OSC 133/7 shell integration (ADR-0008).

## Contract

### Parser State Machine

The parser implements the Paul Williams VT parser model (14 states) with one modification: APC sequences are captured instead of ignored, to support the Kitty graphics protocol.

**States:**

| State               | Entry Action | Purpose                                                                                         |
| ------------------- | ------------ | ----------------------------------------------------------------------------------------------- |
| ground              | —            | Normal operation. Prints characters, executes C0 controls.                                      |
| escape              | clear        | Entered on ESC. Routes to escape_intermediate, csi_entry, dcs_entry, osc_string, or apc_string. |
| escape_intermediate | —            | Collects intermediate bytes during escape sequences.                                            |
| csi_entry           | clear        | Entered on CSI (ESC [). Routes to csi_param, csi_intermediate, or csi_ignore.                   |
| csi_param           | —            | Accumulates numeric parameters and semicolons.                                                  |
| csi_intermediate    | —            | Collects intermediate bytes after CSI params.                                                   |
| csi_ignore          | —            | Consumes malformed CSI sequences without dispatching.                                           |
| dcs_entry           | clear        | Entered on DCS (ESC P).                                                                         |
| dcs_param           | —            | Accumulates DCS parameters.                                                                     |
| dcs_intermediate    | —            | Collects intermediates for DCS.                                                                 |
| dcs_passthrough     | hook         | Passes bytes to device-specific handler. Exit: unhook.                                          |
| dcs_ignore          | —            | Consumes malformed DCS.                                                                         |
| osc_string          | osc_start    | Entered on OSC (ESC ]). Passes bytes via osc_put. Exit: osc_end.                                |
| apc_string          | apc_start    | Entered on APC (ESC \_). Captures content for Kitty graphics. Exit: apc_end.                    |

**Modification from Paul Williams model:** The standard model lumps SOS, PM, and APC into a single `sos_pm_apc_string` state that ignores all content. OakTerm splits APC into its own state (`apc_string`) that captures content and dispatches it to the handler. SOS and PM remain ignored.

**Anywhere transitions:** CAN (0x18), SUB (0x1A), and ESC cancel the current sequence from any state and return to ground (or escape for ESC).

**Parser implementation:** Table-driven. A `[256][State] -> (Action, NextState)` lookup table computed at compile time. The `vte` crate (v0.15+) or equivalent provides this foundation; APC handling extends it.

### Perform Trait

The low-level interface between the parser state machine and the handler. The parser calls these methods as it recognizes sequence boundaries.

```rust
trait Perform {
    /// Printable character in ground state.
    fn print(&mut self, c: char);

    /// C0 or C1 control byte in ground state.
    fn execute(&mut self, byte: u8);

    /// CSI sequence complete. `action` is the final byte (0x40-0x7E).
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char);

    /// ESC sequence complete. `byte` is the final byte.
    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8);

    /// OSC sequence complete. `params` are semicolon-delimited byte slices.
    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool);

    /// DCS sequence started.
    fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char);

    /// DCS data byte.
    fn put(&mut self, byte: u8);

    /// DCS sequence ended.
    fn unhook(&mut self);

    /// APC sequence started (Kitty graphics).
    fn apc_start(&mut self);

    /// APC data byte.
    fn apc_put(&mut self, byte: u8);

    /// APC sequence ended. Handler processes buffered APC content.
    fn apc_end(&mut self);
}
```

The `Params` type holds parsed numeric parameters from CSI/DCS sequences. Parameters are separated by `;` (semicolon) or `:` (colon, for sub-parameters like SGR 38:2:R:G:B). Missing parameters default to 0.

### Handler Interface

The high-level interface that interprets parsed sequences and mutates terminal state. Organized by category. All methods have default no-op implementations to enable incremental development.

#### Character Output

```rust
/// Write character at cursor position, advance cursor.
fn input(&mut self, c: char);

/// Repeat last printed character `count` times (REP, CSI Ps b).
fn repeat_last_char(&mut self, count: usize);
```

#### Cursor Movement

```rust
fn goto(&mut self, row: usize, col: usize);     // CUP: CSI Ps;Ps H
fn goto_line(&mut self, row: usize);             // VPA: CSI Ps d
fn goto_col(&mut self, col: usize);              // CHA: CSI Ps G / HPA: CSI Ps `
fn move_up(&mut self, count: usize);             // CUU: CSI Ps A
fn move_down(&mut self, count: usize);           // CUD: CSI Ps B
fn move_forward(&mut self, count: usize);        // CUF: CSI Ps C
fn move_backward(&mut self, count: usize);       // CUB: CSI Ps D
fn move_down_and_cr(&mut self, count: usize);    // CNL: CSI Ps E
fn move_up_and_cr(&mut self, count: usize);      // CPL: CSI Ps F
fn save_cursor_position(&mut self);              // DECSC: ESC 7
fn restore_cursor_position(&mut self);           // DECRC: ESC 8
```

#### Erasure

```rust
/// ED: CSI Ps J (0=below, 1=above, 2=all, 3=saved lines)
fn clear_screen(&mut self, mode: ClearMode);

/// EL: CSI Ps K (0=right, 1=left, 2=all)
fn clear_line(&mut self, mode: LineClearMode);

/// ECH: CSI Ps X
fn erase_chars(&mut self, count: usize);
```

#### Insertion and Deletion

```rust
fn insert_blank_chars(&mut self, count: usize);  // ICH: CSI Ps @
fn delete_chars(&mut self, count: usize);         // DCH: CSI Ps P
fn insert_blank_lines(&mut self, count: usize);   // IL: CSI Ps L
fn delete_lines(&mut self, count: usize);         // DL: CSI Ps M
```

#### Scrolling

```rust
fn scroll_up(&mut self, count: usize);            // SU: CSI Ps S
fn scroll_down(&mut self, count: usize);          // SD: CSI Ps T
fn reverse_index(&mut self);                       // RI: ESC M
fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>);  // DECSTBM: CSI Ps;Ps r
```

#### Tabs

```rust
fn put_tab(&mut self, count: usize);              // HT: 0x09
fn move_backward_tabs(&mut self, count: usize);   // CBT: CSI Ps Z
fn set_horizontal_tabstop(&mut self);             // HTS: ESC H
fn clear_tabs(&mut self, mode: TabulationClearMode);  // TBC: CSI Ps g
```

#### Attributes (SGR)

```rust
/// SGR: CSI Ps m. Called once per attribute in the sequence.
fn set_attribute(&mut self, attr: Attr);
```

The `Attr` enum covers all SGR attributes:

```rust
enum Attr {
    Reset,
    Bold,
    Dim,
    Italic,
    Underline(UnderlineStyle),   // 4:0 off, 4:1 single, 4:2 double, 4:3 curly, 4:4 dotted, 4:5 dashed
    Blink,
    Inverse,
    Hidden,
    Strikethrough,
    Overline,
    CancelBold,
    CancelDim,
    CancelItalic,
    CancelUnderline,
    CancelBlink,
    CancelInverse,
    CancelHidden,
    CancelStrikethrough,
    CancelOverline,
    Foreground(Color),
    Background(Color),
    UnderlineColor(Option<Color>),  // SGR 58/59
}

enum UnderlineStyle { None, Single, Double, Curly, Dotted, Dashed }

enum Color {
    Default,
    Named(NamedColor),       // 0-7 standard, 8-15 bright
    Indexed(u8),             // 0-255 palette
    Rgb(u8, u8, u8),         // True color
}
```

#### Mode Management

```rust
/// DECSET: CSI ? Ps h
fn set_private_mode(&mut self, mode: PrivateMode);

/// DECRST: CSI ? Ps l
fn reset_private_mode(&mut self, mode: PrivateMode);

/// SM: CSI Ps h
fn set_mode(&mut self, mode: AnsiMode);

/// RM: CSI Ps l
fn reset_mode(&mut self, mode: AnsiMode);
```

#### Screen Buffer

```rust
/// Switch to alternate screen buffer.
/// Three modes with different semantics:
/// - Mode 47: switch only (no cursor save, no clear).
/// - Mode 1047: switch and clear alternate on enter.
/// - Mode 1049: save cursor on primary, switch, clear alternate on enter.
///   On exit (DECRST 1049), restore cursor.
fn enter_alternate_screen(&mut self);

/// Return to primary screen buffer.
fn leave_alternate_screen(&mut self);

/// DECALN: ESC # 8 (fill screen with 'E' for alignment test).
fn decaln(&mut self);
```

#### Charset

```rust
fn set_active_charset(&mut self, index: CharsetIndex);
fn configure_charset(&mut self, index: CharsetIndex, charset: StandardCharset);
```

#### Terminal State

```rust
fn reset_state(&mut self);                         // RIS: ESC c
fn soft_reset(&mut self);                          // DECSTR: CSI ! p
fn set_cursor_style(&mut self, style: CursorStyle);  // DECSCUSR: CSI Ps SP q
fn set_keypad_application_mode(&mut self);         // DECKPAM: ESC =
fn unset_keypad_application_mode(&mut self);       // DECKPNM: ESC >
```

#### Device Communication

```rust
/// DA1: CSI c — respond with device attributes.
fn identify_terminal(&mut self, writer: &mut dyn Write);

/// DA2: CSI > c — respond with secondary device attributes.
fn secondary_device_attributes(&mut self, writer: &mut dyn Write);

/// DSR: CSI 6 n — respond with cursor position report.
fn device_status(&mut self, writer: &mut dyn Write, mode: usize);
```

#### Titles

```rust
fn set_title(&mut self, title: &str);              // OSC 0/2
fn set_icon_name(&mut self, name: &str);           // OSC 1
fn push_title(&mut self);                          // CSI 22;0 t
fn pop_title(&mut self);                           // CSI 23;0 t
```

#### Clipboard

```rust
/// OSC 52: clipboard store. `clipboard` is 'c' (system) or 'p' (primary).
fn clipboard_store(&mut self, clipboard: char, data: &[u8]);

/// OSC 52: clipboard load. Respond with base64-encoded content.
fn clipboard_load(&mut self, clipboard: char, writer: &mut dyn Write);
```

#### Hyperlinks

```rust
/// OSC 8: start hyperlink. `params` is key=value pairs, `uri` is the URL.
fn set_hyperlink(&mut self, params: &str, uri: &str);

/// OSC 8: end hyperlink (empty URI).
fn clear_hyperlink(&mut self);
```

#### Bell

```rust
fn bell(&mut self);   // BEL: 0x07
```

#### C0 Controls

```rust
fn backspace(&mut self);        // BS: 0x08
fn carriage_return(&mut self);  // CR: 0x0D
fn linefeed(&mut self);         // LF: 0x0A
fn newline(&mut self);          // NEL: ESC E
fn substitute(&mut self);       // SUB: 0x1A (print replacement char)
```

#### Shell Integration (ADR-0008)

```rust
/// OSC 133;A — prompt start. Marks current row with SemanticMark::PromptStart.
/// Raw parameter string preserved for future use (e.g., iTerm2's `aid=` parameter).
fn shell_prompt_start(&mut self, raw_params: &str);

/// OSC 133;B — input start. Marks current row with SemanticMark::InputStart.
fn shell_input_start(&mut self);

/// OSC 133;C — command output start. Marks current row with SemanticMark::OutputStart.
fn shell_output_start(&mut self);

/// OSC 133;D — command finished. Marks current row with SemanticMark::OutputEnd.
/// `exit_code` is None if not provided.
fn shell_command_finished(&mut self, exit_code: Option<i32>);

/// OSC 7 — current working directory.
fn set_working_directory(&mut self, uri: &str);
```

#### Kitty Graphics (ADR-0004)

```rust
/// APC sequence identified as Kitty graphics command.
/// `control_data` is the parsed key=value pairs.
/// `payload` is the base64-decoded binary data (accumulated across chunks).
fn kitty_graphics(&mut self, command: KittyGraphicsCommand, writer: &mut dyn Write);
```

```rust
struct KittyGraphicsCommand {
    action: KittyAction,          // T, t, p, d, q
    format: KittyFormat,          // 24, 32, 100
    image_id: Option<u32>,
    placement_id: Option<u32>,
    source_width: Option<u32>,
    source_height: Option<u32>,
    display_cols: Option<u32>,
    display_rows: Option<u32>,
    z_index: Option<i32>,
    quiet: u8,                    // 0, 1, 2
    payload: Vec<u8>,             // decoded binary data
}

enum KittyAction { TransmitAndDisplay, Transmit, Place, Delete, Query }
enum KittyFormat { Rgb24, Rgba32, Png }
```

**APC identification:** APC sequences whose first byte is `G` (0x47) are Kitty graphics commands. The handler parses the remaining content as comma-separated `key=value` pairs terminated by a semicolon, followed by base64-encoded payload data. APC sequences with any other leading byte are silently ignored.

Phase 0 supports:

- Actions: `T` (transmit+display), `t` (transmit), `p` (place), `d` (delete), `q` (query)
- Formats: `f=24` (RGB), `f=32` (RGBA, default), `f=100` (PNG)
- Transmission: `t=d` (direct/inline) with chunking (`m=0/1`)
- Placement: cursor position with `c,r` scaling
- Compression: `o=z` (zlib)

Deferred to later phases: animation (`a=f/a/c`), file/shared-memory transmission (`t=f/s`), Unicode virtual placements (`U=1`).

#### Colors

```rust
/// OSC 4: set palette color at index.
fn set_palette_color(&mut self, index: usize, color: Rgb);

/// OSC 104: reset palette color at index to default.
fn reset_palette_color(&mut self, index: usize);

/// OSC 10/11/12: set foreground/background/cursor color.
fn set_dynamic_color(&mut self, target: DynamicColorTarget, color: Rgb);

/// OSC 110/111/112: reset dynamic color to default.
fn reset_dynamic_color(&mut self, target: DynamicColorTarget);

/// OSC 10/11/12 query: respond with current color.
fn query_dynamic_color(&mut self, target: DynamicColorTarget, writer: &mut dyn Write);
```

### Handler Types

All types referenced by handler method signatures.

```rust
enum ClearMode {
    Below,       // ED 0
    Above,       // ED 1
    All,         // ED 2
    SavedLines,  // ED 3 (clear scrollback)
}

enum LineClearMode {
    Right,  // EL 0
    Left,   // EL 1
    All,    // EL 2
}

enum TabulationClearMode {
    Current,  // TBC 0
    All,      // TBC 3
}

enum CursorStyle {
    BlinkingBlock,     // DECSCUSR 0 or 1
    SteadyBlock,       // DECSCUSR 2
    BlinkingUnderline,  // DECSCUSR 3
    SteadyUnderline,   // DECSCUSR 4
    BlinkingBar,       // DECSCUSR 5
    SteadyBar,         // DECSCUSR 6
}

enum PrivateMode {
    CursorKeys,          // 1 (DECCKM)
    OriginMode,          // 6 (DECOM)
    AutoWrap,            // 7 (DECAWM)
    CursorBlink,         // 12
    CursorVisible,       // 25 (DECTCEM)
    AltScreenLegacy,     // 47
    AppKeypad,           // 66 (DECNKM)
    MouseClick,          // 1000
    MouseCellMotion,     // 1002
    MouseAllMotion,      // 1003
    FocusEvents,         // 1004
    MouseUtf8,           // 1005
    MouseSgr,            // 1006
    AlternateScroll,     // 1007
    AltScreenSaveCursor, // 1049
    BracketedPaste,      // 2004
    SynchronizedOutput,  // 2026
}

enum AnsiMode {
    Insert,       // 4 (IRM)
    AutoNewline,  // 20 (LNM)
}

enum CharsetIndex { G0, G1, G2, G3 }

enum StandardCharset {
    Ascii,             // B
    LineDrawing,       // 0 (DEC Special Character and Line Drawing)
    British,           // A
    Dutch,             // 4
    Finnish,           // C or 5
    French,            // R
    German,            // K
    Italian,           // Y
    Spanish,           // Z
    Swedish,           // H or 7
    Swiss,             // =
}

struct Rgb { r: u8, g: u8, b: u8 }

enum DynamicColorTarget {
    Foreground,   // OSC 10 / 110
    Background,   // OSC 11 / 111
    CursorColor,  // OSC 12 / 112
}

enum SemanticMark {
    None,
    PromptStart,  // OSC 133;A
    InputStart,   // OSC 133;B
    OutputStart,  // OSC 133;C
    OutputEnd,    // OSC 133;D
}
```

### Supported DEC Private Modes

| Mode | Name                      | Phase 0 | Behavior                                                                |
| ---- | ------------------------- | ------- | ----------------------------------------------------------------------- |
| 1    | DECCKM                    | Yes     | Application cursor keys (changes arrow key encoding)                    |
| 6    | DECOM                     | Yes     | Origin mode (cursor addressing relative to scroll region)               |
| 7    | DECAWM                    | Yes     | Auto-wrap mode (wrap at end of line)                                    |
| 12   | Cursor blink              | Yes     | Toggle cursor blink                                                     |
| 25   | DECTCEM                   | Yes     | Cursor visibility                                                       |
| 47   | Alternate screen (legacy) | Yes     | Switch to/from alternate screen buffer (no cursor save, no clear)       |
| 66   | DECNKM                    | Yes     | Application keypad mode                                                 |
| 1000 | Mouse click tracking      | Yes     | Report mouse button press/release                                       |
| 1002 | Mouse cell motion         | Yes     | Report mouse motion while button held                                   |
| 1003 | Mouse all motion          | Yes     | Report all mouse motion                                                 |
| 1004 | Focus events              | Yes     | Report focus in/out                                                     |
| 1005 | UTF-8 mouse               | Yes     | UTF-8 encoded mouse coordinates (legacy)                                |
| 1006 | SGR mouse                 | Yes     | SGR-encoded mouse coordinates (modern)                                  |
| 1007 | Alternate scroll          | Yes     | Mouse wheel scrolls alternate screen                                    |
| 1047 | Alternate screen          | Yes     | Switch to/from alternate screen buffer (no cursor save, clear on enter) |
| 1049 | Alt screen + save cursor  | Yes     | Composite: save cursor, switch to alt screen, clear on enter            |
| 2004 | Bracketed paste           | Yes     | Wrap pasted text in ESC [200~ / ESC [201~                               |
| 2026 | Synchronized output       | Yes     | Buffer output until mode reset, then flush                              |

### Supported ANSI Modes (SM/RM)

| Mode | Name | Phase 0 | Behavior                          |
| ---- | ---- | ------- | --------------------------------- |
| 4    | IRM  | Yes     | Insert mode (insert vs overwrite) |
| 20   | LNM  | Yes     | Automatic newline (LF implies CR) |

## Behavior

### UTF-8 Handling

The parser accepts UTF-8 encoded input. Incomplete multi-byte sequences at buffer boundaries are accumulated and completed across `advance()` calls. Invalid UTF-8 sequences produce U+FFFD (replacement character) in the terminal grid.

### Wide Characters

Characters with East Asian Width property W (wide) or F (fullwidth) occupy two cells. The first cell stores the character. The second cell is a continuation marker (wide_cont flag). Cursor advances by 2 columns after a wide character. Writing to either cell of a wide character clears both cells.

### Alternate Screen Buffer

Three modes control the alternate screen, differing in cursor save/restore and clear behavior:

- **Mode 47 (legacy):** DECSET 47 switches to the alternate buffer. DECRST 47 switches back. No cursor save/restore, no clear on enter.
- **Mode 1047:** DECSET 1047 switches to the alternate buffer and clears it. DECRST 1047 switches back and clears the alternate.
- **Mode 1049:** DECSET 1049 saves the cursor (DECSC), switches to the alternate buffer, and clears it. DECRST 1049 switches back and restores the cursor (DECRC).

All modes preserve the primary buffer's content. Per ADR-0006, lines that scroll off the top of the alternate screen are captured to the primary scrollback if `save_alternate_scrollback` is enabled.

If the terminal is already on the alternate screen and receives a DECSET for the same or different alternate mode, the behavior depends on the mode: 1049 unconditionally saves the cursor and clears; 47 and 1047 are no-ops if already on the alternate screen.

### Synchronized Output (DEC 2026)

When set, the handler buffers screen mutations without marking the screen buffer as dirty. When reset, all buffered mutations are applied and the screen buffer is marked dirty once. This prevents partial rendering of atomic screen updates. The daemon's wire protocol layer (Spec-0001) translates dirty-buffer signals into `DirtyNotify` messages.

### Unknown Sequences

Unknown CSI final bytes, unknown DEC modes, and unknown OSC numbers are silently ignored. The parser logs them at debug level. Unknown sequences never produce visible artifacts or terminal state changes.

### Malformed Sequences

The parser's `csi_ignore` and `dcs_ignore` states consume malformed sequences without dispatching. Malformed OSC/APC sequences (missing ST terminator) are terminated by the next recognized sequence start (ESC, CAN, SUB) or after a configurable maximum length (default: 4 KiB for OSC, 64 MiB for APC/Kitty graphics).

### Kitty Graphics Chunking

When `m=1` is received, the handler accumulates the payload. When `m=0` is received, the handler processes the complete image from all accumulated chunks. If a non-graphics APC sequence arrives mid-chunk, the accumulated data is discarded.

### OSC 133 Storage

Shell integration marks are stored as metadata on the screen buffer row where the mark was received. Marks persist through scrollback. The marks carry no visual representation in Phase 0 — they are data annotations consumed by Phase 1 features (scroll-to-prompt, command selection).

## Constraints

- **Parser throughput:** Target competitive with Ghostty (>100 MB/s for ground-state ASCII). The table-driven state machine and SIMD-optimized UTF-8 decoding enable this.
- **Handler hot path:** `input()` (print character), `set_attribute()` (SGR), `carriage_return()`, and `linefeed()` account for ~98% of handler calls. These methods must avoid heap allocation in the common case.
- **OSC buffer limit:** 4 KiB maximum for OSC payloads (titles, clipboard, hyperlinks). Sequences exceeding this are truncated.
- **APC buffer limit:** 64 MiB maximum for APC payloads (Kitty graphics). This accommodates large images while preventing unbounded memory growth.
- **Parameter limits:** CSI sequences accept at most 32 parameters. Excess parameters are ignored.
- **TERM variable:** OakTerm reports `TERM=xterm-256color`. All sequences expected by `xterm-256color` terminfo are supported.

## References

- [ADR 0004: Kitty Graphics in Core](../adrs/0004-kitty-graphics-in-core.md)
- [ADR 0008: Shell Integration Timing](../adrs/0008-shell-integration-timing.md)
- [Spec 0001: Daemon Wire Protocol](0001-daemon-wire-protocol.md) — RenderUpdate carries screen buffer state produced by this handler
- [01-architecture.md](../ideas/01-architecture.md)
- [36-terminal-fundamentals.md](../ideas/36-terminal-fundamentals.md)
- [18-shell-integration.md](../ideas/18-shell-integration.md)
- [Paul Williams VT parser](https://vt100.net/emu/dec_ansi_parser)
- [vte crate](https://github.com/alacritty/vte)
- [xterm ctlseqs](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)
- [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/)
- [OSC 133 shell integration](https://contour-terminal.org/vt-extensions/osc-133-shell-integration/)
