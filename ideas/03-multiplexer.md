---
title: "Multiplexer"
status: draft
category: core
description: "Workspaces, splits, floating panes, SSH domains, session persistence"
tags: ["splits", "tabs", "workspaces", "ssh", "session-persistence", "floating-panes", "drawer", "popup", "modal", "pane-types"]
---
# Multiplexer


Built-in. Replaces tmux, Zellij, and screen.

## Hierarchy

```
Workspace → Tabs → Panes (tiled or floating)
```

## Workspaces

Switch entire contexts — "work" vs "personal" vs "infra." Palette: `@work` to switch instantly.

## Pane Types

Every pane is the same thing — a terminal (or surface) with a process. What differs is **how it's presented in the layout**. Six presentation styles:

### Tiled
Standard splits. Divide the main area horizontally or vertically. Resizable by dragging borders or keybinds.

```
┌──────────────────┬─────────────────────┐
│                  │                     │
│   Pane A (65%)   │   Pane B (35%)      │
│                  │                     │
│                  ├─────────────────────┤
│                  │                     │
│                  │   Pane C            │
│                  │                     │
└──────────────────┴─────────────────────┘
```

`Ctrl+\` split right, `Ctrl+-` split down.

### Floating
Overlay on top of the tiled layout. Freely positionable, resizable, can be dragged. Persists when hidden — dismiss with `Esc`, bring back with `Ctrl+F`.

```
┌──────────────────────────────────────────┐
│  Tiled content underneath               │
│                                          │
│      ┌─────────────────────┐             │
│      │  Floating pane      │             │
│      │  (draggable)        │             │
│      │  htop               │             │
│      └─────────────────────┘             │
│                                          │
└──────────────────────────────────────────┘
```

Good for: quick `htop`, a one-off command, a calculator, checking something without disrupting your layout.

### Drawer
Slides in from an edge — bottom, right, or left. Configurable height/width. Toggle open/close with a keybind. Like VS Code's integrated terminal panel or T3 Code's terminal drawer.

```
┌──────────────────────────────────────────┐
│                                          │
│   Main tiled content                     │
│                                          │
│                                          │
├──────────────────────────────────────────┤
│  ▼ Drawer (bottom, 30%)                 │
│  ~/project $ npm run dev                │
│  Server running on :3000                │
└──────────────────────────────────────────┘
```

`Ctrl+J` toggle bottom drawer. `Ctrl+Shift+J` toggle right drawer.

Good for: persistent dev server output, log tailing, a shell you check frequently. The drawer remembers its pane and scroll position when closed.

### Popup
Centered overlay with a backdrop dim. Appears and disappears — designed for quick, focused interactions. Closes on `Esc` or when the process exits.

```
┌──────────────────────────────────────────┐
│  ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  │
│  ░░░░┌──────────────────────┐░░░░░░░░░  │
│  ░░░░│  Popup pane          │░░░░░░░░░  │
│  ░░░░│                      │░░░░░░░░░  │
│  ░░░░│  lazygit             │░░░░░░░░░  │
│  ░░░░│                      │░░░░░░░░░  │
│  ░░░░└──────────────────────┘░░░░░░░░░  │
│  ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  │
└──────────────────────────────────────────┘
```

`:popup lazygit` or a keybind. Configurable size (default: 80% x 80%).

Good for: lazygit, a quick file picker, a confirmation dialog, anything you open-do-close.

### Modal
Like popup but **blocks interaction with everything else** until dismissed. Has a visible border or title bar indicating it requires attention. Used sparingly — for confirmations, approvals, or critical input.

```
┌──────────────────────────────────────────┐
│  ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  │
│  ░░┌────────────────────────────┐░░░░░  │
│  ░░│ ⚠ Agent needs approval     │░░░░░  │
│  ░░│                            │░░░░░  │
│  ░░│ claude wants to modify:    │░░░░░  │
│  ░░│   src/auth/middleware.ts   │░░░░░  │
│  ░░│                            │░░░░░  │
│  ░░│  [Approve]  [Deny]  [Diff] │░░░░░  │
│  ░░└────────────────────────────┘░░░░░  │
│  ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  │
└──────────────────────────────────────────┘
```

Not dismissible with `Esc` — requires an explicit action. Plugins can spawn modals via the pane API when they need user input.

Good for: agent approval prompts, destructive confirmations (`:merge` on a production branch), plugin permission requests.

### Sidebar Pane
A pane pinned to the sidebar area. Not the process dashboard — an actual terminal pane that lives in the sidebar region. Like having a narrow shell always visible on the side.

```
┌──────────────────┬─────────────────────────────┐
│ AGENTS           │                             │
│ ◉ feat/auth  ❓  │                             │
│──────────────────│  Main tiled content          │
│ SIDEBAR PANE     │                             │
│ ~/project $ git  │                             │
│ log --oneline    │                             │
│ a1b2c3d fix auth │                             │
│ e4f5g6h add rate │                             │
│──────────────────│                             │
│ SHELLS           │                             │
│ ● scratch        │                             │
└──────────────────┴─────────────────────────────┘
```

`:sidebar-pane` opens a terminal in the sidebar region. Useful for a git log, a watch command, or anything you want persistently visible in a narrow column.

## How Pane Types Compose

All pane types exist within the same tab. A tab can have:

```
Tab: "dev"
├── Tiled: nvim (left 65%) + dev server (right top) + shell (right bottom)
├── Drawer: bottom 30%, test watcher
├── Floating: htop (hidden, toggle with Ctrl+F)
├── Sidebar pane: git log --oneline -f
└── (Popup/Modal: spawned on demand, not persistent)
```

Tabs themselves live in a workspace. Switch tabs to switch entire layouts. Switch workspaces to switch entire contexts.

```
Workspace → Tab → Pane Types (tiled, floating, drawer, popup, modal, sidebar)
```

## Pane Type in Layouts

Layouts can specify pane types:

```lua
layout.define("dev", {
  tabs = {
    { name = "code", panes = {
      { command = "nvim", type = "tiled", split = "left", size = "65%" },
      { command = "npm run dev", type = "tiled", split = "right" },
      { command = "vitest --watch", type = "drawer", edge = "bottom", size = "30%" },
      { command = "git log --oneline -f", type = "sidebar-pane" },
    }},
  },
})
```

## Pane Type in Plugin API

Plugins specify pane type when creating panes:

```rust
pane.create(PaneOptions {
    type: PaneType::Popup,      // tiled, floating, drawer, popup, modal, sidebar
    command: "lazygit",
    size: (80, 80),             // percentage for popup/modal
    edge: None,                 // for drawer: bottom, right, left
    dismissible: true,          // Esc to close
    title: "Git",
    accessible_label: "Git interface popup",
});
```

## Pane Type Conversion

A pane can change type on the fly without losing state:

```
:pane float          # promote current tiled pane to floating
:pane tile           # dock current floating pane into tiled layout
:pane drawer bottom  # move to bottom drawer
:pane popup          # center as popup
```

The process keeps running, scroll position is preserved, nothing restarts. You're just changing how the pane is presented.

## View Modes

| View | Shortcut | When |
|------|----------|------|
| Focused | Click sidebar entry | Deep work in one pane |
| Split | `Ctrl+\` / `Ctrl+-` | Watch agent + work in shell |
| Grid | `Ctrl+G` | Expose-style overview of all panes, live preview, click to focus |
| Sidebar collapsed | `Ctrl+B` | Icon strip only, max terminal space |
| No sidebar | `Ctrl+B` again | Full screen single pane |

## Session Persistence

On quit or crash, serialize:
- Tab names and order
- Pane layout (splits, sizes, floating positions)
- Working directory per pane
- Scroll buffer (configurable: last N lines)
- Running command per pane (if marked restartable)
- Environment variables

On relaunch: "Restore previous session?" — recreates everything.

## SSH Domains

Define remote hosts in config. One keystroke opens a tab that's an SSH session but looks and behaves like a local tab. Splits within it are also remote.

```lua
ssh_domains = {
  { name = "homelab", host = "proxmox.local", user = "jace" },
  { name = "prod",    host = "prod.example.com" },
}
```

Reconnects automatically on network drop. No tmux-inside-SSH needed.

## Layouts

Declarative, version-controllable, shareable:

```lua
layout.define("dev", {
  tabs = {
    { name = "code", panes = {
      { command = "nvim", split = "left", size = "65%" },
      { split = "right", children = {
        { command = "npm run dev", split = "top" },
        { split = "bottom" },
      }},
    }},
    { name = "git", panes = { { command = "lazygit" } } },
  },
})
```

## Clipboard

Community pain point: clipboard over SSH + multiplexer is universally broken.

Since we own both the terminal and the multiplexer, OSC-52 passthrough works everywhere — no configuration, no tmux hacks. Clipboard passes through splits, SSH domains, everything.

## Discoverability (from Zellij)

Minimal status bar shows available mode switches. Pressing `?` shows a context-sensitive shortcut overlay. Configurable: `status_bar = "auto"` shows for 2 weeks then hides.

## Copy Mode

Modal navigation in scrollback. Enter with `Ctrl+Shift+[`, exit with `Esc` or `y` (yank and exit). This is a core feature, not a plugin — it's part of the multiplexer.

Three keybind presets:

```
copy-mode-keybinds = vim     # default
copy-mode-keybinds = emacs   # for emacs users
copy-mode-keybinds = basic   # arrow keys only, no modal
```

### Vim preset (default)

| Key | Action |
|-----|--------|
| `j/k` | Down/up one line |
| `h/l` | Left/right one character |
| `Ctrl+d/u` | Half-page down/up |
| `gg` / `G` | Top/bottom of scrollback |
| `v` | Start visual (character) selection |
| `V` | Start line-wise selection |
| `Ctrl+v` | Start block (rectangular) selection |
| `y` | Yank selection to clipboard and exit |
| `/` | Search forward |
| `?` | Search backward |
| `n/N` | Next/previous search match |
| `w/b` | Word forward/backward |
| `0` / `$` | Start/end of line |
| `Esc` | Exit copy mode |

### Emacs preset

| Key | Action |
|-----|--------|
| `Ctrl+n/p` | Down/up |
| `Ctrl+f/b` | Forward/back character |
| `Ctrl+v / Alt+v` | Page down/up |
| `Alt+<` / `Alt+>` | Top/bottom |
| `Ctrl+Space` | Start selection |
| `Alt+w` | Copy selection and exit |
| `Ctrl+s/r` | Search forward/backward |
| `Ctrl+g` | Exit |

### What copy mode is not

- Not vim keybindings for typing in the shell (that's your shell's vi mode — `set -o vi`)
- Not vim keybindings for pane/tab switching (that's the multiplexer keybinds)
- Not a full vim emulation — just motions, search, and selection in scrollback

## Pain Points Addressed

- tmux sessions don't persist → native session persistence
- tmux keybinds are arcane → discoverable status bar + command palette
- Clipboard over SSH+mux broken → OSC-52 passthrough everywhere
- Image protocols don't work in tmux → both Kitty + Sixel work through multiplexer
- SSH terminfo not on remote hosts → use xterm-256color, advertise extras via escape queries

## Related Docs

- [Architecture](01-architecture.md) — where the multiplexer sits in the layer stack
- [Sidebar](04-sidebar.md) — pane navigation and process dashboard
- [Memory Management](15-memory-management.md) — scroll buffer strategy (ring + disk archive)
- [Session persistence is part of the multiplexer, serialization format TBD]
- [Platform Support](20-platform-support.md) — Wayland vs X11, clipboard handling per platform
