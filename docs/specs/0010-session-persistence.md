---
spec: '0010'
title: Session Persistence
status: draft
date: 2026-04-02
adrs: ['0010']
tags: [core]
---

# 0010. Session Persistence

## Overview

Defines the serialization format for saving and restoring the multiplexer state across daemon restarts. The daemon serializes workspace, tab, and pane layout on exit and restores it on next launch. The layout model (Spec-0007) is the source of truth; this spec defines how it maps to disk.

## Contract

### Session File

A single JSON file at `$OAKTERM_STATE_DIR/session.json`. Default paths:

- Linux: `$XDG_STATE_HOME/oakterm/session.json` (typically `~/.local/state/oakterm/session.json`)
- macOS: `~/Library/Application Support/oakterm/session.json`

```rust
struct SessionFile {
    /// Format version. Increment on breaking changes.
    version: u32,

    /// Timestamp when the session was saved (Unix epoch seconds).
    saved_at: u64,

    /// Daemon version that wrote this file.
    daemon_version: String,

    /// Saved workspaces.
    workspaces: Vec<SavedWorkspace>,

    /// Index of the active workspace.
    active_workspace: usize,
}
```

**Version 1** is defined by this spec. Older versions are not supported (no migration path for v0 since this is the first version).

### Saved Workspace

```rust
struct SavedWorkspace {
    /// Workspace name.
    name: String,

    /// Saved tabs.
    tabs: Vec<SavedTab>,

    /// Index of the active tab.
    active_tab: usize,
}
```

### Saved Tab

```rust
struct SavedTab {
    /// Tab name (custom name or empty for auto-name).
    name: String,

    /// Root of the tiled layout tree.
    layout: SavedLayoutNode,

    /// Floating panes.
    floating: Vec<SavedFloatingPane>,

    /// Which pane was focused. Pane IDs are regenerated on restore,
    /// so focus is identified by position.
    focused_pane: SavedFocusTarget,
}
```

```rust
enum SavedFocusTarget {
    /// Focus was on a tiled pane. Index in depth-first traversal order
    /// of the layout tree (0 = first leaf).
    Tiled(usize),

    /// Focus was on a floating pane. Index into the `floating` list.
    Floating(usize),
}
```

### Saved Layout Node

```rust
enum SavedLayoutNode {
    Container {
        direction: String,   // "horizontal" or "vertical"
        children: Vec<SavedLayoutNode>,
        weights: Vec<f32>,
    },
    Leaf(SavedPane),
}
```

### Saved Pane

```rust
struct SavedPane {
    /// Working directory to restore.
    cwd: String,

    /// Command to re-execute. Empty = default shell.
    /// Only saved if the pane was created with an explicit command
    /// AND the command is marked as restartable.
    command: Option<String>,

    /// Grid dimensions at save time.
    /// Used as a hint for initial layout before the window reports its size.
    cols: u16,
    rows: u16,

    /// Pane title at save time (used for tab naming before the shell sets a title).
    title: String,
}
```

### Saved Floating Pane

```rust
struct SavedFloatingPane {
    /// The pane data.
    pane: SavedPane,

    /// Position and size as fractions of the content area (0.0-1.0).
    /// Stored as fractions, not pixels, so the layout adapts to window resize.
    x_frac: f32,
    y_frac: f32,
    width_frac: f32,
    height_frac: f32,

    /// Visibility state.
    visible: bool,
}
```

## Behavior

### Save

The daemon saves the session in these situations:

1. **Clean exit:** User quits via `oakterm quit`, closes the last window (with `daemon_persist = false`), or the OS sends SIGTERM.
2. **Periodic auto-save:** Every 60 seconds while at least one pane is open. The interval is not configurable (simplicity over flexibility).
3. **Crash recovery:** Not guaranteed. The periodic auto-save provides a best-effort recovery point.

**Save process:**

1. Walk the `MultiplexerState` (Spec-0007) and build the `SessionFile` struct.
2. For each pane, capture `cwd` (from OSC 7) and optionally `command` (if restartable).
3. For floating panes, convert pixel positions to content-area fractions: `x_frac = x / content_width`, `y_frac = y / content_height`, etc.
4. Serialize to JSON with `serde_json`. Pretty-print for debuggability.
5. Write to a temporary file, then atomically rename to `session.json`. This prevents corruption from interrupted writes.

**What is NOT saved:**

- Scrollback content (too large; scrollback is ephemeral).
- Environment variables (security: may contain secrets).
- PTY state (process state is not serializable).
- Pane IDs (regenerated on restore).

### Restore

On daemon startup:

1. Check if `session.json` exists and is valid JSON with a supported version.
2. If the session file exists, prompt the user: "Restore previous session?" via the GUI. Default: yes (auto-confirm after 3 seconds with no input).
3. If restoring:
   a. Recreate workspaces, tabs, and the layout tree from the saved structure.
   b. For each `SavedPane`, spawn a new PTY with the saved `cwd` and `command` (or default shell).
   c. For floating panes, convert fractional positions to pixel positions based on the current window size.
   d. Set the active workspace and tab indices.
4. If not restoring (or no session file), start with a single workspace, single tab, single pane (default shell).
5. Delete the session file after successful restore or decline. The next auto-save will create a fresh one.

**Partial restore:** If a saved pane's `cwd` no longer exists, fall back to `$HOME`. If a saved `command` fails to spawn, replace with the default shell. Partial failures do not abort the entire restore.

### Restartable Commands

By default, only the default shell is restored. Explicit commands (e.g., `nvim`, `npm run dev`) are only restored if the user has marked them as restartable in config:

```lua
oakterm.config.restartable_commands = {
    "nvim",
    "vim",
    "htop",
    "npm run dev",
    "cargo watch",
}
```

Pattern matching: each entry is checked as a prefix of the pane's command string. `"npm run"` matches `"npm run dev"`, `"npm run test"`, etc.

Commands not in this list are replaced with the default shell at the pane's saved `cwd`. This prevents surprising behavior (e.g., re-running a destructive script on restore).

## Constraints

- **File size:** A session with 3 workspaces, 10 tabs, 30 panes produces ~5-10 KB of JSON. Not a concern.
- **Save latency:** JSON serialization of the session struct is sub-millisecond. Atomic file rename is one syscall.
- **Restore latency:** Dominated by PTY spawning (~1-5ms per pane). 30 panes restore in ~50-150ms.
- **Concurrent access:** Only one daemon writes the session file. No locking needed.
- **Forward compatibility:** Unknown JSON fields are ignored on deserialization (`#[serde(deny_unknown_fields)]` is NOT used). New fields can be added in minor versions.
- **Backward compatibility:** The `version` field enables future migration logic. Version 1 is the only supported version.

## References

- [ADR 0010: Layout Tree Model](../adrs/0010-layout-tree-model.md) — layout tree structure
- [ADR 0007: Daemon Architecture](../adrs/0007-daemon-architecture.md) — daemon lifecycle, session persistence
- [Spec 0007: Pane Tree & Layout](0007-pane-tree-layout.md) — MultiplexerState, workspace/tab/pane model
- [Spec 0005: Lua Config Runtime](0005-lua-config-runtime.md) — `restartable_commands` config
- [03-multiplexer.md](../ideas/03-multiplexer.md) — session persistence design
