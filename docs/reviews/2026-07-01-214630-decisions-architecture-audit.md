---
title: Decisions & Architecture Audit
date: 2026-07-01T21:46:30
scope: all 19 ADRs, all 10 specs vs implementation, workspace architecture (11 crates), pipeline/roadmap hygiene + past-review carryovers
---

# Decisions & Architecture Audit

## Scope

Full point-in-time audit at the Phase 0 → Phase 1 boundary: every ADR checked for internal consistency and staleness, every spec checked against the code that claims to implement it, the crate architecture reviewed against ADR-0007 and the Phase 1+ roadmap, and all six past reviews checked for unaddressed action items. Four parallel review passes (ADR decisions, spec conformance, code architecture, pipeline hygiene), findings deduplicated and cross-linked here.

## Findings

### Code Bugs

Spec-conformance defects found in code, not docs. Ordered by severity.

1. **Unknown `msg_type` errors instead of being ignored** (HIGH). Spec-0001 requires unknown message types to be ignored with a debug log — the forward-compatibility guarantee the versioning contract rests on (`docs/specs/0001-daemon-wire-protocol.md:383`). The daemon returns `ErrorCode::InvalidMessage` instead (`crates/oakterm-daemon/src/server.rs:1482-1490`). Any newer client talking to an older daemon breaks.
2. **Cold scrollback archive is write-only** (HIGH). Spec-0004 says `GetScrollback` falls back to the archive for rows pruned from the hot buffer (`docs/specs/0004-scroll-buffer.md:175`). The handler reads only the hot buffer (`server.rs:1102-1168`); `ArchiveManager::read_rows` and the entire `SegmentReader` path have no non-test caller. Pruned scrollback is permanently unreachable, and `has_more` (`server.rs:1127`) can never point at it. Search has the same gap (`crates/oakterm-terminal/src/search.rs:73`).
3. **A11y scroll properties lost on incremental update** (MEDIUM). `scroll_y`/min/max are set only in `build_initial_tree` (`crates/oakterm-a11y/src/lib.rs:51-53`); `build_incremental_update` rebuilds the Terminal node without them, and the GUI scrollback path always triggers the rebuild (`crates/oakterm/src/main.rs:1423`). Screen readers lose scroll position — a real AT bug against Spec-0006 (`:37-39`).
4. **Specced VT sequences are silent no-ops** (MEDIUM). OSC 52 clipboard, OSC 8 hyperlinks, ED 3 clear-scrollback, and DECSTR are in Spec-0002 (`:250, :280-296`, `:123`) but unhandled (`crates/oakterm-terminal/src/handler.rs:591` is an empty arm; cell infra for hyperlinks already exists). Small overrides each.
5. **Grid resize doesn't reflow soft-wrapped lines** (MEDIUM). Spec-0003 mandates unwrap-on-grow / rewrap-on-shrink (`:378-388`); `resize()` only splits/pads rows (`crates/oakterm-terminal/src/grid/mod.rs:206-250`). Wrapped rows keep stale flags and truncated content after resize. Either implement reflow or mark the contract item deferred (and see Status Hygiene).
6. **Selection never surfaced to assistive tech** (MEDIUM). Spec-0006 maps `Selection` → AccessKit `TextSelection` (`:269-299`); the a11y crate's inputs have no selection field and the GUI never passes the selection it tracks (`main.rs:286`). Related: `SetTextSelection` is advertised on the node but dropped by the GUI (`main.rs:1591`).

### Corrections

Doc fixes needing no decision — fix directly.

- **Six one-line idea-doc corrections from the 2026-03-26 audit, still unapplied after 3 months**: iTerm2 GPU claim (`docs/ideas/11-inspiration.md:101`), WezTerm memory claim (`11-inspiration.md:51`), Zellij WASM scoping (`11-inspiration.md:65`), deprecated `NSUserNotification` (`34-notifications.md:53`), discontinued Touch Bar (`20-platform-support.md:38,164`), wrong OSC 7 format (`18-shell-integration.md:34` — persists even though the doc is `decided` and ADR-0008 touched it).
- **Soak-test contradiction (audit item #9, also from 2026-03-26)**: `15-memory-management.md:166-169` puts a 24-hour soak test under "Every PR runs:", contradicting `25-testing.md` (nightly). Move to nightly.
- **ADR-0001 still carries the frame budget ADR-0002 abolished**: "<0.5ms/frame when a screen reader is active" (`docs/adrs/0001-accessibility-in-phase-zero.md:31,79`) is exactly the invented per-component number ADR-0002 cites as the problem. Reframe as a benchmark-relative target.
- **ADR-0008 roadmap edit half-applied**: OSC 133/7 parsing was added to Phase 0 (`docs/ideas/33-roadmap.md:26`) but Phase 3 still lists OSC 133/7 parsing (`:126`) and scroll-to-prompt (`:127`), which ADR-0008 moved to Phase 1. Finish the edit.
- **Stale point-in-time claims in ADRs**: ADR-0015 line 55 says "no OSC 133 handler yet" (landed in #32); ADR-0012 says search/copy-mode messages are "not yet in spec" (all now in Spec-0001); ADR-0007's daemon-crash row (`:151`) reads as full recovery but Spec-0010 excludes scrollback by design.
- **Spec-stale updates (code is right, spec text isn't)**: Spec-0001 missing FindPrompt 0x75/PromptPosition 0x76 (live in `crates/oakterm-protocol/src/message.rs:589-682`) and misdescribing SearchClose's payload; Spec-0002's Perform/Handler trait architecture doesn't match the `vte::ansi::Handler` reality; Spec-0003 understates DECSC state and ScreenSet fields; Spec-0004 `save_alternate_scrollback` default self-contradiction (`:143` vs `:164`, code uses false); Spec-0005 lists `rawget`/`rawset` as available but code strips them, omits shipped `scroll_indicator`/`text_blending`/`text_gamma`/`oakterm.appearance()`, and presents Phase-1 keys/events/layout API that hard-error today; Spec-0007's parallel-array `Container` contradicts the implemented `Vec<Child>` without noting the in-memory-vs-DTO split.

### Contradictions

Conflicts needing a decision — each is an ADR candidate.

1. **Daemon upgrade / version skew** — the ADR that ADR-0007 explicitly owes ("Write the follow-up ADR when persistence lands", `docs/adrs/0007-daemon-architecture.md:154-158`). Persistence has landed (Spec-0010 accepted, prior art already seeded in #34). Highest-priority candidate: state serialization vs side-by-side coexistence, and how tolerated version mismatch is surfaced.
2. **Image payload transport across the daemon/GUI split** — ADR-0004 puts Kitty graphics parsing in the VT layer and compositing in the renderer, but per ADR-0007 those live in different processes and Spec-0001 has no image/blob message at all. 0007's throughput rationale ("screen buffer is small, ~160KB") ignores image volume. The spec audit confirms no APC handling exists in code either. Decide transport + texture ownership before implementing ADR-0004; until then it is unimplementable as specced.
3. **Session-persistence policy is spec-only** — Spec-0010 makes genuine decisions (scrollback deliberately not persisted, restartable-command allowlist, restore-prompt UX) with no ADR, against "Decisions go in ADRs". Back-fill or at minimum cross-reference from ADR-0007.
4. **MPL-2.0 has no ADR** — `26-license.md` is `decided` with no decision record; exactly the class the 2026-06-30 choices audit created ADR-0017/18/19 to fix.
5. **Smaller open items**: Windows `oak_mod` default undefined (ADR-0011 covers only macOS/Linux); clipboard/OSC 52 ownership across the process split; multi-client focus/resize arbitration on shared panes; floating-drawer/popup/modal internal model (ADR-0010 defers it, Spec-0007 covers floating panes only).

### Architecture Risks

Code-level findings against the ADR-0007 model and Phase 1 readiness. Layering is otherwise clean (protocol/pty/renderer/config are leaves; terminal is renderer-independent), unwrap-free daemon/GUI loops, strong tracing, benches on the real hot paths.

1. **GUI depends on the daemon crate** (HIGH). `crates/oakterm` pulls in `oakterm-daemon` solely for `socket_path()`/`acquire_startup_lock()` (`crates/oakterm/src/main.rs:2399,2414`), dragging the whole server plus `oakterm-pty` into the client binary — against ADR-0007's process separation. Move those two functions into `oakterm-protocol` (or a tiny IPC crate). Same trap awaits `oakterm-ctl` when it's built (currently a 3-line stub).
2. **The mux layout tree is orphaned; three seams must grow together** (HIGH). `oakterm-mux` has no consumer. Daemon state is a flat `HashMap<u32, PaneState>` (`server.rs:116-120`), the protocol has no Split/Layout/resize messages (catalog reserves 0xA0-0xA4 but nothing is specced into structs), and the GUI renders only the focused pane full-window. Sequence Phase 1 as a vertical slice — protocol messages + `SavedLayoutNode` DTO first, then daemon tree ownership, then GUI geometry — rather than building more tree operations (TREK-97) ahead of any consumer.
3. **Global `Mutex<PaneManager>` serializes all panes** (MEDIUM). Every PTY read loop holds the single lock across VT parsing (`server.rs:417-431`) and every client read contends on it. With N panes and shared sessions (both Phase 1 goals), a burst on one pane blocks renders of another. Move to per-pane locking before mux lands.
4. **Renderer and a11y are single-grid** (MEDIUM). `RenderPipeline::render` clears the full surface with no origin/scissor (`crates/oakterm-renderer/src/pipeline.rs:284-297`); the a11y tree has one terminal node with no pane hierarchy. Both need their multi-pane shape decided alongside the mux GUI slice, not after — "accessible from day one" argues against retrofitting.
5. **God modules at the seams Phase 1 will stress** (MEDIUM). `server.rs` 1969 LOC with a ~728-line `handle_request` dispatch, `handler.rs` 2818 LOC, `main.rs` 3300 LOC with an `App` god type. Split before mux message handlers land.
6. **Small lifecycle nits** (LOW): hardcoded default pane 0 (`server.rs:196,246`) breaks once layout/persistence create arbitrary IDs; `client_count` counts all client types, so a lingering control client will keep a non-persist daemon alive (`server.rs:271-273`).

### Challenged Decisions

- **ADR-0018's validation gate never ran.** The wgpu decision is conditional on a criterion parity benchmark vs Alacritty/Ghostty (`docs/adrs/0018-gpu-rendering-wgpu.md:62-70`), yet Phase 0 is declared complete. The gate exists as TREK-166 (P2, no epic) and hasn't executed. Run it and record the result in 0018, or downgrade the "must reach parity" language. Don't let the named acceptance gate of an accepted ADR sit in the icebox.
- **ADR-0013 Fig coupling needs a freshness check before acceptance.** The "active community keeps specs current" pro predates verifying post-Amazon-Q maintenance cadence of withfig/autocomplete. Still `proposed`, so cheap to check now.

### Status Hygiene

- **Spec-0004 `complete` is inaccurate** (archive read path unwired — Code Bug 2); **Spec-0003 `complete` overclaims** (no resize reflow — Code Bug 5). Downgrade to `implementing` or explicitly mark the contract items deferred. Spec-0005 `complete` is defensible only if its Phase-1 surface is split out or annotated as deferred (today the spec's own examples hard-error).
- All three README indexes (ADRs, specs, reviews) are accurate. ADR statuses match files. Trekker epics all trace to accepted specs — no "no spec = not ready" violations.
- `03-multiplexer.md` is stuck in `reviewing` despite four accepted ADRs (0010/0011/0012/0016) resolving its questions and Phase 1 underway. Advance it.
- `33-roadmap.md` has no phase-status marker despite Phase 0 being done; one line fixes it.
- Specs 0001/0002 remain `implementing` — correct, both are still being extended.

### Carryovers

The meta-finding of this audit: **ADR-worthy contradictions from past reviews reliably become ADRs; cheap corrections and idea-doc updates reliably rot.** Evidence: 6 of 7 "fix directly" items from 2026-03-26 unapplied; Herdr review (2026-05-06) idea-doc updates to `04-sidebar.md`/`32-agent-control-api.md`/`39-agent-protocol.md` entirely unaddressed; Warp review (2026-04-28) updates to `05-context-engine.md` absent; none of the requested Phase 2/3 icebox/ADR-candidate Trekker tasks were ever filed. Deferring the Phase 2/3 items is defensible under the 2026-06-22 hold-the-line guidance — but they were silently dropped, not parked. The fix is cheap: file the icebox tasks so deferral is a decision, not amnesia.

## Action Items

**Fix code (file as Trekker tasks against the owning specs):**

1. Ignore unknown `msg_type` per Spec-0001 forward-compat (Code Bug 1).
2. Wire `ArchiveManager::read_rows` into `GetScrollback` + archive-aware `has_more`; extend search to the archive or document hot-only (Code Bug 2).
3. Carry scroll properties through incremental a11y updates; surface selection or scope it out of Phase 0 explicitly (Code Bugs 3, 6).
4. Implement OSC 52 / OSC 8 / ED 3 / DECSTR overrides (Code Bug 4).
5. Decide reflow-on-resize: implement or defer explicitly, then fix Spec-0003 status (Code Bug 5).
6. Move `socket_path()`/`acquire_startup_lock()` out of `oakterm-daemon` into `oakterm-protocol` (Architecture Risk 1).
7. Per-pane locking in `PaneManager` before mux integration (Architecture Risk 3).
8. Run TREK-166 (wgpu parity benchmark) and record the result in ADR-0018.

**Corrections (one `docs` pass):** the six 2026-03-26 idea-doc fixes, soak-test move, ADR-0001 budget reframe, ADR-0008 roadmap completion, stale ADR point-in-time claims, roadmap phase-status line, `03-multiplexer.md` status, and the spec-stale updates listed above.

**ADRs to write:** daemon upgrade/version skew (owed by 0007, unblocked); image payload transport (blocks ADR-0004 implementation); session-persistence policy back-fill (or cross-reference); license ADR for MPL-2.0. Smaller: Windows `oak_mod` default in ADR-0011.

**Process:** when a review defers an item, file the icebox task in the same pass — deferral must leave a trace. Attach TREK-166-class validation gates to an epic, never the icebox.

**Phase 1 sequencing:** build the multiplexer as a vertical slice (protocol → daemon tree → GUI geometry → a11y pane hierarchy), splitting `handle_request`/`App` before the new message families land.
