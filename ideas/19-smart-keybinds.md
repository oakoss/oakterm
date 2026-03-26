# Smart Keybinds

Context-aware keybindings that do the right thing based on terminal state.

## Smart Ctrl+C / Ctrl+V

The most requested quality-of-life improvement across terminal discussions.

**Ctrl+C:**
- Text selected → copy to clipboard
- Nothing selected → send SIGINT (interrupt process)
- No more Ctrl+Shift+C for copying

**Ctrl+V:**
- Always paste from clipboard
- No more Ctrl+Shift+V

This matches every other application on the system. Ghostty calls these "performable" keybindings — the action depends on whether it can be performed (text is selected) or falls through to the default (send the raw keycode).

### Platform behavior

| Platform | Copy | Paste | Interrupt |
|----------|------|-------|-----------|
| macOS | Cmd+C (always copies) | Cmd+V (always pastes) | Ctrl+C (always SIGINT) |
| Linux/Windows | Ctrl+C (smart: copy if selected, SIGINT if not) | Ctrl+V (always pastes) | Ctrl+C with no selection |

On macOS, this is already natural — Cmd and Ctrl are separate keys. Smart behavior is only needed on Linux/Windows where Ctrl serves double duty.

```lua
keybinds = {
  -- Linux/Windows default
  { key = "ctrl+c", action = "copy-or-interrupt" },
  { key = "ctrl+v", action = "paste" },
  -- macOS default (set automatically, shown for clarity)
  -- { key = "super+c", action = "copy" },
  -- { key = "super+v", action = "paste" },
  -- { key = "ctrl+c", action = "sigint" },
}
```

Disable with `smart-keybinds = false` if you want traditional terminal behavior (Ctrl+Shift+C to copy on Linux/Windows).

## Hints Mode

Press a hotkey, every actionable pattern on screen gets a short label. Type the label to act on it. Like Vimium for the browser.

```
Ctrl+Shift+H → activate hints

┌────────────────────────────────────────────────┐
│ ~/project $ git log --oneline                  │
│ [a] a1b2c3d Fix auth flow                      │
│ [b] e4f5g6h Add rate limiter                   │
│                                                │
│ ~/project $ cat README.md                      │
│ See [c] https://docs.example.com/setup         │
│ Report bugs at [d] https://github.com/org/repo │
│                                                │
│ ~/project $ ls src/                             │
│ [e] components/  [f] lib/  [g] utils/          │
└────────────────────────────────────────────────┘

Type 'c' → opens URL in browser
Type 'e' → inserts 'components/' at cursor
```

### Built-in Pattern Matchers
- URLs (http/https)
- File paths (relative and absolute)
- Git commit hashes
- IP addresses
- Email addresses

### Custom Patterns via Config
```lua
hints = {
  patterns = {
    { regex = "JIRA-\\d+", action = "open", url = "https://jira.example.com/browse/{match}" },
    { regex = "PR #(\\d+)", action = "open", url = "https://github.com/org/repo/pull/{1}" },
    { regex = "[a-f0-9]{7,40}", action = "copy" },  -- git hashes
  },
}
```

### Actions Per Match
| Action | What it does |
|--------|-------------|
| `open` | Open in default browser (URLs) or editor (file paths) |
| `copy` | Copy to clipboard |
| `insert` | Insert at cursor position in the prompt |
| `run` | Execute a command with the match as argument |

### Plugin Extensible
Plugins can register additional hint patterns and actions via the context engine API.

## Input Broadcast

Type in multiple panes simultaneously — same command on multiple servers at once.

```
Cmd+Shift+P → :broadcast

┌──────────────────────────────────────────────────┐
│  broadcast:  Select panes                        │
├──────────────────────────────────────────────────┤
│  ☑ prod-server-1                                 │
│  ☑ prod-server-2                                 │
│  ☑ prod-server-3                                 │
│  ☐ staging-server                                │
│  ☐ scratch                                       │
│                                              [Start]│
└──────────────────────────────────────────────────┘
```

When active:
- A visual indicator on all broadcasting panes (colored border or badge)
- Everything you type goes to all selected panes
- `Ctrl+Shift+B` to toggle broadcast on/off quickly
- `:broadcast stop` to end

## Environment-Aware Pane Coloring

Visually distinguish dangerous from safe environments. The terminal detects the environment and applies a color tint or border.

```lua
environments = {
  { match = { hostname = "prod*" },    border_color = "#ff4444", label = "PROD" },
  { match = { hostname = "staging*" }, border_color = "#ffaa00", label = "STAGING" },
  { match = { env = "DOCKER_HOST" },   border_color = "#4488ff", label = "DOCKER" },
  { match = { cwd = "*/production/*" }, border_color = "#ff4444" },
}
```

A pane connected to production gets a red left border and a small "PROD" label. You never accidentally run `rm -rf` in the wrong environment because it doesn't look like your local shell.

Works with:
- SSH domain connections (hostname matching)
- Environment variables
- Working directory patterns
- Container detection

## Quake/Dropdown Mode (Plugin)

Global hotkey slides the terminal from the top of the screen. This is a plugin, not core — but the core provides the window management primitives it needs.

Core provides:
- `window.position` — set window position and size
- `window.always_on_top` — keep above other windows
- `window.animate` — slide/fade transitions
- `global_hotkey.register` — system-wide hotkey registration

The quake plugin uses these primitives:

```lua
-- Plugin: quake-mode
plugins = {
  ["quake-mode"] = {
    enabled = true,
    hotkey = "ctrl+`",
    height = "40%",       -- percentage of screen
    position = "top",     -- top, bottom
    animation = "slide",  -- slide, fade, instant
    monitor = "current",  -- which monitor
  },
}
```

The plugin is bundled but disabled by default. Enable it and set your hotkey.

### Why Plugin, Not Core
- Not everyone wants it — it's a specific workflow preference
- The implementation is just window positioning + animation + a global hotkey
- Making it a plugin proves the core primitives are powerful enough
- Someone could write a "spotlight mode" (centered floating terminal) using the same primitives

## Related Docs

- [Configuration](09-config.md) — keybind syntax and naming convention
- [Plugin System](06-plugins.md) — Window and Pane Query primitives used by quake mode, broadcast, env coloring
- [Command Palette](08-command-palette.md) — `:broadcast` and other commands registered via palette
- [Platform Support](20-platform-support.md) — platform-specific keybind behavior (Cmd vs Ctrl)
- [Accessibility](17-accessibility.md) — hints mode as keyboard alternative to clicking
