# Phantom Terminal

A GPU-accelerated, extensible terminal emulator with a plugin-driven process dashboard and context-aware shell.

> **Status: Idea Phase** — Collecting and refining ideas before any implementation. Name is a placeholder.

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
8. **Debugging is built in.** `:debug` gives you full diagnostics — input, escape sequences, plugin state, performance, per-pane info. `phantom doctor` checks your environment. When something breaks, you shouldn't have to guess.

## Principles

- Zero telemetry. No login. No account. No phoning home. Ever.
- AI features are opt-in, BYOK, and work with local models.
- Open protocols — Kitty graphics, standard escape sequences, WASM plugins.
- Platform-native — AppKit on macOS, GTK on Linux, WinUI on Windows. All three from day one. Not Electron.
- Replaces tmux, not complements it.
- Memory-conscious — tiered scroll buffer, per-pane budgets, no pre-allocation.
- MPL 2.0 licensed — core stays open source, plugins can be any license.

## Idea Docs

### Core

| Doc | Topic |
|-----|-------|
| [Architecture](ideas/01-architecture.md) | Layer stack, Rust, server/client model |
| [Renderer](ideas/02-renderer.md) | GPU (wgpu), fonts, fallbacks, ligatures, opacity, color, images |
| [Multiplexer](ideas/03-multiplexer.md) | Workspaces, splits, floating panes, SSH domains, session persistence |
| [Command Palette](ideas/08-command-palette.md) | Unified fuzzy launcher with prefix filters |
| [Configuration](ideas/09-config.md) | First launch setup, settings palette, flat + Lua, dark/light themes |
| [Abstraction Layer](ideas/13-abstraction.md) | Trait seams for swappable backends across all platforms |
| [Shell Integration](ideas/18-shell-integration.md) | Prompt markers, semantic zones, scroll-to-prompt, notifications |
| [Smart Keybinds](ideas/19-smart-keybinds.md) | Context-aware Ctrl+C/V, hints mode, input broadcast, env coloring |
| [Health Check](ideas/28-health-check.md) | Neovim-style `:health` with actionable diagnostics |

### Features (Bundled Plugins)

| Doc | Topic |
|-----|-------|
| [Sidebar](ideas/04-sidebar.md) | Collapsible process dashboard — agents, services, watchers, shells |
| [Context Engine](ideas/05-context-engine.md) | Smart autocomplete, typed completions, `?` NL commands |
| [Agent Management](ideas/07-agent-management.md) | Worktree lifecycle, notifications, `:merge` / `:diff` |
| [Harpoon](ideas/27-harpoon.md) | Pane bookmarks — Ctrl+1-6 direct jump, editable list |

### Cross-Cutting Concerns

| Doc | Topic |
|-----|-------|
| [Plugin System](ideas/06-plugins.md) | WASM runtime, API primitives, capabilities, registry, manager |
| [Performance](ideas/12-performance.md) | Targets, budgets, CI benchmarks |
| [Memory Management](ideas/15-memory-management.md) | Tiered scroll buffer, per-pane budgets, memory attribution |
| [Debugging](ideas/14-debugging.md) | `:debug` commands, plugin profiling, blame chain |
| [Security](ideas/21-security.md) | Escape injection, plugin sandbox, secure input, clipboard controls |
| [Accessibility](ideas/17-accessibility.md) | AccessKit, screen reader, color blindness, extensible a11y API |
| [Theming](ideas/22-theming.md) | Deep customization, TOML format, inheritance, live preview |
| [Internationalization](ideas/23-i18n.md) | Unicode rendering, locale packs via plugins |
| [Platform Support](ideas/20-platform-support.md) | macOS, Linux, Windows — all first-class |
| [Updates](ideas/24-updates.md) | Every update path works, staged updates, rollback |
| [Testing](ideas/25-testing.md) | Unit, integration, platform, perf, security, a11y, VT compliance |
| [License](ideas/26-license.md) | MPL 2.0 — core stays open, registry requires open source |
| [Conventions](ideas/30-conventions.md) | Naming, config syntax, keybinds, file structure |

### Community Plugins (designed for, not built by us)

| Doc | Topic |
|-----|-------|
| [Remote Access](ideas/29-remote-access.md) | WebSocket API, web client, tunnel-agnostic (Tailscale, Cloudflare, etc.) |

### Research

| Doc | Topic |
|-----|-------|
| [Pain Points](ideas/10-pain-points.md) | Community complaints that shaped the design |
| [Inspiration](ideas/11-inspiration.md) | What we took (and left) from each terminal |
| [Wishlist Features](ideas/16-wishlist-features.md) | Community-requested features, prioritized |
