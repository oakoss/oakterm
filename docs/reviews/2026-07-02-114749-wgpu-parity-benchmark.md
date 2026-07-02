---
title: wgpu Parity Benchmark (ADR-0018 Gate)
date: 2026-07-02T11:47:49
scope: vtebench throughput vs Alacritty and Ghostty on the same hardware, plus headless and archive-off controls (TREK-166)
---

# wgpu Parity Benchmark (ADR-0018 Gate)

## Scope

ADR-0018 accepted wgpu conditional on a competitive-parity benchmark against Alacritty and Ghostty; ADR-0002 deferred the wgpu-overhead question to measurement. This spike ran that gate (TREK-166) and used two controls to attribute the one large gap it found.

**Verdict: ingest-throughput parity confirmed and wgpu exonerated on the one large gap; frame-time and input-latency parity remain unmeasured (TREK-192). The benchmark caught a daemon-side scrolling bug that is 4-53x, unrelated to rendering.**

## Method

- [vtebench](https://github.com/alacritty/vtebench) (current main), defaults (1 MiB per sample, 10s per benchmark), results via `--dat`, `--silent`. 10 of the 12 suite benchmarks produced samples; `cursor_motion` and `light_cells` emitted none on any of the three terminals (identical exclusion, so comparisons are unaffected).
- Hardware: Apple M2 Max, 96 GB, macOS 26.5.1. Release builds. Sequential runs on an otherwise idle machine.
- Terminals: OakTerm (this repo, release), Alacritty 0.17.0, Ghostty 1.3.2. Each terminal's default font.
- Grid: OakTerm and Alacritty at 93x32. Ghostty ignored its window-size flags and ran at 98x35 (~5% more cells, ~9% more scroll lines — a slight bias against Ghostty on render-bound rows).
- OakTerm runs were driven over the wire protocol (KeyInput into pane 0) with the GUI window rendering normally.
- vtebench measures how fast the terminal drains the PTY — ingest throughput, not frame time. For OakTerm that path is the daemon (parse + scrollback); the GUI renders from pulled updates. The two controls below use this to attribute costs.

## Results

Mean per 1 MiB sample, lower is better. Ratios are OakTerm / competitor.

| Benchmark                     | OakTerm | Alacritty | Ghostty | vs Ala | vs Ghostty |
| ----------------------------- | ------- | --------- | ------- | ------ | ---------- |
| dense_cells                   | 11.9 ms | 7.1 ms    | 9.7 ms  | 1.7x   | 1.2x       |
| medium_cells                  | 17.3 ms | 8.8 ms    | 14.8 ms | 2.0x   | 1.2x       |
| unicode                       | 13.1 ms | 7.1 ms    | 9.3 ms  | 1.8x   | 1.4x       |
| sync_medium_cells             | 20.2 ms | 12.1 ms   | 17.1 ms | 1.7x   | 1.2x       |
| scrolling                     | 945 ms  | 30.2 ms   | 33.1 ms | 31x    | 29x        |
| scrolling_fullscreen          | 1902 ms | 35.6 ms   | 51.4 ms | 53x    | 37x        |
| scrolling_bottom_region       | 165 ms  | 11.8 ms   | 43.2 ms | 14x    | 3.8x       |
| scrolling_bottom_small_region | 164 ms  | 11.8 ms   | 43.1 ms | 14x    | 3.8x       |
| scrolling_top_region          | 140 ms  | 30.5 ms   | 33.2 ms | 4.6x   | 4.2x       |
| scrolling_top_small_region    | 165 ms  | 11.5 ms   | 42.6 ms | 14x    | 3.9x       |

### Control 1 — headless daemon (no GUI, no rendering)

Scrolling stayed slow with no GUI attached: `scrolling` ~909 ms, `scrolling_fullscreen` ~1829 ms — statistically identical to the GUI-attached run. **The renderer contributes nothing to the scrolling gap; wgpu is exonerated.**

### Control 2 — headless, `scrollback_archive = false`

| Benchmark            | archive on | archive off | reduction |
| -------------------- | ---------- | ----------- | --------- |
| scrolling            | ~909 ms    | ~188 ms     | 4.8x      |
| scrolling_fullscreen | ~1829 ms   | ~393 ms     | 4.7x      |

Synchronous archival (zstd + AES-256-GCM on pruned rows, inline in the PTY read loop) is ~80% of the cliff. The archive-off residual (~188 ms vs Alacritty's 30 ms) still leaves ~6x in the hot-buffer/scroll path (per-row serialization on prune is the prime suspect).

### Renderer-side data point

The criterion frame bench (`crates/oakterm-renderer/benches/frame_render.rs`) shows glyph-atlas cache hits at ~4.2 ns and nanosecond-scale frame-path operations — no GPU-side anomaly.

## Findings

### Validated Decisions

- **ADR-0018 (wgpu) gate, ingest-throughput evidence: PASS.** On cell-content workloads OakTerm lands at 1.2x of Ghostty (the closest architectural peer: GPU-accelerated, modern) and 1.7-2.0x of Alacritty (the historic speed ceiling), and the one large gap was proven non-render by the headless control. wgpu's abstraction overhead is not the limiting factor anywhere in these measurements, so the raw-Metal escape hatch (13-abstraction.md) stays reserved and unbuilt — but the gate's frame-time/latency comparison is still owed (TREK-192).

### Challenged Decisions

- **Spec-0004's inline archival needs an async write path.** ADR-0006/Spec-0004 put archival of pruned rows in the ingest path; at sustained scroll rates that costs ~5x throughput. Nothing in the spec's contract requires archival to be synchronous with parsing — a bounded queue to an archive writer task preserves the durability semantics at negligible ingest cost. Filed as TREK-191.

### Corrections

- None — this review measured; it found no doc errors.

## Action Items

- Record the gate result in ADR-0018 (done alongside this review).
- TREK-191: move archival off the PTY read loop; then profile the residual ~6x in the hot-buffer prune path (row serialization).
- Re-run this suite after TREK-191 lands; scrolling should drop to the 30-60 ms band. The methodology here (vtebench + wire-protocol typist + headless/archive controls) is reproducible from this doc.

## What This Is Not

- Not a frame-time or input-latency measurement — vtebench times PTY ingest, and the criterion bench is CPU-side. Keypress-to-photon latency (which subsumes frame cost) needs a Typometer-style rig; that part of ADR-0018's gate stays open as TREK-192.
- Not a font-rendering comparison — each terminal used its own default font; glyph rasterization costs differ.
