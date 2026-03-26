# Phantom Terminal

A GPU-accelerated, extensible terminal emulator with a plugin-driven process dashboard and context-aware shell.

> **Status: Idea Phase** — Collecting and refining ideas before any implementation.

## Philosophy

The terminal is the oldest developer tool that still works. It doesn't need to become an IDE, a chat app, or an agent dashboard. It needs to stay a terminal — but one that's aware of what you're doing and stays out of your way until you need it.

1. **Extensible by design.** The core is a fast, minimal rendering and multiplexing engine. Everything else — the sidebar, agent management, smart autocomplete, even the browser — is a plugin. The core's job is to provide the right primitives so plugins can build anything.
2. **Boots fast, stays fast.** Sub-frame input latency. 5ms cold start with zero config. The renderer never blocks on plugins, network, or AI.
3. **Everything is a pane.** Agents, dev servers, test watchers, web views, shells — they're all panes. The terminal just knows a little more about each one.
4. **The plugin is the product.** We don't ship features — we ship a platform and a set of bundled plugins. If a plugin can't do something, the answer is to improve the core API, not to hardcode the feature.

## Principles

- **Extensibility is the architecture.** The core exposes primitives (panes, surfaces, metadata, notifications, palette commands). Plugins compose them into features. The sidebar, context engine, and agent manager are all plugins that ship in the box — not special-cased core features.
- Zero telemetry. No login. No account. No phoning home. Ever.
- AI features are opt-in, BYOK, and work with local models.
- Open protocols — Kitty graphics, standard escape sequences, WASM plugins.
- Platform-native — AppKit on macOS, GTK on Linux. Not Electron.
- Replaces tmux, not complements it.
