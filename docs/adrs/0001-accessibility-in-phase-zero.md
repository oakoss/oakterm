---
adr: '0001'
title: Accessibility in Phase 0
status: accepted
date: 2026-03-26
tags: [a11y, renderer]
---

# 0001. Accessibility in Phase 0

## Context

The idea docs list accessibility as a Phase 5 (polish) concern, but the core principles state "accessible from day one." Retrofitting a semantic accessibility layer onto a GPU-rendered texture-based terminal is architecturally painful — the screen is a texture, not a DOM. Windows Terminal built UIA support into the core from the start. No GPU-accelerated terminal currently ships with cross-platform screen reader support.

See [17-accessibility.md](../ideas/17-accessibility.md), [02-renderer.md](../ideas/02-renderer.md), [12-performance.md](../ideas/12-performance.md).

The review audit ([2026-03-26-140000-idea-docs-audit.md](../reviews/2026-03-26-140000-idea-docs-audit.md)) flagged this as an architectural risk: moving accessibility to Phase 5 means every subsequent phase (multiplexer, plugins, shell intelligence) would be built without an accessible foundation.

## Options

### Option A: Phase 0 — build alongside the renderer

Integrate [AccessKit](https://github.com/AccessKit/accesskit) in Phase 0. The accessibility tree is built from the VT parser's screen buffer — the same data source the GPU renderer reads. The renderer and AccessKit are siblings, not parent-child. Lazy activation (via `accesskit_winit`) means zero overhead when no assistive technology is connected.

**Pros:**

- No retrofitting needed. Every subsequent phase builds on an accessible foundation.
- Competitive white space — no GPU terminal ships cross-platform screen reader support.
- AccessKit has a dedicated `Role::Terminal` with VT-100 semantics, text selection, live regions, and scroll properties.
- Production-proven in GPU-rendered Rust apps: egui (wgpu), Bevy (wgpu), Slint, Servo.
- Lazy activation: `update_if_active()` is a no-op when no screen reader is attached. Target of <0.5ms/frame when active, ~0ms inactive.
- Windows and macOS adapters are production-ready. Linux adapter is actively developed and functional.

**Cons:**

- Increases Phase 0 scope.
- Linux `accesskit_unix` adapter is "almost production-ready" but not officially stamped.
- No terminal emulator has implemented `Role::Terminal` with AccessKit yet — OakTerm would be first, with no reference implementation to learn from.

### Option B: Phase 5 — retrofit after the renderer exists

Build the GPU renderer without accessibility considerations. Add screen reader support as a polish item in Phase 5.

**Pros:**

- Smaller Phase 0 scope.

**Cons:**

- Retrofitting a semantic layer onto a texture-based renderer is the approach every GPU terminal has failed to execute on.
- Phases 1-4 (multiplexer, plugins, shell intelligence, networking) would be built without accessibility, requiring rework.
- Contradicts the "accessible from day one" core principle.

### Option C: Accessible architecture in Phase 0, defer implementation

Design the renderer to support AccessKit (shared screen buffer, node ID stability) but don't wire up the adapter until Phase 1-2.

**Pros:**

- Smaller Phase 0 scope than Option A while preserving the architectural path.

**Cons:**

- "Ready but not wired" risks becoming "never wired."
- Cannot validate the architecture works without testing it with real screen readers.

## Decision

**Option A — integrate AccessKit in Phase 0.**

The architectural cost of building it in from the start is low (the screen buffer already exists as the renderer's data source). The cost of retrofitting later is high and unproven. Lazy activation means zero runtime cost for users without screen readers. OakTerm becomes the first GPU-accelerated terminal with cross-platform screen reader support.

## Consequences

- Phase 0 deliverables include AccessKit integration with `Role::Terminal`.
- The renderer architecture maintains the VT parser's screen buffer as a shared data source for both GPU rendering and the AccessKit tree.
- Screen reader support ships on Windows and macOS from the first release. Linux support follows as `accesskit_unix` matures.
- The AccessKit tree must be updated incrementally on screen buffer changes (not full rebuilds).
- Performance target: <0.5ms/frame when a screen reader is active, ~0ms when inactive.
- Update [17-accessibility.md](../ideas/17-accessibility.md) to reflect Phase 0 placement.
- Update [33-roadmap.md](../ideas/33-roadmap.md) to move accessibility from Phase 5 to Phase 0.

## References

- [17-accessibility.md](../ideas/17-accessibility.md)
- [02-renderer.md](../ideas/02-renderer.md)
- [33-roadmap.md](../ideas/33-roadmap.md)
- [AccessKit GitHub](https://github.com/AccessKit/accesskit)
- [AccessKit: Role::Terminal](https://docs.rs/accesskit/latest/accesskit/enum.Role.html)
- [Windows Terminal UIA architecture (PR #1691)](https://github.com/microsoft/terminal/pull/1691)
