---
adr: '0015'
title: Command Blocks UX
status: accepted
date: 2026-07-06
tags: [renderer, shell-integration, plugins]
---

# 0015. Command Blocks UX

## Context

Per [ADR-0008](0008-shell-integration-timing.md), capturing OSC 133 semantic marks (prompt start, input start, output start, output end + exit status) into the screen buffer is Phase 0 scope. That mark data is the substrate for "command blocks" — the Warp-popularized UI where each command and its output is a discrete, addressable unit you can fold, copy in isolation, re-run, or share.

The [2026 competitive review](../reviews/2026-06-22-144218-ultimate-terminal-alignment-audit.md) flagged that we have the substrate but no committed stance on the UI. Two existing docs already lean against a heavyweight implementation:

- [16-wishlist-features.md](../ideas/16-wishlist-features.md) on the block model: "Breaks the Unix stream model. We address the useful parts (scroll-to-prompt, semantic zones) without fundamentally changing how terminal output works."
- [31-brainstorm.md](../ideas/31-brainstorm.md) lists command-output (semantic-zone) selection, and floats collapsible tool-call sections as plugin-territory stretch work.

The core principle is that the terminal "stays a terminal" and does not become an IDE or chat app. The question: do we commit to block UI, defer it to a plugin, or omit it?

Two developments sharpened the question after the original proposal was written (2026-06-22):

1. A hard requirement emerged: block affordances must be enable/disable-able at the config level.
2. Warp's architecture became verifiable: the client was open-sourced on 2026-04-28 (AGPL-3.0), and a reading of the source (2026-07-06) confirmed that Warp's top-level rendering model is a list of block objects (one grid per block); the character grid exists only as a leaf inside a block, and full-screen apps are handled by swapping to a separate plain-grid render element. There is no plain-grid mode to fall back to, which is why repeated user requests to disable blocks (warpdotdev/Warp #3227, closed not-planned; #3189, #9815) could only be answered with cosmetic sub-toggles. Warp's block lifecycle also rides a private DCS/JSON shell protocol carrying command text, cwd, and git state; OSC 133 marks alone provide boundaries and exit status, not rich metadata.

The grid-faithful terminals demonstrate the opposite pattern: VS Code (OSC 633 marks feeding gutter decorations, sticky scroll, and command navigation, each behind its own setting, with `never` clearing the chrome while mark collection continues) and iTerm2 (margin marks and status indicators, toggleable per profile) both treat blocks as decorations over an unchanged grid. The dividing line is folding: decorations that overlay the grid stay cheap and toggleable; folding that reclaims viewport rows forces a projected-viewport model and is the first step toward Warp's widget tree.

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

### Option D: Toggleable block decorations in core, heavy block UI as a plugin

The character grid stays the rendering model. The GUI derives block boundaries from the daemon's existing OSC 133 marks and draws decorations only — a gutter status indicator per command, optional per-block background tint, a sticky command header, block-scoped copy — behind a config toggle, alongside the already-planned scroll-to-prompt and per-command selection. Decorations are suppressed while the alternate screen is active and absent when marks are absent (unintegrated ssh degrades to a plain stream). Everything that departs from the grid — folding that reclaims viewport rows, per-block re-run/share, rich or inline content — stays in the Phase 2 plugin, as in Option B.

Pros: satisfies the toggle requirement (a master switch yields a pure stream, matching the VS Code/iTerm2 pattern); the affordances are cheap overlays with no daemon or wire changes; keeps the folding/widget-tree line exactly where Option B drew it.
Cons: core gains a small decorations layer the plugin-only option avoided; rich block headers (command text, cwd, branch) are out of reach of OSC 133 and stay out of scope.

## Decision

**Accepted: Option D — toggleable block decorations in core; heavy block UI as an optional plugin.** (Originally proposed as Option B; re-scoped 2026-07-06 when the toggle requirement and the Warp source reading made the decoration/widget-tree line precise.)

Because OSC 133 capture is already Phase 0 scope ([ADR-0008](0008-shell-integration-timing.md)), block boundaries are free, and the decoration affordances are GUI-side overlays; VS Code and iTerm2 demonstrate them toggleable over an unchanged grid. Requiring a plugin for a gutter dot would misplace the plugin boundary; adopting Warp's native blocks would make the toggle impossible. The toggle is presentation-only: the daemon records marks unconditionally, and disabling blocks clears the chrome without touching capture, matching the semantics of VS Code's `decorationsEnabled: never`. Full folding/re-run/share lives in an optional plugin, keeping the core "a terminal, not a chat app" while leaving the full UX reachable. This aligns with the position already taken in [16-wishlist-features.md](../ideas/16-wishlist-features.md).

Configuration surface (snake_case per [ADR-0005](0005-lua-sandboxed-config.md)):

```lua
command_blocks = {
  enabled = true,            -- master toggle; false = pure stream, no chrome
  gutter_indicators = true,  -- per-command success/fail indicator
  block_tint = false,        -- per-block background tint
  sticky_command = false,    -- pin the running command's prompt line
}
```

## Consequences

- Prerequisite: this depends on OSC 133 capture per [ADR-0008](0008-shell-integration-timing.md) — landed with OSC 133/7 interception (#32) — and on row metadata carrying the needed marks (a `Row` holds a single `semantic_mark` today, so boundary derivation must handle prompt/input marks colliding on one row).
- Decorations are GUI-side presentation over marks already on the wire: no daemon or protocol changes. The daemon records marks unconditionally; the toggle only governs drawing.
- Decorations are suppressed while the alternate screen is active (the daemon already tracks this) and degrade to a plain stream when marks are absent; these are the two seams Warp's issue tracker shows failing when blocks are structural rather than decorative.
- Rich block metadata (command text, cwd, git state) is not derivable from OSC 133 and is explicitly out of scope for the core decorations; if ever wanted, it requires a richer shell-integration channel (VS Code's OSC 633;E is prior art) and its own decision.
- Block identity is derived terminal-side from mark positions; there is no stable shell-minted block ID (Warp's model). The Phase 2 plugin API design must account for this when defining per-block operations like re-run.
- The Phase 2 plugin API must expose: command block boundaries (derived from stored OSC 133 marks), per-block text extraction, and a viewport-projection (filtered-render) primitive — folding a block must reclaim its viewport rows, not merely mask them.
- Phase 1 scope already includes scroll-to-prompt and per-command selection; the decorations layer joins them behind the `command_blocks` config table.
- The screen-buffer model is unchanged — block boundaries are derived from existing mark metadata. Folding does require a new renderer primitive (a projected/filtered viewport that can collapse row ranges); the character-grid model itself is not altered and folding stays plugin-gated.
- [16-wishlist-features.md](../ideas/16-wishlist-features.md) and [31-brainstorm.md](../ideas/31-brainstorm.md) now point here for the block decision.
- A future spec defines the plugin-facing block API (defer until Phase 2 design).

## References

- [2026 Ultimate Terminal Alignment Audit](../reviews/2026-06-22-144218-ultimate-terminal-alignment-audit.md)
- [ADR-0008: Shell Integration Timing](0008-shell-integration-timing.md)
- [16-wishlist-features.md](../ideas/16-wishlist-features.md)
- [18-shell-integration.md](../ideas/18-shell-integration.md)
- [31-brainstorm.md](../ideas/31-brainstorm.md)
- Warp source (AGPL-3.0, open-sourced 2026-04-28): <https://github.com/warpdotdev/Warp>. Block list model (`app/src/terminal/model/blocks.rs`), alt-screen surface swap (`app/src/terminal/alt_screen/`), block visibility settings (`settings/block_visibility.rs`); studied for architecture only, no code reuse (AGPL vs MPL-2.0)
- Disable-blocks user demand: warpdotdev/Warp issues [#3227](https://github.com/warpdotdev/Warp/issues/3227) (closed not-planned), [#3189](https://github.com/warpdotdev/Warp/issues/3189), [#9815](https://github.com/warpdotdev/Warp/issues/9815)
- VS Code terminal shell integration (OSC 633, toggleable decorations): <https://code.visualstudio.com/docs/terminal/shell-integration>
