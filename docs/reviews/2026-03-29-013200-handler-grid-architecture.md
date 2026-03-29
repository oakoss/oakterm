---
title: Handler-Grid Architecture and Community Pain Points
date: 2026-03-29T01:32:00
scope: alternate screen architecture patterns (alacritty, wezterm, rio, ghostty), community pain points, feature priorities
---

# Handler-Grid Architecture and Community Pain Points

## Scope

Research into how Rust terminal emulators wire the VT handler to the screen buffer, how they implement alternate screen switching (DECSET 1049), and what the terminal emulator community wants. Triggered by TREK-28 (wire alternate screen into daemon) to inform the architectural approach.

## Findings

### Handler-Grid Patterns in the Wild

Three Rust terminal emulators and one Zig emulator (Ghostty) were studied. All share the same core pattern: the handler owns or directly borrows the struct that contains both grids.

**Alacritty/Rio:** `impl Handler for Term<T>`. The `Term` struct owns `grid` (active) and `inactive_grid` as flat sibling fields. On DECSET 1049, `mem::swap` physically exchanges the two Grid values. All handler methods reference `self.grid`, which always points to the active screen. Simple, proven, zero-overhead dispatch.

**wezterm:** `TerminalState` owns a `ScreenOrAlt` wrapper with `Deref`/`DerefMut` that dispatches to the active screen via a boolean flag. Screen switch is a flag flip, not a swap. Advantage: always knows which is primary and which is alternate (useful for `save_alternate_scrollback`). Disadvantage: branch on every grid access.

**Ghostty (Zig):** Uses a `PageList` (doubly-linked list of mmap pages) per screen. The terminal struct owns a `ScreenSet` managing both page lists. Screen switching operates through the `ScreenSet` abstraction rather than a direct `mem::swap`.

**OakTerm constraint:** Our handler doesn't own the grids. The daemon owns them in `Arc<Mutex<>>`, and the handler is a short-lived wrapper created per `process_bytes` call. We can't use the Alacritty "handler IS the owner" pattern directly. The wezterm-style dispatch pattern, adapted with a trait (`TermTarget`), lets us keep the borrowed handler while supporting both bare `Grid` (tests) and `ScreenSet` (daemon).

### Community Pain Points

**Alternate screen modes.** Three distinct modes exist (47, 1047, 1049) with subtle differences in cursor save/restore behavior. Ghostty PR #7471 fixed multiple bugs in their implementation. We should handle all three, not just 1049.

**Scrollback from alternate screen.** The single most common complaint: output from vim/less/man is lost on exit. iTerm2 offers `save_alternate_scrollback` (capture alternate screen scroll-off to primary). Kitty's maintainer instead offers a read-only overlay to peek at primary while in alternate mode. Our Spec-0003 already covers `save_alternate_scrollback`.

**Reflow + saved cursor.** Every terminal gets this wrong. The behavior of DECSC/DECRC saved cursor position during resize/reflow is unspecified in any standard. Real bugs: entering vim, resizing, exiting, losing shell text. Filed against alacritty, kitty, ghostty, tmux, Windows Terminal. Our opportunity: define and document explicit behavior in Spec-0003.

**Scrollback search.** Ghostty shipped without it until v1.3. Was the most requested feature across discussions. We should plan for it.

**OSC-52 clipboard across SSH + multiplexer.** Universally broken because tmux intercepts OSC-52 and uses its own buffer. Since OakTerm owns both terminal and multiplexer, clipboard passthrough can work without tmux's interception problem.

**Cell size and scrollback memory.** Ghostty uses ~12.5 bytes/cell. At 200 columns, a 10 MB scrollback limit only holds ~4,000 lines. Users expect far more. Compression of trailing blank cells in static scrollback is the main optimization (Ghostty discussion #9821). Spec-0003 targets 8-24 bytes/cell with style deduplication; the current implementation is 56 bytes.

### Architecture Lessons

**Style deduplication early.** Ghostty uses a reference-counted style set; cells store a style ID instead of inline style data, and identical styles share a single allocation. Alacritty enforces 24 bytes/cell via a compile-time size test. Our Spec-0003 targets 8-24 bytes/cell with style deduplication, though the current implementation is at 56 bytes (noted as tech debt in EPIC-4). Style deduplication should be planned for the scrollback implementation.

**Parser synchronization.** Naive per-byte locking between parser and renderer wastes most time in locking. Batch-parsing with a single lock hold (our current approach in `pty_read_loop`) is the right pattern. Our daemon architecture naturally batches.

**Crash isolation.** One pane's VT handler panic shouldn't crash all panes. Rust's `catch_unwind` at the pane thread boundary provides isolation. Design for this when adding pane management.

**Daemon memory target.** Zellij's 80 MB idle is considered too high by the community. tmux at 6 MB is the benchmark. No formal daemon memory target has been set for OakTerm yet; the 50 MB per-pane scrollback hot buffer (ADR-0006) is separate from daemon idle memory.

### Validated Decisions

1. **Daemon architecture (ADR-0007).** wezterm and iTerm2 both use daemon/server models for session persistence. Our daemon + wire protocol is the right approach. Crash isolation between GUI and terminal state is a real benefit.

2. **`save_alternate_scrollback` (Spec-0003).** Directly addresses the #1 community complaint about alternate screen.

3. **Wire protocol with sequence numbers (Spec-0001).** The push-notify + pull-data model with dirty tracking is proven. Avoids the per-byte locking trap.

4. **Deferred PTY spawn until first client Resize (TREK-22).** Avoids startup resize noise. Other daemon-based terminals (wezterm) spawn on pane creation, but none defer the default shell until the GUI reports its dimensions.

5. **Stale socket recovery with flock-based startup lock (TREK-34).** Prevents race conditions when multiple GUI clients start simultaneously. tmux's connect-then-lock pattern, adapted.

### Challenged Decisions

1. **ModeFlags as a bitfield for all modes.** Our current 2048-bit bitfield stores every mode. Alacritty uses a `TermMode` bitflags type with explicit named flags. The bitfield approach is simpler but doesn't distinguish between "mode was never set" and "mode was set then cleared." For modes like DECOM (origin mode) that affect other operations, explicit named flags would be clearer. Consider a hybrid: named flags for modes with behavioral effects, bitfield for storage-only modes.

2. **Cell size at 56 bytes.** Already tracked as tech debt, but the research confirms this matters more than expected for scrollback. Every byte per cell multiplies by millions of cells. Style deduplication should be higher priority than currently planned.

3. **Three alternate screen modes.** Our current implementation only plans for 1049. Modes 47 and 1047 have different cursor save/restore semantics. Should be addressed during TREK-28.

## Action Items

### Corrections

- TREK-28 description only mentions mode 1049. Update to include modes 47 and 1047.

### ADR Candidates

- **Reflow + saved cursor behavior.** No standard exists. We need to decide and document: when the grid resizes, what happens to DECSC-saved cursor position? Options: clamp, reflow-aware adjustment, or reset. This affects every cursor-saving operation (1049 enter/exit, DECSC/DECRC).

- **Alternate screen peek.** Should users be able to view the primary screen while in alternate mode? Kitty rejected toggling (corrupts state) but offers a read-only overlay. Worth an ADR if we want to differentiate.

### Missing Specs

- Spec-0002 lists modes 47 (AltScreenLegacy) and 1049 (AltScreenSaveCursor) but is missing mode 1047. The behavioral differences between all three modes, particularly cursor save/restore semantics, are not specified.

### For TREK-28

The `TermTarget` trait approach is the right adaptation of wezterm's pattern for our borrowed-handler architecture. Implement with:

- Free functions for grid manipulation (avoids borrow checker conflicts)
- `TermTarget` trait implemented by both `Grid` (tests) and `ScreenSet` (daemon)
- Handle modes 47, 1047, and 1049 with correct cursor semantics per Ghostty's PR #7471

## References

- [Ghostty alt screen PR #7471](https://github.com/ghostty-org/ghostty/pull/7471)
- [Kitty alternate screen toggle #933](https://github.com/kovidgoyal/kitty/issues/933)
- [Ghostty memory leak fix](https://mitchellh.com/writing/ghostty-memory-leak-fix)
- [Ghostty scrollback memory discussion #9821](https://github.com/ghostty-org/ghostty/discussions/9821)
- [libghostty announcement](https://mitchellh.com/writing/libghostty-is-coming)
- [Windows Terminal reflow #4200](https://github.com/microsoft/terminal/issues/4200)
- [neugierig terminal emulator adventures](https://neugierig.org/software/blog/2016/07/terminal-emulators.html)
- [Complex scripts in terminals](https://thottingal.in/blog/2026/03/22/complex-scripts-in-terminal/)
- [wezterm multiplexing docs](https://wezterm.org/multiplexing.html)
- [iTerm2 session restoration](https://iterm2.com/documentation-restoration.html)
- [tmux scrollback in practice](https://www.freecodecamp.org/news/tmux-in-practice-scrollback-buffer-47d5ffa71c93/)
- [OSC-52 and tmux](https://kalnytskyi.com/posts/on-tmux-osc52-support/)
