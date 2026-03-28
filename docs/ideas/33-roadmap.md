---
title: 'Roadmap'
status: draft
category: cross-cutting
description: 'Phased implementation plan from MVP to full vision'
tags: ['roadmap', 'mvp', 'phases', 'priorities']
---

# Roadmap

What gets built first and why.

## Guiding Principle

Ship a usable terminal as fast as possible. Then add layers. Every phase produces something people can actually use — not a tech demo that becomes useful "eventually."

## Phase 0: Foundation (the thing that renders text)

A terminal that boots, renders text, and runs a shell. Nothing else.

> **Specs:** [0001 Daemon Wire Protocol](../specs/0001-daemon-wire-protocol.md), [0002 VT Parser](../specs/0002-vt-parser.md), [0003 Screen Buffer](../specs/0003-screen-buffer.md), [0004 Scroll Buffer](../specs/0004-scroll-buffer.md), [0005 Lua Config](../specs/0005-lua-config-runtime.md), [0006 Accessibility](../specs/0006-accessibility-bridge.md)

**What ships:**

- Daemon/GUI process split with Unix socket wire protocol ([Spec-0001](../specs/0001-daemon-wire-protocol.md), [ADR-0007](../adrs/0007-daemon-architecture.md))
- VT parser (xterm-256color compatible) with OSC 133/7 parsing ([Spec-0002](../specs/0002-vt-parser.md), [ADR-0008](../adrs/0008-shell-integration-timing.md))
- Kitty graphics protocol in core ([ADR-0004](../adrs/0004-kitty-graphics-in-core.md))
- GPU renderer (wgpu) with glyph atlas and run-based pipeline ([ADR-0009](../adrs/0009-bidi-ligature-preparedness.md))
- Platform-native text shaping behind TextShaper trait (Core Text / HarfBuzz / DirectWrite)
- Font loading with fallback chain
- Ligature-ready architecture (shaper trait, Phase 0 uses simple glyph lookup)
- Single pane, single window
- Sandboxed Lua config with hot-reload and LLS type stubs ([Spec-0005](../specs/0005-lua-config-runtime.md), [ADR-0005](../adrs/0005-lua-sandboxed-config.md))
- Keyboard input, mouse input
- Two-tier scrollback: ring buffer + encrypted disk archive ([Spec-0004](../specs/0004-scroll-buffer.md), [ADR-0006](../adrs/0006-scroll-buffer-architecture.md))
- Alternate screen scrollback capture (iTerm2 model)
- Copy/paste (platform clipboard, OSC 52)
- AccessKit accessibility with Role::Terminal, lazy activation ([Spec-0006](../specs/0006-accessibility-bridge.md), [ADR-0001](../adrs/0001-accessibility-in-phase-zero.md))
- Bundled default themes (dark + light, Lua format)
- True color support, styled underlines, hyperlinks
- BiDi-ready data model (logical order, Direction enum) ([ADR-0009](../adrs/0009-bidi-ligature-preparedness.md))
- Criterion benchmark suite with tiered CI ([ADR-0002](../adrs/0002-performance-philosophy.md))
- macOS + Linux (Windows can follow closely)

**What does NOT ship:**

- No multiplexer, no tabs, no splits
- No plugins
- No sidebar
- No smart anything

**Why this first:** Everything else depends on a correct, fast renderer and VT parser. If this isn't solid, nothing built on top matters. This is also the phase where we establish performance baselines — input latency, FPS, memory.

**Exit criteria:** Can replace Alacritty for basic single-pane use. Performance targets met (<8ms input latency, <30MB memory).

## Phase 1: Multiplexer (replace tmux)

Tabs, splits, and session management.

**What ships:**

- Tabs
- Tiled splits (horizontal, vertical)
- Floating panes
- Workspaces
- Session persistence (serialize on quit, restore on launch)
- Layouts (declarative, in config)
- Copy mode (vim/emacs/basic keybinds)
- Command palette (`:` commands, `>` actions, `@` workspaces, `#` layouts)
- Smart Ctrl+C/V (platform-aware)
- Keybind configuration
- OSC-52 clipboard passthrough through all layers
- Server/client architecture (daemon + windows, shared glyph atlas)
- Status bar (basic — mode, pane title, time)

**What does NOT ship:**

- No plugins yet
- No sidebar
- No SSH domains
- No agents

**Why this order:** The multiplexer is the core value proposition over Alacritty. Once this ships, people can drop tmux. This also establishes the pane model that everything else builds on — sidebar, plugins, agents all operate on panes.

**Exit criteria:** Can replace tmux for daily use. Session restore works. Layouts work. Copy mode works.

## Phase 2: Plugin System (the platform)

The extension runtime and core plugin API.

**What ships:**

- WASM plugin host (Wasmtime)
- Lua config engine (full — event handlers, plugin config, advanced scripting)
- Plugin API primitives: pane, sidebar data model, palette, notify, lifecycle hooks, process, filesystem
- Capability-based permissions
- Plugin manager CLI (`oakterm plugin install/remove/list`)
- Plugin registry (initial, lightweight)
- Bundled plugins (first batch):
  - `sidebar-ui` — the sidebar renderer
  - `sixel-graphics` — Sixel image protocol (Kitty graphics is core per [ADR-0004](../adrs/0004-kitty-graphics-in-core.md))
  - `service-monitor` — services in sidebar
  - `watcher` — test/build watchers in sidebar
- Drawer panes, popup panes, modal panes
- Settings palette (`:settings`, `:keybinds`, `:theme` with live preview)
- Theme system (Lua, inheritance, import, validation)
- Health check (`:health` / `oakterm doctor`)

**What does NOT ship:**

- No agents yet
- No context engine
- No SSH domains
- No remote access

**Why this order:** The plugin system unlocks everything else. Once it ships, the community can start building. The sidebar becomes real. The theme system becomes deep. Dogfood the API with our own bundled plugins before opening it to the world.

**Exit criteria:** Third-party developers can build, publish, and install plugins. Bundled plugins work reliably. Plugin crash doesn't take down the terminal.

## Phase 3: Shell Intelligence (context + agents)

Smart features that know what you're doing.

**What ships:**

- Shell integration (bash, zsh, fish scripts + OSC 133/7 parsing)
- Scroll-to-prompt
- Process completion notifications
- Context engine plugin (smart autocomplete, project detection, typed completions)
- Agent management plugin (`:agent`, worktrees, sidebar status, `:merge`, `:diff`)
- Harpoon plugin (pane bookmarks)
- Agent control API (`oakterm ctl`)
- Hints mode
- Input broadcast
- Environment-aware pane coloring
- Pane Query + Window + Storage + Shell Events plugin APIs

**What does NOT ship:**

- No AI / NL commands yet (context engine works without AI)
- No remote access
- No SSH domains

**Why this order:** Shell integration feeds the context engine and agent management. These are the features that differentiate us — a terminal that's aware of what you're doing. But they all depend on the plugin system (Phase 2) and the multiplexer (Phase 1).

**Exit criteria:** Running 3+ agents in parallel with sidebar status, notifications, and `:merge` workflow. Context engine provides useful completions. Harpoon is daily-driver quality.

## Phase 4: Networking (SSH + remote)

Connect to remote machines and daemons.

**What ships:**

- SSH domains (core — in the multiplexer)
- Remote domains (headless daemon + client connection)
- Headless mode (`oakterm --headless`)
- Web client plugin (for mobile/browser access)
- Auto-reconnection on network drop
- Token and mTLS authentication
- Windows support (if not already shipped)

**Why this order:** Networking is complex and touches security deeply. Better to ship it after the core is stable and the security model is proven with plugins.

**Exit criteria:** Run a headless daemon on a server, connect from desktop terminal, remote panes appear in sidebar alongside local panes. SSH domains work for quick shell access.

## Phase 5: Polish + Community

The long tail.

**What ships:**

- NL commands (`?` prefix with AI backend)
- Quake/dropdown mode plugin
- Browser plugins (lite + webview)
- Auto-tiling layout engine
- Syntax highlighting plugin (tree-sitter)
- Multi-sidebar support
- Advanced status bar widgets
- `oakterm ctl` expanded commands
- Accessibility audit and improvements (note: [ADR 0001](../adrs/0001-accessibility-in-phase-zero.md) moved core accessibility to Phase 0 -- AccessKit integration is built alongside the renderer from day one; this Phase 5 item covers additional audits and polish)
- Performance optimization pass
- Documentation site
- Plugin gallery / theme gallery
- Contributing guide

**This phase never ends.** It's the ongoing development after the core vision is realized.

## Timeline Philosophy

No dates. The phases ship when they're ready. Each phase is usable on its own:

| Phase | What you can use it as                       |
| ----- | -------------------------------------------- |
| 0     | Fast, clean terminal (Alacritty alternative) |
| 1     | Terminal + multiplexer (tmux replacement)    |
| 2     | Extensible terminal platform                 |
| 3     | Smart, agent-aware terminal                  |
| 4     | Remote-capable terminal                      |
| 5     | Full vision                                  |

Each phase is a valid stopping point. If we only ever ship Phase 1, it's still a useful product.

## Related Docs

- [Architecture](01-architecture.md) — what's core vs plugin
- [Performance](12-performance.md) — targets that Phase 0 must meet
- [Plugin System](06-plugins.md) — what Phase 2 delivers
- [Agent Management](07-agent-management.md) — what Phase 3 delivers
- [Remote Access](29-remote-access.md) — what Phase 4 delivers
