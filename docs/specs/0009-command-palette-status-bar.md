---
spec: '0009'
title: Command Palette & Status Bar
status: draft
date: 2026-04-02
adrs: ['0011']
tags: [core]
---

# 0009. Command Palette & Status Bar

## Overview

Defines the command palette (fuzzy-searchable action launcher) and status bar (persistent mode/state indicator). Both are GUI-side UI elements rendered outside the pane content area. The command palette searches an action registry shared with the keybind system (ADR-0011). The status bar displays the current mode, focused pane title, and workspace/tab context.

## Contract

### Action Registry

All executable actions are registered in a central registry. The command palette searches it; keybinds reference it; plugins (Phase 2) will add to it.

```rust
struct ActionRegistry {
    /// All registered actions.
    actions: Vec<RegisteredAction>,
}

struct RegisteredAction {
    /// Unique identifier (e.g., "split_pane_right", "new_tab").
    id: String,

    /// Human-readable label shown in the palette.
    label: String,

    /// Category for grouping in the palette.
    category: ActionCategory,

    /// Keybind hint displayed alongside the label (e.g., "Ctrl+Shift+\").
    /// Resolved from the keybind table at registration time.
    keybind_hint: Option<String>,

    /// Whether this action is currently executable.
    /// Checked before display and before dispatch.
    is_performable: fn(&AppState) -> bool,

    /// The action to execute.
    execute: fn(&mut AppState),
}

enum ActionCategory {
    Pane,
    Tab,
    Workspace,
    Navigation,
    Clipboard,
    View,
    Config,
}
```

**Core actions registered at startup:** split_pane_right, split_pane_down, close_pane, focus_pane_left, focus_pane_right, focus_pane_up, focus_pane_down, new_tab, close_tab, next_tab, previous_tab, new_workspace, switch_workspace, toggle_floating, enter_copy_mode, enter_resize_mode, reload_config, toggle_fullscreen, show_command_palette.

### Command Palette

```rust
struct CommandPalette {
    /// Whether the palette is visible.
    visible: bool,

    /// Current input text.
    query: String,

    /// Filtered and ranked results.
    results: Vec<PaletteResult>,

    /// Index of the selected result.
    selected: usize,
}

struct PaletteResult {
    /// What this result represents.
    kind: PaletteResultKind,

    /// Display label.
    label: String,

    /// Keybind hint (actions only).
    keybind: Option<String>,

    /// Fuzzy match score (higher = better match).
    score: u32,

    /// Character positions in the label that matched the query.
    match_positions: Vec<usize>,
}

enum PaletteResultKind {
    /// An executable action from the registry.
    Action(String),         // action_id

    /// A workspace to switch to (from `@` prefix).
    Workspace(WorkspaceId),

    /// A layout to apply (from `#` prefix).
    Layout(String),         // layout name

    /// A config setting to toggle (from `:` prefix).
    Setting(String),        // config key
}
```

### Prefix Filters

When the query starts with a prefix character, results are scoped to a category:

| Prefix | Scopes to                                 | Example   |
| ------ | ----------------------------------------- | --------- |
| `>`    | Actions (pane, tab, workspace operations) | `> split` |
| `@`    | Workspaces                                | `@ work`  |
| `#`    | Layouts                                   | `# dev`   |
| `:`    | Settings (live config toggle)             | `: font`  |

No prefix searches all categories. The prefix character is stripped from the query before matching.

### Fuzzy Matching

The matcher scores query characters against the label:

1. Each query character must appear in the label in order (subsequence match).
2. Scoring bonuses: consecutive matches (+3), match at word boundary (+2), match at start of label (+1).
3. Scoring penalties: gap between matches (-1 per gap character).
4. Results sorted by score descending. Ties broken by label length (shorter first).

Non-performable actions are excluded from results (checked via `is_performable`).

### Status Bar

A single-line bar at the bottom of the window.

```rust
struct StatusBar {
    /// Left-aligned segments.
    left: Vec<StatusSegment>,

    /// Center-aligned segments.
    center: Vec<StatusSegment>,

    /// Right-aligned segments.
    right: Vec<StatusSegment>,
}

enum StatusSegment {
    /// Current mode (e.g., "COPY", "RESIZE"). Hidden in normal mode.
    Mode(String),

    /// Active workspace name.
    Workspace(String),

    /// Tab list with active indicator.
    Tabs(Vec<TabInfo>),

    /// Focused pane title.
    PaneTitle(String),

    /// Current time.
    Clock(String),

    /// Static text.
    Text(String),
}

struct TabInfo {
    name: String,
    active: bool,
    index: usize,
}
```

**Default layout:**

```text
[COPY] work | 1:code  2:git  3:logs                      ~/project  14:30
 mode  ws     tabs                                        pane title  clock
```

- Left: mode indicator (only when in copy/resize mode), workspace name, tab list.
- Right: focused pane title (truncated to fit), clock.
- Center: empty by default.

**Discoverability:** When a mode is active (copy, resize), the status bar shows available keys:

```text
[COPY] j/k:move  v:select  y:yank  /:search  q:quit
```

This hint text is configurable and auto-hides after `status_bar_hint_duration` (default: 2 weeks of first use, then hidden).

## Behavior

### Palette Lifecycle

1. User presses `oak_mod + P` (or configured keybind).
2. Palette appears centered at the top of the window, overlaying pane content.
3. User types to filter. Results update on each keystroke.
4. `Up`/`Down` or `Ctrl+p`/`Ctrl+n` navigate results.
5. `Enter` executes the selected action and closes the palette.
6. `Escape` closes the palette without executing.
7. If no results match, the palette shows "No matching actions."

### Palette Default View

When opened with an empty query, the palette shows:

1. Recent actions (last 5 executed via palette, deduplicated).
2. All actions grouped by category, sorted alphabetically within each group.

### Status Bar Updates

The status bar re-renders when:

- The active mode changes (enter/exit copy mode, resize mode).
- The focused pane changes (title, cwd).
- A tab is created, closed, renamed, or switched.
- The active workspace changes.
- The clock ticks (once per minute).

The status bar does not re-render on pane content changes (that would be every frame).

### Configuration

```lua
oakterm.config.status_bar = true           -- show/hide
oakterm.config.status_bar_position = "bottom"  -- "top" or "bottom"
oakterm.config.status_bar_hint_duration = "2w" -- auto-hide key hints after 2 weeks
```

Status bar configuration is part of Spec-0005 (Lua Config Runtime) addendum.

## Constraints

- **Palette render latency:** Fuzzy matching over ~50 core actions is sub-millisecond. Plugin actions (Phase 2) may grow this to ~500 actions; matching should stay under 1ms.
- **Status bar height:** Exactly 1 row of the configured font. Subtracted from the pane content area (see Spec-0007 pane dimension calculation).
- **Tab bar height:** Exactly 1 row when tabs > 1, 0 rows when only 1 tab. Also subtracted from content area.

## References

- [ADR 0011: Keybind Dispatch](../adrs/0011-keybind-dispatch.md) — action registry, performable actions
- [Spec 0007: Pane Tree & Layout](0007-pane-tree-layout.md) — tab/workspace model, pane dimension calculation
- [Spec 0005: Lua Config Runtime](0005-lua-config-runtime.md) — configuration API
- [08-command-palette.md](../ideas/08-command-palette.md) — design exploration, prefix filters
- [03-multiplexer.md](../ideas/03-multiplexer.md) — discoverability, status bar
