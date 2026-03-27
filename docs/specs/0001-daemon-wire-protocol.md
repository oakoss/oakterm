---
spec: '0001'
title: Daemon Wire Protocol
status: draft
date: 2026-03-26
adrs: ['0007']
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

### Message Catalog

#### Infrastructure Messages (0x00-0x09)

| msg_type | Name        | Direction | Serial   | Payload                                                 |
| -------- | ----------- | --------- | -------- | ------------------------------------------------------- |
| `0x00`   | (reserved)  | —         | —        | Invalid. Must never appear in a valid frame.            |
| `0x01`   | ClientHello | C→D       | Request  | Handshake (see above)                                   |
| `0x02`   | ServerHello | D→C       | Response | Handshake response (see above)                          |
| `0x03`   | Ping        | Either    | Request  | Empty                                                   |
| `0x04`   | Pong        | Either    | Response | Empty (echoes Ping serial)                              |
| `0x05`   | Error       | D→C       | Response | `error_code: u32`, `message_len: u16`, `message: UTF-8` |
| `0x06`   | Shutdown    | D→C       | Push (0) | `reason: u8` (0=clean, 1=crash, 2=upgrade)              |

**Error codes (0x05 Error payload):**

| Code | Name                | Meaning                                          |
| ---- | ------------------- | ------------------------------------------------ |
| 1    | `UNKNOWN_PANE`      | Requested pane_id does not exist                 |
| 2    | `INVALID_MESSAGE`   | Message type not allowed on this connection type |
| 3    | `MALFORMED_PAYLOAD` | Payload deserialization failed                   |
| 4    | `INTERNAL_ERROR`    | Daemon encountered an unexpected error           |
| 5    | `PANE_EXITED`       | Pane exists but the child process has exited     |
| 6    | `PERMISSION_DENIED` | Operation not permitted for this client          |

Error codes 0 and 7-255 are reserved. Codes 256+ are available for future use.

#### GUI Protocol — Input (0x64-0x6F)

| msg_type | Name       | Direction | Serial   | Payload                                                                             |
| -------- | ---------- | --------- | -------- | ----------------------------------------------------------------------------------- |
| `0x64`   | KeyInput   | C→D       | Push (0) | `pane_id: u32`, `key_data_len: u16`, `key_data: bytes`                              |
| `0x65`   | MouseInput | C→D       | Push (0) | `pane_id: u32`, `event_type: u8`, `x: u16`, `y: u16`, `modifiers: u8`, `button: u8` |
| `0x66`   | Resize     | C→D       | Push (0) | `pane_id: u32`, `cols: u16`, `rows: u16`, `pixel_width: u16`, `pixel_height: u16`   |
| `0x67`   | Detach     | C→D       | Push (0) | Empty. Client is disconnecting cleanly.                                             |

#### GUI Protocol — Rendering (0x70-0x7F)

| msg_type | Name            | Direction | Serial   | Payload                                                                          |
| -------- | --------------- | --------- | -------- | -------------------------------------------------------------------------------- |
| `0x70`   | DirtyNotify     | D→C       | Push (0) | `pane_id: u32`. Daemon signals that pane content has changed.                    |
| `0x71`   | GetRenderUpdate | C→D       | Request  | `pane_id: u32`, `since_seqno: u64`                                               |
| `0x72`   | RenderUpdate    | D→C       | Response | See RenderUpdate payload below                                                   |
| `0x73`   | GetScrollback   | C→D       | Request  | `pane_id: u32`, `start_row: i64`, `count: u32`                                   |
| `0x74`   | ScrollbackData  | D→C       | Response | `pane_id: u32`, `start_row: i64`, `rows_len: u32`, `has_more: u8`, `rows: bytes` |

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

#### GUI Protocol — Pane Management (0x90-0x9F)

| msg_type | Name               | Direction | Serial   | Payload                                                                                                      |
| -------- | ------------------ | --------- | -------- | ------------------------------------------------------------------------------------------------------------ |
| `0x90`   | CreatePane         | C→D       | Request  | `command_len: u16`, `command: UTF-8` (empty = default shell), `cwd_len: u16`, `cwd: UTF-8` (empty = inherit) |
| `0x91`   | CreatePaneResponse | D→C       | Response | `pane_id: u32`                                                                                               |
| `0x92`   | ClosePane          | C→D       | Request  | `pane_id: u32`                                                                                               |
| `0x93`   | ClosePaneResponse  | D→C       | Response | Empty. Confirms pane closed. Error response (0x05) if pane_id is unknown.                                    |
| `0x94`   | FocusPane          | C→D       | Push (0) | `pane_id: u32`                                                                                               |
| `0x95`   | ListPanes          | C→D       | Request  | Empty                                                                                                        |
| `0x96`   | ListPanesResponse  | D→C       | Response | `pane_count: u16`, `panes: [PaneInfo]`                                                                       |

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

**Scrollback:** Scrollback data is not included in `RenderUpdate`. When the user scrolls up, the GUI sends `GetScrollback { pane_id, start_row, count }` to fetch archived rows on demand. If the requested range exceeds the max frame payload (16 MiB), the daemon returns as many rows as fit in a single frame with `has_more=1`. The client requests the next chunk using `start_row + rows_len` as the new `start_row`.

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
