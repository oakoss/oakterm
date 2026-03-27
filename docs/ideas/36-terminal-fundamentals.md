---
title: 'Terminal Fundamentals'
status: draft
category: core
description: 'Baseline terminal behavior — cursor, bell, scrollbar, padding, text styles, env vars, window, links, process handling'
tags:
  [
    'cursor',
    'bell',
    'scrollbar',
    'padding',
    'bold',
    'underline',
    'links',
    'process',
    'env',
    'reflow',
    'fundamentals',
  ]
---

# Terminal Fundamentals

The features every terminal ships that users expect to just work. These must be correct before any smart feature matters.

## Cursor

### Style

Four styles, configurable and changeable by applications via DECSCUSR escape sequence:

```lua
-- In config.lua
cursor_style = "block"              -- block, bar, underline, hollow
cursor_style_blink = true           -- true, false
cursor_blink_interval = 750         -- milliseconds, 0 = no blink
cursor_blink_timeout = 5            -- seconds of no input before blink stops, 0 = never stop
```

Applications (vim, zsh vi-mode) can change cursor style via escape sequences. When the application exits, the cursor reverts to the configured default.

### Color

Cursor color is independent of text color:

```lua
-- In config.lua
cursor_color = "#f5e0dc"            -- cursor fill color
cursor_text_color = "#1e1e2e"       -- text rendered under the cursor
```

If unset, cursor uses reverse video (swap fg/bg). Theme can override both — see [Theming](22-theming.md).

### Unfocused cursor

When the window loses focus, the cursor changes to hollow block by default:

```lua
-- In config.lua
cursor_unfocused_style = "hollow"   -- hollow, unchanged, bar, underline, hidden
```

### Thickness

For bar and underline cursors:

```lua
-- In config.lua
cursor_bar_width = 2              -- pixels
cursor_underline_height = 2       -- pixels
```

## Bell

Four independent bell behaviors. Mix and match:

```lua
-- In config.lua
bell = {
  audio = false,                  -- system beep / custom sound
  visual = false,                 -- flash the pane background briefly
  badge = true,                   -- show badge on tab when bell fires in background pane
  dock = true,                    -- bounce dock icon (macOS) / flash taskbar (Windows/Linux)
  visual_duration = 100,          -- milliseconds
  audio_file = "~/sounds/ping.wav",
  audio_volume = 0.5,             -- 0.0 to 1.0
}
```

Visual bell uses the `visual_bell` color from the theme.

## Alternate Screen Buffer

Standard behavior: programs like vim, less, htop switch to the alternate screen. When they exit, the primary screen (with scrollback) is restored.

```lua
-- In config.lua
alt_screen_scroll_mouse = 3       -- in alt screen, mouse wheel sends this many arrow keys (like WezTerm)
```

No scrollback is available in the alternate screen — this is the VT standard. Programs that want scrolling in alt screen handle it themselves.

## Scrollbar

```lua
-- In config.lua
scrollbar = "auto"                  -- auto, always, never
scrollbar_width = 8                 -- pixels
scrollbar_position = "right"        -- right, left
```

`auto` shows the scrollbar when scrolled away from the bottom, hides it at the bottom. Kitty's `scrolled` behavior — appears when you need it, gone when you don't.

Scrollbar colors come from the theme (`scrollbar-thumb`, `scrollbar-track`).

## Padding

Space between the terminal content and the window edge:

```lua
-- In config.lua
padding = {
  x = 4,                         -- left and right, in points
  y = 4,                         -- top and bottom, in points
  balance = true,                 -- center content when cells don't perfectly fill the window
  color = "extend",               -- background, extend (theme bg color fills padding)
}
```

Or per-side:

```lua
-- In config.lua
padding = {
  left = 4,
  right = 4,
  top = 8,
  bottom = 4,
}
```

## Text Styles

### Bold

```lua
-- In config.lua
bold_is_bright = false            -- default: bold text uses bold font weight, not bright ANSI color
font_family_bold = "auto"         -- auto (synthesize from main font), or explicit font name
```

`bold_is_bright = true` maps bold text to the bright ANSI color variant (traditional terminal behavior). Default is `false` — bold uses actual font weight, which looks better with modern fonts.

### Italic

```lua
-- In config.lua
font_family_italic = "auto"         -- auto (synthesize), or explicit font name
font_family_bold_italic = "auto"    -- same
font_synthetic_style = true         -- synthesize bold/italic if the font doesn't have them
```

### Underline styles

Full support for styled underlines via SGR escape sequences:

| SGR   | Style           | Config adjustment                           |
| ----- | --------------- | ------------------------------------------- |
| `4:0` | Off             |                                             |
| `4:1` | Single straight | `underline_position`, `underline_thickness` |
| `4:2` | Double          |                                             |
| `4:3` | Curly/wavy      |                                             |
| `4:4` | Dotted          |                                             |
| `4:5` | Dashed          |                                             |

Colored underlines via SGR 58/59 (set/clear underline color). Used by LSP diagnostics in terminal editors.

```lua
-- In config.lua
underline_position = "auto"         -- auto, or pixel offset from baseline
underline_thickness = "auto"        -- auto, or pixel value
```

### Strikethrough

Full support via SGR 9 (on) / SGR 29 (off):

```lua
-- In config.lua
strikethrough_position = "auto"     -- auto, or pixel offset
strikethrough_thickness = "auto"    -- auto, or pixel value
```

## Window

### Initial size

```lua
-- In config.lua
window_width = 120                -- columns (cell count)
window_height = 40                -- rows (cell count)
```

### Initial position

```lua
-- In config.lua
window_position_x = "auto"          -- auto (OS decides) or pixel value
window_position_y = "auto"
```

### Remember state

```lua
-- In config.lua
window_save_state = true          -- remember size and position across sessions
```

### Startup mode

```lua
-- In config.lua
window_startup_mode = "windowed"    -- windowed, maximized, fullscreen
```

### Decorations

```lua
-- In config.lua
window_decorations = "native"       -- native, none (borderless)
```

On macOS with `native`: standard title bar with traffic lights. On Linux: GTK4 client-side decorations. On Windows: WinUI title bar.

## Environment Variables

Every pane's child process inherits these:

| Variable               | Value                       | Notes                                                                   |
| ---------------------- | --------------------------- | ----------------------------------------------------------------------- |
| `TERM`                 | `xterm-256color`            | Universal compatibility — no custom terminfo to install on remote hosts |
| `COLORTERM`            | `truecolor`                 | Signals 24-bit color support                                            |
| `TERM_PROGRAM`         | `oakterm`                   | Identifies the terminal to shells and tools                             |
| `TERM_PROGRAM_VERSION` | `0.7.0`                     | Terminal version                                                        |
| `OAKTERM_SOCKET`       | `/tmp/oakterm-<uid>/socket` | Daemon socket for `oakterm ctl`                                         |
| `OAKTERM_PANE_ID`      | `pane-a1b2c3d4`             | This pane's unique ID                                                   |
| `OAKTERM_WORKSPACE`    | `work`                      | Current workspace name                                                  |

**Why `xterm-256color` instead of a custom TERM:**
Ghostty uses `xterm-ghostty`, Kitty uses `xterm-kitty` — both break SSH to servers that don't have the terminfo installed. This is the #1 SSH complaint across both terminals. We use `xterm-256color` (universally available) and advertise extra capabilities via standard DA (Device Attributes) escape sequence responses.

Users who want a custom TERM can set it:

```lua
-- In config.lua
term = "xterm-256color"             -- default
```

Custom environment variables:

```lua
-- In config.lua
env = {
  EDITOR = "nvim",
  GIT_EDITOR = "nvim",
  FOO = "bar",
}
```

## TERM Type and Capabilities

We set `TERM=xterm-256color` but support features beyond what xterm-256color declares:

| Capability                                | How we advertise                             |
| ----------------------------------------- | -------------------------------------------- |
| True color (24-bit)                       | `COLORTERM=truecolor` + correct DA responses |
| Styled underlines (curly, dotted, dashed) | Via escape sequence support (SGR 4:1-4:5)    |
| Kitty graphics protocol                   | Respond to graphics protocol queries         |
| OSC 8 hyperlinks                          | Render on receive                            |
| Synchronized output (DEC mode 2026)       | Respond to mode query                        |
| Bracketed paste                           | Respond to mode query                        |
| OSC 52 clipboard                          | Respond based on security config             |

No custom terminfo to install. Everything works over SSH because `xterm-256color` is on every server.

## Text Reflow on Resize

When the terminal is resized:

- **Wider**: soft-wrapped lines unwrap into single lines
- **Narrower**: long lines re-wrap
- Viewport anchored to content you're reading, not the top or bottom
- Alternate screen content is NOT reflowed (standard behavior — apps handle their own resize via SIGWINCH)

The VT parser tracks which line breaks are soft (from wrapping) vs hard (from the program's `\n`). Only soft breaks are reflowed.

## Clickable Links

### OSC 8 Hyperlinks (explicit)

Programs can emit semantic hyperlinks:

```text
\e]8;id=link1;https://example.com\e\\Click here\e]8;;\e\\
```

Rendered as clickable text with the display text the program chose. Hover shows the URL. Click opens in default browser.

### URL Detection (implicit)

The terminal detects URLs in output via regex and makes them clickable:

```lua
-- In config.lua
link_detection = true             -- default
link_click_modifier = "super"     -- super (Cmd/Ctrl) + click to open, or "none" for plain click
```

Detection is wrap-aware — URLs that span across wrapped lines are recognized as one link. Balanced delimiter handling — `https://en.wikipedia.org/wiki/Rust_(language)` includes the closing `)`.

URL hover shows the destination and underlines the link using the `url-color` from the theme.

### File path detection

Paths like `src/components/Button.tsx:42:15` are detected when shell integration provides the pane's cwd for resolution. Click opens in `$EDITOR` at the specified line.

```lua
-- In config.lua
link_file_paths = true            -- default, requires shell integration for relative path resolution
```

## Process Handling

### On tab/pane close

```lua
-- In config.lua
close_confirm = "smart"             -- smart, always, never
```

`smart` behavior (default):

- **Idle shell** (no child process beyond the shell itself) → close immediately
- **Running process** → show confirmation: "npm run build is running. Close? [Yes] [Cancel]"
- **Agent pane** → stronger warning: "Agent feat/auth is working. Closing will abandon changes."

Process detection uses the PTY process tree, not just the direct child.

### On terminal quit

```lua
-- In config.lua
quit_confirm = "smart"              -- smart, always, never
```

`smart`: confirm only if any pane has a running foreground process. Idle shells don't trigger confirmation.

### Signal handling

When a pane is closed:

1. Send `SIGHUP` to the child process group
2. Wait briefly for graceful shutdown
3. Send `SIGTERM` if still running
4. Send `SIGKILL` as last resort

### Exit behavior

```lua
-- In config.lua
close_on_exit = true              -- close the pane when the process exits (default)
hold_on_exit = false              -- keep the pane open after process exits, showing exit code
```

`hold_on_exit = true` is useful for debugging — shows "[Process exited with code 1]" and lets you read the last output.

## Scrollback Navigation

| Action               | Default keybind  | Notes                                                           |
| -------------------- | ---------------- | --------------------------------------------------------------- |
| Scroll up one line   | `Shift+Up`       |                                                                 |
| Scroll down one line | `Shift+Down`     |                                                                 |
| Page up              | `Shift+PageUp`   |                                                                 |
| Page down            | `Shift+PageDown` |                                                                 |
| Scroll to top        | `Shift+Home`     |                                                                 |
| Scroll to bottom     | `Shift+End`      |                                                                 |
| Mouse wheel          | Platform-native  | Smooth scrolling on macOS, line-by-line on Linux (configurable) |

```lua
-- In config.lua
scroll_multiplier = 3             -- lines per scroll event (mouse wheel)
scroll_to_bottom_on_input = true  -- snap to bottom when you start typing
```

In the alternate screen (vim, less), mouse wheel sends arrow keys to the application instead of scrolling the terminal.

## Initial Working Directory

```lua
-- In config.lua
working_directory = "inherit"       -- inherit, home, or absolute path
```

- `inherit` — new panes/tabs inherit the cwd of the focused pane (via shell integration OSC 7) or the parent process
- `home` — start in `$HOME`
- `/path/to/dir` — start in a specific directory

New splits and tabs inherit the current pane's working directory by default:

```lua
-- In config.lua
split_inherit_cwd = true          -- default
tab_inherit_cwd = true            -- default
```

## Shell Selection

```lua
-- In config.lua
shell = "auto"                      -- auto, or explicit path
shell_login = true                  -- run as login shell (default)
```

`auto` resolves in order:

1. `$SHELL` environment variable
2. User's login shell from passwd
3. `/bin/sh` as last resort

Explicit: `shell = "/opt/homebrew/bin/fish"`

With arguments: use Lua:

```lua
shell = { "/bin/zsh", "-l", "--no-rcs" }
```

## Word Selection Boundaries

Characters that break word selection on double-click:

```lua
-- In config.lua
word_delimiters = " \t\n{}[]()\"'`,;:@|<>"
```

Default includes whitespace, brackets, quotes, and common punctuation. This means `src/components/Button.tsx` selects as one word (no `/` or `.` in the delimiter set).

Kitty inverts this — you specify characters that are INCLUDED in words. We follow the delimiter approach (like WezTerm and Alacritty) because it's more intuitive: "these characters break words."

## Tab Stop Width

Standard 8-column tab stops. Not configurable via terminal config — this is a VT standard. Applications can set custom tab stops via HTS (Horizontal Tab Set) and TBC (Tab Clear) escape sequences.

## Cursor Key Mode

Application mode vs normal mode is controlled entirely by the application via DECCKM escape sequence. Not user-configurable — this is standard VT behavior. Applications (vim, readline) toggle this automatically.

## Right-to-Left / BiDi

Initial support for BiDi text rendering:

```lua
-- In config.lua
bidi = "auto"                       -- auto, force-ltr, true
```

- `auto` — detect BiDi text and render with correct visual order
- `force-ltr` — disable BiDi reordering for performance (Kitty's approach)
- `true` — always apply BiDi algorithm

WezTerm has the most mature BiDi support. We follow their approach with `bidi_enabled` + `bidi_direction`. This is a rendering concern — the VT grid stays left-to-right, but visual reordering happens for BiDi text.

## What's Not Configurable (and shouldn't be)

Some things are terminal standards and should not be user-configurable:

- **Tab stop width** — always 8 columns (VT standard)
- **Cursor key mode** — application-driven via DECCKM
- **Alternate screen switching** — application-driven
- **Character encoding** — always UTF-8 (no legacy encodings)
- **VT escape sequence set** — always xterm-compatible

These are not opinions — they're standards. Deviating breaks programs.

## Related Docs

- [Renderer](02-renderer.md) — GPU rendering, font handling, image protocols
- [Theming](22-theming.md) — cursor, scrollbar, bell, link colors
- [Configuration](09-config.md) — config file format and naming convention
- [Multiplexer](03-multiplexer.md) — pane types, splits, tabs
- [Shell Integration](18-shell-integration.md) — cwd tracking, prompt detection
- [Smart Keybinds](19-smart-keybinds.md) — smart Ctrl+C/V, hints mode
- [Security](21-security.md) — bracketed paste, clipboard controls
- [Platform Support](20-platform-support.md) — platform-specific window behavior
- [Performance](12-performance.md) — input latency targets
