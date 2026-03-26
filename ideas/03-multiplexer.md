---
title: "Multiplexer"
status: draft
category: core
description: "Workspaces, splits, floating panes, SSH domains, session persistence"
tags: ["splits", "tabs", "workspaces", "ssh", "session-persistence", "floating-panes"]
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

- **Tiled** — standard splits, resizable
- **Floating** — overlay on top of tiled layout. `Ctrl+F` to spawn, `Esc` to dismiss. Persists when hidden. Good for quick htop, git diff, one-off commands.

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

## Vi-like Copy Mode (from Contour)

Modal navigation in scrollback — vi keybindings for search, selection, and yanking. Not a shell feature, it's native to the terminal.

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
