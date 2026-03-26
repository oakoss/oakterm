---
title: 'Configuration'
status: draft
category: cross-cutting
description: 'First launch setup, settings palette, flat + Lua, dark/light themes'
tags: ['config', 'lua', 'settings-palette', 'first-launch', 'dark-mode']
---

# Configuration

Configuration should be dead simple to start and powerful when you need it. You shouldn't need to read docs to change your font.

## First Launch Experience

On first launch with no config file, Phantom works with sensible defaults. But it also offers an interactive setup:

```text
Welcome to Phantom.

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

## Two Tiers of Config Files

### Flat Config (basics)

`~/.config/phantom/config` — key-value pairs, no ceremony:

```ini
font-family = JetBrains Mono
font-size = 14
font-ligatures = true
font-fallbacks = Symbols Nerd Font, Apple Color Emoji
appearance = system
theme-dark = catppuccin-mocha
theme-light = catppuccin-latte
cursor-style = block
scrollback-lines = 10000
```

`appearance` accepts: `system` (follows OS), `dark`, or `light`. When set to `system`, the terminal listens for OS appearance changes and switches between `theme-dark` and `theme-light` instantly. No restart, no flicker.

Single `theme` still works as a shorthand — it sets both dark and light to the same value and locks appearance to that theme regardless of OS setting.

Familiar to Ghostty users. The settings palette reads and writes this file.

### Lua Config (programmable)

`~/.config/phantom/config.lua` — for when you need logic, conditionals, or dynamic behavior:

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

### Precedence

Lua config takes priority if both exist. The flat config is syntactic sugar — it maps 1:1 to Lua settings.

### Project-Level Config

`<project>/.phantom/config` or `<project>/.phantom/config.lua` — project-specific overrides:

```ini
# .phantom/config in a monorepo
font-size = 13
theme = github-dark
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

Plugin settings live in the same config file, namespaced:

```ini
# In flat config
agent.default-provider = claude
agent.auto-worktree = true
context-engine.ai-backend = ollama
context-engine.ai-model = codellama:7b
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

In the flat config file:

```ini
keybind = ctrl+\ = split-right
keybind = ctrl+- = split-down
keybind = ctrl+f = split-float
keybind = ctrl+b = toggle-sidebar
keybind = ctrl+g = grid-view
```

In Lua (for conditionals):

```lua
keybinds = {
  { key = "ctrl+\\", action = "split-right" },
  { key = "ctrl+-", action = "split-down" },
  { key = "ctrl+f", action = "split-float", when = "not_floating" },
}
```

## Migration

- Reads Ghostty config format automatically. Warns about unsupported keys, maps the rest.
- `phantom migrate ghostty` — converts a Ghostty config to Phantom format
- `phantom migrate kitty` — same for Kitty
- `phantom migrate wezterm` — best-effort Lua→Lua translation

## Naming Convention

Consistency across all config surfaces:

| Context                      | Convention        | Example                                               |
| ---------------------------- | ----------------- | ----------------------------------------------------- |
| Flat config keys             | kebab-case        | `font-family`, `theme-dark`, `scrollback-lines`       |
| Flat config plugin namespace | `plugin-name.key` | `agent.default-provider`, `context-engine.ai-backend` |
| Lua config keys              | snake_case        | `font_size`, `ssh_domains`, `shell_integration`       |
| Lua config plugin namespace  | table             | `plugins["agent-manager"].default_provider`           |
| Keybind actions              | kebab-case        | `split-right`, `toggle-sidebar`, `copy-or-interrupt`  |
| CLI flags                    | kebab-case        | `--log-level`, `--log-filter`                         |

The flat config and Lua config map 1:1. `font-family` in flat = `font_family` in Lua. The settings palette handles the translation.

## Design Principles

1. **Zero-config is valid.** The terminal works perfectly with no config file.
2. **The palette is the settings UI.** No separate preferences window, no JSON editing required.
3. **Live preview everything.** Themes, fonts, colors — see the change before committing.
4. **Write to file, not magic state.** Every change the palette makes is written to the config file. `cat config` always shows the truth.
5. **Progressive complexity.** Flat file → Lua → project overrides. You only reach for the next level when you need it.

## Related Docs

- [Theming](22-theming.md) — theme file format (TOML)
- [Conventions](30-conventions.md) — naming conventions for config keys
- [Plugin System](06-plugins.md) — plugin settings namespace
- [Platform Support](20-platform-support.md) — platform-aware keybind defaults
