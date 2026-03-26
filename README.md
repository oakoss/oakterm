# Phantom Terminal

A GPU-accelerated, extensible terminal emulator with a plugin-driven process dashboard and context-aware shell.

> **Status: Idea Phase** — Collecting and refining ideas before any implementation.

## Philosophy

The terminal is the oldest developer tool that still works. It doesn't need to become an IDE, a chat app, or an agent dashboard. It needs to stay a terminal — but one that's aware of what you're doing and stays out of your way until you need it.

## Core Principles

1. **Performance is non-negotiable.** The terminal must be the fastest thing on your screen. Sub-frame input latency. 5ms cold start. Plugins never block the render loop. If a feature makes typing feel slower, it doesn't ship.
2. **Extensible by design.** The core is a fast, minimal rendering and multiplexing engine. Everything else — the sidebar, agent management, smart autocomplete, even the browser — is a plugin. The core's job is to provide the right primitives so plugins can build anything.
3. **Abstracted at every seam.** The core defines traits/interfaces, not concrete implementations. GPU backend, WASM runtime, text shaper, platform layer — all behind abstractions. If something better comes along, swap it in without rewriting the terminal.
4. **Everything is a pane.** Agents, dev servers, test watchers, web views, shells — they're all panes. The terminal just knows a little more about each one.
5. **The plugin is the product.** We don't ship features — we ship a platform and a set of bundled plugins. If a plugin can't do something, the answer is to improve the core API, not to hardcode the feature.

6. **Debugging is built in.** `:debug` gives you full diagnostics — input, escape sequences, plugin state, performance, per-pane info. `phantom doctor` checks your environment. When something breaks, you shouldn't have to guess.

## Principles

- Zero telemetry. No login. No account. No phoning home. Ever.
- AI features are opt-in, BYOK, and work with local models.
- Open protocols — Kitty graphics, standard escape sequences, WASM plugins.
- Platform-native — AppKit on macOS, GTK on Linux. Not Electron.
- Replaces tmux, not complements it.
