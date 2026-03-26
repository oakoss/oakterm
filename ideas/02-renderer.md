---
title: 'Renderer'
status: draft
category: core
description: 'GPU (wgpu), fonts, fallbacks, ligatures, opacity, color, images'
tags: ['gpu', 'wgpu', 'fonts', 'ligatures', 'images', 'opacity', 'color']
---

# Renderer

## Goals

- Sub-frame input latency (target: <8ms, competitive with Alacritty and Foot)
- Platform-native text shaping — Core Text on macOS, HarfBuzz on Linux
- Ligature support from day one (Alacritty's #50 has been open since 2017)
- Inline image compositing via Kitty graphics protocol + Sixel fallback
- Both image protocols work through the multiplexer (unlike tmux where neither works)

## Approach

- `wgpu` for GPU rendering (WebGPU abstraction over Metal/Vulkan/DX12)
- Rio terminal already proves wgpu works for terminals
- Glyph atlas shared across windows via server/client architecture (from Foot)
- Unicode 16.0 grapheme cluster width — not legacy `wcwidth()`
- Pixel-smooth scrolling (from Contour)

## Font Rendering

Community pain point: every terminal gets complaints about font rendering on macOS.

Solution: defer to the platform.

- macOS: Core Text for shaping and rasterization
- Linux: HarfBuzz for shaping, FreeType for rasterization
- Don't fight the OS — let it do what it's good at

## Font Fallback Chain

You shouldn't have to use patched fonts. Use the font you actually want and let the terminal handle missing glyphs by falling back through a chain.

```lua
font = {
  family = "JetBrains Mono",
  fallbacks = {
    "Symbols Nerd Font",       -- nerd font icons
    "Apple Color Emoji",       -- emoji (macOS)
    "Noto Color Emoji",        -- emoji (Linux)
  },
  size = 14,
}
```

How it works:

1. Render glyph with primary font (`JetBrains Mono`)
2. Glyph missing? Try first fallback (`Symbols Nerd Font`)
3. Still missing? Try next fallback, and so on
4. Last resort: platform default symbol font

This means:

- Use any font you want — no patching, no Nerd Font versions
- Nerd Font symbols work by having the symbols-only font in the fallback chain
- Emoji just works — platform emoji font is in the chain
- CJK characters can be handled by adding a CJK font to the chain
- The glyph atlas caches resolved glyphs per-codepoint so fallback lookup only happens once

The fallback chain should be per-style (regular, bold, italic) so bold text can fall back to a bold variant of the symbols font if available.

Kitty and Ghostty both do font fallback, but configuration varies. Ours should be explicit and ordered — you control exactly which font provides which glyphs.

## Ligatures

Alacritty has had a ligature request open since 2017 (issue #50) with no plans to implement. This is a solved problem — Kitty, Ghostty, and WezTerm all support ligatures. We ship with them on by default.

Ligatures matter for coding fonts like:

- **Fira Code** — `=>`, `->`, `!=`, `>=`, `===`, `<|>`, `>>`, `|>`
- **JetBrains Mono** — `!=`, `<=`, `>=`, `-->`, `<->`
- **Cascadia Code** — `www`, `&&`, `||`, `::`, `===`
- **Monaspace** — texture healing + ligatures
- **Iosevka** — extensive ligature sets

How it works:

- HarfBuzz / Core Text handle ligature substitution during text shaping — the font's OpenType `liga` and `calt` tables define which glyph sequences get replaced
- The renderer treats a ligature as a single glyph spanning multiple cells
- Cursor movement still advances per-character, not per-ligature

```lua
font = {
  family = "JetBrains Mono",
  ligatures = true,           -- default: true
  -- Or selectively disable specific ligatures:
  -- disabled_ligatures = { ">=", "!=" },
  fallbacks = {
    "Symbols Nerd Font",
    "Apple Color Emoji",
  },
  size = 14,
}
```

Disabling ligatures entirely is one setting: `ligatures = false`. No recompilation, no patching, no config gymnastics.

## Window Opacity & Blur

```ini
background-opacity = 0.9
background-blur = true
```

- `background-opacity` — 0.0 (fully transparent) to 1.0 (fully opaque, default)
- `background-blur` — platform-native blur behind the transparent window
  - macOS: NSVisualEffectView (vibrancy/blur)
  - Linux/GTK: compositor-dependent (KDE/Sway support blur, GNOME does not)
- Text stays fully opaque regardless of background opacity — only the background is affected
- Works with both dark and light themes
- Can be set per-theme so your dark theme is slightly transparent and your light theme is opaque:

```ini
theme-dark = catppuccin-mocha
theme-light = catppuccin-latte
theme-dark.background-opacity = 0.85
theme-dark.background-blur = true
theme-light.background-opacity = 1.0
```

In Lua for more control:

```lua
window = {
  opacity = 0.9,
  blur = true,
  -- or dynamic based on focus:
  opacity_unfocused = 0.7,  -- dim when not focused
}
```

Settable from the palette with live preview — slide the opacity and see it change in real time.

## Color Handling

Community pain point: programs can't reliably detect true color support.

Solution:

- Set `COLORTERM=truecolor` in child processes
- Respond correctly to DA (Device Attributes) queries
- Forward COLORTERM through SSH domains
- Default theme uses readable ANSI colors (no blue-on-black)

## Image Protocols

Community pain point: Sixel vs Kitty graphics fragmentation, neither works in tmux.

Solution: support both, composited in the GPU pipeline alongside text.

- Kitty graphics protocol as primary (de facto standard for modern terminals)
- Sixel as fallback (legacy compatibility)
- Both pass through the built-in multiplexer — no tmux image bugs

## Related Docs

- [Architecture](01-architecture.md) — where the renderer sits in the layer stack
- [Abstraction Layer](13-abstraction.md) — `GpuBackend`, `TextShaper`, `FontRasterizer` traits
- [Performance](12-performance.md) — latency and FPS targets
- [Theming](22-theming.md) — color definitions that the renderer applies
- [Accessibility](17-accessibility.md) — AccessKit tree maintained alongside rendering
- [Platform Support](20-platform-support.md) — per-platform rendering backends
