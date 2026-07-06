---
title: Codebase Improvement Audit
date: 2026-07-06T23:23:28
scope: Whole-workspace code audit (correctness, security, performance, tests, tech debt, DX, docs, direction) at 60c80c4
---

# Codebase Improvement Audit

## Scope

A read-only, whole-workspace audit run across nine categories via four parallel
review passes, at commit `60c80c4` (Phase 1, tab/workspace data model just
landed). Each finding was vetted by re-reading the cited code; by-design
behavior and already-tracked items were rejected rather than reported.

The repo is in genuinely good shape: clippy pedantic clean, no `TODO`/`FIXME`
in Rust source, a strong docs pipeline, and sound foundations in the areas most
likely to be weak — the Lua sandbox (`config/lib.rs`), socket permissions
(`0700` + `flock`), wire-decode bounds (16 MiB payload cap, per-field length
re-checks), atomic session writes, and the `pty` `pre_exec` path were all
reviewed and found solid.

## Findings

Ordered by leverage (impact ÷ effort, discounted by confidence and fix-risk).
Effort S = hours, M = a day-ish, L = multi-day, for the fix including tests.

### Correctness & security

- **[SEC-1] Clamp client-controlled resize dimensions** (S, LOW risk, HIGH conf)
  — `requests/input.rs:169` passes `Resize.cols`/`Resize.rows` (each a full
  `u16`) straight to `screens.resize_all`; `grid/mod.rs:206` `Grid::resize`
  rejects only zero. A single `Resize{cols:65535, rows:65535}` frame requests
  ~65535×65535×24 bytes (multiple TB) → allocator abort / OOM-kill of the shared
  persistent daemon, destroying every pane and every connected client's session.
  Fix: clamp/reject above a documented max (e.g. 2000×2000) in the handler,
  return `MalformedPayload`; cap inside `Grid::resize` as defense in depth.

- **[SEC-2] `u16` overflow in X10 mouse-coordinate encode** (S, LOW, HIGH)
  — `requests/input.rs:303-304` computes `(x + 32).min(255)` where
  `x = msg.x.saturating_add(1)` is a client-controlled `u16`. For `msg.x`
  near 65535 the `+ 32` overflows before the `.min(255)`: panics in debug/test,
  wraps to a wrong coordinate byte in release. Reachable when legacy X10 mouse
  reporting is active (mode 1000 on, SGR 1006 off). Fix: widen to `u32` or use
  `saturating_add(32).min(255)` for both `cx`/`cy`.

- **[COR-1] Raw PTY fd written after releasing the pane lock** (M, MED, MED)
  — `requests/input.rs:40-47` (`key_input`) copies `fd` out of
  `PtyState::Running`, `drop(pane)` releases the lock, then writes to the
  now-unlocked fd via `BorrowedFd::borrow_raw`; same shape in `mouse_input`
  (`:89-124`) and the `Running` arm of `resize` (`:159`). A concurrent pane
  exit (read loop sets `Exited`, drops `Pty`, closes the master fd) can make the
  integer be reused by a later open/accept before the write lands — a keystroke
  can be misdirected to another pane's PTY, a scrollback file, or another
  client's socket. Narrow timing window; silent misdirection, not an error.
  Fix: hold the guard across the write, or route input through the read-loop's
  owned handle over a channel. **Needs a design call before code.**

- **[COR-2] Handshake rejects a fragmented `ClientHello`** (S, LOW, MED)
  — `server/mod.rs:471-481` reads one socket chunk and decodes once; if the
  hello splits across reads (`decode` → `Ok(None)`), the handshake fails with
  "no frame" instead of reading more. Rare on local `AF_UNIX`, but an incorrect
  framing assumption the steady-state loop doesn't share. Fix: loop
  read + decode until a full frame or the handshake timeout.

### Performance

- **[PERF-1] Full visible grid re-shaped and re-allocated every redraw** (L, MED, HIGH)
  — `render_grid.rs:418-429` does `ch.to_string()` (heap) + `shaper.shape()`
  (fresh `Vec<ShapedGlyph>`) per non-blank cell; the glyph atlas caches
  rasterization only, not shaping or the `String`. `main.rs` calls
  `assemble_frame` unconditionally in `RedrawRequested`, and cursor blink
  requests a redraw (`main.rs:644`), so a blinking cursor with zero output
  re-shapes the whole screen ~2×/sec. For a 200×50 pane that is ~10k
  `String` + ~10k `Vec` allocs + 10k shape calls per pane per frame, defeating
  the daemon's dirty-tracking. Fix: cache `shape()` per `(char, font_key, size)`
  and skip re-emitting glyph instances for rows whose seqno is unchanged.
  **Gate on PERF-4 benches.**

- **[PERF-2] Per-cell `Vec` allocation in `RenderUpdate` encode** (S, LOW, HIGH)
  — `protocol/render.rs:89-107` allocates a fresh `Vec<u8>` per `WireCell`;
  `wire.rs:31-51` first materializes a `Vec<WireCell>` for the whole row, so each
  dirty row is walked and allocated twice. Top buffers start from `Vec::new()`
  with no `with_capacity` despite a known 16-byte fixed cell size. Fix: add
  `encode_into(&mut Vec<u8>)`, pre-size from row/cell counts, encode straight
  to the buffer. Wire format byte-identical; covered by existing round-trip tests.

- **[PERF-3] `dirty_rows` builds two full vectors per render request** (S, LOW, HIGH)
  — `grid/mod.rs:255-262` collects a `Vec<u16>` of indices;
  `requests/render.rs:55-61` re-iterates it, re-indexing back into `g.lines` to
  build `Vec<DirtyRow>`. The index vector is pure intermediate garbage. Fix: a
  `dirty_rows_iter(since)` yielding `(idx, &Row)`.

- **[PERF-4] No bench for glyph assembly or wire encode** (M, LOW, HIGH)
  — `benches/frame_render.rs` covers atlas lookup / bg colors / uniforms, not
  the PERF-1 `glyph_instances` loop (no bench crate in `oakterm` at all); the
  PERF-2 encode path has no bench. `benches/row_codec.rs` benches the _scroll_
  codec, easy to mistake for coverage. These benches are the verification story
  for PERF-1/2. Fix: add a `render.rs`-encode bench in `oakterm-protocol` and a
  glyph-assembly bench feeding a synthetic grid through the shaper.

### Tests

- **[TEST-1] Protocol version-mismatch reject path has zero coverage** (S, LOW, HIGH)
  — `server/mod.rs:491` rejects `protocol_version_major` mismatch and sends a
  `VersionMismatch` `ServerHello`; tests cover only the happy path and older-minor
  acceptance. The one gate keeping incompatible clients out is unexercised. Fix:
  one integration test handshaking with `VERSION_MAJOR + 1`, asserting the status
  and connection teardown.

- **[TEST-2] Kill-deadline flake is a two-test class** (M, MED, HIGH)
  — `close_pane_kills_streaming_child_promptly` (`integration.rs:327`, the known
  flake) and `close_pane_kills_idle_child_promptly` (`:252`) both assert a 500ms
  wall-clock budget via a real-timer poll; the idle variant is one scheduling
  hiccup from the same intermittent failure. Fix: await a determinate exit/reap
  acknowledgement instead of a stopwatch; keep a generous outer timeout as a
  safety net, not the assertion. Apply to both so they don't drift apart.

- **[TEST-3] Pin the `SavedLayoutNode` persisted format** (S, LOW, HIGH)
  — session save is well-tested but there is no restore side, so the persisted
  shape can drift from what a future loader expects with no test catching it.
  A serialize → deserialize → reconstruct round-trip test locks the format now,
  ahead of restore (TREK-120). Cheap insurance.

### DX & docs

- **[DX-1] No Rust vulnerability scanning; CI names absent tooling** (S, LOW, HIGH)
  — `codeql.yml:26` comment points at cargo-audit/cargo-deny, but neither exists
  (no `deny.toml`, no mise task, no workflow). Zero RustSec advisory coverage on
  ~40 crates including native/unsafe-adjacent deps. Fix: a `cargo-deny`
  (advisories + licenses + bans) mise task and CI job — also enforces the
  MPL-2.0 license posture.

- **[DX-2] README has no build/run/install section** (S, LOW, HIGH)
  — a fresh clone can't build or launch from the README; the only setup line
  (`mise install`) lives in agent-facing `CLAUDE.md`. Fix: a "Build & Run"
  section covering `mise install`, `mise run check`, and how the `oakterm` /
  `oakterm-daemon` binaries start.

- **[DX-3] Drop the unused `prost` dependency** (S, LOW, HIGH)
  — `Cargo.toml:57` declares `prost = "0.14.4"` in `[workspace.dependencies]`;
  no crate references it. The hand-rolled wire codec is a deliberate choice, so
  the dep is dead weight implying a codegen path that doesn't exist. Fix: delete
  the line. (The larger "migrate the codec to prost" is L / HIGH-risk and **not**
  recommended.)

- **[DOC-1] Phase-0 "complete" vs four Phase-0 specs still `implementing`** (S, LOW, MED)
  — `33-roadmap.md:13` and `README.md:5` say Phase 0 is complete, but specs
  0001-0004 read `status: implementing` (only 0005/0006 are `complete`). Spec
  0001 demonstrably still gains messages, so the roadmap wording is the likelier
  stale side. Fix: reconcile — soften the roadmap to "foundation shipped,
  hardening ongoing", or bump the specs.

## Rejected / by-design (do not re-audit)

- Duplicate objc2/AppKit binding trees (`cargo tree -d`) — driven by
  `accesskit_macos` trailing on objc2 0.5; upstream-blocked, no local fix.
  Track the accesskit release that adopts 0.6; not a finding.
- `topology_snapshot` single-tab assumption, floating panes unpopulated,
  workspace switch/close/rename deferred, transient empty `MultiplexerState`,
  input-encoding gaps — all documented decisions (Spec-0007/0011, TREK-104/105/
  119/209/210/236-238).
- Lua sandbox, socket perms, wire-decode bounds, session-write atomicity, `pty`
  `pre_exec` — reviewed, sound.
- Client pane-exit `ReaderState` leak — already tracked as TREK-157.
- `oakterm/src/main.rs` size, border_drag extraction, BorderInteraction enum,
  ScrollbackData de-indent — already tracked.

## Not audited

GUI-side decode of daemon responses and keybind/action dispatch (`oakterm`
client internals), `oakterm-mux` geometry math (`geometry.rs`/`ops.rs`), the
scrollback archive on-disk decode path against a corrupt/hostile same-user
archive file, and the VT `handler.rs` per-control-sequence work. `cargo audit`
was not run (binary absent). DOC-1's resolution direction is inferred, not
verified per-spec.

## Deferred architecture notes (shaped, unscheduled)

- **Protocol dispatch duplication** — adding one message touches 4-5 files across
  3 crates; daemon (`requests/mod.rs`) and client (`daemon_conn.rs`) re-implement
  the `MSG_*` match independently. A message-descriptor trait would collapse the
  two match sites. (M)
- **Two layout-geometry walkers** — `mux/geometry.rs` (unit-square, mux tree) and
  `oakterm/layout.rs` (pixel-space, wire tree) independently derive pane rects and
  border adjacency; only the mux side is authoritative, so the GUI copy can
  silently disagree about hit-testing. (L)

## Action Items

Filed as trekker work in four batches (see the tasks referencing this review):

1. **Daemon hardening** — SEC-1, SEC-2, COR-2, TEST-1 (one batch); COR-1 separate
   (needs design call).
2. **Perf track** — PERF-4 benches first, then PERF-2 + PERF-3, then PERF-1.
3. **Hygiene chore batch** — DX-1, DX-2, DX-3, DOC-1, TEST-2.
4. **Restore prep** — TEST-3 folded into TREK-120.

Direction items (session restore, `ClientType` multi-client, release pipeline)
land in their existing/new tasks, not this review.
