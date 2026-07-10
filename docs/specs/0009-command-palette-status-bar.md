---
spec: '0009'
title: Command Palette & Status Bar
status: implementing
date: 2026-04-02
adrs: ['0011']
tags: [core]
---

# 0009. Command Palette & Status Bar

## Overview

Defines the command palette (fuzzy-searchable action launcher) and status bar (persistent mode/state indicator). Both are GUI-side UI elements rendered outside the pane content area. The command palette searches an action registry shared with the keybind system (ADR-0011). The status bar displays the current mode, focused pane title, and workspace/tab context.

## Contract

### Action Registry

All executable actions are registered in a central registry (`oakterm-config::actions`). The command palette searches it; keybinds reference it; plugins (Phase 2) will add to it.

As built, action identity is a typed enum rather than a `String` id: `ActionId` gives exhaustive matching internally, and `as_str()` produces the `snake_case` boundary identifier used by Lua config and display. The registry stores no behavior (no `execute` field, no `fn(&AppState)`), so the catalog is pure data, unit-testable without an event loop:

- **Performability** is a method on `ActionId` taking an `ActionContext`, a plain `Copy` snapshot of the GUI state it depends on (pane/tab counts, focus-direction availability) built by the GUI at query time.
- **Execution** lives GUI-side: the palette's confirm path maps the `ActionId` to its dispatch descriptor (`ActionDesc`) via an exhaustive match in `main.rs`.

```rust
enum ActionId {
    SplitPaneRight,
    SplitPaneDown,
    // ... one variant per registered action
}

impl ActionId {
    const ALL: [ActionId; N];                        // catalog order
    fn as_str(self) -> &'static str;                 // "split_pane_right"
    fn label(self) -> &'static str;                  // "Split Pane Right"
    fn category(self) -> ActionCategory;
    fn is_performable(self, ctx: ActionContext) -> bool;
}

/// Snapshot of GUI state performability depends on. Copy, Default.
struct ActionContext {
    pane_count: usize,
    tab_count: usize,
    can_focus_left: bool,
    can_focus_right: bool,
    can_focus_up: bool,
    can_focus_down: bool,
}

/// Catalog entry: id + hint resolved from the active bindings.
/// Fields are private; only ActionRegistry constructs entries.
struct RegisteredAction {
    id: ActionId,
    keybind_hint: Option<String>,   // display form, e.g. "Cmd+P"
}

struct ActionRegistry {
    actions: Vec<RegisteredAction>,
}

impl ActionRegistry {
    /// Builds the core catalog, resolving each keybind hint from the
    /// registry's effective bindings (shadowed chords never shown).
    fn core(keybinds: &KeybindRegistry) -> Self;
    fn actions(&self) -> &[RegisteredAction];
    fn find(&self, id: ActionId) -> Option<&RegisteredAction>;
    fn performable(&self, ctx: ActionContext) -> impl Iterator<Item = &RegisteredAction>;
}

/// Ord follows declaration order = the palette's group display order.
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

**Registration policy:** only actions with a working handler register; the catalog never contains entries that would execute as no-ops. Registered today: split_pane_right, split_pane_down, close_pane, focus_pane_left, focus_pane_right, focus_pane_up, focus_pane_down, new_tab, close_tab, next_tab, previous_tab, toggle_fullscreen, show_command_palette, reload_config.

**Target set** (register as their features land): new_workspace, switch_workspace, toggle_floating, enter_copy_mode, enter_resize_mode.

### Command Palette

The palette core (`oakterm/src/palette.rs`) is pure: no GPU or event-loop types. Callers pass the live `ActionRegistry` and an `ActionContext` snapshot on every mutating call; the state stores neither.

```rust
/// Result rows visible at once; the window scrolls to keep the
/// selection in view.
const MAX_VISIBLE_RESULTS: usize = 10;

struct PaletteState {
    visible: bool,

    /// Current input text (prefix included).
    query: String,

    /// Filtered and ranked results.
    results: Vec<PaletteResult>,

    /// Index of the selected result.
    selected: usize,

    /// First visible result row. Moves only when the selection would
    /// leave the visible window, so Up/Down move the cursor, not the
    /// list. Reset to 0 on every query change.
    window_start: usize,

    /// Session-only recent-action history (not persisted). Deduplicated,
    /// most recent first, capped at 5. Survives across opens.
    recent: Vec<ActionId>,
}

struct PaletteResult {
    /// What this result represents.
    kind: PaletteResultKind,

    /// Display label.
    label: String,

    /// Keybind hint (actions only).
    keybind: Option<String>,

    /// Fuzzy match score. i32: gap penalties can push a valid match
    /// negative; callers rank, they don't threshold.
    score: i32,

    /// Character positions in the label that matched the query.
    match_positions: Vec<usize>,
}

enum PaletteResultKind {
    /// An executable action from the registry.
    Action(ActionId),

    /// A workspace to switch to (from `@` prefix). Carries the wire-side
    /// u32 id (TabList); the daemon's WorkspaceId newtype is not a GUI
    /// dependency.
    Workspace(u32),

    /// A layout to apply (from `#` prefix).
    Layout(String),         // layout name

    /// A config setting to toggle (from `:` prefix).
    Setting(String),        // config key
}
```

Confirming a result returns its `PaletteResultKind` to the caller and hides the palette; the GUI executes it. Only `Action` has a provider today; workspace, layout, and setting scopes return empty results until their features land.

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

The matcher scores query characters against the label (case-insensitive):

1. Each query character must appear in the label in order (subsequence match).
2. Scoring bonuses: consecutive matches (+3), match at word boundary (+2), match at start of label (+1).
3. Scoring penalties: gap between matches (-1 per gap character).
4. Results sorted by score descending. Ties broken by label length (shorter first), then catalog order (stable sort).

Among all valid alignments of the query, the highest-scoring one wins (dynamic programming over alignments), so `match_positions` highlights the characters a user would expect (word starts and consecutive runs) rather than the leftmost subsequence. Scores are `i32` and can go negative when gap penalties outweigh bonuses; a negative score is still a valid match.

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
3. User types to filter. Results update on each keystroke; selection and scroll window reset to the top.
4. `Up`/`Down` or `Ctrl+p`/`Ctrl+n` navigate results. At most `MAX_VISIBLE_RESULTS` (10) rows are visible; the window scrolls only when the selection crosses its edge.
5. `Enter` executes the selected action and closes the palette.
6. `Escape` closes the palette without executing.
7. If no results match, the palette shows "No matching actions."

While the palette is visible it captures all keyboard input: keybind chords and PTY forwarding are bypassed. A config reload closes the palette (the registry it was filtering against is replaced).

### Palette Default View

When opened with an empty query, the palette shows:

1. Recent actions (last 5 executed via palette, deduplicated, most recent first).
2. All other performable actions grouped by category (in `ActionCategory` declaration order), sorted alphabetically within each group. Actions already shown as recents are excluded from the grouped list.

Recents are session-only: they are not persisted across restarts.

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

- **Palette render latency:** Fuzzy matching over the core catalog (14 actions today, ~50 once the target set fills in) is sub-millisecond. Plugin actions (Phase 2) may grow this to ~500 actions; matching should stay under 1ms.
- **Status bar height:** Exactly 1 row of the configured font. Subtracted from the pane content area (see Spec-0007 pane dimension calculation).
- **Tab bar height:** Exactly 1 row when tabs > 1, 0 rows when only 1 tab. Also subtracted from content area.

## References

- [ADR 0011: Keybind Dispatch](../adrs/0011-keybind-dispatch.md) — action registry, performable actions
- [Spec 0007: Pane Tree & Layout](0007-pane-tree-layout.md) — tab/workspace model, pane dimension calculation
- [Spec 0005: Lua Config Runtime](0005-lua-config-runtime.md) — configuration API
- [08-command-palette.md](../ideas/08-command-palette.md) — design exploration, prefix filters
- [03-multiplexer.md](../ideas/03-multiplexer.md) — discoverability, status bar
