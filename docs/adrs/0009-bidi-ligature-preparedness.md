---
adr: '0009'
title: BiDi and Ligature Preparedness
status: accepted
date: 2026-03-27
tags: [renderer, core]
---

# 0009. BiDi and Ligature Preparedness

## Context

Bidirectional text (RTL/BiDi) and ligatures are features that most GPU terminals add after initial release. Both are architecturally expensive to retrofit if the wrong foundation is laid:

- **BiDi**: If the screen buffer stores cells in visual order (left-to-right as displayed), adding BiDi later requires rewriting the grid, cursor, selection, and scrollback. If cells are stored in logical order (as received from the PTY), BiDi is a render-time reordering pass that doesn't change the data model.

- **Ligatures**: If the renderer processes one cell at a time, adding ligatures later requires rearchitecting the rendering pipeline to process runs of cells through a text shaper. If the renderer is run-based from the start, ligature support means swapping in a real shaper.

Alacritty's decade-long inability to add ligatures and BiDi demonstrates the cost of not preparing the architecture early.

## Options

### Option A: Implement BiDi and ligatures in Phase 0

Full implementation: UBA algorithm, HarfBuzz integration, RTL cursor movement, BiDi-aware selection.

**Pros:** Complete from day one.

**Cons:** Massive scope increase for Phase 0. BiDi alone is one of the hardest problems in terminal emulation (cursor movement, selection, line wrapping all become complex). Not needed for the MVP.

### Option B: Prepare the architecture, defer implementation

Add ~150 lines of trait definitions, identity implementations, and reserved fields. No user-visible BiDi or ligature features in Phase 0. The architecture supports adding them later without rework.

**Pros:** Minimal Phase 0 cost (~150 lines). Prevents the two most expensive retrofitting categories. Implementation deferred to when it's needed.

**Cons:** Reserved fields and identity-function traits add minor complexity.

### Option C: Defer everything

Build Phase 0 without any BiDi or ligature consideration. Add them when needed.

**Pros:** Simplest Phase 0.

**Cons:** High risk of Alacritty-style architectural lock-in. Visual-order grid storage and per-cell rendering are the natural defaults and both are wrong for future BiDi/ligature support.

## Decision

**Option B — prepare the architecture, defer the implementation.**

### Screen Buffer (Spec-0003 updates)

- **Cells stored in logical order.** Cells are stored in the order received from the PTY, not in display order. This is the natural default and must not be violated by any optimization.
- **`direction` field on Row.** A `Direction` enum (`Ltr | Rtl | Auto`), defaulting to `Ltr`. Costs 1 byte per row. Phase 0 always uses `Ltr`. BiDi implementation later uses this field without changing the Row layout.
- **`bidi_mode` flag on Grid.** A `BidiMode` enum (`Off | Implicit | Explicit`), defaulting to `Off`. Phase 0 always uses `Off`.

### Coordinate Mapping

Cursor movement and selection must go through a coordinate-mapping abstraction:

```rust
trait CoordinateMapper {
    fn logical_to_visual(&self, logical_col: u16, row: u16) -> u16;
    fn visual_to_logical(&self, visual_col: u16, row: u16) -> u16;
}
```

Phase 0 implementation: identity function (returns the input unchanged). When BiDi is added, a UBA-based mapper replaces the identity without changing cursor or selection code.

### Rendering Pipeline

The renderer must process **runs of cells**, not individual cells:

1. Group consecutive cells with the same font and attributes into runs.
2. Process each run through a `TextShaper` trait.
3. Render the shaped output to the GPU.

Phase 0: each run is one cell, and the shaper is a simple glyph lookup (no HarfBuzz). The architecture supports multi-cell runs and real shaping from the start.

```rust
trait TextShaper {
    fn shape(&self, run: &TextRun) -> Vec<ShapedGlyph>;
    fn metrics(&self, font: FontKey, size: f32) -> FontMetrics;
    fn rasterize(&self, font: FontKey, glyph_id: u32, size: f32) -> GlyphBitmap;
}
```

Phase 0 implementation: `SimpleShaper` that maps each character to its glyph ID via the font's cmap table. Ligature-capable shapers (HarfBuzz, Core Text, DirectWrite) slot in behind the same trait.

## Consequences

- Spec-0003 (Screen Buffer) updated with `direction` field on Row, `bidi_mode` on Grid, and `Direction`/`BidiMode` enums.
- Phase 0 renderer must use a run-based pipeline with `TextShaper` trait, even though runs are single cells initially.
- Phase 0 cursor and selection must go through `CoordinateMapper`, even though it's the identity function.
- BiDi implementation (future) requires: UBA algorithm, `CoordinateMapper` implementation, BiDi escape sequence handling. Does NOT require data model changes.
- Ligature implementation (future) requires: HarfBuzz/Core Text integration behind `TextShaper`. Does NOT require data model or pipeline changes.
- Neither BiDi nor ligatures are user-visible in Phase 0.

## References

- [Spec 0003: Screen Buffer](../specs/0003-screen-buffer.md)
- [02-renderer.md](../ideas/02-renderer.md)
- [23-i18n.md](../ideas/23-i18n.md)
- [Unicode Bidirectional Algorithm](https://unicode.org/reports/tr9/)
- [Terminal BiDi proposal](https://terminal-wg.pages.freedesktop.org/bidi/)
