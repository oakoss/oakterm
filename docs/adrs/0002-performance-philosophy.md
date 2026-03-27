---
adr: '0002'
title: Performance Philosophy
status: accepted
date: 2026-03-26
tags: [renderer, core]
---

# 0002. Performance Philosophy

## Context

Multiple idea docs claim independent per-component performance budgets that contradict each other when summed:

- [12-performance.md](../ideas/12-performance.md): "no feature may add >0.5ms to input latency"
- [17-accessibility.md](../ideas/17-accessibility.md): budgets "<0.5ms/frame" for accessibility
- [14-debugging.md](../ideas/14-debugging.md): shows plugin overhead of 1.31ms across 5 plugins

These are additive. Applied independently, they exceed any reasonable frame budget. The review audit flagged this as a contradiction requiring a decision.

Additionally, the <8ms total frame target is unvalidated with wgpu. Alacritty achieves 2-5ms with raw OpenGL. Ghostty uses direct Metal/OpenGL. wgpu adds an abstraction layer whose overhead for terminal rendering has not been benchmarked.

## Options

### Option A: Fixed per-component budgets

Allocate specific millisecond targets per pipeline stage (VT parse: 0.5ms, shaping: 1.0ms, atlas: 0.5ms, GPU: 2.0ms, a11y: 0.5ms, plugins: 1.5ms, headroom: 1.0ms) summing to a hard 8ms ceiling.

**Pros:**

- Clear contracts for each component.
- Easy to identify which component is over budget.

**Cons:**

- Numbers are invented without code to measure against.
- The idea docs already demonstrate this approach produces contradictions.
- The 8ms total target is unvalidated with wgpu.
- Rigid budgets discourage trading latency between components based on actual profiling data.

### Option B: Performance as a design principle

No fixed budgets. Benchmark from day one. Measure end-to-end input latency on every PR. Regressions are blocking. Target competitive parity with the fastest terminals (Alacritty, Ghostty). Specific numbers come from measurement, not pre-implementation estimates.

**Pros:**

- Grounded in reality — numbers come from actual code, not guesses.
- Flexible — allows trading latency between components based on profiling.
- Adapts as the codebase evolves.
- Competitive benchmarking keeps the bar high without arbitrary ceilings.

**Cons:**

- No upfront contracts for component authors.
- "Don't regress" requires a benchmark suite from Phase 0.
- Competitive targets may shift as competitors improve.

### Option C: Hard total target, flexible allocation

Set an 8ms total frame budget but let components float within it based on profiling.

**Pros:**

- Has a ceiling while allowing internal flexibility.

**Cons:**

- The 8ms number is still unvalidated with wgpu.
- Requires the same benchmark infrastructure as Option B.
- Adds an artificial constraint that may not match what wgpu can actually deliver.

## Decision

**Option B — performance is a design principle, not a budget.**

The rule is: measure everything, regress nothing, target competitive parity with Alacritty and Ghostty. Performance targets come from benchmarking real code, not from pre-implementation spreadsheets. Every PR runs benchmarks. Regressions are blocking.

Phase 0 includes setting up a benchmark framework (criterion) for end-to-end input latency, VT parser throughput, and frame rendering time. Competitive baselines are established by benchmarking Alacritty and Ghostty on the same hardware.

## Consequences

- Remove the scattered "0.5ms per feature" and fixed frame budget claims from idea docs. Replace with "performance is measured, not budgeted."
- Phase 0 deliverables include a criterion benchmark suite covering input latency, VT parse throughput, and frame time.
- CI benchmark strategy is tiered: unit tests + lint on every PR and push to main; smoke benchmarks on PRs and pushes to main that touch hot paths (renderer, VT parser, buffer); full benchmark suite on weekly schedule and release tags. Regressions beyond noise threshold are blocking.
- The wgpu overhead question is answered by Phase 0 prototyping, not assumption.
- When performance and features conflict, performance wins — but "performance" means measured regression on the benchmark suite, not theoretical concern.
- Update [12-performance.md](../ideas/12-performance.md) to reflect this philosophy.

## References

- [12-performance.md](../ideas/12-performance.md)
- [17-accessibility.md](../ideas/17-accessibility.md)
- [14-debugging.md](../ideas/14-debugging.md)
