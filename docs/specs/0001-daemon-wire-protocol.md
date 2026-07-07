---
spec: '0001'
title: Daemon Wire Protocol
status: implementing
date: 2026-03-26
adrs: ['0007', '0020']
tags: [core]
---

# 0001. Daemon Wire Protocol

## Overview

Defines the binary protocol between the OakTerm daemon and its clients (GUI, `oakterm ctl`, third-party). The daemon owns terminal state (PTYs, VT parser, screen buffers, plugins, config). Clients handle rendering, input, and window management. This spec covers framing, handshake, message types, flow control, and error handling over Unix domain sockets.

## Contract

### Framing

Every message is a frame: a fixed 13-byte header followed by a variable-length payload.

```text
Offset  Size  Field           Encoding
──────  ────  ──────────────  ─────────────────────────
0       2     magic           0x4F54 ("OT"), big-endian
2       1     flags           bitfield (see below)
3       2     msg_type        u16 little-endian
5       4     serial          u32 little-endian
9       4     payload_length  u32 little-endian
13      N     payload         opaque bytes
```

**magic**: Protocol identifier. Must be `0x4F54`. Any other value means this is not an OakTerm protocol connection.

**flags**:

| Bit | Name       | Meaning                                                                                          |
| --- | ---------- | ------------------------------------------------------------------------------------------------ |
| 0   | compressed | Payload is zstd-compressed. Reserved for Phase 4 remote access. Must be 0 for local connections. |
| 1-7 | reserved   | Must be 0. Receivers ignore unknown flags.                                                       |

**msg_type**: Message type discriminant. `0x00`-`0x63` reserved for protocol infrastructure. `0x64`-`0xC7` for GUI protocol (input, rendering, notifications, pane management). `0xC8`-`0xDF` for control protocol. `0xE0`-`0xFFFF` reserved for future use.

**serial**: Request/response correlation.

- Requests use a non-zero serial chosen by the sender. Monotonically increasing per connection.
- Responses echo the request's serial.
- Unilateral pushes (notifications) use serial `0`.
- Maximum outstanding requests: limited by u32 range (~4 billion). No practical limit.

**payload_length**: Byte count of the payload. Maximum: 16 MiB (16,777,216 bytes). Frames exceeding this limit are rejected.

**payload**: Opaque bytes. The framing layer treats the payload as a byte blob. Payload serialization format (protobuf via prost, bincode, etc.) is an implementation choice outside this spec's scope. This spec defines framing and message semantics; the serialization layer sits between framing and application code.

### Handshake

The first exchange after TCP/Unix socket connection. Both handshake messages use the standard frame format with reserved msg_type values.

**ClientHello** (msg_type: `0x01`, serial: 1):

```text
Field                    Type              Notes
───────────────────────  ────────────────  ──────────────────────────────
protocol_version_major   u16 LE            Breaking changes increment this
protocol_version_minor   u16 LE            Additive changes increment this
client_type              u8                0=GUI, 1=control, 2=third-party
client_name_len          u16 LE            Length of client_name in bytes
client_name              UTF-8 bytes       Human-readable name (for debugging/logging)
```

**ServerHello** (msg_type: `0x02`, serial: 1):

```text
Field                    Type              Notes
───────────────────────  ────────────────  ──────────────────────────────
status                   u8                0=accepted, 1=version_mismatch, 2=auth_rejected, 3=server_full
protocol_version_major   u16 LE            Server's protocol version
protocol_version_minor   u16 LE
server_version_len       u16 LE            Length of server_version in bytes
server_version           UTF-8 bytes       OakTerm version string (e.g., "0.1.0")
```

**Version negotiation rules:**

- Major version mismatch: server responds with `status=1` (version_mismatch) and closes the connection.
- Minor version mismatch: server responds with `status=0` (accepted). Both sides tolerate unknown message types by ignoring them.
- The negotiated version is the minimum of client and server major versions. (In practice, Phase 0 has only major version 1.)

**Connection state after handshake:**

- `client_type` is fixed for the connection lifetime. GUI clients receive render updates. Control clients receive command responses only.
- If `status != 0`, the server closes the connection after sending ServerHello.
- After successful handshake, both sides may send frames freely according to the message catalog.

**Versioning governance:**

The negotiation rules above describe how a mismatch is _handled_; these rules define what constitutes each kind of change, so that "major = breaking" is unambiguous:

- **Additive (minor bump):** a new `msg_type`. Appending a field to an existing message's payload counts as additive only when the chosen serialization tolerates unknown trailing data (protobuf does; a fixed hand-rolled binary layout does not) — otherwise a field addition is breaking. A minor bump must never change the meaning, type, order, or width of any existing field.
- **Breaking (major bump):** removing or repurposing a `msg_type`; changing the type, order, width, or semantics of any existing field; or altering the framing or handshake layout.
- **Retired `msg_type` numbers are never reused.** A removed message type's number is burned, not recycled, so a stale peer can never misinterpret a new message as an old one.
- Forward compatibility within a major version rests on the unknown-`msg_type` rule (ignore the frame — see [Error Cases](#error-cases)); additive changes are only safe because of it.
- Client obligation: gate new request types on the peer's advertised minor version, and never block waiting for a response to a `msg_type` the peer may not know — an ignored frame produces no response by design.

**Version history:**

This spec defines protocol version **major 1, minor 3**. Minor bumps are recorded here in implementation order; the advertised `VERSION_MINOR` constant ships with the first implementation of each bump's messages, so binaries may lag the spec by one minor version.

| Version | Change                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| ------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1.0     | Initial Phase 0 protocol: framing, handshake, version negotiation, and the message catalog.                                                                                                                                                                                                                                                                                                                                                                                                                      |
| 1.1     | Added `RequestShutdown` (0x07) and `ShutdownAck` (0x08) for client-initiated save-then-exit ([ADR-0020](../adrs/0020-daemon-upgrade-version-skew.md)). Additive — no existing message or field changed; unknown to older daemons, which fall back to a manual restart.                                                                                                                                                                                                                                           |
| 1.2     | Added `ListTabs` (0xAF) and `TabList` (0xB0) for tab enumeration (tab bar, TREK-107). Additive — unknown to older daemons, which ignore the frame. Also fixed `GetLayoutTree` to resolve `tab_id` literally (TREK-264); pre-1.2 binaries sent a placeholder `tab_id = 0` and relied on the unimplemented resolution serving the active tab — a behavior change, tolerated without a major bump because clients and daemons still ship in lockstep (same exception as 1.3's layout note).                         |
| 1.3     | Inserted `input_flags` and `kitty_kbd_flags` into the `RenderUpdate` (0x72) payload after `alt_screen` ([Spec-0011](0011-input-encoding.md)), growing the fixed prefix 25 → 27 bytes. Layout exception: a positional insertion is breaking under this spec's own rule, recorded as a minor bump only because client and daemon still ship in lockstep with no independently versioned client released; once one exists, positional insertions are major bumps. Ships with the protocol-1.3 implementation batch. |

### Message Catalog

#### Infrastructure Messages (0x00-0x09)

| msg_type | Name            | Direction | Serial   | Payload                                                 |
| -------- | --------------- | --------- | -------- | ------------------------------------------------------- |
| `0x00`   | (reserved)      | —         | —        | Invalid. Must never appear in a valid frame.            |
| `0x01`   | ClientHello     | C→D       | Request  | Handshake (see above)                                   |
| `0x02`   | ServerHello     | D→C       | Response | Handshake response (see above)                          |
| `0x03`   | Ping            | Either    | Request  | Empty                                                   |
| `0x04`   | Pong            | Either    | Response | Empty (echoes Ping serial)                              |
| `0x05`   | Error           | D→C       | Response | `error_code: u32`, `message_len: u16`, `message: UTF-8` |
| `0x06`   | Shutdown        | D→C       | Push (0) | `reason: u8` (0=clean, 1=crash, 2=upgrade)              |
| `0x07`   | RequestShutdown | C→D       | Request  | `reason: u8` (0=quit, 1=upgrade)                        |
| `0x08`   | ShutdownAck     | D→C       | Response | `status: u8` (0=accepted, 1=save_failed)                |

**Error codes (0x05 Error payload):**

| Code | Name                | Meaning                                                                                                      |
| ---- | ------------------- | ------------------------------------------------------------------------------------------------------------ |
| 1    | `UNKNOWN_PANE`      | Requested pane_id does not exist                                                                             |
| 2    | `INVALID_MESSAGE`   | Message type not allowed on this connection type                                                             |
| 3    | `MALFORMED_PAYLOAD` | Payload deserialization failed                                                                               |
| 4    | `INTERNAL_ERROR`    | Daemon encountered an unexpected error                                                                       |
| 5    | `PANE_EXITED`       | Pane exists but the child process has exited                                                                 |
| 6    | `PERMISSION_DENIED` | Operation not permitted for this client                                                                      |
| 7    | `LAYOUT_REJECTED`   | Layout operation violates a Spec-0007 constraint (minimum pane size, or the panes share no resizable border) |
| 8    | `UNKNOWN_TAB`       | Requested tab_id does not exist                                                                              |
| 9    | `UNKNOWN_WORKSPACE` | Requested workspace_id does not exist                                                                        |

Error codes 0 and 10-255 are reserved. Codes 256+ are available for future use. New codes are assigned from the reserved range without a version bump — codes are data inside an existing message, not new wire surface — so clients must treat an unknown code as an opaque failure, keyed by the human-readable `message`. (`LAYOUT_REJECTED` was assigned this way with the first split-topology implementation, TREK-98; `UNKNOWN_TAB`/`UNKNOWN_WORKSPACE` with the tab bar, TREK-107.)

**`RequestShutdown` (0x07) and `ShutdownAck` (0x08):** A client asks the daemon
to persist session state and exit — the single save-then-exit path shared by
`oakterm quit` (`reason=0`, quit) and the coordinated daemon upgrade of
[ADR-0020](../adrs/0020-daemon-upgrade-version-skew.md) (`reason=1`, upgrade).
As an infrastructure message it is accepted on both GUI and control
connections, which the upgrade flow needs: the new GUI re-connects speaking the
daemon's older protocol solely to deliver it. On receipt the daemon saves the
[Spec-0010](0010-session-persistence.md) session file, then replies with
`ShutdownAck`: `status=0` (accepted) once the save succeeds, after which it
broadcasts `Shutdown` (0x06) to the remaining clients and exits; `status=1`
(save_failed) if the session could not be persisted, in which case the daemon
aborts the shutdown and keeps running so no state is lost — a silent-loss exit
would be worse than a stuck quit, and a client can still bring a default-mode
daemon down by disconnecting. The broadcast
`Shutdown` inherits the request's intent: `reason=0` (quit) maps to `Shutdown`
`reason=0` (clean), `reason=1` (upgrade) maps to `Shutdown` `reason=2`
(upgrade). An unknown `reason` value is a malformed payload (see
[Error Cases](#error-cases)).

#### GUI Protocol — Input (0x64-0x6F)

| msg_type | Name       | Direction | Serial   | Payload                                                                             |
| -------- | ---------- | --------- | -------- | ----------------------------------------------------------------------------------- |
| `0x64`   | KeyInput   | C→D       | Push (0) | `pane_id: u32`, `key_data_len: u16`, `key_data: bytes`                              |
| `0x65`   | MouseInput | C→D       | Push (0) | `pane_id: u32`, `event_type: u8`, `x: u16`, `y: u16`, `modifiers: u8`, `button: u8` |
| `0x66`   | Resize     | C→D       | Push (0) | `pane_id: u32`, `cols: u16`, `rows: u16`, `pixel_width: u16`, `pixel_height: u16`   |
| `0x67`   | Detach     | C→D       | Push (0) | Empty. Client is disconnecting cleanly.                                             |

#### GUI Protocol — Rendering & Search (0x70-0x7F)

| msg_type | Name            | Direction | Serial   | Payload                                                                                             |
| -------- | --------------- | --------- | -------- | --------------------------------------------------------------------------------------------------- |
| `0x70`   | DirtyNotify     | D→C       | Push (0) | `pane_id: u32`. Daemon signals that pane content has changed.                                       |
| `0x71`   | GetRenderUpdate | C→D       | Request  | `pane_id: u32`, `since_seqno: u64`                                                                  |
| `0x72`   | RenderUpdate    | D→C       | Response | See RenderUpdate payload below                                                                      |
| `0x73`   | GetScrollback   | C→D       | Request  | `pane_id: u32`, `start_row: i64`, `count: u32`                                                      |
| `0x74`   | ScrollbackData  | D→C       | Response | `pane_id: u32`, `start_row: i64`, `has_more: u8`, `total_rows: u32`, `rows_len: u32`, `rows: bytes` |
| `0x75`   | FindPrompt      | C→D       | Request  | `pane_id: u32`, `from_offset: i64`, `direction: u8` (0xFF=older, 0x01=newer)                        |
| `0x76`   | PromptPosition  | D→C       | Response | `pane_id: u32`, `offset: i64`, `found: u8` (0=no prompt found; `offset` is 0 when `found` is 0)     |

`FindPrompt` locates the next `PromptStart` mark relative to `from_offset`,
which shares the coordinate space of `GetScrollback.start_row` (negative
offset from the viewport bottom). `direction` searches toward older rows
(0xFF) or newer rows (0x01). `PromptPosition.offset` is the found prompt's
offset in that same space; when `found` is 0 no prompt exists in the search
direction and `offset` is 0.

**RenderUpdate payload (0x72):**

```text
Field              Type        Notes
─────────────────  ──────────  ──────────────────────────────────
pane_id            u32 LE
seqno              u64 LE      New sequence number after this update
cursor_x           u16 LE      Cursor column
cursor_y           u16 LE      Cursor row
cursor_style       u8          0=block, 1=underline, 2=bar, 3=hidden
cursor_visible     u8          0=hidden, 1=visible
bg_r               u8          Dynamic background red (OSC 11 or default)
bg_g               u8          Dynamic background green
bg_b               u8          Dynamic background blue
bracketed_paste    u8          1 if DECSET 2004 is active
alt_screen         u8          1 if the active grid is the alternate screen
                               (smcup). Clients use this to route wheel
                               events: alt → forward to app, primary →
                               host scrollback.
input_flags        u8          Since 1.3 (Spec-0011). bit0: DECCKM cursor-key
                               mode; bit1: application keypad mode (mode 66);
                               bits2-3: modifyOtherKeys level (0-2);
                               bits4-7: reserved, must be 0.
kitty_kbd_flags    u8          Since 1.3 (Spec-0011). Kitty keyboard
                               progressive-enhancement flags at the top of the
                               active grid's flag stack; 0 = protocol disabled
                               (legacy encoding).
dirty_row_count    u16 LE      Number of dirty row entries
dirty_rows         [DirtyRow]  Array of dirty row data (see below)
```

**DirtyRow:**

```text
Field              Type        Notes
─────────────────  ──────────  ──────────────────────────────────
row_index          u16 LE      Row position in the visible grid
cell_count         u16 LE      Number of cells in this row
cells              [Cell]      Array of cell data (see Cell below)
semantic_mark      u8          0=none, 1=prompt_start, 2=input_start, 3=output_start, 4=output_end
mark_metadata_len  u16 LE      Length of optional mark metadata
mark_metadata      bytes       Exit status for output_end, CWD for prompt_start, etc.
```

**Cell:**

```text
Field              Type        Notes
─────────────────  ──────────  ──────────────────────────────────
codepoint          u32 LE      Unicode codepoint (0 = empty cell)
fg_r               u8          Foreground red
fg_g               u8          Foreground green
fg_b               u8          Foreground blue
fg_type            u8          0=default, 1=rgb, 2=indexed (fg_r = palette index)
bg_r               u8          Background red
bg_g               u8          Background green
bg_b               u8          Background blue
bg_type            u8          0=default, 1=rgb, 2=indexed (bg_r = palette index)
flags              u16 LE      Bitfield: bold(0), italic(1), underline(2), strikethrough(3),
                               inverse(4), blink(5), dim(6), hidden(7), wide(8), wide_cont(9)
extra_len          u16 LE      Length of optional extra data (0 for most cells)
extra              bytes       Hyperlink URL, combining characters, underline color, etc.
```

Cell size: 16 bytes fixed + variable extra. The `extra` field keeps the common case compact (most cells have no hyperlinks or combining characters). The full Cell type definition is covered in Spec-0003 (Screen Buffer); this is the wire representation for the protocol.

#### GUI Protocol — Notifications (0x80-0x8F)

| msg_type | Name          | Direction | Serial   | Payload                                                               |
| -------- | ------------- | --------- | -------- | --------------------------------------------------------------------- |
| `0x80`   | TitleChanged  | D→C       | Push (0) | `pane_id: u32`, `title_len: u16`, `title: UTF-8`                      |
| `0x81`   | SetClipboard  | D→C       | Push (0) | `clipboard: u8` (0=system, 1=primary), `data_len: u32`, `data: bytes` |
| `0x82`   | Bell          | D→C       | Push (0) | `pane_id: u32`                                                        |
| `0x83`   | PaneExited    | D→C       | Push (0) | `pane_id: u32`, `exit_code: i32`                                      |
| `0x84`   | ConfigChanged | D→C       | Push (0) | `config_data_len: u32`, `config_data: bytes`                          |

#### GUI Protocol — Search (0x77-0x7B)

| msg_type | Name             | Direction | Serial   | Payload                                                                                                                                                                        |
| -------- | ---------------- | --------- | -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `0x77`   | SearchScrollback | C→D       | Request  | `pane_id: u32`, `flags: u8`, `query_len: u16`, `query: UTF-8`                                                                                                                  |
| `0x78`   | SearchResults    | D→C       | Response | `pane_id: u32`, `total_matches: u32`, `active_index: u32` (0xFFFFFFFF = none), `active_row_offset: i64`, `capped: u8`, `visible_count: u16`, `visible_matches: [VisibleMatch]` |
| `0x79`   | SearchNext       | C→D       | Request  | `pane_id: u32`                                                                                                                                                                 |
| `0x7A`   | SearchPrev       | C→D       | Request  | `pane_id: u32`                                                                                                                                                                 |
| `0x7B`   | SearchClose      | C→D       | Push (0) | `pane_id: u32`                                                                                                                                                                 |

**SearchScrollback flags:**

| Bit | Name           | Meaning                                                |
| --- | -------------- | ------------------------------------------------------ |
| 0   | REGEX          | Treat query as regex (default: literal)                |
| 1   | CASE_SENSITIVE | Match case exactly (default: smart case for literal)   |
| 2   | WRAP           | Wrap around at buffer boundaries (not yet implemented) |
| 3-7 | reserved       | Must be 0                                              |

**VisibleMatch:**

```text
Field              Type        Notes
─────────────────  ──────────  ──────────────────────────────────
row                u16 LE      Viewport row (0 = top of viewport)
col_start          u16 LE      Column of match start
col_end            u16 LE      Column of match end (exclusive)
is_active          u8          1 if this is the currently selected match, 0 otherwise
```

`SearchNext` and `SearchPrev` advance the active match index forward or backward and return a new `SearchResults` response. `SearchClose` clears search highlights and state for the pane.

#### GUI Protocol — Pane Management (0x90-0x96)

| msg_type | Name               | Direction | Serial   | Payload                                                                                                      |
| -------- | ------------------ | --------- | -------- | ------------------------------------------------------------------------------------------------------------ |
| `0x90`   | CreatePane         | C→D       | Request  | `command_len: u16`, `command: UTF-8` (empty = default shell), `cwd_len: u16`, `cwd: UTF-8` (empty = inherit) |
| `0x91`   | CreatePaneResponse | D→C       | Response | `pane_id: u32`                                                                                               |
| `0x92`   | ClosePane          | C→D       | Request  | `pane_id: u32`                                                                                               |
| `0x93`   | ClosePaneResponse  | D→C       | Response | Empty. Confirms pane closed. Error response (0x05) if pane_id is unknown.                                    |
| `0x94`   | FocusPane          | C→D       | Push (0) | `pane_id: u32`                                                                                               |
| `0x95`   | ListPanes          | C→D       | Request  | Empty                                                                                                        |
| `0x96`   | ListPanesResponse  | D→C       | Response | `pane_count: u16`, `panes: [PaneInfo]`                                                                       |

`CreatePane.command` is a single shell-style string. The daemon shlex-splits
it into program + args at PTY spawn time (e.g., `"htop --tree"` becomes
`program=htop`, `args=["--tree"]`). Malformed quoting (unclosed quote, etc.)
produces an `ErrorMessage` (0x05) with `ErrorCode::MalformedPayload` —
client error, not a daemon fault. The same parsing rule applies to
`SplitPane` (0xA0) and, when implemented, `NewTab` (0xA7).

`CreatePane.cwd` is a UTF-8 path. If the directory doesn't exist at spawn
time, the daemon falls back to `$HOME` → `/` and logs a warning; the spawn
still succeeds.

Every pane lives in the Spec-0007 layout tree. The first `CreatePane`
seeds the tree's root; until the tab model lands, a later `CreatePane`
(which carries no placement) enters the tree as a horizontal split of the
focused pane. `SplitPane` (0xA0) chooses its own target and direction.

**PaneInfo:**

```text
Field              Type        Notes
─────────────────  ──────────  ──────────────────────────────────
pane_id            u32 LE
title_len          u16 LE
title              UTF-8
cols               u16 LE
rows               u16 LE
pid                u32 LE      Child process PID (0 if exited)
exit_code          i32 LE      -1 if still running
cwd_len            u16 LE
cwd                UTF-8       Current working directory (from OSC 7, empty if unknown)
```

#### GUI Protocol — Copy Mode (0x97-0x9A)

See Spec-0008 for full copy mode behavior.

| msg_type | Name          | Direction | Serial   | Payload                                                                                                                                       |
| -------- | ------------- | --------- | -------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `0x97`   | EnterCopyMode | C→D       | Push (0) | `pane_id: u32`. Pins the pane's viewport offset for this client. New PTY output continues but does not scroll the pinned viewport.            |
| `0x98`   | ExitCopyMode  | C→D       | Push (0) | `pane_id: u32`. Unpins the viewport. Scroll position jumps to follow live output.                                                             |
| `0x99`   | YankSelection | C→D       | Request  | `pane_id: u32`, `start_row: i64`, `start_col: u16`, `end_row: i64`, `end_col: u16`, `selection_type: u8` (0=character, 1=line, 2=block)       |
| `0x9A`   | YankResponse  | D→C       | Response | `text_len: u32`, `text: UTF-8`. The extracted text for the requested selection range, resolved across hot buffer and disk archive boundaries. |

The daemon tracks which clients have pinned viewports per pane (a set of client IDs, not a boolean). Scroll-on-output is suppressed per-client while its ID is in the set. See ADR-0012.

#### GUI Protocol — Split Topology, Tabs & Workspaces (0xA0-0xB0)

See Spec-0007 for the layout tree model.

| msg_type | Name              | Direction | Serial   | Payload                                                                                                                                                                                                                      |
| -------- | ----------------- | --------- | -------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `0xA0`   | SplitPane         | C→D       | Request  | `pane_id: u32`, `direction: u8` (0=horizontal: children left-to-right, 1=vertical: children top-to-bottom), `command_len: u16`, `command: UTF-8`, `cwd_len: u16`, `cwd: UTF-8`                                               |
| `0xA1`   | SplitPaneResponse | D→C       | Response | `new_pane_id: u32`                                                                                                                                                                                                           |
| `0xA2`   | ResizePane        | C→D       | Push (0) | `pane_id: u32`, `neighbor_pane_id: u32`, `delta: i16` in grid cells along the border axis (columns for a vertical border, rows for a horizontal one; positive = grow pane_id). Error if the panes share no resizable border. |
| `0xA3`   | SwapPane          | C→D       | Request  | `pane_id_a: u32`, `pane_id_b: u32`                                                                                                                                                                                           |
| `0xA4`   | SwapPaneResponse  | D→C       | Response | Empty. Confirms swap completed.                                                                                                                                                                                              |
| `0xA5`   | GetLayoutTree     | C→D       | Request  | `workspace_id: u32`, `tab_id: u32`                                                                                                                                                                                           |
| `0xA6`   | LayoutTree        | D→C       | Response | `tree_len: u32`, `tree: bytes` (JSON-encoded layout tree, see below)                                                                                                                                                         |
| `0xA7`   | NewTab            | C→D       | Request  | `workspace_id: u32`, `command_len: u16`, `command: UTF-8`, `cwd_len: u16`, `cwd: UTF-8`                                                                                                                                      |
| `0xA8`   | NewTabResponse    | D→C       | Response | `tab_id: u32`, `pane_id: u32`                                                                                                                                                                                                |
| `0xA9`   | CloseTab          | C→D       | Request  | `tab_id: u32`                                                                                                                                                                                                                |
| `0xAA`   | CloseTabResponse  | D→C       | Response | Empty. Confirms tab closed.                                                                                                                                                                                                  |
| `0xAB`   | SwitchTab         | C→D       | Push (0) | `tab_id: u32`                                                                                                                                                                                                                |
| `0xAC`   | NewWorkspace      | C→D       | Request  | `name_len: u16`, `name: UTF-8`                                                                                                                                                                                               |
| `0xAD`   | NewWorkspaceResp  | D→C       | Response | `workspace_id: u32`, `tab_id: u32`, `pane_id: u32`                                                                                                                                                                           |
| `0xAE`   | SwitchWorkspace   | C→D       | Push (0) | `workspace_id: u32`                                                                                                                                                                                                          |
| `0xAF`   | ListTabs          | C→D       | Request  | Empty. Returns the active workspace's tabs.                                                                                                                                                                                  |
| `0xB0`   | TabList           | D→C       | Response | `workspace_id: u32`, `name_len: u16`, `workspace_name: UTF-8`, `active_tab: u32`, `tab_count: u16`, then per tab: `tab_id: u32`, `focused_pane: u32`, `name_len: u16`, `name: UTF-8`                                         |

**`SplitPane` (0xA0):** The new pane follows the `CreatePane` lifecycle — created unspawned; the client's first `Resize` for it determines dimensions and spawns the PTY. Focus moves to the new pane (Spec-0007 Split). The daemon rejects the request with `LAYOUT_REJECTED` when the split would leave any resulting pane below the Spec-0007 minimum (2 columns × 1 row) along the split axis — the new pane, the shrunk target, or a sibling scaled by a same-direction insert.

**`ResizePane` (0xA2):** `delta` is in grid cells because the daemon has no pixel geometry — the client converts its pixel drag using its cell size. The daemon converts cells to Spec-0007 weight space and clamps so neither side drops below the minimum pane size. The accepted pane pairs are those sharing a resizable border: adjacent sibling subtrees of one container where each pane touches the shared edge and their extents overlap (Spec-0007 Resize) — the pairs a border drag in the GUI naturally produces. Failures (`UNKNOWN_PANE`, `LAYOUT_REJECTED`) are pushed as `Error` frames with serial 0.

**`GetLayoutTree` (0xA5):** `tab_id` is literal — the seeded default tab is 0, and there is no "active tab" sentinel; a client resolves the active tab via `ListTabs`. An unknown `tab_id` yields `UNKNOWN_TAB`. Tab IDs are unique across workspaces, so `workspace_id` needs no resolution.

**`ListTabs` (0xAF) / `TabList` (0xB0):** Returns the active workspace's tabs in workspace order (the order the tab bar renders). `active_tab` is the active tab's id. Each tab's `name` is its explicit name, falling back to its focused pane's title (Spec-0007 tab naming); empty when neither is set — clients display the tab index. `focused_pane` is the pane focus moves to when the tab activates, letting a client focus correctly after `SwitchTab` without another round-trip. In the transient empty multiplexer state (no workspace yet) all fields are zero/empty with `tab_count = 0`. Tab topology has no change push: a client refreshes with `ListTabs` after its own tab/workspace operations; cross-client notification is deferred with the other per-client notification work.

**GetLayoutTree response:** The `tree` payload is a JSON-encoded layout tree for a single tab. Based on Spec-0010's `SavedLayoutNode` structure but with live `pane_id: u32` at each leaf instead of `SavedPane`. This gives the GUI the pane IDs it needs to correlate with render updates. JSON is used because layout tree queries are infrequent (on tab switch, not per frame) and the tree is small (~1-5 KB).

The JSON shape is externally tagged snake_case (Spec-0010 serialization must match this casing):

```json
{
  "container": {
    "direction": "horizontal",
    "children": [{ "leaf": { "pane_id": 0 } }, { "leaf": { "pane_id": 1 } }],
    "weights": [0.5, 0.5]
  }
}
```

Receivers reject trees whose containers have mismatched `children`/`weights` lengths, fewer than 2 children, or non-finite/non-positive weights.

#### Control Protocol (0xC8-0xDF)

Used by `oakterm ctl` and automation. Only available on connections with `client_type=1`.

| msg_type | Name        | Direction | Serial   | Payload                                                                              |
| -------- | ----------- | --------- | -------- | ------------------------------------------------------------------------------------ |
| `0xC8`   | CtlCommand  | C→D       | Request  | `command_len: u16`, `command: UTF-8` (JSON-encoded command)                          |
| `0xC9`   | CtlResponse | D→C       | Response | `status: u8` (0=ok, 1=error), `body_len: u32`, `body: UTF-8` (JSON-encoded response) |

The control protocol uses JSON for command/response payloads because `oakterm ctl` is a CLI tool where human readability and scripting compatibility matter more than serialization performance.

### Flow Model

**Push-notify + pull-data with sequence numbers.**

The daemon does not push screen content to GUI clients. Instead:

1. When a pane's screen buffer changes (PTY output processed), the daemon sends `DirtyNotify { pane_id }` to all GUI clients subscribed to that pane.
2. The GUI client wakes up and sends `GetRenderUpdate { pane_id, since_seqno }`.
3. The daemon responds with `RenderUpdate` containing all dirty rows since `since_seqno`, the current cursor state, and a new `seqno`.
4. The GUI renders the update and stores the new `seqno` for the next request.

**Coalescing:** Multiple `DirtyNotify` messages between polls coalesce naturally. The GUI pulls once and gets the cumulative diff. The daemon tracks dirty state per pane, not per notification.

**Idle behavior:** When no PTY output is produced, no messages flow. Zero CPU when idle.

**Initial sync:** After handshake, the GUI sends `GetRenderUpdate { pane_id, since_seqno: 0 }` to get the full current screen state.

**Multiple panes:** Each pane has its own sequence number space. The GUI subscribes to panes by sending the first `GetRenderUpdate` for each pane. `DirtyNotify` is per-pane.

**Scrollback:** Scrollback data is not included in `RenderUpdate`. When the user scrolls up, the GUI sends `GetScrollback { pane_id, start_row, count }` to fetch archived rows on demand. If the requested range exceeds the max frame payload (16 MiB), the daemon returns as many rows as fit in a single frame with `has_more=1`. The client requests the next chunk using `start_row + rows_len` as the new `start_row`. Every response includes `total_rows`, the current size of the daemon's hot scrollback buffer, which the client uses to clamp its viewport offset to a valid range.

## Behavior

### Normal Operation

1. Client connects to `$OAKTERM_SOCKET`.
2. Client sends `ClientHello`. Server responds with `ServerHello`.
3. If accepted, client creates or lists panes via pane management messages.
4. For each visible pane, client sends `GetRenderUpdate { since_seqno: 0 }` to get initial state.
5. Daemon sends `DirtyNotify` when pane content changes. Client pulls updates.
6. Client sends `KeyInput` / `MouseInput` for user actions. Daemon writes to PTY.
7. Daemon sends notifications (`TitleChanged`, `Bell`, `PaneExited`, etc.) as they occur.

### Disconnection

- **Clean disconnect:** Client sends `Detach`, then closes the socket. Daemon cleans up client subscriptions.
- **Unclean disconnect:** Daemon detects socket close (read returns 0 or error). Same cleanup as clean disconnect.
- **Client-requested shutdown:** A client sends `RequestShutdown`. The daemon persists the session file, replies `ShutdownAck` (`status=0`), and then follows the daemon-shutdown path below (broadcast `Shutdown`, drain, close). If the save fails it replies `ShutdownAck` (`status=1`) and keeps running. Reason and status encodings are in the message catalog.
- **Daemon shutdown:** Daemon sends `Shutdown` to all connected clients, waits up to 1 second for clients to close, then closes all sockets.

### Error Cases

| Condition                                            | Behavior                                                                                                                                       |
| ---------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| Magic bytes don't match `0x4F54`                     | Close connection immediately. Log warning.                                                                                                     |
| Payload exceeds 16 MiB                               | Close connection. Log error.                                                                                                                   |
| Unknown msg_type                                     | Ignore the frame (skip payload bytes). Log at debug level. This enables minor version compatibility.                                           |
| Malformed payload (deserialization error)            | Send `Error` response if the frame had a non-zero serial. Log error. Do not close connection — the framing is intact, only the payload is bad. |
| Frame received mid-handshake (before ServerHello)    | Close connection.                                                                                                                              |
| GUI message on control connection                    | Send `Error` response.                                                                                                                         |
| Control message on GUI connection                    | Send `Error` response.                                                                                                                         |
| `GetRenderUpdate` for unknown pane_id                | Send `Error` response with appropriate error code.                                                                                             |
| `RequestShutdown` with an unknown `reason` value     | Send `Error` response (`MALFORMED_PAYLOAD`). Do not shut down; the daemon keeps running.                                                       |
| `SplitPane` below the minimum pane size              | Send `Error` response (`LAYOUT_REJECTED`). The layout tree is unchanged.                                                                       |
| `ResizePane` between panes with no shared border     | Push an `Error` frame (`LAYOUT_REJECTED`, serial 0). The layout tree is unchanged.                                                             |
| Serial collision (client reuses an in-flight serial) | Undefined behavior. Clients must use unique serials for outstanding requests.                                                                  |

### Reconnection

When a GUI client detects a daemon disconnect:

1. Attempt to reconnect to `$OAKTERM_SOCKET` with exponential backoff (100ms, 200ms, 400ms, up to 5s).
2. If the daemon is still running, the handshake succeeds and the client re-syncs by requesting pane list and current render state.
3. If the daemon exited, the client may start a new daemon (if persistence is off) or display a "daemon unavailable" message.

Running processes and scroll history survive GUI disconnection because the daemon owns all terminal state.

## Constraints

- **Frame header overhead:** 13 bytes per message. For typical screen updates (2-7 KB payload), overhead is < 1%.
- **Latency:** Unix domain socket round-trip is ~0.2μs. The protocol adds no meaningful latency beyond serialization.
- **Throughput:** Full-screen updates at 60fps = ~120 KB × 60 = ~7.2 MB/s. Unix sockets handle 1-4 GB/s. No bottleneck.
- **Max frame size:** 16 MiB. A full 200×50 screen at 24 bytes/cell is ~240 KB. Scrollback requests exceeding the max frame size are automatically chunked via `has_more` (see Flow Model).
- **Max outstanding requests:** Practical limit is the u32 serial space. Clients should not have more than ~1000 outstanding requests.
- **Handshake timeout:** Server closes the connection if `ClientHello` is not received within 5 seconds.
- **Ping interval:** Either side may send `Ping` at any time. If no `Pong` is received within 10 seconds, the connection is considered dead.
- **Socket path:** `$XDG_RUNTIME_DIR/oakterm/socket` on Linux, `$TMPDIR/oakterm-<uid>/socket` on macOS, `\\.\pipe\oakterm-<sid>` on Windows (named pipe). Parent directory created with `0700` permissions on Unix. Socket file permissions `0700`.

## References

- [ADR 0007: Daemon Architecture](../adrs/0007-daemon-architecture.md)
- [01-architecture.md](../ideas/01-architecture.md)
- [29-remote-access.md](../ideas/29-remote-access.md)
- [32-agent-control-api.md](../ideas/32-agent-control-api.md)
- [tokio-util LengthDelimitedCodec](https://docs.rs/tokio-util/latest/tokio_util/codec/length_delimited/)
- [WezTerm mux protocol](https://github.com/wezterm/wezterm/blob/main/codec/src/lib.rs)
- [Zellij client-server protocol](https://github.com/zellij-org/zellij/tree/main/zellij-utils/src/client_server_contract)
