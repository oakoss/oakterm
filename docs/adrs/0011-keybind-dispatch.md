---
adr: '0011'
title: Keybind Dispatch Architecture
status: proposed
date: 2026-04-02
tags: [core]
---

# 0011. Keybind Dispatch Architecture

## Context

Phase 1 adds multiplexer keybinds (split, tab, workspace, copy mode, resize), a command palette, and user-configurable keybindings via Lua. The multiplexer idea doc ([03-multiplexer.md](../ideas/03-multiplexer.md)) specifies concrete keybinds (`Ctrl+\` split right, `Ctrl+-` split down, `Ctrl+F` floating, `Ctrl+J` drawer) but does not specify how they are dispatched or how conflicts with terminal applications are handled.

The keybind system must avoid conflicts with terminal applications (Ctrl+C, Ctrl+D, Ctrl+R all have meaning), remain discoverable for new users, and let power users remap everything. These goals pull in different directions.

The Lua config spec ([Spec-0005](../specs/0005-lua-config-runtime.md)) already defines `oakterm.keybind(key, action)` with chord syntax like `"ctrl+d"` and `"super+shift+t"`. This is a registration API; it does not prescribe a dispatch model.

Research covered tmux (prefix key), Zellij (mode-based), WezTerm (hybrid leader + key tables + direct), Ghostty (direct modifier with `performable:`/`unconsumed:` qualifiers), and Kitty (`kitty_mod` variable).

## Options

### Option A: Prefix key (tmux model)

A prefix key (e.g., `Ctrl+B`) activates a command table. Subsequent keypress triggers the action. All multiplexer keybinds require two keystrokes.

**Pros:**

- Near-zero conflict risk. Only the prefix key itself can conflict.
- Familiar to tmux/screen users.

**Cons:**

- Two-keystroke overhead for every multiplexer action, including frequent operations like pane switching.
- Default `Ctrl+B` conflicts with readline backward-char. Users almost universally remap it.
- Low discoverability. No visual indicator of available commands after pressing the prefix.

### Option B: Mode-based (Zellij model)

Multiple named modes (normal, pane, tab, resize, scroll). `Ctrl+<letter>` switches modes. Within a mode, unmodified keys trigger actions.

**Pros:**

- High discoverability. A status bar shows the current mode and available keys.
- Unmodified keys within a mode are ergonomic (just press `h/j/k/l` to navigate panes).

**Cons:**

- Mode-switch keys use `Ctrl+<letter>`, which collides with terminal apps. Zellij's `Ctrl+P` (pane mode) shadows Ctrl+P in vim, bash, and many TUI apps. This is the most common Zellij complaint.
- Mental overhead of tracking the current mode. Users accidentally type in the wrong mode.
- Requires a "locked" escape-hatch mode that passes all input to the application, undermining the mode model.

### Option C: Direct modifier with `oak_mod` (Kitty/Ghostty hybrid)

A configurable modifier variable (`oak_mod`, default `Ctrl+Shift`) prefixes all multiplexer keybinds. Copy mode and resize mode use key tables (unmodified keys within the mode). Optional leader key for tmux converts.

**Pros:**

- Single-keystroke multiplexer actions (`Ctrl+Shift+\` to split).
- `Ctrl+Shift` is almost never used by terminal applications, so conflicts are rare by design.
- Key tables for copy mode and resize mode give ergonomic unmodified keys where it matters (vim motions in copy mode).
- Leader key option means tmux users can configure their familiar workflow.
- Discoverable: status bar shows mode when in copy/resize mode; command palette shows all keybinds.
- Kitty proves `kitty_mod` works. Reassigning the variable shifts all shortcuts at once.

**Cons:**

- `Ctrl+Shift` is ergonomically heavier than plain `Ctrl` or a prefix key.
- `Ctrl+Shift` does not work with non-Latin keyboard layouts on some platforms (Kitty addresses this with its keyboard protocol, which requires app opt-in).
- More keybind "types" to understand (direct, key table, leader) than a pure prefix or pure mode model.

## Decision

**Option C — Direct modifier with configurable `oak_mod`, key tables for modal contexts, optional leader key.**

`Ctrl+Shift` avoids terminal app conflicts by default. Key tables give copy mode and resize mode ergonomic unmodified keys. The optional leader key covers tmux converts. The idea doc's keybinds (`Ctrl+\`, `Ctrl+-`, etc.) will be updated to use `oak_mod` equivalents.

### Dispatch layers

Input flows through these layers in order:

1. **Leader sequence.** If a leader key is configured and was just pressed, the next keypress is matched against the leader table. If no match, the leader keypress and the current keypress are both sent to the application.
2. **Key table.** If a key table is active (copy mode, resize mode), the keypress is matched against that table. Unmatched keys are dropped (not forwarded) in modal tables, or forwarded in passthrough tables.
3. **Default bindings.** The keypress is matched against the default keybind table (`oak_mod` + key). If no match, the keypress is forwarded to the focused pane's PTY.

### Default keybinds

`oak_mod` defaults to `Ctrl+Shift` on Linux, `Cmd` on macOS (matching platform conventions).

| Key                 | Action                                   |
| ------------------- | ---------------------------------------- |
| `oak_mod + \`       | Split right                              |
| `oak_mod + -`       | Split down                               |
| `oak_mod + W`       | Close pane                               |
| `oak_mod + H/J/K/L` | Focus pane left/down/up/right            |
| `oak_mod + T`       | New tab                                  |
| `oak_mod + [1-9]`   | Switch to tab N                          |
| `oak_mod + F`       | Toggle floating pane                     |
| `oak_mod + Enter`   | Toggle pane zoom (fullscreen within tab) |
| `oak_mod + [`       | Enter copy mode                          |
| `oak_mod + P`       | Command palette                          |
| `oak_mod + R`       | Enter resize mode                        |

### Key tables

Copy mode and resize mode activate a key table that captures unmodified keys:

- **Copy mode table:** vim or emacs preset (configurable). `h/j/k/l`, `v`, `y`, `/`, `Esc` to exit.
- **Resize mode table:** `h/j/k/l` to resize, `Enter` or `Esc` to exit.

Key tables are modal: unmatched keys are dropped, not forwarded to the PTY. The status bar shows the active mode.

### Configuration

These config fields (`oak_mod`, `leader`, `copy_mode_keybinds`) are new additions to Spec-0005.

```lua
-- Change the modifier for all default keybinds
oakterm.config.oak_mod = "ctrl+shift"  -- default on Linux
oakterm.config.oak_mod = "super"       -- default on macOS

-- Override individual keybinds
oakterm.keybind("oak_mod+\\", oakterm.action.split_pane({ direction = "right" }))
oakterm.keybind("oak_mod+d", oakterm.action.close_pane())

-- tmux-style leader key
oakterm.config.leader = { key = "ctrl+b", timeout = 1000 }
oakterm.keybind("leader+%", oakterm.action.split_pane({ direction = "right" }))
oakterm.keybind("leader+\"", oakterm.action.split_pane({ direction = "down" }))

-- Copy mode preset
oakterm.config.copy_mode_keybinds = "vim"  -- or "emacs" or "basic"
```

`oak_mod` in a keybind string is expanded at registration time to the configured modifier. This means changing `oak_mod` after keybinds are registered does not retroactively update them — `oak_mod` must be set before keybinds.

### Performable actions

Borrowed from Ghostty: some actions are context-dependent. `oakterm.action.copy()` only activates when text is selected. If no text is selected, the keypress passes through to the application. Ghostty implements this as a qualifier on the keybind string (`performable:ctrl+c=copy`); OakTerm declares it per-action instead, so the keybind system checks `action.is_performable()` before consuming the keypress.

## Consequences

- The idea doc's keybinds (`Ctrl+\`, `Ctrl+-`, `Ctrl+F`, `Ctrl+J`, `Ctrl+B`, `Ctrl+G`) will be updated to use `oak_mod` equivalents. This is a documentation change, not a compatibility concern (Phase 1 is new functionality).
- Spec-0005 (Lua Config) will need additions: `oakterm.config.oak_mod`, `oakterm.config.leader`, `oakterm.config.copy_mode_keybinds`, key table registration API.
- The wire protocol does not need changes for keybind dispatch; keybinds are resolved in the GUI process before input is forwarded to the daemon.
- Copy mode key table and resize mode key table will be specified in their respective specs (Spec-0008 Copy Mode).
- The status bar spec must include mode indicator display.

## References

- [03-multiplexer.md](../ideas/03-multiplexer.md) — keybinds, copy mode, view modes
- [08-command-palette.md](../ideas/08-command-palette.md) — palette invocation
- [Spec-0005: Lua Config Runtime](../specs/0005-lua-config-runtime.md) — `oakterm.keybind()` API
- [Kitty kitty_mod](https://sw.kovidgoyal.net/kitty/overview/) — configurable modifier variable
- [Ghostty keybind qualifiers](https://ghostty.org/docs/config/keybind) — `performable:`, `unconsumed:`
- [Zellij keybind issues](https://github.com/zellij-org/zellij/issues/1399) — Ctrl conflicts
- [WezTerm key tables](https://wezterm.org/config/key-tables.html) — leader + table stacking
- [tmux key tables](https://man.openbsd.org/tmux.1) — prefix + named tables
