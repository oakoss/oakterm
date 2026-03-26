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
