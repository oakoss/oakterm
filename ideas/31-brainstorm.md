---
title: "Brainstorm — Unsorted Ideas"
status: draft
category: cross-cutting
description: "Raw ideas to be evaluated and potentially promoted to their own docs"
tags: ["brainstorm", "syntax", "tiling", "layouts", "sidebars", "scrollback", "zed"]
---
# Brainstorm — Unsorted Ideas

Raw ideas captured during discussion. Each needs evaluation — some will become features, some will fold into existing docs, some won't make the cut.

## Syntax Highlighting in Terminal Output

Built-in syntax parsing so terminal output (logs, diffs, JSON, code) gets highlighted automatically. Not just ANSI colors from programs — the terminal itself understands the content.

**How it could work:**
- Tree-sitter parsers running on pane output (same library Zed, Neovim, Helix use)
- Auto-detect content type: JSON, YAML, diffs, stack traces, log formats
- Highlight in real-time as output streams
- Configurable — enable per pane type or globally
- Works on plain `cat file.rs` output that has no ANSI colors

**Open questions:**
- Performance cost of parsing every line of output? Tree-sitter is fast but terminal output can be high-volume
- Should this be core or plugin? Leaning plugin — uses `pane.output` + a tree-sitter WASM module
- Does it conflict with programs that send their own ANSI colors? Need a priority system: program colors win over auto-highlight
- Which languages/formats to support by default?

## Auto-Tiling Layout Engine

Automatic pane tiling that arranges panes without manual splitting. Like a tiling window manager but inside the terminal.

**Modes:**
- `tiling = auto` — new panes auto-arrange (spiral, columns, main+stack)
- `tiling = manual` — you place panes yourself with splits (current default)
- `tiling = off` — single pane only, tabs for everything

**Auto-tiling algorithms:**
- **Main + stack** — one large pane on the left, new panes stack on the right (like dwm/i3 default)
- **Columns** — equal-width columns, new pane adds a column
- **Spiral** — fibonacci-style splitting (like bspwm)
- **Grid** — auto-arrange into a grid based on pane count

```
tiling-mode = auto
tiling-algorithm = main-stack
tiling-main-ratio = 0.6
```

**Interaction with pane types:** Auto-tiling only affects tiled panes. Floating, drawer, popup, modal, and sidebar panes are unaffected.

**Should be core** — it's part of the multiplexer layout engine.

## Panel Layout Presets (Quick Layouts)

Named layout configurations you can switch between instantly from the palette or keybinds. Different from saved layouts (which create tabs with specific commands) — these rearrange existing panes.

```
Cmd+Shift+P → :layout

┌──────────────────────────────────────────────────┐
│  layout:  Search layouts                         │
├──────────────────────────────────────────────────┤
│  Presets                                         │
│  ⊞ Main + Stack          Alt+1                  │
│  ⊞ Equal Columns         Alt+2                  │
│  ⊞ Grid                  Alt+3                  │
│  ⊞ Focused (one pane)    Alt+4                  │
│  ⊞ Side by Side          Alt+5                  │
│                                                  │
│  Saved                                           │
│  ⊞ dev (3 tabs, 5 panes)                        │
│  ⊞ monitoring (2 tabs, 4 panes)                 │
└──────────────────────────────────────────────────┘
```

Keybinds for quick switching:
```
keybind = alt+1 = layout-main-stack
keybind = alt+2 = layout-columns
keybind = alt+3 = layout-grid
keybind = alt+4 = layout-focused
keybind = alt+5 = layout-side-by-side
```

Presets rearrange the current tab's panes instantly. Your pane processes keep running — only the layout changes.

## Claude Code Scrollback Buffer Issue

The #1 bug report across Ghostty + Claude Code: the terminal jumps to the top or bottom of scrollback erratically when Claude is streaming output.

**Root cause:** Claude Code uses rapid terminal redraws — 4,000-6,700 scroll events per second. It also uses alternate screen + cursor movements that interact badly with scrollback management.

**Our approach (multiple layers):**

1. **Synchronized output (DEC mode 2026)** — batch terminal updates between begin/end markers. The renderer only draws complete frames, eliminating flicker and partial-render jumps. This is the standard fix and we support it.

2. **Agent-aware scroll pinning** — when a pane is marked as an agent (via the agent-manager plugin), the terminal pins the user's scroll position. If you've scrolled up to read something, the agent's new output appends below but doesn't yank your viewport. A "new output below ↓" indicator appears to jump back to the bottom.

3. **Output rate throttling for rendering** — if a pane is producing >1000 lines/second, the renderer skips intermediate frames and only draws the latest state. The scroll buffer still captures everything — only the visual rendering is throttled. This prevents the GPU from thrashing on pathological output.

4. **Separate scroll regions** — the agent's streaming output and your scrollback are managed as separate regions internally. Scrolling up enters "review mode" which freezes viewport position until you explicitly return to the bottom (press `G` in copy mode, or click the "↓" indicator).

5. **Ring buffer ceiling** — even with massive agent output, memory stays bounded (see [Memory Management](15-memory-management.md)). The ring buffer means old output rolls off, never growing unbounded.

These are all core features, not plugins. The scroll buffer, rendering pipeline, and viewport management are deeply integrated.

## Multi-Sidebar Configuration

Instead of one fixed sidebar on the left, support multiple sidebar panels that are independently configurable.

```
┌───────────┬────────────────────────────┬───────────┐
│ LEFT      │                            │ RIGHT     │
│           │                            │           │
│ AGENTS    │  Main terminal content     │ NOTES     │
│ ◉ feat/   │                            │ todo.md   │
│ ◉ tests/  │                            │ - fix auth│
│           │                            │ - add test│
│ SERVICES  │                            │           │
│ ▶ dev     │                            │ GIT       │
│ ▶ docker  │                            │ main      │
│           │                            │ 3 ahead   │
│ SHELLS    │                            │ 2 files   │
│ ● scratch │                            │           │
└───────────┴────────────────────────────┴───────────┘
```

**Configuration:**

```lua
sidebars = {
  left = {
    enabled = true,
    width = 220,
    default = "collapsed",    -- "collapsed", "expanded", "hidden"
    sections = { "agents", "services", "watchers", "shells" },
  },
  right = {
    enabled = true,
    width = 200,
    default = "hidden",
    sections = { "git-status", "notes" },
  },
  -- bottom = { ... }  -- could support bottom sidebar too
}
```

Flat config:
```
sidebar-left-enabled = true
sidebar-left-width = 220
sidebar-left-default = collapsed
sidebar-right-enabled = false
```

**Tabs within sidebars:**
Each sidebar can have tabs to cycle through different views without expanding the sidebar width:

```
┌───────────┐
│[Proc][Git] │  ← tabs at top of sidebar
│────────────│
│ AGENTS     │
│ ◉ feat/    │
│ ◉ tests/   │
│ SERVICES   │
│ ▶ dev      │
└────────────┘
```

`Ctrl+B` toggles left sidebar. `Ctrl+Shift+B` toggles right sidebar. Tabs within a sidebar cycle with clicking or a keybind.

**Plugin integration:**
Plugins register which sidebar(s) they want their sections in:

```rust
sidebar.register_section(SidebarSection {
    name: "agents",
    preferred_sidebar: "left",    // suggestion, user can override
    accessible_label: "Agent processes",
});
```

Users can drag sections between sidebars or configure placement in config.

**This is still a plugin** — the sidebar-ui plugin handles the rendering of one or more sidebars. The core provides the data model. A multi-sidebar is just the sidebar-ui plugin supporting multiple instances.

## Zed-Inspired Patterns to Adopt

From the Zed architecture research:

1. **Batched instanced GPU rendering** — typed scene graph (quads, sprites, paths), batched by type and texture, single instanced draw call per batch. This is the foundation of fast rendering.

2. **Glyph atlas with `etagere` allocator** — proven, efficient atlas packing for font glyphs.

3. **WASM Component Model + WIT** — Zed uses the WASM Component Model with WIT (WebAssembly Interface Types) for extensions, not raw WASM. This is a stricter, more type-safe plugin contract. Worth evaluating vs. raw Wasmtime.

4. **Damage tracking** — Zed rebuilds the entire element tree every frame (expensive). For terminals, most of the screen is static between frames. Track which cells changed and only re-render dirty regions. Zed doesn't do this — we should.

5. **Headless mode pattern** — Zed's `remote_server` runs the full project model without UI. Our headless daemon uses the same pattern — full multiplexer + plugins, no renderer.

6. **Pre-rasterize ASCII** — for monospaced terminal rendering, pre-rasterize the entire ASCII range (32-126) on startup into the glyph atlas. Skip font shaping for ASCII entirely. Only fall back to full OpenType shaping for non-ASCII (CJK, emoji, combining marks).

7. **Channel-based message passing** — subsystems communicate via channels, not direct calls. Terminal event loop, plugin host, and renderer are decoupled.

## Configurable Status Bar

Like tmux's status line or Neovim's lualine. Core renders the bar, plugins register widgets.

- Position: bottom, top, or hidden
- Three segments: left, center, right
- Widgets: `{mode}`, `{pane_title}`, `{git_branch}`, `{test_status}`, `{memory}`, `{ports}`, `{time}`, `{agent_count}`, etc.
- Plugins register custom widgets (watcher provides `{test_status}`, agent-manager provides `{agent_count}`)
- User places widgets wherever they want in config
- Themeable — colors from the theme's `status-bar-bg`/`status-bar-fg`
- Can be hidden completely: `status-bar = none`
- Should respect `prefers-reduced-motion` for any animated widgets
- Accessible — screen reader can read status bar content
