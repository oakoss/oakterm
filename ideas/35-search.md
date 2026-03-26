---
title: 'Search'
status: draft
category: core
description: 'Search within terminal output — regex, cross-pane, per-command'
tags: ['search', 'regex', 'find', 'scrollback', 'highlights']
---

# Search

Search within terminal output. Core feature — not a plugin.

## Basic Search

`Ctrl+Shift+F` (or `/` in copy mode) opens a search bar at the bottom of the pane:

```text
┌──────────────────────────────────────────────────┐
│  Terminal output with matches highlighted        │
│                                                  │
│  Some text with [error] and more [error] here    │
│  Another line with no match                      │
│  This line has an [error] too                    │
│                                                  │
├──────────────────────────────────────────────────┤
│  Search: error          3 matches  [2/3]  ↑ ↓   │
└──────────────────────────────────────────────────┘
```

- Type to search — results highlight instantly (incremental search)
- `Enter` / `n` — next match
- `Shift+Enter` / `N` — previous match
- `Esc` — close search, leave highlights briefly then clear
- Match count and current position shown

## Regex Search

Toggle regex mode with a button or `Alt+R`:

```text
Search: error|warn.*timeout    [regex]  12 matches  [1/12]  ↑ ↓
```

Full regex syntax (Rust `regex` crate — fast, no backtracking).

## Case Sensitivity

- Default: smart case (case-insensitive unless you type an uppercase letter)
- Toggle with `Alt+C`:
```text
  Search: Error    [case]  2 matches
```

## Search Scope

By default, search operates on the current pane's visible + scrollback content. But you can scope it:

### Per-command search (requires shell integration)

`Ctrl+Shift+F` then `Alt+S` scopes search to the output of a single command:

```text
Search: error    [scope: last command]  1 match
```

Arrow through commands to search within different ones. Shell integration markers (OSC 133) define command boundaries.

### Cross-pane search

`Ctrl+Shift+F` then `Alt+A` searches across all panes:

```text
Search: error    [scope: all panes]  7 matches across 3 panes

  ● scratch:     2 matches
  ◉ feat/auth:   4 matches
  ▶ dev server:  1 match
```

Select a result to jump to that pane and scroll to the match.

## Persistent Highlights

Separate from search — persistent regex-based highlighting that colors output as it streams. Like iTerm2's triggers.

```lua
highlights = {
  { pattern = "error", color = "red", style = "bold" },
  { pattern = "warn", color = "yellow" },
  { pattern = "\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}", color = "dim" },   -- timestamps
  { pattern = "PASS", color = "green" },
  { pattern = "FAIL", color = "red", style = "bold" },
}
```

Flat config:

```ini
highlight = error red bold
highlight = warn yellow
highlight = PASS green
highlight = FAIL red bold
```

These apply to all pane output in real-time. Different from search — highlights are always on, search is on-demand.

Persistent highlights could also be a plugin (uses `pane.output` for pattern matching) but basic support in core makes sense since it's tied to the renderer.

## Search Colors

Themed via [Theming](22-theming.md):

```toml
search-match-bg           = "#f9e2af"
search-match-fg           = "#1e1e2e"
search-selected-bg        = "#fab387"
search-selected-fg        = "#1e1e2e"
```

## Keyboard Summary

| Key                 | Action                   |
| ------------------- | ------------------------ |
| `Ctrl+Shift+F`      | Open search bar          |
| `/` (in copy mode)  | Open search bar          |
| `Enter` / `n`       | Next match               |
| `Shift+Enter` / `N` | Previous match           |
| `Alt+R`             | Toggle regex mode        |
| `Alt+C`             | Toggle case sensitivity  |
| `Alt+S`             | Scope to current command |
| `Alt+A`             | Scope to all panes       |
| `Esc`               | Close search             |

## Related Docs

- [Multiplexer](03-multiplexer.md) — copy mode integrates with search (`/` and `?`)
- [Shell Integration](18-shell-integration.md) — command boundaries for per-command search
- [Theming](22-theming.md) — search highlight colors
- [Accessibility](17-accessibility.md) — search results announced to screen reader
