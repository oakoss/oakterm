---
title: Architecture Deepening Review
date: 2026-07-06T23:30:00
scope: Whole-workspace depth-and-testability audit (Ousterhout lens) at 29e36e2
---

# Architecture Deepening Review

## Scope

A read-only architecture pass over the whole workspace at commit `29e36e2`,
run through John Ousterhout's depth lens rather than the bug/security lens of the
[Codebase Improvement Audit](2026-07-06-232328-codebase-improvement-audit.md).
The question here is not "is this code correct" but "which modules are **shallow**
(interface nearly as complex as the implementation, pass-through ceremony) versus
**deep** (a narrow interface hiding real complexity)" — and, for the shallow ones,
whether collapsing them would **concentrate** logic behind a testable seam.

Four parallel readers covered the daemon request path, the `oakterm-mux` layout
model, the `oakterm` client, and the VT handler + wire protocol. Every cited line
was re-read before it reached this document. The **deletion test** governs each
verdict: if you deleted an abstraction, would its complexity _concentrate_
sensibly (it was shallow — collapse it) or _scatter and duplicate_ (it was deep —
keep it)?

## Headline

The two files most likely to be condemned as god-files — `terminal/handler.rs`
(2818 lines) and `protocol/message.rs` (1740) — are **not**. Both are deep,
well-localized modules whose line counts are inflated by co-located test blocks;
the deletion test says keep them. The real shallowness is one layer in: **logic
trapped inside the winit event loop with no test surface**, and **per-handler /
per-message ceremony copy-pasted across the daemon and client**. That is where
the leverage is — and where three of the project's own tracked bugs originated.

## Findings — Deepen (high confidence)

Ordered by leverage. Each passes the deletion test as a _deepening_: collapsing
the current spread concentrates complexity behind a narrow, testable interface.

### D1. Extract the PTY input encoder — filed

`oakterm/src/main.rs:2456` (`key_to_bytes`), `:2500` (`winit_to_chord`), `:2858`
(`encode_mouse_modifiers`), orchestrated inline at `:975–1035`.

`key_to_bytes` takes only `(key, text)` — **no mode state** — hardcodes
`b"\x1b[A"` for arrows, and returns `None` on any modified key. It is welded into
a `&mut self` event arm that also clears selection, exits scrollback, sends the
frame, and resets blink. The test module imports exactly
`{assemble_frame, drain_wheel_notches, plan_pane_syncs, try_init_font}` — the
encoder has **zero unit tests**. Spec-0011 (accepted) is a pre-written test suite
with no home: it enumerates six divergences (arrows ignore DECCKM §1, all
modifiers dropped §2, DECKPAM stored-but-never-encoded §5, text-first branch
leaks compose state §6), each "a bug against this contract."

**Deepen:** an `input` module exposing `encode_key(event, InputModes) -> KeyOutcome`,
with `InputModes` riding on `PaneView` from the RenderUpdate mode-flags amendment
(TREK-236). The event arm shrinks to normalize → chord lookup → `encode_key` → send.

**Deletion test:** deep — deleting it scatters the escape-sequence table back into
the event loop and re-couples encoding to the platform event type, making
mode-aware encoding untestable again. Prerequisite for TREK-222.

### D2. Give scrollback one owner — filed

Offset writes at `main.rs:546 · 859 · 1464 · 1508 · 1616 · 2376`; grid
enter/exit calls at `:547 · 893 · 1510 · 1614 · 1821`; snapshot private to
`ClientGrid` (`render_grid.rs:98`).

`PaneView.viewport_offset` and `ClientGrid.live_snapshot` encode the same boolean
"are we scrolled." The invariant `offset > 0 ⟺ grid.is_scrolled()` is held by hand
at eight sites, in disagreeing orders: `:1508→1510` sets offset then
`enter_scrollback`; `:1614→1616` reverses it; `:1464` and `:2376` move the offset
with no grid call at all. Since `apply_update` vs `apply_update_while_scrolled` is
selected by `grid.is_scrolled()`, any desynced site routes a live update into the
wrong buffer — the **TREK-139** corruption class.

**Deepen:** a `Viewport` owner (`enter`, `advance`, `return_to_live`, `clamp`) that
drives `enter_scrollback`/`exit_scrollback` internally so offset and snapshot can
never be set independently; the five external grid calls collapse to zero.

**Deletion test:** deep — the abstraction is currently absent and complexity is
already scattered across eight sites; introducing the owner concentrates it and
makes the "forgot to sync" bug unrepresentable, testable as a pure state machine.

### D3. Extract the wheel-routing decision — filed

`main.rs:1190–1245` (the `MouseWheel` arm); contrast the already-extracted,
11-test `drain_wheel_notches` at `:324`.

The pixel→notch accumulator was extracted and stopped regressing. The routing
decision beside it — `if viewport_offset() > 0 || shift || !alt_screen { host }
else { forward }` — was left inline, welded to `&mut self`, `self.daemon`, and a
magic `count.min(5)` clamp, with a prose comment standing in for the missing
executable spec. It is a pure function of `(scrolled, shift, alt_screen, notches)`.
Bug locus for **TREK-138 / 139 / 151**.

**Deepen:** `route_wheel(scrolled, shift, alt_screen, notches) -> WheelRoute`,
beside `drain_wheel_notches`; the arm becomes drain → route → match.

**Deletion test:** deep — deleting it re-buries a four-input truth table (including
the load-bearing "already-scrolled keeps host-scrolling even on alt screen" case)
in an event arm; a future touch/trackpad path would duplicate it.

### D4. Collapse the daemon handler ritual — filed

~16× decode-or-`MalformedPayload`, ~11× lock-or-`UnknownPane`, ~10×
encode-or-`InternalError` across `daemon/src/requests/{input,render,panes,layout,scrollback,search}.rs`.
Canonical shape: `render.rs:19–35`.

Each handler is three copies of the same error-plumbing wrapped around one or two
lines of real work; `panes::create_pane` is 32 lines around a single `pm.create`.
The boilerplate is the part that varies subtly (which error code, which log
message), so it cannot be skimmed.

**Deepen:** a `Message` trait (`const MSG_TYPE` + decode/encode) plus two
combinators in `requests/mod.rs` —
`respond::<Req,Resp>(frame, |req| -> Result<Resp, ErrorCode>)` absorbing
decode+encode, and `with_live_pane(frame, panes, |guard, req| …)` absorbing the
lock. Handlers shrink to their domain line.

**Challenge to the recorded "dispatch registry" note:** the `requests/mod.rs`
dispatch `match` is already deep and flat — a runtime registry would trade
compile-time exhaustiveness for nothing. The shallowness is in the handler
_bodies_, not the router. This is the correct read of the audit's "Protocol
dispatch duplication" deferred note.

### D5. A `PaneState::pty_write` chokepoint — folded into TREK-242

`daemon/src/requests/input.rs:39–55` (key), `:81–127` (mouse), `:177–194`
(resize); `pane.rs:19–39` (`PtyState`).

There is no "write bytes to this pane's PTY" operation. Three handlers each
re-implement: match `pty_state`, copy the raw `fd` out of `Running`, `drop(pane)`
to release the lock, `unsafe { BorrowedFd::borrow_raw(fd) }` + `rustix::io::write`.
The daemon's internal PTY representation leaks into three sites, and this is
exactly where the **COR-1** fd-reuse hazard lives, so any fix must be applied
three times.

**Deepen:** `PaneState::pty_write(&self, bytes) -> PtyWriteOutcome` owning the
state-match, the single audited `unsafe`, and the error mapping — the one
chokepoint where COR-1's design call is made once. **Already tracked as TREK-242**
(this review adds the chokepoint framing rather than a duplicate task).

### D6. One home for layout geometry — filed

Weight→extent written twice: `mux/geometry.rs:366` (unit-square) vs
`oakterm/layout.rs:57` (pixel). Adjacency written twice: `geometry.rs:289`
(structural) vs `layout.rs:145` (pixel-band).

No single source of truth for "given this tree and this outer rect, what rect does
each pane get, and which panes flank each border." On a border drag both run; the
client picks the flanking pair by rounded pixels, the daemon re-derives it
structurally and can reject with `NotAdjacentSiblings`.

**Refinement of the recorded "Two layout-geometry walkers" note:** the failure is
**not silent corruption** — it fails safe as a border that visibly refuses to drag
when the two models disagree at a sliver/corner boundary. Real, but bounded.

**Deepen:** lift the pixel-geometry walker into `oakterm-mux` as a shared kernel
generic over "container = direction + weighted children"; the mux's `pane_rect` /
`border_extents` become thin projections, and `panes_share_border` /
`border_panes` collapse to one adjacency function. Both walkers pass the deletion
test as deep (you cannot push pixel geometry into the daemon — it has no window
size, and multi-client means per-client dimensions), so the remedy is unification
behind a kernel, not collapsing a side. **Design fork worth grilling first:** the
kernel must be generic over the mux `LayoutNode` and the wire DTO. L effort.

## Findings — Worth exploring

Real friction, smaller leverage or a design question attached. **D7 is filed**
(it pairs with D4 and has a design fork). **W8–W12 are shaped here and can be
promoted to tasks when the surrounding code is next touched.**

### D7. A `WireMessage` descriptor trait — filed

`protocol/src/{message,input,render}.rs`; consumer matches at
`daemon/requests/mod.rs` and `oakterm/daemon_conn.rs`.

Nothing at the type level binds `KeyInput ↔ MSG_KEY_INPUT`; the binding is
re-asserted inside 20 hand-written `to_frame` impls, and only ~20 of ~35 messages
have one — the rest are framed at the call site. `encode` is `Vec<u8>` for some
messages and `io::Result<Vec<u8>>` for others, which is _why_ no default emerged.
**Design fork:** normalize the `encode` signature first, then a trait binds the
type and unifies framing. **Challenge:** it cannot collapse the two consumer
`match` sites — daemon (requests) and client (responses) dispatch to disjoint
_behavior_, which is irreducible. Extends the "Protocol dispatch duplication" note
alongside D4.

### W8. A private-mode support table — shaped

`terminal/src/handler.rs:751` (`set/unset_private_mode`), `:1167`
(`report_private_mode`), `grid/mod.rs:15` (`ModeFlags`).

"Which private modes we implement" lives in three places that can drift: the
set/unset path special-cases `{47,1047,1049,6,25}` with a blind catch-all, while
`report_private_mode` hardcodes a _different_ 18-mode literal list as "handled."
Add mode 1016 and you must remember the report list too, with no compiler help. A
mode table (number · side-effecting? · DECRPM-reportable?) gives one edit point and
a natural test ("every mode with a set-handler is reportable").

### W9. Move mouse encoding into the terminal crate — shaped

`daemon/requests/input.rs:82` (mode-gating), `:296` (`encode_mouse_sgr`).

Deep terminal-protocol knowledge (X10/SGR framing, the +32 offset, mode-gating,
alt-scroll→arrow synthesis) sits in a daemon request handler, which reaches across
the seam to pull six mode bits off the grid and re-implements the encoding that
pairs with them. Move it to `oakterm-terminal` beside the mode state; the handler
shrinks to decode → `encode_mouse` → `pty_write` (composes with D5). SEC-2's
overflow fix and its tests move to the terminal crate too.

### W10. Route four arms through the disconnect seam — shaped

`main.rs:1024 · 1176 · 1232 · 1941` bypass the `send_or_disconnect` seam (`:2147`).

The seam calls `daemon.shutdown()` to unblock the reader and lets the
`Disconnected` event drive exit. The two hottest write paths (keystrokes, mouse)
plus paste re-implement it inline with `send_frame + daemon=None +
event_loop.exit()` — the _less_ correct sequence — so the policy now has five
implementations. Route them through the seam; the policy gets one test.

### W11. Co-locate ops ↔ geometry; one descent helper — shaped

`mux/geometry.rs:241 · :171`, `ops.rs:227 · :338`.

Understanding one split or resize means reading the `locate_*` half in
`geometry.rs` and the mutating half in `ops.rs` — one operation across two files.
The "descend a `Vec<usize>` child path, else `unreachable!`" primitive is
open-coded four times. Consolidate the descent pair; consider co-locating each
`locate`+mutate. **Not** a `state.rs`/`ops.rs` merge — that split is by concern
and correct.

### W12. One pixels→grid rule — shaped

`main.rs:2568` (`window_to_grid_dims`, untested, on the resize hot path) and
`layout::grid_dims` (tested) both convert pixels→cells with subtly different
padding responsibilities. Have the former compute the content rect and delegate the
divide+clamp to the latter, so a cell-metric or DPI change is reasoned about once.
Low risk.

## Confirmed deep — leave it

Retired so the next review does not re-suggest them. Each _scatters_ when removed.

- **`handler.rs` is a deep module, not a god-file.** Implementation is lines
  1–1207; ~1600 lines are co-located tests. Interface is two symbols
  (`process_bytes` + the 7-method `TermTarget` trait). SGR/color parsing isn't even
  here — `vte` owns it. All VT semantics in one file is correct locality.
- **`PaneManager` is deep.** Its `PaneId`-wrapping delegations look thin, but
  `create`/`split_create`/`remove`/`focus` hold the "mux pane-ids == pane-map keys"
  invariant with dual-write + debug-asserts. Delete it and that sync scatters into
  every handler.
- **The `MSG_*` constants and the dispatch `match` are good.** The constant table
  is a fine central registry; the flat one-arm-per-message router reads as a
  protocol table with compile-time exhaustiveness. A runtime registry concentrates
  nothing.
- **Intra-message encode/decode do not drift.** For every message,
  `encode`/`decode`/`to_frame` sit in the same impl block. The hand-tuned decode
  bounds are load-bearing — a blanket `#[derive(Wire)]` would discard them.
- **The hand-rolled codec is a decided tradeoff.** Spec-0001 leaves serialization
  out of scope but bakes in the consequence: fixed binary layout means field-append
  is breaking-by-construction, so minor versions add message _types_, not _fields_.
  Keep it; the one doc nit is to state this in the spec, not just the audit.
- **The mux newtypes earn their place.** `PaneId`/`TabId`/`WorkspaceId` prevent ID
  cross-mixing; `Child{node,weight}` makes a weight/child mismatch unrepresentable
  (Spec-0007). No shallow wrapper to collapse.
- **`state.rs` / `ops.rs` split is justified** — different types and concerns
  (`MultiplexerState`/`Tab` orchestration vs the pure `LayoutNode` tree algebra).

## Action items

Filed as standalone trekker tasks referencing this review:

- **D1 → TREK-257**, **D2 → TREK-258**, **D3 → TREK-259** (client extractions,
  retire tracked bug classes) — priority 2. D1 gates TREK-222; D2 relates to
  TREK-139; D3 relates to TREK-151/139.
- **D4 → TREK-260**, **D6 → TREK-261**, **D7 → TREK-262** (daemon/mux/protocol
  refactors, uncommitted) — priority 3.
- **D5** — folded into existing TREK-242 (comment CMT-178) as the `pty_write`
  chokepoint framing, not a duplicate task.

Not filed (shaped in this doc, promote when the code is next touched): **W8–W12**.

Top recommendation: start with **D1** — highest leverage, lowest blast radius
(pure extraction), and Spec-0011 already supplies the test suite. Then D2/D3 apply
the same "pull the pure decision out of the event arm" move. D4/D5 are an
independent daemon track. D6 is the one L — grill the generic-kernel fork before
committing.
