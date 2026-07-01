---
adr: '0017'
title: Rust as the Implementation Language
status: accepted
date: 2026-06-30
tags: [core, renderer, security]
---

# 0017. Rust as the Implementation Language

## Context

The foundational stack — Rust for the core, [wgpu](0018-gpu-rendering-wgpu.md) for rendering, [Wasmtime](0019-wasm-plugin-runtime-wasmtime.md) for plugins, Lua for config — is stated as fact in `CLAUDE.md` and [01-architecture.md](../ideas/01-architecture.md) but was never recorded as a decision. The 2026-06-30 choices audit flagged the language, GPU abstraction, and plugin runtime as load-bearing decisions with no ADR behind them.

The language choice underlies three core principles at once: performance is non-negotiable (sub-frame input latency), secure by default (the terminal parses hostile untrusted byte streams — escape-sequence injection is a named threat), and accessible from day one. It is the one decision the rest of the stack is built on top of, so it earns its own record even though the project is already implemented in Rust.

## Options

### Option A: Rust

Systems language with compile-time memory safety and no garbage collector.

**Pros:**

- Memory safety without a GC. GC pauses are incompatible with sub-frame latency; here safety is also a _security control_ — a terminal parses adversarial escape sequences, and memory-safety closes the injection-to-corruption path by construction.
- The exact ecosystem this project needs is Rust-native: wgpu, Wasmtime, winit, swash/cosmic-text shaping, and AccessKit. AccessKit is the only portable accessibility library of its kind and it is Rust-first — day-one accessibility is far cheaper here than through an FFI boundary.
- `Send`/`Sync` make the daemon/client split ([ADR-0007](0007-daemon-architecture.md)) tractable without data races.

**Cons:**

- Steeper contributor on-ramp than a managed language.
- Compile times.

### Option B: C++

Native performance parity with Rust.

**Pros:**

- Peak performance; mature GPU tooling.

**Cons:**

- Memory-unsafe against exactly the hostile input a terminal handles — the injection threat becomes a memory-corruption threat.
- Weaker, fragmented package story; no AccessKit equivalent; concurrency safety is manual.

### Option C: Zig

Modern systems language, strong C interop.

**Pros:**

- Simple, fast, excellent C interop (Ghostty is written in Zig).

**Cons:**

- Pre-1.0, moving target; no borrow-checker (manual memory safety against hostile input); no AccessKit-equivalent, no Rust-grade wgpu/Wasmtime equivalents — we would build the ecosystem ourselves.

### Option D: A managed language (Go, etc.)

Garbage-collected, fast to write.

**Cons:**

- GC pauses violate the latency principle; weaker native/GPU interop; not suited to a render loop with a hard frame budget.

## Decision

**Option A — Rust.**

Rust is the only option that delivers native performance _and_ memory safety against hostile input _and_ a mature, Rust-native ecosystem for every seam this project depends on (GPU, WASM, shaping, accessibility). Memory safety here is a security control, not just an ergonomic benefit — it directly serves the "secure by default" principle against escape-sequence injection.

## Consequences

- The remainder of the stack ([wgpu](0018-gpu-rendering-wgpu.md), [Wasmtime](0019-wasm-plugin-runtime-wasmtime.md), winit, AccessKit, swash) follows from choosing Rust.
- `unsafe` is confined and justified per use; the memory-management design deliberately avoids `unsafe mmap` ([ADR-0006](0006-scroll-buffer-architecture.md)).
- Contributor onboarding assumes Rust fluency; the on-ramp cost is accepted as the price of the safety and ecosystem gains.

## References

- [01-architecture.md](../ideas/01-architecture.md)
- [13-abstraction.md](../ideas/13-abstraction.md)
- [21-security.md](../ideas/21-security.md)
- [ADR-0007: Daemon Architecture](0007-daemon-architecture.md)
