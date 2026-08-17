---
adr: '0018'
title: GPU Rendering via wgpu
status: accepted
date: 2026-06-30
tags: [renderer, core, abstraction]
---

# 0018. GPU Rendering via wgpu

## Context

[02-renderer.md](../ideas/02-renderer.md) and [13-abstraction.md](../ideas/13-abstraction.md) specify GPU rendering behind a trait seam, and `CLAUDE.md` names wgpu as the backend, but the choice of wgpu over raw native graphics APIs was never recorded. [ADR-0002](0002-performance-philosophy.md) explicitly flagged the open risk: "wgpu adds an abstraction layer whose overhead for terminal rendering has not been benchmarked," and deferred resolution to Phase 0 prototyping. This ADR records wgpu as the rendering abstraction and names the competitive-parity benchmark as its validation gate.

The tension is direct: the fastest terminals (Ghostty ~2ms on direct Metal, Alacritty on raw OpenGL) talk to the GPU natively, and performance is non-negotiable. Any abstraction layer must justify its overhead.

## Options

### Option A: wgpu behind a GPU trait seam

Portable Rust-native GPU abstraction (WebGPU/WGSL) targeting Metal, Vulkan, DX12, and GL.

**Pros:**

- One WGSL shader path and one renderer across macOS/Linux/Windows, instead of maintaining Metal + Vulkan + DX12 backends separately.
- Rust-native and production-proven on text-heavy GPU workloads (Zed, Bevy, Firefox use it).
- The abstraction principle already mandates a swappable GPU seam; wgpu _is_ that seam, and if its overhead proves unacceptable the seam lets us drop to raw Metal for a platform without rewriting the terminal.

**Cons:**

- An abstraction layer over the native driver, with overhead that must be measured against direct-Metal competitors.

### Option B: Raw native APIs per platform

Metal on macOS, Vulkan/DX on others — the Ghostty approach.

**Pros:**

- Peak performance; no abstraction overhead; direct control.

**Cons:**

- Three separate GPU backends and shader languages to build and maintain — the largest ongoing cost in the renderer, for a solo/small effort.
- Contradicts "abstracted at every seam."

### Option C: A 2D graphics library (Skia, etc.)

Render text and cells through a higher-level 2D API.

**Cons:**

- Large C++ dependency, weaker Rust integration, and less control over the glyph-atlas/compositing hot path than a shader we own.

### Option D: OpenGL only

Single legacy API (Alacritty's approach via glutin).

**Cons:**

- Deprecated on macOS; no modern-API access (Metal/Vulkan); a dead end for the compositing and image-protocol work ([ADR-0004](0004-kitty-graphics-in-core.md)) the renderer must do.

### Option E: GPUI (Zed's UI framework)

_Added 2026-08-17, evaluated after acceptance — full analysis in the [GPUI evaluation](../reviews/2026-08-17-181057-gpui-evaluation.md)._ Zed's GPU-accelerated framework; production-proven for terminal rendering (Zed's terminal, tty7).

**Pros:**

- Fast terminal rendering demonstrated in shipping products; Apache-2.0.
- AccessKit and text shaping built in.

**Cons:**

- Wrong layer: a full application framework (owns the event loop, windowing, taffy layout, text system), not a GPU abstraction — adopting it replaces winit, the render loop, the glyph atlas, and the a11y bridge, and its widget-tree value targets chrome that OakTerm deliberately renders as terminal cells.
- wgpu underneath anyway on Linux (moved off blade in early 2026, per third-party reporting) and web; on macOS its direct Metal duplicates what this ADR's escape hatch already reserves behind the trait seam — at a far smaller blast radius.
- Reintroduces on macOS/Windows the multi-backend, multi-shader-language maintenance surface Option B was rejected for (carried by Zed, but its churn flows to consumers pre-1.0).
- Consumption risk: pre-1.0 with routine breaking changes, crates.io publishing stalled mid-restructure (current code requires a git dependency on the Zed monorepo), single vendor whose roadmap is Zed.

## Decision

**Option A — wgpu, behind a GPU trait seam, with a competitive-parity benchmark as the acceptance gate.**

wgpu collapses three native backends into one WGSL codebase while the abstraction seam preserves the escape hatch to raw Metal if overhead demands it. The wgpu-overhead question from [ADR-0002](0002-performance-philosophy.md) is answered empirically: OakTerm's frame time and end-to-end input latency are benchmarked against Alacritty and Ghostty on the same hardware, and wgpu must reach competitive parity. If it cannot, the seam contains the blast radius of switching a platform to native.

## Consequences

- A criterion benchmark comparing OakTerm frame/latency against Alacritty and Ghostty is the validation gate for this decision (tracked as a renderer spike; per [ADR-0002](0002-performance-philosophy.md)'s benchmark discipline).
- **Gate progress (2026-07-02, TREK-166): ingest-throughput parity confirmed; frame-time and input-latency parity remain open (TREK-192).** vtebench on the same hardware put OakTerm at 1.2x of Ghostty and 1.7-2.0x of Alacritty on cell-content workloads; the one large gap (scrolling, 4-53x) reproduced headlessly with rendering removed and is a daemon-side scrollback issue (TREK-191), not wgpu overhead. Nothing measured implicates wgpu, so the raw-Metal escape hatch stays reserved and unbuilt — but the gate closes only when a keypress-to-photon comparison (which subsumes frame cost) runs against Alacritty and Ghostty. Full method and numbers: [2026-07-02 parity benchmark](../reviews/2026-07-02-114749-wgpu-parity-benchmark.md).
- Shaders are authored in WGSL; the glyph atlas and cell compositing target the wgpu pipeline.
- The GPU backend stays behind a trait ([13-abstraction.md](../ideas/13-abstraction.md)); a raw-Metal fallback is _reserved_ but not built unless benchmarks force it.
- The image-compositing API ([ADR-0004](0004-kitty-graphics-in-core.md)) is expressed in terms of the wgpu pipeline.

## References

- [02-renderer.md](../ideas/02-renderer.md)
- [13-abstraction.md](../ideas/13-abstraction.md)
- [ADR-0002: Performance Philosophy](0002-performance-philosophy.md)
- [ADR-0004: Kitty Graphics in Core](0004-kitty-graphics-in-core.md)
- [GPUI Evaluation (2026-08-17)](../reviews/2026-08-17-181057-gpui-evaluation.md)
