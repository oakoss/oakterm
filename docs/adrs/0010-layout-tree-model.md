---
adr: '0010'
title: Layout Tree Model
status: proposed
date: 2026-04-02
tags: [core]
---

# 0010. Layout Tree Model

## Context

Phase 1 introduces tiled splits, floating panes, tabs, and workspaces. The multiplexer idea doc ([03-multiplexer.md](../ideas/03-multiplexer.md)) describes nested splits and declarative Lua layouts but does not specify how split state is represented internally.

The layout model determines how splits are created, how resize propagates, how layouts are serialized, and how the Lua layout API maps to internal state. Every pane operation depends on this choice.

Research covered tmux, WezTerm, Ghostty, Zellij, and i3/sway.

## Options

### Option A: Binary tree (WezTerm, Ghostty)

Every split produces exactly two children. A split node stores a direction (horizontal/vertical) and the sizes of its two children.

**Pros:**

- Simplest data structure. Each node has exactly two children.
- Trivial serialization (serde derive in WezTerm).
- Resize only propagates between two siblings.

**Cons:**

- Three-way splits create lopsided trees: splitting into thirds requires split 66/33, then split the 66 into 50/50. The "middle" pane sits at a different tree depth than its visual neighbors.
- Resize of visually adjacent panes may require propagating through multiple tree levels.
- Declarative layouts must encode flat pane lists as nested binary splits. The Lua layout API in the idea doc (`panes = { A, B, C }`) does not map naturally.
- Ghostty users have filed requests for i3-style flat splitting because the binary model produces unintuitive resize behavior with 3+ panes.

### Option B: N-ary tree with auto-flattening (tmux, i3/sway)

Internal nodes are containers with a direction and N children. Leaves are panes. Single-child containers are automatically removed (flattened) to keep the tree minimal.

**Pros:**

- Thirds, quarters, and arbitrary N-way splits are first-class. A container with direction=horizontal and 3 children at 33% each.
- The Lua layout API maps directly: each `panes = { ... }` list becomes a container's children.
- Resize propagates among siblings within a container. Visually adjacent panes are always siblings, so resize is intuitive.
- Proven at scale by tmux (~19 years) and i3/sway (~17 years).
- Auto-flattening keeps the tree clean when panes are closed: if closing a pane leaves a container with one child, the container is removed and its child is promoted.

**Cons:**

- More complex than binary tree. Container operations must handle N children.
- Resize algorithm must distribute space among N siblings (proportional allocation).
- Serialization is slightly more complex (variable-length child lists).

### Option C: Flexbox/container model (Zellij)

Similar to Option B but with CSS flexbox semantics: percentage-based sizing, constraint solving, and swap layouts that match containers by constraint predicates.

**Pros:**

- Most expressive layout definitions. Percentage sizing is intuitive for users.
- Swap layouts allow runtime layout switching with constraint matching (`max_panes`, `min_panes`).

**Cons:**

- Most complex implementation. Constraint solving for percentages requires iterative adjustment.
- Zellij's grid-based approach can fight the tree model, leading to edge cases.
- Swap layout matching adds complexity we do not need in Phase 1.

## Decision

**Option B — N-ary tree with auto-flattening.**

Binary trees create usability problems for 3+ pane layouts: lopsided trees, unintuitive resize propagation, and Lua layouts that don't map cleanly. The flexbox model solves those problems but adds constraint-solving complexity we don't need. The n-ary tree with auto-flattening is the pragmatic middle ground, and tmux and i3 prove it works at scale.

### Data model

```text
Workspace
  └── Tab[]
        └── LayoutNode (tree)
              ├── Container { direction, children: [LayoutNode], sizes: [f32] }
              └── Leaf { pane_id }
```

- **Container** holds N children and a direction (horizontal or vertical). `sizes` is a list of proportional weights summing to 1.0 (e.g., `[0.33, 0.33, 0.34]` for thirds).
- **Leaf** references a pane by ID.
- **Auto-flattening:** After any mutation (close pane, merge containers), walk the tree and remove containers with a single child. If a container's only child is another container with the same direction, merge their children.
- **Floating panes** are stored in a separate ordered list per tab, not in the split tree. They have absolute position and size, independent of the tiled layout.

### Split operations

- **Split right/down:** Replace the focused leaf with a new container holding the original pane and a new pane. Direction is horizontal (right) or vertical (down). Both children get equal weight.
- **Split within existing container:** If the focused pane's parent container has the same direction as the requested split, insert a new sibling rather than nesting. This keeps the tree flat (i3 behavior).
- **Close:** Remove the leaf. Auto-flatten its parent. Redistribute the closed pane's weight among siblings.
- **Resize:** Adjust weights of adjacent siblings within their container. Minimum pane size enforced (e.g., 2 columns, 1 row).

### Resize algorithm

Proportional redistribution within a container:

1. User drags a border between pane A and pane B (siblings in the same container).
2. Delta pixels are converted to a weight delta based on the container's total pixel extent.
3. A's weight increases by delta, B's weight decreases by delta.
4. Weights are clamped to enforce minimum pane size.
5. Weights are renormalized to sum to 1.0.

For window resize (terminal window grows or shrinks), all containers recalculate pixel sizes from their existing weights. No weight changes needed — weights are proportional.

**Cross-container borders:** Resize only operates between siblings within a single container. In layouts like `(A over B) | (C over D)`, the horizontal border on the left (between A and B) and the horizontal border on the right (between C and D) are in different containers and cannot be dragged as a single unified border. This is the same limitation as tmux and i3. The user resizes each container's children independently. This trade-off is inherent to tree-based layouts; a constraint-solving model (Option C) could handle unified cross-container borders but at significant complexity cost.

## Consequences

- A future Spec-0007 will formalize the n-ary tree with the types above.
- The wire protocol (Spec-0001) needs new messages for split topology: `SplitPane`, `ResizePane`, `SwapPane`, and a tree-aware `ListPanes` response. These should use the 0xA0-0xAF range to leave the pane management range (0x90-0x9F) for ADR-0012's copy mode messages.
- The Lua layout API maps directly to this tree: each `panes = { ... }` list is a container.
- Session persistence serializes the tree of containers and leaves with their weights.
- Floating panes are a separate data structure, not part of the split tree.
- Drawers, popups, modals, and sidebar panes (from the idea doc) are also separate from the split tree, each with their own positioning model. Their internal representation is deferred to the spec.
- Multiple windows share a single GUI process with one `wgpu::Device` and one glyph atlas texture. Each window gets its own `wgpu::Surface` but all rendering uses the shared device and atlas. This matches Ghostty and Kitty's approach and is the natural model for wgpu.

## References

- [03-multiplexer.md](../ideas/03-multiplexer.md) — pane types, layouts, view modes
- [33-roadmap.md](../ideas/33-roadmap.md) — Phase 1 deliverables
- [ADR-0007: Daemon Architecture](0007-daemon-architecture.md) — daemon owns pane state
- [tmux layout.c](https://github.com/tmux/tmux/blob/master/layout.c) — n-ary tree implementation
- [i3 Tree Migration](https://i3wm.org/docs/tree-migrating.html) — auto-flattening containers
- [WezTerm bintree](https://github.com/wezterm/wezterm/blob/main/mux/src/tab.rs) — binary tree, contrast model
- [Ghostty split discussion](https://github.com/ghostty-org/ghostty/discussions/2480) — user requests for i3-style splits
