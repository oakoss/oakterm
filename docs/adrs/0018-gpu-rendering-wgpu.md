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

## Decision

**Option A — wgpu, behind a GPU trait seam, with a competitive-parity benchmark as the acceptance gate.**

wgpu collapses three native backends into one WGSL codebase while the abstraction seam preserves the escape hatch to raw Metal if overhead demands it. The wgpu-overhead question from [ADR-0002](0002-performance-philosophy.md) is answered empirically: OakTerm's frame time and end-to-end input latency are benchmarked against Alacritty and Ghostty on the same hardware, and wgpu must reach competitive parity. If it cannot, the seam contains the blast radius of switching a platform to native.

## Consequences

- A criterion benchmark comparing OakTerm frame/latency against Alacritty and Ghostty is the validation gate for this decision (tracked as a renderer spike; per [ADR-0002](0002-performance-philosophy.md)'s benchmark discipline).
- Shaders are authored in WGSL; the glyph atlas and cell compositing target the wgpu pipeline.
- The GPU backend stays behind a trait ([13-abstraction.md](../ideas/13-abstraction.md)); a raw-Metal fallback is _reserved_ but not built unless benchmarks force it.
- The image-compositing API ([ADR-0004](0004-kitty-graphics-in-core.md)) is expressed in terms of the wgpu pipeline.

## References

- [02-renderer.md](../ideas/02-renderer.md)
- [13-abstraction.md](../ideas/13-abstraction.md)
- [ADR-0002: Performance Philosophy](0002-performance-philosophy.md)
- [ADR-0004: Kitty Graphics in Core](0004-kitty-graphics-in-core.md)
