# Command Palette

`Cmd+Shift+P` opens a unified fuzzy launcher. One palette, everything reachable.

## Design

Combines Ghostty's action launcher with Warp's session switcher. No prefix = fuzzy search across all categories.

## Prefix Filters

| Prefix | Scopes to |
|--------|-----------|
| `>` | Terminal actions (split, new tab, toggle sidebar) |
| `@` | Workspaces and sessions |
| `#` | Layouts |
| `ssh:` | SSH domains |
| `:` | Settings (live toggle) |
| `?` | Natural language command help |

## Default View

```
┌─────────────────────────────────────────────────┐
│  >                                              │
├─────────────────────────────────────────────────┤
│  Sessions                                       │
│  >_ finance-tracker  main  :3000   Current      │
│  >_ api-server       feat/auth  :8080  2m       │
│  >_ dotfiles         main              15m      │
│                                                 │
│  Actions                                        │
│     Split Pane Right         Ctrl+Shift+R       │
│     New Floating Pane        Ctrl+F             │
│     Toggle Sidebar           Ctrl+B             │
│     Connect SSH Domain...    Ctrl+Shift+S       │
│                                                 │
│  Layouts                                        │
│     dev (3 tabs, 5 panes)                       │
│     monitoring (2 tabs, 4 panes)                │
└─────────────────────────────────────────────────┘
```

## Plugin Integration

Plugins register their own commands and palette sections:
- Agent plugin: `:agent`, `:merge`, `:diff`, `:agents`
- Docker plugin: `:docker up`, `:docker logs nginx`
- Service plugin: `:service start`, `:service restart`

## Chained Git Actions (from T3 Code)

`:pr` in the palette runs commit + push + PR creation as one flow. Not a GUI button — a command.
