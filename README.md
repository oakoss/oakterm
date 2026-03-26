# Phantom Terminal

A GPU-accelerated terminal emulator with a plugin-driven process dashboard and context-aware shell.

> **Status: Idea Phase** — Collecting and refining ideas before any implementation.

## Philosophy

The terminal is the oldest developer tool that still works. It doesn't need to become an IDE, a chat app, or an agent dashboard. It needs to stay a terminal — but one that's aware of what you're doing and stays out of your way until you need it.

1. **Boots fast, stays fast.** Sub-frame input latency. 5ms cold start with zero config. The renderer never blocks on plugins, network, or AI.
2. **Everything is a pane.** Agents, dev servers, test watchers, shells — they're all processes in panes. The terminal just knows a little more about each one.
3. **Every smart feature is a plugin.** The core ships lean. Bundled plugins add the intelligence. Disable what you don't want. Replace what you don't like.

## Principles

- Zero telemetry. No login. No account. No phoning home. Ever.
- AI features are opt-in, BYOK, and work with local models.
- Open protocols — Kitty graphics, standard escape sequences, WASM plugins.
- Platform-native — AppKit on macOS, GTK on Linux. Not Electron.
- Replaces tmux, not complements it.
