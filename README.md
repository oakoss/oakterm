# OakTerm

A GPU-accelerated, extensible terminal emulator with a plugin-driven process dashboard and context-aware shell.

> **Status: Active Implementation** — Phase 0 terminal foundation is implemented; the next roadmap focus is Phase 1 multiplexer work.

## Philosophy

The terminal is the oldest developer tool that still works. It doesn't need to become an IDE, a chat app, or an agent dashboard. It needs to stay a terminal — but one that's aware of what you're doing and stays out of your way until you need it.

## Core Principles

1. **Performance is non-negotiable.** The terminal must be the fastest thing on your screen. Sub-frame input latency. 5ms cold start. Plugins never block the render loop. If a feature makes typing feel slower, it doesn't ship.
2. **Extensible by design.** The core is a fast, minimal rendering and multiplexing engine. Everything else — the sidebar, agent management, smart autocomplete, even the browser — is a plugin. The core's job is to provide the right primitives so plugins can build anything.
3. **Secure by default.** Terminals handle passwords, API keys, and production access. Escape sequence injection is mitigated, plugin permissions are capability-based, clipboard reads are blocked by default, and the Lua config sandbox has no shell access. Security defaults are strict and relaxable, never the other way around.
4. **Abstracted at every seam.** The core defines traits/interfaces, not concrete implementations. GPU backend, WASM runtime, text shaper, platform layer — all behind abstractions. If something better comes along, swap it in without rewriting the terminal.
5. **Accessible from day one.** Zero modern GPU-rendered terminals have functional screen reader support. We ship with AccessKit integration, a full accessibility tree, high-contrast themes, and keyboard-only navigation. Accessibility is in the core, not a plugin.
6. **Everything is a pane.** Agents, dev servers, test watchers, web views, shells — they're all panes. The terminal just knows a little more about each one.
7. **The plugin is the product.** We don't ship features — we ship a platform and a set of bundled plugins. If a plugin can't do something, the answer is to improve the core API, not to hardcode the feature.
8. **Debugging is built in.** `:debug` gives you full diagnostics — input, escape sequences, plugin state, performance, per-pane info. `oakterm doctor` checks your environment. When something breaks, you shouldn't have to guess.

## Principles

- Zero telemetry. No login. No account. No phoning home. Ever.
- AI features are opt-in, BYOK, and work with local models.
- Open protocols — Kitty graphics, standard escape sequences, WASM plugins.
- Platform-native — AppKit on macOS, GTK on Linux, WinUI on Windows. All three from day one. Not Electron.
- Replaces tmux, not complements it.
- Memory-conscious — tiered scroll buffer, per-pane budgets, no pre-allocation.
- MPL 2.0 licensed — core stays open source, plugins can be any license.

## Documentation

```text
ideas/          Exploration — brainstorming, research, design sketches
docs/adrs/      Decisions — resolve open questions from ideas
docs/specs/     Contracts — formal definitions that code must satisfy
```

Ideas explore possibilities. ADRs resolve questions that ideas surface. Specs formalize decided designs into implementation contracts.

## Idea Docs

### Core

| Doc                                                             | Topic                                                                            |
| --------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| [Architecture](docs/ideas/01-architecture.md)                   | Layer stack, Rust, server/client model                                           |
| [Renderer](docs/ideas/02-renderer.md)                           | GPU (wgpu), fonts, fallbacks, ligatures, opacity, color, images                  |
| [Multiplexer](docs/ideas/03-multiplexer.md)                     | Workspaces, splits, floating panes, SSH domains, session persistence             |
| [Command Palette](docs/ideas/08-command-palette.md)             | Unified fuzzy launcher with prefix filters                                       |
| [Configuration](docs/ideas/09-config.md)                        | First launch setup, settings palette, flat + Lua, dark/light themes              |
| [Abstraction Layer](docs/ideas/13-abstraction.md)               | Trait seams for swappable backends across all platforms                          |
| [Shell Integration](docs/ideas/18-shell-integration.md)         | Prompt markers, semantic zones, scroll-to-prompt, notifications                  |
| [Smart Keybinds](docs/ideas/19-smart-keybinds.md)               | Context-aware Ctrl+C/V, hints mode, input broadcast, env coloring                |
| [Health Check](docs/ideas/28-health-check.md)                   | Neovim-style `:health` with actionable diagnostics                               |
| [Terminal Fundamentals](docs/ideas/36-terminal-fundamentals.md) | Cursor, bell, scrollbar, padding, text styles, env vars, links, process handling |

### Features (Bundled Plugins)

| Doc                                                   | Topic                                                              |
| ----------------------------------------------------- | ------------------------------------------------------------------ |
| [Sidebar](docs/ideas/04-sidebar.md)                   | Collapsible process dashboard — agents, services, watchers, shells |
| [Context Engine](docs/ideas/05-context-engine.md)     | Smart autocomplete, typed completions, `?` NL commands             |
| [Agent Management](docs/ideas/07-agent-management.md) | Worktree lifecycle, notifications, `:merge` / `:diff`              |
| [Harpoon](docs/ideas/27-harpoon.md)                   | Pane bookmarks — Ctrl+1-6 direct jump, editable list               |

### Cross-Cutting Concerns

| Doc                                                     | Topic                                                              |
| ------------------------------------------------------- | ------------------------------------------------------------------ |
| [Plugin System](docs/ideas/06-plugins.md)               | WASM runtime, API primitives, capabilities, registry, manager      |
| [Performance](docs/ideas/12-performance.md)             | Targets, budgets, CI benchmarks                                    |
| [Memory Management](docs/ideas/15-memory-management.md) | Tiered scroll buffer, per-pane budgets, memory attribution         |
| [Debugging](docs/ideas/14-debugging.md)                 | `:debug` commands, plugin profiling, blame chain                   |
| [Security](docs/ideas/21-security.md)                   | Escape injection, plugin sandbox, secure input, clipboard controls |
| [Accessibility](docs/ideas/17-accessibility.md)         | AccessKit, screen reader, color blindness, extensible a11y API     |
| [Theming](docs/ideas/22-theming.md)                     | Deep customization, TOML format, inheritance, live preview         |
| [Internationalization](docs/ideas/23-i18n.md)           | Unicode rendering, locale packs via plugins                        |
| [Platform Support](docs/ideas/20-platform-support.md)   | macOS, Linux, Windows — all first-class                            |
| [Updates](docs/ideas/24-updates.md)                     | Every update path works, staged updates, rollback                  |
| [Testing](docs/ideas/25-testing.md)                     | Unit, integration, platform, perf, security, a11y, VT compliance   |
| [License](docs/ideas/26-license.md)                     | MPL 2.0 — core stays open, registry requires open source           |
| [Notifications](docs/ideas/34-notifications.md)         | OS notifications, in-terminal banners, history, DND mode           |
| [Search](docs/ideas/35-search.md)                       | Regex search, cross-pane, per-command, persistent highlights       |
| [Conventions](docs/ideas/30-conventions.md)             | Naming, config syntax, keybinds, file structure                    |
| [Agent Control API](docs/ideas/32-agent-control-api.md) | CLI for agents to interact with the terminal (`oakterm ctl`)       |

### Remote & Headless

| Doc                                                             | Topic                                                                             |
| --------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| [Remote Access & Headless Mode](docs/ideas/29-remote-access.md) | Headless daemon on servers, native client connection, web client, tunnel-agnostic |

### Planning

| Doc                                       | Topic                                                                                  |
| ----------------------------------------- | -------------------------------------------------------------------------------------- |
| [Roadmap](docs/ideas/33-roadmap.md)       | Phased implementation: MVP → multiplexer → plugins → agents → networking               |
| [Brainstorm](docs/ideas/31-brainstorm.md) | Raw ideas: syntax highlighting, auto-tiling, status bar, multi-sidebar, error handling |

### Research

| Doc                                                     | Topic                                       |
| ------------------------------------------------------- | ------------------------------------------- |
| [Pain Points](docs/ideas/10-pain-points.md)             | Community complaints that shaped the design |
| [Inspiration](docs/ideas/11-inspiration.md)             | What we took (and left) from each terminal  |
| [Wishlist Features](docs/ideas/16-wishlist-features.md) | Community-requested features, prioritized   |
