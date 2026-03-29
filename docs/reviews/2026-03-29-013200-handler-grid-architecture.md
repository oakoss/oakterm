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

### Charset Mapping (addendum, 2026-03-29)

Research into DEC Special Character and Line Drawing charset mapping across four emulators:

- **Alacritty**: delegates directly to `vte::ansi::StandardCharset::map()`. No custom table.
- **wezterm**: own `remap_grapheme()` table (uses its own parser, not vte). Maps 0x60-0x7E only, does not map 0x5F.
- **Ghostty**: own comptime-initialized `[256]u16` lookup table. Maps 0x60-0x7E, does not map 0x5F. Also supports British charset.
- **vte**: maps 0x5F-0x7E (32 characters). Most complete table. Only supports Ascii and SpecialCharacterAndLineDrawing.

All four emulators agree on the 0x60-0x7E codepoint mappings (byte-identical). The 0x5F→space mapping is vte/xterm convention; wezterm and Ghostty skip it.

Decision: oakterm delegates to vte's `StandardCharset::map()` via `map_charset()` in handler.rs (same pattern as Alacritty). The function is documented as an extension point for user config overrides when Lua config lands.

### OSC Colors and Shell Integration (addendum, 2026-03-29)

Research into how emulators handle palette colors, dynamic colors, and shell integration marks.

**OSC 4 palette colors (0-255):**

- **Alacritty**: `Option<Rgb>` overlay array (269 slots, all `None`). `set_color` sets the slot; `reset_color` sets it back to `None`. Renderer resolves: check overlay first, fall back to config palette. Cleanest pattern.
- **wezterm**: copy-on-write fork of the config palette. `palette_mut()` clones the config palette on first write, then mutates in-place. Reset restores individual indices from config; if result matches config, drops the fork.
- **Kitty**: dual arrays (`color_table[256]` + `orig_color_table[256]`). Reset copies from orig back to current. Also has a color stack for push/pop.

**OSC 10/11/12 dynamic colors (foreground/background/cursor):**

- **Alacritty**: same overlay array, indices 256-258. OSC 11 background change affects both cell defaults AND the window clear color. `renderer.clear()` uses the resolved bg every frame.
- **wezterm**: named fields on `ColorPalette`. Changing background via OSC 11 changes window clear, padding, and default cell background simultaneously.
- **Kitty**: `DynamicColors` struct with `configured` + `overridden` layers. Also updates macOS titlebar color on bg change.

**Key insight for oakterm:** OSC 11 (background) must change both the Grid's default bg AND the renderer's window clear color. Our current architecture sends colors per-cell via RenderUpdate. For default-bg cells, the GUI renderer needs to know the terminal's dynamic background color. Options: (a) include dynamic_bg in RenderUpdate, (b) resolve at the daemon before serialization.

**OSC 133 shell integration:**

- **vte 0.15 does NOT handle OSC 133.** Must parse manually or ignore.
- **Alacritty**: not supported.
- **wezterm**: per-cell `SemanticType` (2-bit field in cell attributes). All cells inherit the current semantic type. Zones reconstructed by scanning.
- **Kitty**: per-line `prompt_kind` (2-bit field in line attributes). Simpler than per-cell.

Our Spec-0003 uses per-row `SemanticMark` (kitty's approach). Since vte doesn't dispatch OSC 133, we'd need to implement `osc_dispatch` on the Handler to catch it. For Phase 0.2, storing marks on rows is sufficient; per-cell semantics can come later if needed.

**OSC 7 working directory:**

- **vte does NOT handle OSC 7.**
- **Alacritty**: not stored; reads child CWD from `/proc`.
- **wezterm**: `current_dir: Option<Url>` on terminal state.
- **Kitty**: raw bytes on Screen object.

Decision for oakterm: store as `Option<String>` on Grid, like wezterm. Useful for tab titles and prompt navigation.

### Ghostty Background Rendering Pipeline (addendum, 2026-03-29)

Ghostty does NOT use the dynamic background as the Metal clear color. The clear color is hardcoded to transparent black `{0,0,0,0}`. Instead, the background is a full-screen triangle pass using a dedicated `bg_color_fragment` shader. This avoids CPU-side color space conversion.

**Rendering order:** (1) clear to transparent black, (2) full-screen bg_color pass, (3) per-cell backgrounds (default-bg cells are transparent, showing the bg_color through), (4) text glyphs.

**Color state:** `DynamicRGB` has `.default` (config) and `.override` (OSC 11). `.get()` returns override if set, else default. On OSC 11 change, the terminal sends a `color_change` message to the Surface, which on macOS propagates through Combine to update `NSWindow.backgroundColor` (titlebar reacts).

**Per-cell bg:** Default-bg cells get `{0,0,0,0}` in the bg_cells buffer. Only cells with explicit SGR colors get non-zero entries. The shader blends cell bg over global bg for text contrast calculation.

**Color space:** Ghostty stores colors as raw sRGB bytes in uniforms. All conversion (sRGB→Display P3, linearization) happens in GPU shaders. This avoids the gamma/color-space bugs that Ghostty #2125 documents (still open as of 2026).

**Implication for oakterm:** Our bg_colors buffer already uses packed ABGR per cell. We need to add a similar "global background" uniform to the GPU pipeline, set it from the daemon's `dynamic_bg` field (sent via RenderUpdate), and make default-bg cells transparent so the global bg shows through. The renderer changes are in the shader + uniform setup, not the cell data.

### Community Color/Theme Wishlist (addendum, 2026-03-29)

**Priority-ranked findings from across the ecosystem:**

1. **OS dark/light mode auto-switching** — the #1 request. 161 downvotes on Alacritty's refusal (issue #5999). Ghostty solved it with `theme = light:X,dark:Y` config. Kitty added OS appearance detection. The full cascade requires: OS detection → terminal theme switch → OSC 11 response update → app notification.

2. **Correct gamma/color space (sRGB, Display P3)** — Ghostty #2125 still open. Kitty fixed sRGB in #2249 via shader gamma correction. Modern Macs use Display P3 (25% wider gamut). GPU terminals rendering in sRGB on P3 displays show desaturated colors.

3. **OSC 10/11 query support** — critical for vim, neovim (auto-background since v0.10), bat, delta, starship. tmux #1919 historically broke passthrough. We now support this.

4. **Minimum contrast auto-adjustment** — accessibility feature. Adopted by iTerm2, Ghostty, kitty, wezterm, VS Code. Ghostty's shader blends cell bg + global bg for contrast calculation.

5. **Background images** — 246 reactions in Ghostty #3645. Ghostty ships `bg_image_fragment` shader that composites over bg_color.

6. **Toggle transparency keybind** — 110 reactions in Ghostty #5047. Screen-sharing use case.

7. **Per-tab/pane color coding** — 76 reactions in Ghostty #2509. iTerm2 parity.

8. **Live theme hot-reload** — wezterm's differentiator. Expected baseline.

**Key pain points:** Blue on black is the most common unreadable-color complaint. 256-color indices bypass themes. Theme switching doesn't update already-rendered content. Vim background mismatch creates ugly borders (fixable with our OSC 11 support).

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
