---
spec: '0007'
title: Pane Tree & Layout
status: draft
date: 2026-04-02
adrs: ['0010']
tags: [core]
---

# 0007. Pane Tree & Layout

## Overview

Defines the data model for the multiplexer's pane hierarchy: workspaces, tabs, the n-ary split tree, and floating panes. The daemon owns this model and exposes it to GUI clients via the wire protocol (Spec-0001). The Lua config runtime (Spec-0005) provides a declarative layout API that maps to this model. Implements ADR-0010.

## Contract

### Pane

A terminal surface with a PTY process. Panes are the leaves of the layout tree. The pane itself is defined by Spec-0003 (screen buffer) and Spec-0004 (scrollback). This spec defines how panes are arranged, not their internal content.

```rust
struct Pane {
    /// Unique identifier, assigned by the daemon. Monotonically increasing.
    /// Pane IDs are never reused within a daemon session.
    id: PaneId,

    /// Current title (from OSC 0/2 or shell integration).
    title: String,

    /// Current working directory (from OSC 7). Empty if unknown.
    cwd: String,

    /// Child process PID. 0 if exited.
    pid: u32,

    /// Exit code. None if still running.
    exit_code: Option<i32>,

    /// Grid dimensions (cols, rows).
    cols: u16,
    rows: u16,
}

/// Opaque pane identifier. u32 on the wire (Spec-0001).
struct PaneId(u32);
```

### Layout Node

The recursive tree structure for tiled splits. Each node is either a container (internal node) or a leaf (pane reference).

```rust
enum LayoutNode {
    Container(Container),
    Leaf(PaneId),
}

struct Container {
    /// Split direction. Children are arranged along this axis.
    direction: SplitDirection,

    /// Ordered list of children. Minimum 2 children
    /// (enforced by auto-flattening).
    children: Vec<LayoutNode>,

    /// Proportional weights for each child, summing to 1.0.
    /// `weights.len() == children.len()` is an invariant.
    /// Called "sizes" in ADR-0010; renamed to "weights" to distinguish
    /// from pixel sizes.
    weights: Vec<f32>,
}

enum SplitDirection {
    /// Children arranged left-to-right.
    Horizontal,
    /// Children arranged top-to-bottom.
    Vertical,
}
```

**Container invariants:**

- `children.len() >= 2`. A container with fewer than 2 children is invalid and must be auto-flattened (see Behavior).
- `weights.len() == children.len()`.
- All weights are positive: `w > 0.0` for every weight.
- Weights sum to 1.0 (within floating-point tolerance: `|sum - 1.0| < 0.001`).
- No two adjacent containers share the same direction. If a container's child is another container with the same direction, they must be merged (see auto-flattening).

### Tab

A tab contains one tiled layout tree and a separate list of floating panes. Only one tab is active per workspace at a time.

```rust
struct Tab {
    /// Unique identifier, assigned by the daemon.
    id: TabId,

    /// User-visible name. Defaults to the focused pane's title.
    /// Can be renamed explicitly.
    name: String,

    /// Root of the tiled layout tree. A tab with one pane has a bare Leaf,
    /// not a Container. Auto-flattening enforces this: a Container with
    /// one child at the root is replaced by its single child.
    layout: LayoutNode,

    /// Floating panes, ordered by z-index (last = topmost).
    /// Floating panes are not part of the tiled layout tree.
    floating: Vec<FloatingPane>,

    /// The focused pane within this tab (tiled or floating).
    focused_pane: PaneId,
}

struct TabId(u32);

struct FloatingPane {
    /// The pane.
    pane_id: PaneId,

    /// Position relative to the tab's content area, in pixels.
    x: f32,
    y: f32,

    /// Size in pixels.
    width: f32,
    height: f32,

    /// Whether the floating pane is currently visible.
    visible: bool,
}
```

**Tab invariants:**

- Every `PaneId` in the layout tree and floating list is unique within the tab.
- `focused_pane` references a pane that exists in either the layout tree or the floating list.
- A tab always contains at least one pane. Closing the last pane closes the tab.

### Workspace

A workspace is an independent context containing tabs. Switching workspaces switches the entire visible state.

```rust
struct Workspace {
    /// Unique identifier.
    id: WorkspaceId,

    /// User-visible name (e.g., "work", "personal").
    name: String,

    /// Ordered list of tabs. At least one tab.
    tabs: Vec<Tab>,

    /// Index of the active tab in `tabs`.
    active_tab: usize,
}

struct WorkspaceId(u32);
```

**Workspace invariant:** `tabs` is never empty. Closing the last tab in a workspace closes the workspace. Closing the last workspace exits the daemon (unless `daemon_persist` is enabled).

### Multiplexer State

The top-level state owned by the daemon.

```rust
struct MultiplexerState {
    /// All workspaces. At least one.
    workspaces: Vec<Workspace>,

    /// Index of the active workspace.
    active_workspace: usize,

    /// Monotonic counter for generating unique IDs.
    next_pane_id: u32,
    next_tab_id: u32,
    next_workspace_id: u32,
}
```

## Behavior

### Split

When the user splits a pane:

1. **Same-direction optimization:** If the target pane's parent container has the same direction as the requested split, insert a new pane as a sibling. The new pane gets weight `1.0 / (N + 1)` where N is the current child count. Existing siblings' weights are scaled by `N / (N + 1)` to make room.
2. **Different direction or bare leaf:** Replace the target leaf with a new container. The container's direction is the requested split direction. It has two children: the original pane and the new pane, each with weight 0.5.
3. The new pane is spawned with the default shell (or a specified command). Its grid dimensions are calculated from the container's pixel allocation and the pane's weight.
4. Focus moves to the new pane.

**Example:** Splitting pane B right in this layout:

```text
Container(H) [A: 0.5, B: 0.5]
```

B's parent is horizontal, the split is horizontal (right), so same-direction optimization applies:

```text
Container(H) [A: 0.33, B: 0.33, C: 0.34]
```

### Close

When a pane is closed:

1. Remove the leaf from its parent container.
2. Redistribute the closed pane's weight proportionally among remaining siblings.
3. **Auto-flatten:** If the parent container now has one child:
   - If the container is the layout root, replace the root with the remaining child.
   - Otherwise, replace the container in its parent with the remaining child. The child inherits the container's weight in the grandparent.
4. **Same-direction merge:** After flattening, if a container's child is another container with the same direction, merge: splice the child container's children and weights into the parent at that position.
5. If this was the last pane in the tab, close the tab. If the last tab in the workspace, close the workspace.
6. Focus moves to the nearest sibling (prefer the pane to the left/above).

### Resize

Resize adjusts the weight boundary between two adjacent siblings in the same container.

1. Identify the two siblings on either side of the dragged border.
2. Convert the pixel delta to a weight delta: `delta_weight = delta_pixels / container_pixel_extent`.
3. Increase one sibling's weight and decrease the other's by `delta_weight`.
4. Clamp both weights to enforce minimum pane size (2 columns wide, 1 row tall).
5. Renormalize all weights in the container to sum to 1.0.

**Window resize:** When the terminal window changes size, all containers recalculate pixel sizes from their existing weights. Weights do not change. Each pane's grid dimensions (cols, rows) are recalculated from its new pixel allocation and the cell size. The daemon sends `Resize` to each pane's PTY with the new dimensions.

**Cross-container borders:** Resize operates between siblings only. Borders that visually align across containers (e.g., a horizontal border spanning two vertical containers) cannot be dragged as a unified border. Each container's children are resized independently. This matches tmux and i3 behavior.

### Focus Navigation

Directional focus (left, right, up, down) finds the nearest pane in the requested direction:

1. From the focused pane's pixel bounds, project a ray in the requested direction.
2. Among all visible panes (tiled and floating), find the pane whose bounds intersect the ray and are closest to the origin pane.
3. If no pane is found (edge of screen), wrap or do nothing (configurable).

Focus navigation is purely geometric, not tree-structural. This avoids confusing behavior when the tree structure does not match the visual layout.

### Tab Operations

- **New tab:** Append a new tab with a single pane (default shell). The new tab becomes active.
- **Close tab:** Close all panes in the tab. If this is the last tab, close the workspace.
- **Switch tab:** By index (1-9) or by cycling (next/previous). The previously active tab retains its layout and pane state.
- **Move tab:** Reorder within the workspace.
- **Rename tab:** Set a custom name. If no custom name, the tab name follows the focused pane's title.

### Workspace Operations

- **New workspace:** Create with a name and a single tab containing a single pane.
- **Switch workspace:** By name or by cycling. The previously active workspace retains all state.
- **Close workspace:** Close all tabs and panes. If this is the last workspace and `daemon_persist` is disabled, the daemon exits.
- **Rename workspace:** Change the display name.

### Floating Pane Operations

- **Create floating:** Spawn a new pane positioned at the center of the tab's content area, sized to 80% x 80% of the content area.
- **Toggle visibility:** Hide a floating pane (remains alive, not visible) or show it again.
- **Move:** Update `x`, `y` position. Clamped to keep at least 20px visible within the content area.
- **Resize:** Update `width`, `height`. Minimum size: 10 columns x 3 rows.
- **Promote to tiled:** Remove from the floating list and insert into the tiled layout tree as a split of the currently focused tiled pane.
- **Demote to floating:** Remove a tiled pane from the layout tree (closing the leaf, auto-flattening) and add it to the floating list at the center of the content area.

Floating panes render above the tiled layout. The topmost floating pane (last in the list) receives keyboard input when focused. Clicking a floating pane brings it to the top of the z-order.

### Auto-Flattening

After any mutation to the layout tree, the following invariants are restored:

1. **Single-child containers:** If a container has exactly one child, replace the container with its child. If the container was in a parent, the child inherits the container's weight.
2. **Same-direction nesting:** If a container's child is another container with the same direction, merge the child's children and weights into the parent at the child's position. The child's weights are scaled by the child's weight in the parent.

**Weight scaling during merge:** If parent container has weights `[0.4, 0.6]` and the second child (weight 0.6) is a same-direction container with weights `[0.5, 0.5]`, the merged weights are `[0.4, 0.3, 0.3]` (0.6 \* 0.5 = 0.3 each).

Auto-flattening runs after every split, close, promote, and demote operation. It is idempotent.

### Pane Dimension Calculation

Converting weights to grid dimensions:

1. Start from the tab's content area in pixels (window size minus padding, minus tab bar height, minus status bar height).
2. For each container, distribute its pixel extent among children by weight. Subtract 1 pixel per internal border (between siblings).
3. At each leaf, convert pixel dimensions to grid dimensions: `cols = floor(pixel_width / cell_width)`, `rows = floor(pixel_height / cell_height)`. Minimum: 1 column, 1 row.
4. Send `Resize { pane_id, cols, rows }` to each pane whose dimensions changed.

## Constraints

- **Maximum panes per tab:** Implementation-defined, recommended minimum 64.
- **Maximum tabs per workspace:** Implementation-defined, recommended minimum 32.
- **Maximum workspaces:** Implementation-defined, recommended minimum 16.
- **Maximum tree depth:** No hard limit, but auto-flattening keeps depth proportional to the number of distinct split directions used. Typical depth is 2-4 for realistic layouts.
- **Minimum pane size:** 2 columns, 1 row (enough to display a cursor). Splits that would create a pane smaller than this are rejected.
- **Weight precision:** `f32` provides ~7 significant digits. For proportional weights summing to 1.0, this is sufficient for any practical number of splits.
- **ID space:** `u32` pane/tab/workspace IDs. At one pane created per second, overflow takes ~136 years.

## References

- [ADR 0010: Layout Tree Model](../adrs/0010-layout-tree-model.md)
- [ADR 0007: Daemon Architecture](../adrs/0007-daemon-architecture.md) — daemon owns multiplexer state
- [Spec 0001: Daemon Wire Protocol](0001-daemon-wire-protocol.md) — pane management messages
- [Spec 0003: Screen Buffer](0003-screen-buffer.md) — pane grid model
- [Spec 0005: Lua Config Runtime](0005-lua-config-runtime.md) — layout declaration API
- [03-multiplexer.md](../ideas/03-multiplexer.md) — pane types, layouts, workspaces
