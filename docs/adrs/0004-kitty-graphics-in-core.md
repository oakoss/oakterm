---
adr: '0004'
title: Kitty Graphics in Core
status: accepted
date: 2026-03-26
tags: [renderer, plugins]
---

# 0004. Kitty Graphics in Core

## Context

Three idea docs disagree on where image protocol support lives:

- [01-architecture.md](../ideas/01-architecture.md): lists Kitty graphics rendering as a plugin
- [02-renderer.md](../ideas/02-renderer.md): describes it as part of the GPU pipeline (core), composited alongside text
- [03-multiplexer.md](../ideas/03-multiplexer.md): says image protocols "work through the multiplexer" (implies core)

The review audit flagged this as a contradiction. The decision determines whether the renderer needs image compositing primitives from day one or whether the plugin API must be powerful enough to composite images into the GPU pipeline.

The Kitty graphics protocol is the de facto standard for terminal image display, adopted by Ghostty, WezTerm, Konsole, Warp, and iTerm2.

## Options

### Option A: Plugin only

All image protocols are WASM plugins. The core renderer handles text only.

**Pros:**

- Smallest possible core.
- Consistent with "the plugin is the product" principle.

**Cons:**

- The plugin API would need GPU texture access, z-ordering, and compositing primitives — a massively complex API surface required before Phase 2.
- No competitor treats image protocol support as a plugin.
- Image display is escape-sequence-driven (VT parser handles it), making it a terminal fundamental like color or cursor rendering.

### Option B: Core only, closed

Kitty graphics protocol baked into the renderer. No extension point for other image protocols.

**Pros:**

- Simple implementation. No API surface to design.

**Cons:**

- Other protocols (Sixel, iTerm2 inline images) would require core changes or remain unsupported.
- Contradicts the extensibility principle.

### Option C: Core + image compositing API

Kitty graphics protocol parsed by the VT layer and composited by the renderer as a core feature. The renderer exposes image placement primitives (place texture at cell position with dimensions) that plugins can use to implement other image protocols.

**Pros:**

- Kitty protocol is built in because it's the standard — works out of the box.
- Core uses the same image placement API internally that plugins will use, ensuring the API is real and tested.
- Community can add Sixel, iTerm2 inline images, and future protocols as plugins without core changes.
- The image compositing API is a natural part of the renderer — the renderer already composites text, cursor, and selection. Image regions are the same problem.

**Cons:**

- Larger core than Option A.
- Image placement API must be designed as part of the renderer, not deferred to Phase 2.

## Decision

**Option C — Kitty graphics protocol in core, image compositing API exposed for plugins.**

Image display is a terminal fundamental, not an optional extension. The Kitty graphics protocol is the de facto standard and belongs in the VT parser and renderer. The renderer exposes image placement primitives that core uses internally and plugins can use to add other protocols.

## Consequences

- The VT parser handles Kitty graphics escape sequences (APC-based protocol) in Phase 0.
- The renderer composites image regions alongside text cells using its image placement API.
- The multiplexer forwards image data between panes as part of VT stream forwarding.
- Phase 2 plugin API includes image placement primitives: place texture at cell (X, Y) with width, height, z-order, and crop parameters.
- Community plugins can implement Sixel, iTerm2 inline images (`OSC 1337`), and future protocols using the same API core uses.
- Update [01-architecture.md](../ideas/01-architecture.md) to move Kitty graphics from the plugin list to core renderer features.
- The Phase 2 bundled plugins list should remove `kitty-graphics` (it's core, not a plugin).

## References

- [01-architecture.md](../ideas/01-architecture.md)
- [02-renderer.md](../ideas/02-renderer.md)
- [03-multiplexer.md](../ideas/03-multiplexer.md)
- [Kitty graphics protocol specification](https://sw.kovidgoyal.net/kitty/graphics-protocol/)
