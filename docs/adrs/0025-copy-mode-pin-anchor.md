---
adr: '0025'
title: Copy-Mode Pin Anchor
status: accepted
date: 2026-08-17
tags: [core]
---

# 0025. Copy-Mode Pin Anchor

## Context

Copy mode pins a per-client base so scrollback coordinates stay stable while the user navigates ([ADR-0012](0012-copy-mode-scrollback-access.md), [Spec-0008](../specs/0008-copy-mode.md)). The TREK-110 review surfaced a race in how that base is chosen (TREK-297): `pin_copy_mode` sets `base = history_len()` at the moment the **daemon** processes EnterCopyMode, but the client froze its grid a round trip earlier, at whatever update it had last painted. Output arriving in the gap advances `history_len` without advancing the client's snapshot, so daemon row 0 and the client's frozen row 0 differ by however many lines scrolled between. Every cache fill is then internally consistent but uniformly offset from what the user sees, and YankSelection copies rows the user never pointed at.

The offset is zero on a quiet pane and N rows on a busy one — and entering copy mode on a busy pane (reading something that just scrolled past) is the motivating use case.

Nothing on the wire can express the client's anchor today: `CopyMode` carries only `pane_id`, and `RenderUpdate` carries a `seqno` but no absolute position, so the client cannot name the coordinates it froze at. Any fix is constrained on several sides:

- **Scrolled entry**: copy mode can be entered from a scrolled viewport, whose painted page was served by a GetScrollback resolved against the history length at _that_ request — a different instant than the last RenderUpdate. A partially scrolled page is assembled from both sources, so it can span two instants. And when scrolled, the client's `freeze_live` machinery has already snapshotted — nothing further freezes at entry, and the saved live snapshot keeps absorbing RenderUpdates while scrolled.
- **In-place edits**: output that rewrites visible rows without scrolling (progress lines) changes content without changing `history_len`, so no anchor value can make a daemon-side read of visible rows match the client's frozen pixels.
- **Resize**: shrink captures rows into scrollback at absolute indices no earlier anchor predicts, which is why `invalidate_pins_after_resize` clears pins today; a client-supplied historical anchor must not resurrect coordinates a resize invalidated — including a resize that ran while no pins existed.
- **Screen switches**: `history_len` freezes on the alternate screen (without `save_alternate_scrollback`), so an anchor alone cannot distinguish primary from alt content in the live grid.
- **Failure visibility**: entry is currently a serial-0 push; a rejected anchor would surface as an uncorrelatable serial-0 error, and `copy_mode_base` falls back to live `history_len()` when no pin exists — so a client that believes it is pinned but is not receives silently shifted rows, the exact defect under repair.

Spec-0008's coordinate principle — the client's viewport **offset** is never authoritative for daemon coordinates — was added after the same defect appeared at three layers (a pin that ignored the offset, a spec sentence claiming the daemon records it, and coordinates seeded from a page not yet on screen). Any fix must not reintroduce offset-derived coordinates.

## Options

### Option A: Ack carrying the daemon's pinned base

EnterCopyMode gains a response frame carrying `base = history_len()` as pinned; the client translates its coordinates onto it.

**Pros:**

- Smallest daemon change; the ack half is needed by any option (see failure visibility above).

**Cons:**

- The client cannot compute the translation delta without knowing the absolute position of its own frozen snapshot — which requires the same published-base fields as Option C, plus translation logic duplicated in every client.
- The user still entered copy mode against rows the pin does not cover; translation corrects the arithmetic, not the anchor.

### Option B: Client sends its last-applied seqno; daemon maps seqno → history

EnterCopyMode carries the client's last-applied `seqno`; the daemon pins at the `history_len` that update corresponded to.

**Pros:**

- Anchors where the client froze, with no new RenderUpdate field.

**Cons:**

- The daemon keeps no seqno → history_len mapping and would need one (a per-pane ring with eviction semantics), adding state, a fallback path when the seqno has aged out, and a new class of failure the client cannot observe.
- Does nothing for scrolled entry (the painted page is keyed by a scrollback request, not a seqno) or for in-place edits.

### Option C: Daemon publishes anchors; client echoes the painted base; visible rows stay client-owned (chosen)

The daemon publishes its history length on the replies that paint content, the client echoes the base bound to what it is painting when it requests entry, the daemon validates before pinning, and the yank/navigation contract splits at the immutability boundary: the daemon serves only rows below the base (history that already existed when the client froze), while everything at or above the base resolves from a snapshot the client takes of its own painted cells at entry. The full contract is in the Decision.

**Pros:**

- The pin lands on what the user sees: the anchor is always a daemon-published coordinate echoed back, never a client-derived one — a refinement of Spec-0008's principle, not a violation.
- The in-place-edit hazard leaves the well-behaved client's path entirely: the daemon serves nothing whose content can drift after the freeze, and the client's own entry snapshot is by definition what the user selected.
- Little daemon bookkeeping (one watermark, one ack); validation is two comparisons. Multi-client stays trivial: each client echoes its own base.
- The client already owns text extraction from its grid for normal (non-copy-mode) selection copies; resolving visible rows locally reuses that machinery.

**Cons:**

- Wire changes: fixed-prefix layout insertions in `RenderUpdate` and `ScrollbackData`, entry becomes a serial-correlated request/ack pair, and the shared `CopyMode` struct splits. Breaking under Spec-0001's classification; ships under the spec's recorded lockstep exception (below).
- Yank becomes a two-source stitch in the client for selections spanning the boundary, with per-selection-type split rules the spec must pin down.

### Option D: Drain pending RenderUpdates before entering

The client flushes queued updates and enters immediately after.

**Pros:**

- No wire change.

**Cons:**

- Narrows the race without closing it; output arriving after the drain but before the daemon processes the enter still shifts the pin. Does nothing for in-place edits.

## Decision

**Option C.** The contract:

1. **Published anchors.** `RenderUpdate` gains `history_len: u64` and `ScrollbackData` gains the serve-time base its `served_start_row` was resolved against. Both count rows pushed to the pane's one shared scrollback (with `save_alternate_scrollback`, alt rows append to it; insertion is always at the top, never below an observed base).
2. **Entry is a correlated request.** `EnterCopyMode { pane_id, base }` carries a real serial; success is an ack echoing the accepted base, failure an Error at that serial (`InvalidMessage` for an out-of-range base — the code for well-formed frames carrying invalid values). The client issues no cache fills until the ack. Validation precedes mutation: a rejected enter leaves any existing pin for that client untouched. Exit remains a push.
3. **Base selection.** The client echoes the base bound to the painted content: the last-applied RenderUpdate's value when at offset 0; the serving ScrollbackData's value when the page is entirely scrollback. A partially scrolled page mixes both sources and can span two instants — entry from one requires the two bases to coincide; otherwise the client refetches the scrollback page (one round trip) and enters once the page is single-instant.
4. **Resize watermark.** `PaneState` keeps `resize_watermark`, initially 0, updated to `history_len()` after **any** resize that advanced it — whether or not pins existed at the time — and monotonic because grow never reclaims rows from scrollback. The daemon accepts `resize_watermark <= base <= history_len()`; a base below the watermark names coordinates a resize capture invalidated. On rejection the client abandons entry and may retry after the next painted update.
5. **Pin invalidation is pushed.** When a resize invalidates pins, the daemon pushes a copy-mode-invalidated notification to each affected client (the initiating client's existing local teardown is unchanged); clients exit copy mode and discard in-flight fills. This closes the current silent gap where another client's resize unpins this client and its next fill falls back to live coordinates — and `copy_mode_base`'s fallback to `history_len()` is retired for fills issued under an acked pin.
6. **Ownership split.** At entry (post-ack) the client snapshots the **painted viewport cells** — a dedicated entry snapshot, not the still-mutating saved live snapshot — and copy mode addresses exactly the painted page: rows `[viewport_top, viewport_top + rows)` in pin space. Rows at or above 0 resolve from that snapshot; when the page is entirely scrollback no such rows are addressable. Rows below 0 are daemon-served: immutable in index and content **where retained** — rows lost to hot-buffer pruning without an archive, archive eviction, or era resets read back as blanks per Spec-0004's gap policy, unchanged by this ADR. (Cell colors resolve at serve time against the current palette — pre-existing, affects rendering only, never yanked text.)
7. **Yank stitch.** Selections spanning the boundary split at row −1/0: the daemon half runs with its end at (−1, full row width) for character and line selections, the client applies the true end column on its half, and the halves join with `\n` matching the daemon's row join; block selections apply the same column rectangle to both halves independently. Whole-selection requests at or above 0 never reach the daemon.
8. **Navigation clamps.** While pinned, FindPrompt and SearchScrollback results clamp to rows below 0 in pin space; the client finds visible-page matches against its entry snapshot and merges. Results are never allowed to name rows the frozen page does not show.
9. **Alt screen.** Copy-mode entry on the alternate screen is refused client-side in this contract (splicing primary history beneath alt content has no coherent display); revisit if demand appears.

GetScrollback keeps its existing clamp to history. The client's standing invariant — rows at or above 0 are never requested from the daemon — remains true by construction, because the pin base now equals the entry snapshot's absolute anchor.

## Consequences

- **Wire**: `RenderUpdate` and `ScrollbackData` gain their anchor fields — fixed-prefix layout insertions, breaking under Spec-0001's classification, shipped under the spec's recorded lockstep exception by extending the pending input-flags amendment (TREK-236's batch; Spec-0001 records it as the 1.5 row). `CopyMode` splits into `EnterCopyMode { pane_id, base }` (serial-correlated, acked) and `ExitCopyMode { pane_id }` (push); `to_exit_frame` moves to the exit type. A copy-mode-invalidated push is added.
- **Version gate**: `COPY_MODE_MIN_MINOR` moves from 4 to the batch's minor (5). This is load-bearing, not housekeeping: `CopyMode::decode` tolerates trailing bytes, so a 1.4 daemon receiving the new enter frame would silently ignore `base` and pin at its own `history_len()` — the original defect, undetectable through a push.
- **Daemon**: `pin_copy_mode` takes the validated client base; `PaneState` gains the resize watermark, updated at the `resize_all` sites; `invalidate_pins_after_resize` additionally emits the invalidation push; the `copy_mode_base` live fallback is retired for pinned reads. Yank tier-walking is otherwise unchanged (it already resolves by absolute index); the live-grid tier stops being reachable from a well-behaved copy-mode client, though the daemon-side code remains for non-copy-mode reads.
- **Client**: PaneView tracks the base bound to the painted content per source; entry gates on the protocol minor, on having a base, and on page single-instant-ness; entry takes the dedicated painted-cells snapshot; yank stitches per the split rules, reusing the existing selection text extraction; in-flight fills are discarded on invalidation.
- **Specs**: Spec-0001 and Spec-0008 amendments land with the implementing task (message shapes, ack/nack, navigation clamps, stitch rules). Spec-0008's coordinate principle gains the refinement that clients never _derive_ daemon coordinates from local state but may _echo_ daemon-published absolute coordinates. ADR-0012's viewport-offset pinning sentence is corrected in this change with a pointer here.
- **Tracking**: TREK-297 becomes the implementing task. For TREK-114 this settles the render-source question (cache below 0, entry snapshot at and above 0, no translation layer); CMT-237's accessibility question (what the a11y bridge reports during copy mode) remains open there.

## References

- [ADR-0012: Copy Mode Scrollback Access](0012-copy-mode-scrollback-access.md) — the pin design this ADR corrects
- [Spec-0008: Copy Mode](../specs/0008-copy-mode.md) — coordinate space and cache contract
- [Spec-0001: Daemon Wire Protocol](../specs/0001-daemon-wire-protocol.md) — versioning rules and the 1.5 amendment row
- [Spec-0004: Scroll Buffer](../specs/0004-scroll-buffer.md) — gap policy for destroyed history rows
- [03-multiplexer.md](../ideas/03-multiplexer.md) — copy-mode feature definition
- TREK-297 (problem statement), CMT-237 (TREK-114 coordination)
