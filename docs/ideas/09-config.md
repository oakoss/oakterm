---
title: 'Configuration'
status: reviewing
category: cross-cutting
description: 'First launch setup, settings palette, Lua config, dark/light themes'
tags: ['config', 'lua', 'settings-palette', 'first-launch', 'dark-mode']
---

# Configuration

Configuration should be dead simple to start and powerful when you need it. You shouldn't need to read docs to change your font.

> **Note:** [ADR 0005](../adrs/0005-lua-sandboxed-config.md) resolved configuration as Lua-only with snake_case keys. Configuration uses sandboxed Lua 5.4 with a single entry point (`config.lua`) and `require()` for multi-file organization. No `io`, `os`, `package`, or `debug` standard library access. The Lua/WASM boundary is defined by capabilities: Lua handles config values and event reactions with no side effects beyond the terminal; WASM plugins handle anything requiring I/O, network, or persistent storage.

## First Launch Experience

On first launch with no config file, OakTerm works with sensible defaults. But it also offers an interactive setup:

```text
Welcome to OakTerm.

Let's set up the basics. You can change any of this later.

Font:         [JetBrains Mono    ▾]    ← dropdown lists installed monospace fonts
Size:         [14                  ]
Appearance:   [○ System  ○ Light  ● Dark]
Dark Theme:   [○ ○ ○ ○ ○ ○ ○ ○ ○ ○]    ← visual theme previews
              catppuccin-mocha ✓
Light Theme:  [○ ○ ○ ○ ○ ○ ○ ○ ○ ○]
              catppuccin-latte ✓

Plugins:
  ☑ Smart autocomplete (context engine)
  ☑ Agent management (sidebar + worktrees)
  ☑ Service monitoring (dev servers, docker)
  ☐ Browser (text-mode web browsing)

                          [Save & Start]
```

This writes a config file for you. No command line flags, no manual file creation.

## Settings Palette

After setup, every setting is changeable from inside the terminal via the command palette:

```text
Cmd+Shift+P → :settings

┌──────────────────────────────────────────────────┐
│  settings:  Search settings                      │
├──────────────────────────────────────────────────┤
│  Font Family          JetBrains Mono             │
│  Font Size            14                         │
│  Font Ligatures       ✓ enabled                  │
│  Appearance           System (auto)              │
│  Dark Theme           catppuccin-mocha           │
│  Light Theme          catppuccin-latte           │
│  Cursor Style         block                      │
│  Scrollback Lines     10000                      │
│  Status Bar           auto                       │
│  Sidebar              left                       │
│  ...                                             │
└──────────────────────────────────────────────────┘
```

- Search to filter
- Click or Enter to edit inline
- Changes apply immediately (live preview)
- Changes write back to the config file automatically

**Theme preview in the palette** — when browsing themes, the terminal updates live as you arrow through the list. Pick one and it sticks. Like VS Code's theme picker.

**Font preview in the palette** — same idea. Arrow through installed monospace fonts, see the change live, pick one.

## Config File

`~/.config/oakterm/config.lua` — Lua-only configuration, from simple values to logic and conditionals.

`appearance` accepts: `"system"` (follows OS), `"dark"`, or `"light"`. When set to `"system"`, the terminal listens for OS appearance changes and switches between `theme_dark` and `theme_light` instantly. No restart, no flicker. A single `theme` value works as a shorthand — it sets both dark and light to the same value and locks appearance to that theme regardless of OS setting.

```lua
-- Dynamic font size based on display
if display.scale > 1 then
  font_size = 13
else
  font_size = 15
end

-- SSH domains
ssh_domains = {
  { name = "homelab", host = "proxmox.local", user = "jace" },
}

-- Layouts
layout.define("dev", {
  tabs = {
    { name = "code", panes = {
      { command = "nvim", split = "left", size = "65%" },
      { command = "npm run dev", split = "right" },
    }},
  },
})

-- Plugins
plugins = {
  ["agent-manager"]  = { enabled = true },
  ["context-engine"] = { enabled = true, ai = { backend = "ollama" } },
}

-- Project detection for auto-populating sidebar
project.detect = {
  { file = "docker-compose.yml", services = { "docker compose up -d" } },
  { file = "package.json", script = "dev", services = { "npm run dev" } },
  { file = "vitest.config.ts", watchers = { "vitest --watch" } },
}

-- Workspace setup scripts
workspace.on_create = function(ws)
  if ws:has_file("package.json") then ws:run("pnpm install") end
  if ws:has_file(".env.example") and not ws:has_file(".env") then
    ws:run("cp .env.example .env")
  end
end
```

### Project-Level Config

`<project>/.oakterm/config.lua` — project-specific overrides:

```lua
-- .oakterm/config.lua in a monorepo
font_size = 13
theme = "github-dark"
```

This lets teams share terminal config per-repo without touching personal settings.

## Plugin Settings

Plugins register their own settings, which appear in the same palette:

```text
Cmd+Shift+P → :settings agent

┌──────────────────────────────────────────────────┐
│  settings:  agent                                │
├──────────────────────────────────────────────────┤
│  Agent: Default Provider    claude               │
│  Agent: Auto Worktree       ✓ enabled            │
│  Agent: Notify on Done      ✓ enabled            │
│  Agent: Notify on Approval  ✓ enabled            │
│  Agent: Setup Script        pnpm install         │
└──────────────────────────────────────────────────┘
```

Plugin settings live in the same config file, namespaced as Lua tables:

```lua
plugins = {
  ["agent-manager"] = {
    default_provider = "claude",
    auto_worktree = true,
  },
  ["context-engine"] = {
    ai_backend = "ollama",
    ai_model = "codellama:7b",
  },
}
```

## Keybind Configuration

Keybinds are settings too — searchable and editable from the palette:

```text
Cmd+Shift+P → :keybinds

┌──────────────────────────────────────────────────┐
│  keybinds:  split                                │
├──────────────────────────────────────────────────┤
│  Split Pane Right      Ctrl+\                    │
│  Split Pane Down       Ctrl+-                    │
│  Split Pane Float      Ctrl+F                    │
│                                 [Edit] [Reset]   │
└──────────────────────────────────────────────────┘
```

Click Edit → press your desired key combo → done. Written to config.

In `config.lua`:

```lua
keybinds = {
  { key = "ctrl+\\", action = "split-right" },
  { key = "ctrl+-", action = "split-down" },
  { key = "ctrl+f", action = "split-float", when = "not_floating" },
  { key = "ctrl+b", action = "toggle-sidebar" },
  { key = "ctrl+g", action = "grid-view" },
}
```

## Migration

- Reads Ghostty config format automatically. Warns about unsupported keys, maps the rest.
- `oakterm migrate ghostty` — converts a Ghostty config to OakTerm format
- `oakterm migrate kitty` — same for Kitty
- `oakterm migrate wezterm` — best-effort Lua→Lua translation

## Naming Convention

Consistency across all config surfaces:

| Context                 | Convention | Example                                              |
| ----------------------- | ---------- | ---------------------------------------------------- |
| Config keys             | snake_case | `font_size`, `ssh_domains`, `shell_integration`      |
| Config plugin namespace | table      | `plugins["agent-manager"].default_provider`          |
| Keybind actions         | kebab-case | `split-right`, `toggle-sidebar`, `copy-or-interrupt` |
| CLI flags               | kebab-case | `--log-level`, `--log-filter`                        |

All configuration uses Lua with snake_case keys. See [ADR 0005](../adrs/0005-lua-sandboxed-config.md).

## Design Principles

1. **Zero-config is valid.** The terminal works perfectly with no config file.
2. **The palette is the settings UI.** No separate preferences window, no JSON editing required.
3. **Live preview everything.** Themes, fonts, colors — see the change before committing.
4. **Write to file, not magic state.** Every change the palette makes is written to the config file. `cat config` always shows the truth.
5. **Progressive complexity.** Simple values → conditionals → event handlers → project overrides. You only reach for the next level when you need it.

## Related Docs

- [Theming](22-theming.md) — theme file format (TOML)
- [Conventions](30-conventions.md) — naming conventions for config keys
- [Plugin System](06-plugins.md) — plugin settings namespace
- [Platform Support](20-platform-support.md) — platform-aware keybind defaults
