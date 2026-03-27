---
title: 'Harpoon-Style Pane Bookmarks'
status: draft
category: plugin
description: 'Pane bookmarks — Ctrl+1-6 direct jump, editable list'
tags: ['navigation', 'bookmarks', 'pane-switching', 'muscle-memory']
---

# Harpoon-Style Pane Bookmarks

Inspired by ThePrimeagen's Harpoon for Neovim. A small, ordered, persistent list of panes you jump to by index — like speed dial, not a contact list.

## The Problem

You have 8 panes open — 2 agents, a dev server, a test watcher, 2 shells, docker logs, and a scratch terminal. You constantly switch between 3-4 of them. Today that means:

- Clicking the sidebar
- Cycling through tabs
- Using the command palette to search

All of these require visual confirmation — you look, find, click. Harpoon replaces that with **deterministic muscle memory**.

## How It Works

### Mark

`Ctrl+Shift+M` — add the current pane to your harpoon list. It fills the first empty slot.

### Jump

Direct-jump by index, no prefix key needed:

```text
Ctrl+1 → slot 1
Ctrl+2 → slot 2
Ctrl+3 → slot 3
Ctrl+4 → slot 4
```

That's it. Ctrl+1 always goes to the same pane. After 5 minutes, your fingers know: "my editor is 1, the agent is 2, dev server is 3, shell is 4."

### Quick Menu

`Ctrl+Shift+H` — toggle a floating overlay showing the harpoon list:

```text
┌──────────────────────────────────────┐
│  Harpoon                             │
├──────────────────────────────────────┤
│  1  ● scratch       ~/project       │
│  2  ◉ feat/auth     claude  ❓       │
│  3  ▶ next dev      :3000           │
│  4  👁 vitest        14/14 passing   │
│  5  (empty)                          │
│  6  (empty)                          │
│                                      │
│  [Enter] jump  [d] remove  [↕] move │
└──────────────────────────────────────┘
```

- Arrow keys to navigate, Enter to jump
- `d` to remove an entry
- Drag or `Shift+↑/↓` to reorder
- The list is editable — same UX as Harpoon's quick menu

### Automatic Cleanup

When a pane closes (agent merged, process exited), its harpoon slot becomes empty. The next `:mark` fills the empty slot. Slots don't collapse — if slot 2 closes, Ctrl+2 does nothing until you fill it again. This preserves muscle memory.

## Per-Workspace Scoping

Each workspace has its own harpoon list. Switch workspaces, switch harpoon context. The "work" workspace remembers your 4 panes, the "personal" workspace remembers different ones.

Harpoon lists persist across sessions — part of the session serialization.

## What Makes This Different From Tab Switching

| Feature      | Tab switching (Cmd+1-9)                                             | Harpoon                          |
| ------------ | ------------------------------------------------------------------- | -------------------------------- |
| Scope        | All tabs by position                                                | Curated subset you chose         |
| Stability    | Changes when you open/close/reorder tabs                            | Stable until you modify the list |
| Mental model | "My agent is the 3rd tab... wait, I opened a new one, now it's 4th" | "My agent is always Ctrl+2"      |
| Size         | Grows with every tab                                                | Fixed at 4-6 slots               |
| Persistence  | Lost on restart (unless session restore)                            | Persisted, survives restarts     |

## Configuration

```lua
-- In config.lua
plugins["harpoon"] = {
  slots = 6,
  per_workspace = true,
  auto_remove_closed = true,
  preserve_scroll_position = true,
}
```

## Implementation: Plugin

Harpoon is a bundled plugin, not core. It uses:

- `pane.metadata` — read pane info for the list display
- `pane.focus` — switch to a pane by ID
- `palette.command` — register `:harpoon` and `:mark`
- `hook.on_pane_close` — auto-remove closed panes
- `notify` — visual feedback on mark/jump
- Key registration for the direct-jump bindings

The core provides everything it needs. Someone could write an alternative (e.g., frecency-based auto-harpoon that learns your patterns) using the same primitives.

## Prior Art

- **Harpoon** (Neovim) — the original, for file bookmarks
- **tmux-harpoon** — port for tmux sessions (Alt+Q/W/E/R to jump)
- **Zellij Harpoon** — WASM plugin for Zellij panes
- **Tuxmux** — Rust tmux manager with a "jump list" inspired by Harpoon

No terminal emulator has this built in. Closest is Cmd+1-9 for tab switching, which is positional (fragile) rather than bookmarked (stable).

## Related Docs

- [Plugin System](06-plugins.md) — `pane.list`, `pane.focus`, `storage` APIs
- [Sidebar](04-sidebar.md) — complements sidebar for quick navigation
- [Multiplexer](03-multiplexer.md) — workspace scoping for harpoon lists
- [Configuration](09-config.md) — keybind configuration
