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
# Terminal chrome
tab-bar-bg           = "#181825"
tab-bar-fg           = "#6c7086"
tab-active-bg        = "#1e1e2e"
tab-active-fg        = "#cdd6f4"
tab-active-indicator = "#cba6f7"

# Sidebar
sidebar-bg           = "#11111b"
sidebar-fg           = "#cdd6f4"
sidebar-section-fg   = "#6c7086"
sidebar-active-bg    = "#1e1e2e"
sidebar-badge-info   = "#89b4fa"
sidebar-badge-warn   = "#f9e2af"
sidebar-badge-error  = "#f38ba8"
sidebar-badge-success= "#a6e3a1"

# Command palette
palette-bg           = "#1e1e2e"
palette-fg           = "#cdd6f4"
palette-border       = "#313244"
palette-match-fg     = "#f9e2af"   # highlighted matching characters
palette-selected-bg  = "#313244"

# Splits and borders
split-border         = "#313244"
pane-active-border   = "#cba6f7"

# Status bar
status-bar-bg        = "#181825"
status-bar-fg        = "#6c7086"

# Scrollbar
scrollbar-thumb      = "#585b70"
scrollbar-track      = "transparent"

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

Everything visual:

| Category | What's customizable |
|----------|-------------------|
| Terminal colors | All 16 ANSI colors, 256-color overrides, fg/bg/cursor/selection |
| UI chrome | Tab bar, sidebar, palette, status bar, split borders |
| Badges/indicators | Info, warn, error, success colors for sidebar badges |
| Window | Opacity, blur, unfocused dimming |
| Cursor | Style, color, blink |
| Scrollbar | Thumb and track colors |
| Pane borders | Active/inactive border colors |

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
