# Plugin System

Extensibility is the core architecture, not an afterthought. The terminal is a platform — a fast rendering and multiplexing engine that exposes primitives. Plugins compose those primitives into features.

If something can't be built as a plugin, the answer is to improve the core API — not to hardcode the feature.

## Runtime

- **Lua** for config and keybinds (proven by WezTerm)
- **WASM** for plugins (sandboxed, fast, polyglot)
- Wasmtime as the WASM runtime (Rust-native)

## Core Primitives

The core exposes these primitives. Everything else is built on top of them by plugins.

### Panes (terminal)
- Create terminal panes (shell, command, floating, tiled)
- Attach metadata (key-value pairs — branch, status, context %, etc.)
- Read pane output (last N lines)
- Send input to a pane
- Control scroll behavior per pane

### Pane Surfaces (non-terminal)
- Request a native rendering surface within a pane region
- Surface types: `webview` (WebKit/WebKitGTK), `canvas` (raw pixel buffer)
- Core allocates the rectangle, routes keyboard/mouse events to the plugin
- Core composites the surface alongside GPU-rendered terminal content
- Platform abstraction over NSView (macOS) and GtkWidget (Linux)

This is the primitive that enables a browser plugin, a markdown previewer, an image viewer, a media player — without the core knowing anything about any of them.

### Sidebar Data Model (core)
- Register sections (agents, containers, pods, browser tabs — anything)
- Add/remove/update entries within a section
- Set icons, labels, badges, progress bars
- Handle click → focus a pane
- Note: the sidebar **data model** is a core primitive. The sidebar **renderer** (the visual UI that draws it) is a bundled plugin (`sidebar-ui`) that can be replaced with a bottom bar, floating HUD, or nothing.

### Command Palette
- Register commands with any name (:agent, :docker, :browse, :merge)
- Register palette sections
- Register keybindings
- Register prefix filters

### Notifications
- Set badge on any pane or sidebar entry
- Trigger attention signals (❓, ✓, ✗, custom)
- Register for the Cmd+Shift+U attention cycle

### Context Engine
- Register completion providers (per command + argument position)
- Register project type detectors
- Register proactive suggestion triggers

### Pane Query
- `pane.list()` — enumerate all panes with their metadata
- `pane.get(id)` — get a specific pane's full state
- `pane.focus(id)` — switch focus to a pane
- `pane.set_border_color(id, color)` — set visual border color
- `pane.set_label(id, text)` — set a floating label on a pane
- Enables: harpoon (enumerate + focus), environment coloring (border + label), input broadcast (enumerate + select)

### Window
- `window.position(x, y, w, h)` — set window position and size
- `window.always_on_top(bool)` — keep above other windows
- `window.animate(type, duration)` — slide/fade transitions
- `global_hotkey.register(key, callback)` — system-wide hotkey registration
- Enables: quake/dropdown mode, spotlight mode, any global-access plugin

### Storage
- `storage.get(key)` — read plugin-local persistent data
- `storage.set(key, value)` — write plugin-local persistent data
- `storage.delete(key)` — remove a key
- Scoped per-plugin, persists across sessions
- Stored in `~/.local/state/phantom/plugins/<name>/data`
- Enables: harpoon (persist bookmark list), any plugin with state

### Shell Integration Events
- `shell.on_prompt_start` — fired when shell draws a prompt (OSC 133;A)
- `shell.on_command_start` — fired when user executes a command (OSC 133;B)
- `shell.on_command_finish(exit_code)` — fired when command completes (OSC 133;D)
- `shell.on_cwd_change(path)` — fired when working directory changes (OSC 7)
- `shell.get_prompts(pane)` — list all prompt positions in scrollback
- Parsed by the VT parser (core), exposed to plugins as events
- Enables: scroll-to-prompt, process notifications, semantic zones, context engine

### Lifecycle Hooks
- on_pane_create / on_pane_close
- on_pane_focus / on_pane_blur
- on_directory_change
- on_workspace_create
- on_process_exit
- on_surface_resize

### Process
- Spawn and manage child processes
- Read stdout/stderr
- Detect process state changes

### Filesystem
- Read files (for project detection, config)
- Watch files/directories for changes

### Network
- HTTP requests (for API integrations, AI backends)
- WebSocket connections

### Accessibility
- `announce(message, priority)` — send text to screen reader (polite or assertive)
- `set_live_region(pane, politeness)` — mark a pane for auto-announcement
- `set_role(element, role)` — semantic role (alert, status, log, progressbar)
- `label(element, text)` — accessible name (required for all UI elements)
- `description(element, text)` — accessible description
- `value(element, current, min, max)` — for progress bars and meters
- Plugins that add UI **must** provide accessible labels — the API rejects entries without them

## Capability-Based Permissions

Plugins declare what they need. Users approve once per plugin.

```
Plugin: agent-manager
Requests:
  ✓ sidebar.section
  ✓ pane.create
  ✓ pane.metadata
  ✓ process.spawn
  ✓ fs.read
  ✓ notify
  ✗ pane.surface      (not requested)
  ✗ network           (not requested)

Plugin: browser-webview
Requests:
  ✓ pane.surface       (type: webview)
  ✓ sidebar.section
  ✓ palette.command
  ✓ network
  ✓ notify
```

A plugin can't access what it didn't request. A text-mode browser plugin needs only `pane.create` and `process.spawn`. A full WebView browser needs `pane.surface`. The permission model makes the difference visible.

## The Litmus Test

Every feature we design should pass this test:

> **Could a third-party developer build this as a plugin with the current API?**

If no — we're missing a primitive. Add the primitive, not the feature.

| Feature | Primitives used |
|---------|----------------|
| Agent sidebar | `sidebar.section` + `pane.create` + `process.spawn` + `notify` + `fs.read` |
| Context engine | `context.provider` + `fs.read` + `hook.directory_change` |
| Docker manager | `sidebar.section` + `process.spawn` + `pane.create` + `notify` |
| WebView browser | `pane.surface(webview)` + `sidebar.section` + `palette.command` + `network` |
| Markdown preview | `pane.surface(webview)` + `fs.read` + `fs.watch` |
| Image viewer | `pane.surface(canvas)` + `fs.read` |
| k8s pod manager | `sidebar.section` + `process.spawn` + `pane.create` + `network` + `notify` |
| Theme | `theme.register` (colors, no special permissions) |
| Port monitor | `process.spawn` + `notify` + `sidebar.section` |
| AI autocomplete | `context.provider` + `network` |

If a plugin idea can't be expressed as a combination of primitives, we need a new primitive.

## Bundled Plugins

Ship by default, can be disabled. They use the exact same API as community plugins — no special access, no private APIs.

| Plugin | What it does |
|--------|-------------|
| `sidebar-ui` | The sidebar itself — replaceable |
| `agent-manager` | Agent sidebar, worktree lifecycle, notifications, :agent/:merge/:diff |
| `context-engine` | Smart autocomplete, project detection, ? NL commands |
| `git-worktree` | :workspace new creates worktree + tab + shell |
| `service-monitor` | Services sidebar, port detection, crash alerts |
| `watcher` | Watchers sidebar, parses test/type/build output |
| `kitty-graphics` | Inline image rendering (Kitty + Sixel) |
| `browser-lite` | Text-mode browser (Carbonyl/w3m) in a floating pane |

## Community Plugin Examples

| Plugin | What it does |
|--------|-------------|
| `browser-webview` | Full native WebView browser in a pane |
| `docker-manager` | Sidebar section for containers |
| `k8s-pods` | Sidebar section for Kubernetes pods |
| `port-monitor` | Detect and display listening ports |
| `ai-provider-ollama` | Local LLM backend for ? commands |
| `theme-catppuccin` | Color scheme |
| `container-shell` | Spawn shells inside Docker/Podman containers (Ptyxis-style) |
| `markdown-preview` | Live preview of .md files in a WebView pane |
| `media-player` | Play audio/video in a pane |
| `serial-terminal` | Connect to serial ports (Tabby-style) |

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

## Plugin Manager & Registry

### CLI

```
phantom plugin install docker-manager
phantom plugin remove docker-manager
phantom plugin list
phantom plugin search browser
phantom plugin update
phantom plugin update docker-manager
phantom plugin info docker-manager
```

### In the Palette

```
Cmd+Shift+P → :plugins

┌──────────────────────────────────────────────────┐
│  plugins:  Search plugins                        │
├──────────────────────────────────────────────────┤
│  Installed                                       │
│  ✓ agent-manager        bundled    enabled       │
│  ✓ context-engine       bundled    enabled       │
│  ✓ docker-manager       v1.2.0     enabled       │
│  ○ browser-lite         bundled    disabled      │
│                                                  │
│  Available                                       │
│  ↓ k8s-pods             Kubernetes pod manager   │
│  ↓ browser-webview      Native WebView browser   │
│  ↓ serial-terminal      Serial port connections  │
│  ↓ theme-catppuccin     Catppuccin color scheme  │
└──────────────────────────────────────────────────┘
```

- Toggle enabled/disabled inline
- Install with Enter — shows permission request before first enable
- One-click update when new version available

### The Registry

A lightweight, public index — not an app store. **All registry plugins must be open source.**

- Static API or git repo mapping plugin names to WASM binary URLs
- Each entry: name, version, description, declared capabilities, source repo URL, checksum
- **Open source required** — any OSI-approved license (MIT, Apache, MPL, GPL, etc.)
- Source repo URL is verified and linked — users can always inspect the code
- Community submits plugins via PR
- No approval gate beyond "valid WASM binary with manifest + open source license"
- No reviews, no ratings — stars on the plugin's source repo are enough
- No install count telemetry
- Sideloading bypasses the registry — install any WASM binary from URL or local path (unreviewed, user accepts risk)

### Plugin Manifest

Every plugin ships a `phantom-plugin.toml`:

```toml
[plugin]
name = "docker-manager"
version = "1.2.0"
description = "Sidebar section for Docker containers"
authors = ["someone"]
repository = "https://github.com/someone/phantom-docker"
license = "MIT"
min-core-version = "0.5.0"

[capabilities]
sidebar = true
pane-create = true
process-spawn = true
notify = true
network = false
fs-read = true
pane-surface = false
```

### What It's Not

- Not a paid marketplace — all registry plugins are open source
- Not a walled garden — sideload any WASM binary from a URL or local path
- No auto-update by default — `phantom plugin update` is explicit
- No telemetry on installs, usage, or anything else
