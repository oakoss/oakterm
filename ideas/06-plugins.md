# Plugin System

The core terminal ships lean (~5MB). Everything smart is a plugin — some bundled, some community.

## Runtime

- **Lua** for config and keybinds (proven by WezTerm)
- **WASM** for plugins (sandboxed, fast, polyglot)
- Wasmtime as the WASM runtime (Rust-native)

## Capability-Based Permissions

Plugins request what they need. Users approve once.

```
Plugin: agent-manager
Requests:
  ✓ sidebar.section    (add UI section)
  ✓ pane.create        (open panes)
  ✓ pane.metadata      (read/write status)
  ✓ process.spawn      (start agents)
  ✓ fs.read            (detect project type)
  ✓ notify             (badge panes)
  ✗ network            (not requested)
```

A plugin can't do what it didn't ask for.

## Plugin API Surface

### Sidebar
- Register a section (agents, containers, pods)
- Add/remove/update entries
- Set icons, labels, badges, progress bars
- Handle click → focus a pane

### Panes
- Create panes (shell, command, floating)
- Attach metadata (branch, status, context %)
- Read pane output (last N lines)
- Send input to a pane

### Command Palette
- Register commands (:agent, :docker, :merge)
- Register palette sections
- Register keybindings

### Notifications
- Set badge on pane/sidebar entry
- Trigger attention signals
- Register for Cmd+Shift+U jump cycle

### Context Engine
- Register completion providers (per command)
- Register project detectors
- Register suggestion triggers

### Lifecycle Hooks
- on_pane_create
- on_pane_close
- on_directory_change
- on_workspace_create
- on_process_exit

## Bundled Plugins

Ship by default, can be disabled:

| Plugin | What it does |
|--------|-------------|
| `agent-manager` | Agent sidebar, worktree lifecycle, notifications, :agent/:merge/:diff |
| `context-engine` | Smart autocomplete, project detection, ? NL commands |
| `git-worktree` | :workspace new creates worktree + tab + shell |
| `service-monitor` | Services sidebar, port detection, crash alerts |
| `watcher` | Watchers sidebar, parses test/type/build output |
| `ssh-domains` | Remote multiplexing from config |
| `kitty-graphics` | Inline image rendering (Kitty + Sixel) |

## Community Plugin Examples

| Plugin | What it does |
|--------|-------------|
| `docker-manager` | Sidebar section for containers |
| `k8s-pods` | Sidebar section for Kubernetes pods |
| `port-monitor` | Detect and display listening ports |
| `ai-provider-ollama` | Local LLM backend for ? commands |
| `theme-catppuccin` | Color scheme |
| `container-shell` | Spawn shells inside Docker/Podman containers (Ptyxis-style) |

## Minimal Config Example

Someone who just wants a fast terminal:

```lua
plugins = {
  ["agent-manager"]  = { enabled = false },
  ["context-engine"] = { enabled = false },
  ["service-monitor"] = { enabled = false },
  ["watcher"]        = { enabled = false },
}
```

Still boots in 5ms. Still the fastest terminal. No smart features.

## Why WASM

- Sandboxed — buggy plugin can't crash the terminal or read unauthorized files
- Fast — near-native speed, no GC pauses
- Polyglot — Rust, Go, Zig, C, AssemblyScript, anything → WASM
- Portable — same plugin works on macOS and Linux
- Proven — Zellij already ships a WASM plugin system

## The Sidebar Itself Is a Plugin

Even the sidebar chrome is a plugin (`sidebar-ui`). Replaceable with:
- A bottom bar
- A floating HUD
- Nothing — just palette and keybinds
- A right sidebar

The core provides the data model. Plugins provide the presentation.
