---
title: Renderer Architecture Review
date: 2026-03-28T16:00:00
scope: 'TREK-15 wgpu renderer design — pipeline, atlas, color, cursor'
---

# Renderer Architecture Review

## Scope

Pre-implementation review of the wgpu GPU renderer (TREK-15). Researched Alacritty, Ghostty, Kitty, WezTerm, Rio, and Zed terminal rendering pipelines, glyph atlas strategies, color correctness, and cursor UX. Decisions documented here guide the Phase 0.1 implementation.

## Decisions

### Render Pipeline: Two-Pass (Ghostty Style)

**Pass 1 — Background:** Full-screen fragment shader reading a flat cell-bg buffer. Each cell's background color is a value in a buffer indexed by `(row * cols + col)`.

**Pass 2 — Text:** Instanced quads sampling from the glyph atlas. Each instance is a cell position + atlas coordinates + foreground color.

**Why two-pass:** Enables Kitty graphics protocol images to be layered between backgrounds and text in future phases. Ghostty uses this approach. Alacritty renders rects and glyphs in a similar multi-step pipeline. Single-pass (Rio) has fewer draw calls but prevents image layering without a refactor.

**Alternatives considered:**

- Single-pass combined bg+text (Rio): fewer draw calls, but locks out image layering
- Conditional single/multi-pass (Kitty): adapts to content, but complex conditional logic
- Full-screen shader computing cell index from pixel coords (Alacritty PR #4373): 1.5-2x speedup, but experimental

### Glyph Atlas: Dual Format with LRU Eviction

| Parameter            | Value                             | Rationale                                                                            |
| -------------------- | --------------------------------- | ------------------------------------------------------------------------------------ |
| Grayscale format     | `R8Unorm`                         | 1 byte/pixel. Text is 95%+ of glyphs.                                                |
| Color format         | `Rgba8UnormSrgb`                  | 4 bytes/pixel. Emoji, colored glyphs.                                                |
| Initial size         | 256x256                           | Match glyphon. WezTerm's 4096x4096 default contributed to ~65 MiB GPU memory/window. |
| Growth               | Double per axis                   | Cap at `device.limits().max_texture_dimension_2d`.                                   |
| Packing              | `etagere::BucketedAtlasAllocator` | Battle-tested in Firefox/WebRender.                                                  |
| Eviction             | LRU (skip in-use glyphs)          | Handles long sessions. WezTerm hit 1.4 GB without eviction.                          |
| Subpixel positioning | No                                | Monospace text doesn't benefit.                                                      |
| Sampling             | `FilterMode::Nearest`             | Crisp text. Linear causes blur (WezTerm complaints).                                 |
| DPI change           | Full atlas rebuild                | Universal approach. No terminal maintains multi-DPI atlases.                         |
| Shared across tabs   | Atlas in renderer, not per-pane   | Ghostty shares atlas across surfaces to reduce per-pane overhead.                    |

**Community pain points addressed:**

- WezTerm #306: ~65 MiB GPU memory on startup (large atlas contributing) → start at 256x256
- WezTerm #2626: 1.4 GB after 18 hours → LRU eviction
- WezTerm #6686: blurry text → `FilterMode::Nearest`
- Alacritty: 4x memory waste on all-RGBA → dual atlas

### Color: sRGB Hardware + Text Contrast (B+C Hybrid)

**Surface format:** `Rgba8UnormSrgb` — GPU handles sRGB ↔ linear conversion on texture read/framebuffer write automatically.

**Text contrast:** Fragment shader adjusts glyph alpha based on fg/bg luminance difference. ~20 lines of shader code. Prevents thin-looking text on dark backgrounds (common complaint).

**Why not pure hardware (B):** Correct blending but thin text on dark backgrounds.

**Why not full manual LUT (C):** More shader code for the same sRGB conversion the hardware does for free. We keep only the text contrast piece.

**Deferred:** Wide gamut (Display P3, Rec.2020) — Phase 4+ when Rio proves the approach.

**Community pain points addressed:**

- Alacritty #118: text compositing artifacts from gamma-space blending → sRGB-correct pipeline
- WezTerm #3625: color differences between backends → hardware sRGB
- Kitty's approach validated: text contrast knob improves perceived quality

### Cursor: Contrast-Aware with Config Precedence

**Phase 0.1 scope:**

| Feature           | Implementation                                                    |
| ----------------- | ----------------------------------------------------------------- |
| Styles            | Block, beam, underline, hollow (DECSCUSR 0-6)                     |
| Rendering         | Rects in background pass                                          |
| Blinking          | CPU-side toggle, 530ms default                                    |
| Blink timeout     | Stop after 15s idle (Kitty default)                               |
| Unfocused         | Configurable: hollow (default), block, beam, underline, unchanged |
| Colors            | Contrast-aware reverse video                                      |
| DECSCUSR 0        | Restore to user config default, not terminal hardcoded            |
| Config precedence | User config > shell integration > escape sequence > default       |
| Wide chars        | Block cursor spans 2 cells over CJK                               |

**Deferred:**

- Cursor trail / smooth movement → Phase 1
- Easing functions for blink → Phase 1
- Cursor guide / crosshair → Phase 0.3 (accessibility)
- Cursor opacity → Phase 1
- `prefers-reduced-motion` → Phase 0.3 (Spec-0006)
- Cursor shaders → Phase 2 (plugin system)
- Multiple cursors protocol → future (as of March 2026, no editor widely supports it)

**Community pain points addressed:**

- Ghostty #2806: shell integration overrides user blink setting → explicit precedence model
- Windows Terminal #1604: DECSCUSR 0 restores wrong default → user config default
- Kitty text contrast: naive reverse video fails on similar colors → luminance check
- Alacritty #8816: cursor invisible on click-reposition during blink off-phase → reset blink phase on cursor movement

## Validated Decisions

- Two-pass pipeline proven by Ghostty; Alacritty uses similar multi-step approach
- Dual atlas format is standard (Ghostty, Rio, glyphon)
- sRGB surface format eliminates a class of color bugs
- `etagere` allocator battle-tested in Firefox
- LRU eviction handles CJK/emoji sessions without unbounded growth

### Glyph Rasterization: Swash with Backend Abstraction

**Phase 0.1:** `SwashShaper` — pure Rust, hinting (TrueType + CFF), color emoji (sbix, CBDT, COLR). No C dependencies.

**Later phases:** Add platform-native backends behind the `TextShaper` trait:

- `CoreTextShaper` — macOS native, pixel-perfect Apple rendering
- `FreetypeShaper` — Linux native, respects fontconfig (lcdfilter, hintstyle, rgba)
- `DirectWriteShaper` — Windows native

**Config:** `font_rasterizer = "auto"` (default — native if available, swash fallback), `"native"`, `"swash"`.

**Why not native from Phase 0:** C build dependencies (FreeType, fontconfig) complicate CI and cross-compilation. The `TextShaper` trait means swapping is a new implementation, not a refactor. Swash quality is good enough for Phase 0 (Rio and COSMIC Terminal ship with it).

**Why native later:** Platform-native rasterizers match system rendering. Linux users configure fontconfig extensively. macOS users expect Core Text rendering. Every "best-in-class" terminal (Alacritty, Ghostty, Kitty) uses platform-native. Text quality is the one thing a terminal cannot compromise on.

**Crates eliminated:**

- fontdue: no hinting, disqualifying for terminal text
- ab_glyph: no hinting, same issue
- cosmic-text: full text layout engine, overkill (we only need rasterization)
- crossfont (Alacritty's): tightly coupled to Alacritty, primarily monospace

### Window + Surface Architecture

| Decision           | Choice                                             | Rationale                                                           |
| ------------------ | -------------------------------------------------- | ------------------------------------------------------------------- |
| Event loop         | winit on main thread                               | Universal pattern across all terminals                              |
| GPU ownership      | Renderer struct owns device/queue/pipelines        | Separates GPU from input handling                                   |
| Surface lifetime   | `Arc<Window>` for winit 0.30+                      | Community consensus, avoids unsafe lifetime hacks                   |
| Present mode       | `AutoVsync`, user-configurable                     | Graceful fallback. Fifo too rigid for all monitors.                 |
| Power preference   | `LowPower` default, configurable                   | Terminal doesn't need discrete GPU                                  |
| First frame        | Clear to bg → present → show window                | Eliminates white flash (Alacritty pattern)                          |
| Idle rendering     | Event-driven only                                  | Render on: PTY output, input, cursor blink timer, resize            |
| Cursor blink timer | Fire at blink interval, one frame per transition   | Not at display refresh rate. Avoids Ghostty's ProMotion 10-15% CPU. |
| Surface errors     | Handle all 5 wgpu variants                         | Reconfigure on Outdated, skip on Timeout/Occluded                   |
| DPI changes        | `ScaleFactorChanged` → reconfigure + rebuild atlas | wgpu doesn't handle this automatically                              |
| Texture format     | sRGB variant of first supported format             | Check `SURFACE_VIEW_FORMATS` before setting view_formats            |
| Alpha mode         | `PostMultiplied` → `PreMultiplied` → `Auto`        | Rio + WezTerm pattern, supports transparency                        |

**Community pain points addressed:**

- Ghostty Discussion #10397: 10-15% idle CPU from cursor blink at 120Hz → timer-based, not refresh-rate-based
- WezTerm #2027: high idle CPU compared to competitors → event-driven rendering
- Alacritty #297: white flash on startup → clear bg before window visible
- wgpu #5353: surface outdated on resize → handle all error variants
- WezTerm #3565: panic on devices lacking `SURFACE_VIEW_FORMATS` support → check capability first

**Deferred:**

- Software renderer fallback → Phase 1 (Iced pattern)
- Custom frame pacing / `repaint_delay` config → Phase 1 (Kitty model)
- Background blur → Phase 1+ (fragile across compositors)

## Action Items

1. Add `etagere` and `swash` to workspace dependencies; add `wgpu`, `etagere`, `swash` to `oakterm-renderer`
2. Implement TREK-15 following decisions above
3. Track cursor trail and smooth movement for Phase 1 planning
4. Track cursor guide for Phase 0.3 accessibility work
5. Track native rasterizer backends (Core Text, FreeType, DirectWrite) for Phase 1

## References

- [Alacritty renderer source](https://github.com/alacritty/alacritty/tree/master/alacritty/src/renderer)
- [Ghostty rendering pipeline](https://deepwiki.com/ghostty-org/ghostty/5.3-rendering-pipeline-and-shaders)
- [Kitty shaders](https://github.com/kovidgoyal/kitty/blob/master/kitty/shaders.c)
- [Rio Sugarloaf renderer](https://github.com/raphamorim/rio/tree/main/sugarloaf/src)
- [Glyphon wgpu text rendering](https://github.com/grovesNL/glyphon)
- [Etagere atlas allocator](https://github.com/nical/etagere)
- [WezTerm atlas issues #306, #2626](https://github.com/wezterm/wezterm/issues/306)
- [Alacritty sRGB issue #118](https://github.com/alacritty/alacritty/issues/118)
- [Ghostty cursor blink override #2806](https://github.com/ghostty-org/ghostty/issues/2806)
- [Kitty multiple cursors protocol](https://sw.kovidgoyal.net/kitty/multiple-cursors-protocol/)
