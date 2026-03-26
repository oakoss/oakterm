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
