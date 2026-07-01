---
adr: '0019'
title: WASM Plugin Runtime via Wasmtime
status: accepted
date: 2026-06-30
tags: [plugins, core, security, abstraction]
---

# 0019. WASM Plugin Runtime via Wasmtime

## Context

[06-plugins.md](../ideas/06-plugins.md) and the "plugin is the product" principle require a sandboxed WASM runtime for third-party plugins. `CLAUDE.md` names Wasmtime, but the choice over other WASM runtimes was never recorded. The runtime must satisfy three constraints that competing engines meet unevenly:

1. **Never block the render loop** — a runaway or malicious plugin must be preemptible by the host.
2. **Capability-based security** — plugins get only the capabilities they are granted (secure by default).
3. **A typed API contract** — the plugin API is defined in WIT / the Component Model, so the runtime's component support must be first-class, not bolted on.

## Options

### Option A: Wasmtime

Bytecode Alliance's runtime; Cranelift JIT; reference implementation of the Component Model and WASI Preview 2.

**Pros:**

- Reference implementation of the Component Model / WIT — the project's chosen plugin-API contract is native, not emulated.
- **Epoch-based interruption** and fuel metering let the host preempt a plugin that overruns its time budget, which is exactly the "plugins never block the render loop" guarantee.
- WASI Preview 2 capability model maps onto capability-based plugin permissions.
- Production-hardened (Fastly, Fermyon, Shopify); actively maintained; Rust-native.

**Cons:**

- Cranelift JIT is a larger dependency than an interpreter-only runtime.

### Option B: Wasmer

Mature alternative runtime with multiple backends.

**Pros:**

- Fast; multiple compiler backends; broad platform support.

**Cons:**

- Weaker/later Component Model story than Wasmtime; past licensing/governance friction makes it a less stable long-term foundation than a Bytecode Alliance project.

### Option C: WAMR (WebAssembly Micro Runtime)

Tiny interpreter/AOT runtime.

**Pros:**

- Minimal footprint; good for constrained embeds.

**Cons:**

- Interpreter performance and less mature Component Model tooling; the footprint win doesn't matter for a desktop terminal, while the component-model and preemption gaps do.

### Option D: Extism

Higher-level plugin framework built on top of a WASM runtime.

**Pros:**

- Convenient plugin ergonomics out of the box.

**Cons:**

- Wraps a runtime we would rather control directly, precisely for the epoch-interruption / render-loop-preemption guarantee; the extra layer works against "abstracted at every seam" by hiding the engine we need to configure.

## Decision

**Option A — Wasmtime.**

Wasmtime is the only option that natively provides all three requirements: first-class Component Model / WIT (the plugin-API contract), epoch-based preemption (the render-loop guarantee), and a WASI capability model (capability-based permissions) — under stable Bytecode Alliance governance. The JIT dependency size is an acceptable cost for a desktop application.

## Consequences

- The plugin API is defined in WIT and hosted through the Component Model.
- Epoch interruption is configured so the host can preempt plugins that exceed their execution budget; plugins run off the render-critical path.
- Plugin capabilities are wired through WASI Preview 2 grants — deny by default, relaxable per manifest.
- The runtime sits behind an abstraction seam ([13-abstraction.md](../ideas/13-abstraction.md)) so it remains swappable if a stronger WASM runtime emerges.

## References

- [06-plugins.md](../ideas/06-plugins.md)
- [13-abstraction.md](../ideas/13-abstraction.md)
- [21-security.md](../ideas/21-security.md)
