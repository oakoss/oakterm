---
title: "Theming"
status: draft
category: cross-cutting
description: "Deep customization, TOML format, inheritance, live preview"
tags: ["themes", "toml", "colors", "ui-chrome", "live-preview", "wcag"]
---
# Theming


Users should be able to change anything they want. The theming system is deep — not just 16 ANSI colors, but every visual element the terminal renders.

## Theme File Format

Themes are TOML files in `~/.config/phantom/themes/`:

```toml
# ~/.config/phantom/themes/my-theme.toml
[metadata]
name = "My Custom Theme"
author = "jace"
variant = "dark"   # "dark" or "light" — used for auto-switching

[colors]
# Terminal colors (standard 16)
black          = "#1e1e2e"
red            = "#f38ba8"
green          = "#a6e3a1"
yellow         = "#f9e2af"
blue           = "#89b4fa"
magenta        = "#cba6f7"
cyan           = "#94e2d5"
white          = "#cdd6f4"
bright-black   = "#585b70"
bright-red     = "#f38ba8"
bright-green   = "#a6e3a1"
bright-yellow  = "#f9e2af"
bright-blue    = "#89b4fa"
bright-magenta = "#cba6f7"
bright-cyan    = "#94e2d5"
bright-white   = "#a6adc8"

# Extended (256-color palette overrides are optional)
# color-16 = "#fab387"
# ...

# Semantic colors
foreground     = "#cdd6f4"
background     = "#1e1e2e"
cursor         = "#f5e0dc"
selection-fg   = "#1e1e2e"
selection-bg   = "#f5e0dc"

[ui]
# Tab bar
tab-bar-bg                = "#181825"
tab-bar-fg                = "#6c7086"
tab-active-bg             = "#1e1e2e"
tab-active-fg             = "#cdd6f4"
tab-active-indicator      = "#cba6f7"
tab-inactive-bg           = "#181825"
tab-inactive-fg           = "#6c7086"
tab-inactive-hover-bg     = "#313244"
tab-inactive-hover-fg     = "#cdd6f4"
tab-new-bg                = "#181825"
tab-new-fg                = "#6c7086"

# Sidebar
sidebar-bg                = "#11111b"
sidebar-fg                = "#cdd6f4"
sidebar-section-fg        = "#6c7086"
sidebar-active-bg         = "#1e1e2e"
sidebar-badge-info        = "#89b4fa"
sidebar-badge-warn        = "#f9e2af"
sidebar-badge-error       = "#f38ba8"
sidebar-badge-success     = "#a6e3a1"

# Command palette
palette-bg                = "#1e1e2e"
palette-fg                = "#cdd6f4"
palette-border            = "#313244"
palette-match-fg          = "#f9e2af"   # highlighted matching characters
palette-selected-bg       = "#313244"

# Splits and borders
split-border              = "#313244"
pane-active-border        = "#cba6f7"
pane-inactive-border      = "#313244"
pane-bell-border          = "#f9e2af"   # flash color when bell rings in a pane

# Status bar
status-bar-bg             = "#181825"
status-bar-fg             = "#6c7086"

# Scrollbar
scrollbar-thumb           = "#585b70"
scrollbar-track           = "transparent"

# Search
search-match-bg           = "#f9e2af"
search-match-fg           = "#1e1e2e"
search-selected-bg        = "#fab387"
search-selected-fg        = "#1e1e2e"

# URL hover
url-color                 = "#f5e0dc"

# Marks (for hints mode labels)
mark-1-bg                 = "#89b4fa"
mark-1-fg                 = "#1e1e2e"
mark-2-bg                 = "#cba6f7"
mark-2-fg                 = "#1e1e2e"
mark-3-bg                 = "#74c7ec"
mark-3-fg                 = "#1e1e2e"

# Visual bell
visual-bell               = "#313244"

[window]
# These can be overridden per-theme
opacity              = 1.0
blur                 = false
# opacity-unfocused  = 0.8

[cursor]
style                = "block"    # block, bar, underline
blink                = true
```

## What Users Can Theme

Everything visual — approximately 60+ properties:

| Category | What's customizable |
|----------|-------------------|
| Terminal colors | All 16 ANSI colors, 256-color overrides, fg/bg/cursor/selection |
| Tab bar | Bar bg, active/inactive/hover/new tab states (fg + bg each) |
| Sidebar | Background, foreground, sections, active state, badge colors (4 levels) |
| Command palette | Background, foreground, border, match highlight, selected |
| Splits and borders | Split divider, active/inactive/bell pane borders |
| Status bar | Background, foreground |
| Scrollbar | Thumb and track colors |
| Search | Match and selected-match colors (fg + bg) |
| URL hover | Hover underline color |
| Marks | 3 levels of mark fg/bg (for hints mode) |
| Visual bell | Flash color |
| Window | Opacity, blur, unfocused dimming |
| Cursor | Style, color, text-under-cursor color, blink |

## Bundled Themes

Ship with a solid set so the first-launch experience is good:

- **Default Dark** — our own, passes WCAG AA
- **Default Light** — our own, passes WCAG AA
- **High Contrast Dark** — passes WCAG AAA, bundled for accessibility
- **High Contrast Light** — passes WCAG AAA, bundled for accessibility
- Popular community themes pre-bundled (with attribution):
  - Catppuccin (Mocha, Latte, Frappe, Macchiato)
  - Tokyo Night
  - Dracula
  - Nord
  - Solarized (Dark, Light)
  - Gruvbox (Dark, Light)
  - One Dark / One Light
  - Rosé Pine

## Per-Tab Colors and Titles

Tabs are more than just names — they're visual indicators of state and context.

### Custom tab titles

Set a tab's title manually or let it auto-detect:

```
# Palette
:tab rename "API Server"

# Keybind
Ctrl+Shift+T → rename current tab
```

Tab titles can also be set programmatically by:
- Shell integration (running command becomes the title, idle shows cwd)
- Plugins (agent-manager sets title to branch name)
- The user (double-click tab to rename, persists across sessions)
- OSC escape sequences (programs can set their own title via standard `OSC 0` / `OSC 2`)
- Terminal title (`OSC 0`) and tab title are separate — a program setting the terminal title doesn't overwrite your custom tab name unless you want it to

Config:
```
tab-title-mode = custom          # keep my name, ignore OSC
tab-title-mode = shell           # auto from running command / cwd
tab-title-mode = osc             # let programs set the title
tab-title-mode = auto            # shell integration when idle, OSC when running (default)
```

### Per-tab color coding

Tabs can have individual color overrides — independent of the global theme:

```
┌──────────────────────────────────────────────────────────────┐
│ ● scratch  │ ● API Server  │ ◉ feat/auth  │ ◉ add-tests   │
│            │   🟢           │   🔴          │   🟡           │
└──────────────────────────────────────────────────────────────┘
```

Colors can be set:

**Manually per-tab:**
```
:tab color #a6e3a1         # set current tab's accent color
:tab color reset           # back to theme default
```

**Automatically by plugins:** The agent-manager plugin sets tab colors based on agent state:

```lua
-- Agent manager plugin sets these via pane.metadata
-- The sidebar-ui and tab-bar read the metadata
agent_tab_colors = {
  working     = "theme:sidebar-badge-info",     -- theme's info color
  needs_input = "theme:sidebar-badge-warn",     -- theme's warn color
  done        = "theme:sidebar-badge-success",  -- theme's success color
  error       = "theme:sidebar-badge-error",    -- theme's error color
}
```

The key insight: plugins set **semantic state** (`working`, `needs_input`), and the theme maps those states to colors. If you switch themes, the colors update. If you want custom colors for agent states, override them in config — not in the plugin.

**Automatically by environment detection** (from [Smart Keybinds](19-smart-keybinds.md)):

```lua
environments = {
  { match = { hostname = "prod*" }, tab_color = "#f38ba8", label = "PROD" },
  { match = { hostname = "staging*" }, tab_color = "#f9e2af", label = "STAGING" },
}
```

### Terminal title (window title)

The window title is separate from tab titles:

```
window-title = Phantom             # static
window-title = {cwd}               # dynamic: working directory
window-title = {tab} — {cwd}      # tab name + cwd
window-title = Phantom — {tab}    # default
```

Plugins and programs can update the window title via `OSC 0` / `OSC 2`. This is controlled by the same `tab-title-mode` config — when set to `custom`, programs can't change the window title either.

## Theme Picker with Live Preview

From the palette:

```
Cmd+Shift+P → :theme

┌──────────────────────────────────────────────────┐
│  theme:  Search themes                           │
├──────────────────────────────────────────────────┤
│  ● Catppuccin Mocha        dark     ✓ current   │
│  ○ Catppuccin Latte        light                │
│  ○ Tokyo Night             dark                  │
│  ○ Dracula                 dark                  │
│  ○ Nord                    dark                  │
│  ○ High Contrast Dark      dark     a11y        │
│  ○ My Custom Theme         dark     local       │
│  ...                                             │
│                                                  │
│  [Browse Community Themes]                       │
└──────────────────────────────────────────────────┘
```

Arrow keys to preview live — the terminal updates instantly as you move through the list. Enter to apply. Esc to cancel (reverts to previous theme).

## Community Themes

Themes are **data packages**, not WASM plugins. They're just `.toml` files — no code execution, no permissions needed.

Distributable as:
- Single `.toml` files (drop in `~/.config/phantom/themes/`)
- Via the registry as a data package: `phantom theme install catppuccin` (downloads the TOML files, no WASM involved)
- Via a dedicated theme gallery on the website (browse, preview, one-click install)

Themes use the same registry infrastructure as plugins but are a different package type (`type = "theme"` in the manifest). They require no capabilities and run no code.

## Theme Authoring

`phantom theme create` scaffolds a new theme file with all fields documented. `phantom theme validate my-theme.toml` checks for missing fields, contrast issues, and accessibility.

```
$ phantom theme validate my-theme.toml

✓ All required colors defined
✓ Foreground/background contrast: 7.2:1 (WCAG AAA)
⚠ Yellow on background contrast: 3.8:1 (below WCAG AA 4.5:1)
  → Consider darkening yellow or lightening background
✓ Selection colors readable
✓ UI chrome colors complete
✓ Badge colors distinguishable
```

## Theme Inheritance

Themes can extend other themes and override specific values:

```toml
[metadata]
name = "My Catppuccin Tweaks"
extends = "catppuccin-mocha"

[colors]
# Only override what you want to change
cursor = "#ff0000"

[ui]
pane-active-border = "#ff0000"
```

## Import from Other Terminals

```
phantom theme import ghostty ~/.config/ghostty/config
phantom theme import kitty ~/.config/kitty/kitty.conf
phantom theme import alacritty ~/.config/alacritty/alacritty.toml
phantom theme import iterm2 ~/my-profile.itermcolors
```

Converts the terminal's color scheme to a Phantom theme file. Maps what it can, leaves UI chrome at defaults.

## Related Docs

- [Accessibility](17-accessibility.md) — WCAG contrast requirements for themes
- [Configuration](09-config.md) — `theme-dark` / `theme-light` / `appearance` settings
- [Renderer](02-renderer.md) — opacity and blur per-theme
- [Conventions](30-conventions.md) — theme file and display name conventions
