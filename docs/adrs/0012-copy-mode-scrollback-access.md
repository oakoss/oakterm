---
adr: '0012'
title: Copy Mode Scrollback Access
status: proposed
date: 2026-04-02
tags: [core]
---

# 0012. Copy Mode Scrollback Access

## Context

Phase 1 adds copy mode: modal navigation through scrollback with vim/emacs keybinds, visual selection, and yank-to-clipboard. The multiplexer idea doc ([03-multiplexer.md](../ideas/03-multiplexer.md)) specifies copy mode keybinds and behavior but not where copy mode state lives.

OakTerm's daemon/GUI split (ADR-0007) creates a design question: the daemon owns scrollback (ring buffer + encrypted disk archive, Spec-0004), but the GUI owns rendering and input. Copy mode cursor movement is high-frequency (holding `j` repeats at keyboard repeat rate, ~30-60 events/second). The scrollback can be millions of lines with the disk archive enabled.

The wire protocol supports chunk-based scrollback access (`GetScrollback`/`ScrollbackData`, Spec-0001). Search messages (`SearchScrollback`/`SearchResults`/`SearchNext`/`SearchPrev`) are implemented in code (0x77-0x7B) but not yet documented in the spec. Selection coordinates use `i64` row indices where negative values reference scrollback; this coordinate space is shared between the wire protocol's `GetScrollback.start_row` (Spec-0001) and the screen buffer's `SelectionAnchor` (Spec-0003).

Research covered tmux (fully server-side), WezTerm (single-process overlay), Zellij (server-side with editor delegation), Kitty (client-side with pager delegation), and Alacritty (fully client-side).

## Options

### Option A: Fully daemon-side

All copy mode state (cursor position, selection anchors, mode) lives in the daemon. Every cursor movement is an IPC round-trip: GUI sends keystroke, daemon updates cursor, daemon sends updated render state.

**Pros:**

- Single source of truth. No state synchronization between GUI and daemon.
- Copy mode state survives GUI crash.
- Selection coordinate resolution against scrollback (including disk archive) is trivial because the daemon has direct access.
- Proven by tmux, which handles all copy mode server-side.

**Cons:**

- One IPC round-trip per keystroke. At 60 keystrokes/second, that is 60 round-trips/second. Unix socket round-trip is ~1-5 microseconds, so latency is not the bottleneck. But each round-trip triggers a full render update cycle (DirtyNotify → GetRenderUpdate → RenderUpdate), which is heavier than tmux's character-cell redraw.
- Daemon must track GUI-specific state (copy mode is a per-client concept). Multi-client scenarios complicate this.

### Option B: Fully client-side

GUI holds the full scrollback buffer (or a copy of it) and manages all copy mode state locally.

**Pros:**

- Zero IPC for cursor movement.

**Cons:**

- Duplicates scrollback memory in the GUI process. With millions of lines and disk archive, this is impractical.
- GUI cannot access disk-archived lines without requesting them from the daemon anyway.
- Search must either be duplicated in the GUI or delegated to the daemon, defeating the purpose.
- Copy mode state lost on GUI crash.

### Option C: Hybrid — daemon authority, GUI viewport cache

Daemon owns scrollback data, search, and selection coordinate resolution. GUI caches a viewport-sized window of scrollback rows and tracks the copy mode cursor position locally. When the cursor moves past the cached window boundaries, the GUI requests new chunks from the daemon.

**Pros:**

- Cursor movement within the cached window has zero IPC. Holding `j` through 50 visible rows triggers no daemon requests.
- Scrolling past the cache boundary triggers a single `GetScrollback` request for the next chunk. At ~63 microseconds per disk archive frame read (Spec-0004), this is imperceptible.
- Search is daemon-side (existing protocol messages). Results stream back as row indices; the GUI highlights matches within its cached window.
- Selection anchors use daemon-coordinate-space `i64` row indices (Spec-0003). When the user yanks, the GUI sends the selection range to the daemon, which resolves the text across hot buffer and disk archive boundaries.
- Daemon state is minimal: it knows a client is in copy mode (to suppress pane output scrolling) but does not track cursor position.
- Multi-client friendly: each GUI tracks its own copy mode cursor independently.

**Cons:**

- GUI must manage a row cache and handle cache misses (boundary crossings).
- Coordinate translation between GUI cache offsets and daemon row indices.
- Copy mode cursor position lost on GUI crash (acceptable; re-entering copy mode is trivial).

## Decision

**Option C — Hybrid with daemon authority and GUI viewport cache.**

Full client-side is impractical: the GUI cannot duplicate millions of scrollback lines, and it has no access to the disk archive. Full daemon-side adds an IPC round-trip per keystroke, each triggering a GPU render cycle. The hybrid splits the work: the GUI caches enough rows for smooth cursor movement, while the daemon handles search and text extraction.

### State ownership

| State                  | Owner  | Rationale                                                                  |
| ---------------------- | ------ | -------------------------------------------------------------------------- |
| Scrollback rows        | Daemon | Hot buffer + disk archive, existing ownership                              |
| Search index/results   | Daemon | Existing search messages in code (0x77-0x7B), to be added to Spec-0001     |
| Copy mode active set   | Both   | Daemon tracks which clients have pinned viewports; GUI activates key table |
| Cursor position        | GUI    | High-frequency updates, no daemon involvement                              |
| Selection anchors      | GUI    | Coordinates in daemon row-index space, resolved at yank time               |
| Yanked text extraction | Daemon | Resolves selection range across hot buffer + archive boundaries            |

### Protocol additions

New messages for copy mode in the 0x97-0x9F range (pane management, per ADR-0010 which reserves 0xA0-0xAF for split topology):

| Message         | Direction | Purpose                                                                                  |
| --------------- | --------- | ---------------------------------------------------------------------------------------- |
| `EnterCopyMode` | C→D       | Pins the pane's viewport offset; new output continues but does not scroll the viewport   |
| `ExitCopyMode`  | C→D       | Unpins the viewport; scroll position jumps to follow live output                         |
| `YankSelection` | C→D       | Request: selection start/end as `i64` row + `u16` col. Response: extracted text as UTF-8 |

`GetScrollback` and `SearchScrollback` already exist and are sufficient for viewport cache fills and search.

### Viewport pinning

When the daemon receives `EnterCopyMode`, it records the pane's current viewport offset as the pinned position. The VT parser continues processing PTY output normally (the child process must not block on a full buffer), and new lines scroll into scrollback as usual. But the copy-mode viewport stays fixed at the pinned offset. The `i64` row indices cached by the GUI remain stable because the viewport does not move.

On `ExitCopyMode`, the daemon discards the pinned offset and snaps the viewport to follow live output. Any rows that scrolled past during copy mode are now in scrollback.

### Viewport cache design

- Cache holds N rows (configurable, default: 3x visible rows — one screen above, visible, one screen below).
- Cache is indexed by daemon row coordinates (`i64`), stable while the viewport is pinned.
- On cursor movement past cache boundary, GUI sends `GetScrollback { pane_id, start_row, count }` to fill the next chunk.
- Prefetch: when cursor enters the top or bottom 25% of the cache, start fetching the next chunk in the background to avoid visible latency.
- Cache is invalidated when exiting copy mode (pane output may have changed the scrollback).

### Copy mode entry/exit flow

1. User presses `oak_mod + [`.
2. GUI activates the copy mode key table (ADR-0011).
3. GUI sends `EnterCopyMode { pane_id }` to daemon. Daemon pins the viewport offset for this client (see "Viewport pinning" above).
4. GUI sends `GetScrollback` to fill the initial cache (visible rows + buffer above/below).
5. User navigates with vim/emacs keys. Cursor moves within the cached window at zero IPC cost.
6. When cursor crosses a cache boundary, GUI fetches the next chunk.
7. User presses `/` to search. GUI sends `SearchScrollback` to daemon. Results stream back. GUI highlights matches within its cache.
8. User selects text (`v` + movement) and yanks (`y`). GUI sends `YankSelection { start, end }` to daemon. Daemon extracts the text, responds with UTF-8. GUI writes to clipboard and exits copy mode.
9. GUI sends `ExitCopyMode { pane_id }`. Daemon resumes normal scrolling.

## Consequences

- Spec-0001 (wire protocol) needs `EnterCopyMode`, `ExitCopyMode`, and `YankSelection` messages. The existing search messages (0x77-0x7B) should also be added to the spec.
- A future Spec-0008 (Copy Mode) will define the key tables, selection types, and viewport cache behavior.
- The GUI process gains a `CopyModeState` struct per pane: cursor position, selection anchors, cached rows, search highlights.
- The daemon gains a per-pane set of client IDs with pinned viewports. Scroll-on-output is suppressed for a client while its ID is in the set. Other clients viewing the same pane continue to see live output.
- Multi-client: each client can be in copy mode independently on the same pane. `EnterCopyMode` adds the client ID to the set; `ExitCopyMode` removes it. Each client's pinned offset is independent.
- The existing `GetScrollback`/`ScrollbackData` (Spec-0001) and `SearchScrollback`/`SearchResults` (implemented, not yet in spec) messages are reused without modification.

## References

- [03-multiplexer.md](../ideas/03-multiplexer.md) — copy mode keybinds and behavior
- [ADR-0007: Daemon Architecture](0007-daemon-architecture.md) — daemon/GUI state ownership
- [ADR-0011: Keybind Dispatch](0011-keybind-dispatch.md) — key tables for copy mode
- [Spec-0001: Daemon Wire Protocol](../specs/0001-daemon-wire-protocol.md) — existing scrollback and search messages
- [Spec-0003: Screen Buffer](../specs/0003-screen-buffer.md) — Selection struct, `i64` row indices
- [Spec-0004: Scroll Buffer & Archive](../specs/0004-scroll-buffer.md) — two-tier scrollback, ~63μs archive read latency
- [tmux Copy Mode](https://deepwiki.com/tmux/tmux/6.2-copy-mode) — fully server-side implementation
- [Alacritty Vi Mode](https://deepwiki.com/alacritty/alacritty/4.2-vi-mode) — fully client-side implementation
