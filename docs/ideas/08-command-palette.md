---
title: 'Command Palette'
status: draft
category: core
description: 'Unified fuzzy launcher with prefix filters'
tags: ['ui', 'fuzzy-search', 'commands', 'keybinds']
---

# Command Palette

`Cmd+Shift+P` (macOS) / `Ctrl+Shift+P` (Linux/Windows) opens a unified fuzzy launcher. One palette, everything reachable.

## Design

Combines Ghostty's action launcher with Warp's session switcher. The palette is the single entry point for every action in the terminal — core features, plugin commands, settings, workspaces, layouts, and SSH connections.

No prefix = fuzzy search across all categories. Type to filter instantly.

## Prefix Filters

| Prefix | Scopes to                     | Example                               |
| ------ | ----------------------------- | ------------------------------------- |
| `>`    | Terminal actions              | `> split` shows split commands        |
| `@`    | Workspaces and sessions       | `@ work` switches to "work" workspace |
| `#`    | Layouts                       | `# dev` loads the "dev" layout        |
| `ssh:` | SSH domains                   | `ssh: homelab` connects               |
| `:`    | Settings (live toggle)        | `: font` shows font settings          |
| `?`    | Natural language command help | `? find large files`                  |

## Default View

When opened with no input, shows recent/relevant items grouped:

```text
┌─────────────────────────────────────────────────┐
│  >                                              │
├─────────────────────────────────────────────────┤
│  Sessions                                       │
│  >_ finance-tracker  main  :3000   Current      │
│  >_ api-server       feat/auth  :8080  2m       │
│  >_ dotfiles         main              15m      │
│                                                 │
│  Actions                                        │
│     Split Pane Right         Ctrl+\             │
│     New Floating Pane        Ctrl+F             │
│     Toggle Sidebar           Ctrl+B             │
│     Connect SSH Domain...    Ctrl+Shift+S       │
│                                                 │
│  Layouts                                        │
│     dev (3 tabs, 5 panes)                       │
│     monitoring (2 tabs, 4 panes)                │
└─────────────────────────────────────────────────┘
```

## Registered Commands

Core and plugins both register commands. The palette doesn't distinguish between them — they all appear in the same search results.

### Core Commands

| Command           | Action                         |
| ----------------- | ------------------------------ |
| `:health`         | Run full health check          |
| `:debug`          | Open debug overview            |
| `:debug memory`   | Memory attribution             |
| `:debug perf`     | Toggle performance overlay     |
| `:debug plugins`  | Plugin performance             |
| `:debug input`    | Input key inspector            |
| `:debug escape`   | Escape sequence inspector      |
| `:debug security` | Security status                |
| `:settings`       | Open settings palette          |
| `:keybinds`       | Open keybind editor            |
| `:theme`          | Theme picker with live preview |
| `:update`         | Install available update       |
| `:layout <name>`  | Load a saved layout            |

### Plugin Commands (registered by bundled plugins)

| Command                | Plugin          | Action                            |
| ---------------------- | --------------- | --------------------------------- |
| `:agent <provider>`    | agent-manager   | Launch an agent in a new worktree |
| `:agents`              | agent-manager   | List all agents with status       |
| `:merge`               | agent-manager   | Merge current agent's worktree    |
| `:diff`                | agent-manager   | Show current agent's changes      |
| `:harpoon`             | harpoon         | Open harpoon quick menu           |
| `:mark`                | harpoon         | Add current pane to harpoon       |
| `:broadcast`           | smart-keybinds  | Start input broadcast             |
| `:service start <cmd>` | service-monitor | Add a service to the sidebar      |
| `:watch <cmd>`         | watcher         | Add a watcher to the sidebar      |
| `:plugins`             | core            | Open plugin manager               |

### Community Plugin Commands (examples)

| Command               | Plugin         | Action                    |
| --------------------- | -------------- | ------------------------- |
| `:docker up`          | docker-manager | Start docker compose      |
| `:docker logs <name>` | docker-manager | Tail container logs       |
| `:browse <url>`       | browser-lite   | Open URL in floating pane |
| `:k8s context <name>` | k8s-pods       | Switch kubectl context    |

## Plugin Integration

Plugins register commands via the `palette.command` API primitive:

```rust
palette.register(PaletteCommand {
    name: ":docker logs",
    description: "View logs for a Docker container",
    accessible_description: "View logs for a Docker container",
    action: |args| { /* ... */ },
    completions: |partial| { /* return container names */ },
});
```

Plugins can also register:

- **Palette sections** — custom groupings in the default view
- **Keybindings** — shortcut keys for their commands
- **Prefix filters** — custom prefixes (e.g., `docker:` to scope to Docker commands)
- **Dynamic completions** — argument-aware suggestions (`:docker logs` + Tab shows container names)

## Chained Actions

Some palette commands chain multiple operations:

| Command                 | What it does                                   |
| ----------------------- | ---------------------------------------------- |
| `:pr`                   | Commit + push + create PR (from T3 Code)       |
| `:merge`                | Commit + merge worktree + cleanup + close pane |
| `:workspace new <name>` | Create worktree + open tab + cd into it        |

These run sequentially, showing progress. If any step fails, the chain stops and shows the error.

## Accessibility

The palette is fully keyboard-navigable and screen reader accessible:

- Arrow keys to move through results
- Enter to select
- Esc to close
- Screen reader announces: result count, selected item, item description
- High-contrast mode respects palette colors from theme

## Configuration

Palette behavior is configurable:

```ini
# Flat config
palette-show-recent = true
palette-max-results = 20
palette-position = center
```

```lua
-- Lua config
palette = {
  show_recent = true,
  max_results = 20,
  position = "center",  -- "center" or "top"
}
```

## Related Docs

- [Plugin System](06-plugins.md) — `palette.command` API primitive
- [Configuration](09-config.md) — keybind configuration for palette shortcuts
- [Accessibility](17-accessibility.md) — screen reader requirements for UI elements
- [Smart Keybinds](19-smart-keybinds.md) — hints mode and broadcast registered via palette
