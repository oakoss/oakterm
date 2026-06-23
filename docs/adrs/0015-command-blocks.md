---
adr: '0015'
title: Command Blocks UX
status: proposed
date: 2026-06-22
tags: [renderer, shell-integration, plugins]
---

# 0015. Command Blocks UX

## Context

Per [ADR-0008](0008-shell-integration-timing.md), capturing OSC 133 semantic marks (prompt start, input start, output start, output end + exit status) into the screen buffer is Phase 0 scope. That mark data is the substrate for "command blocks" — the Warp-popularized UI where each command and its output is a discrete, addressable unit you can fold, copy in isolation, re-run, or share.

The [2026 competitive review](../reviews/2026-06-22-144218-ultimate-terminal-alignment-audit.md) flagged that we have the substrate but no committed stance on the UI. Two existing docs already lean against a heavyweight implementation:

- [16-wishlist-features.md](../ideas/16-wishlist-features.md) on the block model: "Breaks the Unix stream model. We address the useful parts (scroll-to-prompt, semantic zones) without fundamentally changing how terminal output works."
- [31-brainstorm.md](../ideas/31-brainstorm.md) lists command-output (semantic-zone) selection, and floats collapsible tool-call sections as plugin-territory stretch work.

The core principle is that the terminal "stays a terminal" and does not become an IDE or chat app. The question: do we commit to block UI, defer it to a plugin, or omit it?

## Options

### Option A: Native blocks in core (Warp model)

Render command+output as first-class block widgets in the core renderer — folding, per-block re-run/copy/share, block-level navigation.

Pros: best-in-class UX; differentiates against traditional terminals.
Cons: changes the rendering model from a character grid toward a widget tree; pulls the core toward an IDE/chat surface, contradicting the "stays a terminal" principle; high blast radius on the Phase 0 renderer; Warp is the heavy prior art here and it is explicitly not "just a terminal."

### Option B: Block boundaries in the core data model, block UI as a plugin

Core exposes OSC 133 block boundaries (from the Phase 0 mark data) through the plugin API (Phase 2). The character grid stays the rendering model. An optional bundled plugin builds folding/re-run/copy-per-block on top. Phase 1 ships the lightweight subset that needs no plugin: scroll-to-prompt and per-command output selection (already planned).

Pros: honors "plugin is the product" and "stays a terminal"; reserves the data hooks at zero retrofit cost (reuses the Phase 0 OSC 133 mark data); ships the genuinely useful 80% (scroll-to-prompt, per-command select) without block-widget weight; lets an optional plugin own the maximal UX.
Cons: full Warp-style blocks are not available out of the box; the plugin API must expose block boundaries and a region-render primitive.

### Option C: Deliberate omission

No blocks, ever. Ship only scroll-to-prompt and per-command selection (Phase 1). Keep the pure Unix stream model.

Pros: simplest; maximal "stays a terminal" purity.
Cons: forecloses a genuinely good UX that our substrate makes nearly free; cedes the workflow entirely to Warp and others; the data is captured regardless, so omission leaves value on the table.

## Decision

**Proposed: Option B — block boundaries in the core data model, block UI as an optional plugin.**

Because OSC 133 capture is already Phase 0 scope ([ADR-0008](0008-shell-integration-timing.md)), reserving the plugin-facing block-boundary API alongside it costs nothing and avoids a retrofit later. Phase 1 ships the lightweight subset (scroll-to-prompt, per-command selection) that delivers most of the value without changing the rendering model. Full folding/re-run/share lives in an optional plugin, keeping the core "a terminal, not a chat app" while leaving the full UX reachable. This aligns with the position already taken in [16-wishlist-features.md](../ideas/16-wishlist-features.md).

Status is `proposed` pending acceptance.

## Consequences

- Prerequisite: this depends on OSC 133 capture actually landing per [ADR-0008](0008-shell-integration-timing.md) — the current Phase 0 code has no OSC 133 handler yet — and on row metadata carrying the needed marks (a `Row` holds a single `semantic_mark` today, so boundary derivation must handle prompt/input marks colliding on one row).
- The Phase 2 plugin API must expose: command block boundaries (derived from stored OSC 133 marks), per-block text extraction, and a viewport-projection (filtered-render) primitive — folding a block must reclaim its viewport rows, not merely mask them.
- Phase 1 scope already includes scroll-to-prompt and per-command selection; no change there.
- The screen-buffer model is unchanged — block boundaries are derived from existing mark metadata. Folding does require a new renderer primitive (a projected/filtered viewport that can collapse row ranges); the character-grid model itself is not altered.
- If accepted, update [16-wishlist-features.md](../ideas/16-wishlist-features.md) and [31-brainstorm.md](../ideas/31-brainstorm.md) to point at this ADR for the block decision.
- A future spec defines the plugin-facing block API (defer until Phase 2 design).

## References

- [2026 Ultimate Terminal Alignment Audit](../reviews/2026-06-22-144218-ultimate-terminal-alignment-audit.md)
- [ADR-0008: Shell Integration Timing](0008-shell-integration-timing.md)
- [16-wishlist-features.md](../ideas/16-wishlist-features.md)
- [18-shell-integration.md](../ideas/18-shell-integration.md)
- [31-brainstorm.md](../ideas/31-brainstorm.md)
