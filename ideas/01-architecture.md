---
title: 'Architecture'
status: draft
category: core
description: 'Layer stack, Rust, server/client model'
tags: ['rust', 'wgpu', 'server-client', 'layer-stack']
---

# Architecture

## Layer Stack

```text
┌───────────────────────────────────────────────────────┐
│  Native Platform Shell (AppKit / GTK4 / WinUI 3)      │
├───────┬───────────────────────────────────────────────┤
│       │  GPU Renderer (wgpu)                          │
│ Side- │  - Platform-native text shaping               │
│ bar   │  - Kitty + Sixel graphics protocols           │
│ (plug │  - Glyph atlas (shared across windows)        │
│  -in) │───────────────────────────────────────────────│
│       │  VT Parser                                    │
│ Ctrl+B│  - xterm/VT220 compatibility                  │
│ toggle│  - Shell integration markers (OSC 133/7)      │
│       │───────────────────────────────────────────────│
│       │  Multiplexer                                  │
│       │  - Splits, tabs, workspaces, floating panes   │
│       │  - SSH domains (core — deep mux integration)  │
│       │  - Session persistence & restore              │
│       │  - Scroll buffer (ring + disk archive)        │
│       │───────────────────────────────────────────────│
│       │  Accessibility (AccessKit)                    │
│       │  - A11y tree alongside renderer               │
│       │  - VoiceOver / NVDA / Orca                    │
│       │───────────────────────────────────────────────│
│       │  Extension Runtime                            │
│       │  - Lua config engine                          │
│       │  - WASM plugin host (Wasmtime)                │
│       │  - Plugin API primitives                      │
├───────┴───────────────────────────────────────────────┤
│  Bundled Plugins                                      │
│  - sidebar-ui, agent-manager, context-engine          │
│  - service-monitor, watcher, harpoon, browser-lite    │
│  - kitty-graphics, quake-mode                         │
└───────────────────────────────────────────────────────┘
```

## What's Core vs Plugin

The line is simple: **core provides primitives, plugins compose them into features.**

| Core (ships in the binary)             | Plugin (WASM, can be disabled) |
| -------------------------------------- | ------------------------------ |
| Renderer, VT parser                    | Sidebar UI                     |
| Multiplexer (splits, tabs, workspaces) | Agent management               |
| SSH domains                            | Context engine / autocomplete  |
| Session persistence                    | Service monitor                |
| Shell integration parsing (OSC 133/7)  | Watcher                        |
| Scroll buffer (ring + disk archive)    | Harpoon (pane bookmarks)       |
| Accessibility tree (AccessKit)         | Quake/dropdown mode            |
| Plugin host + API primitives           | Browser (lite and webview)     |
| Config engine (flat + Lua)             | Kitty graphics rendering       |
| Health check (`:health`)               | Docker/k8s manager             |
| Clipboard (OSC-52 passthrough)         | Themes (data packages)         |
| Security (sandbox, escape filtering)   | Locale packs                   |
| Platform shell (AppKit/GTK4/WinUI)     | Remote access                  |

If a feature deeply integrates with the renderer, multiplexer, or VT parser — it's core. If it can be expressed as a combination of plugin API primitives — it's a plugin.

## Language

**Pure Rust.**

| Component          | Crate / Approach                                           |
| ------------------ | ---------------------------------------------------------- |
| GPU renderer       | `wgpu` (WebGPU — Metal/Vulkan/DX12)                        |
| Text shaping       | Core Text (macOS), HarfBuzz (Linux), DirectWrite (Windows) |
| Font rasterization | Core Text (macOS), FreeType (Linux), DirectWrite (Windows) |
| Async / networking | `tokio`                                                    |
| WASM plugin host   | `wasmtime`                                                 |
| Platform native    | `objc2` (AppKit), `gtk4-rs` (GTK4), `windows-rs` (WinUI 3) |
| VT parser          | Custom (based on `vte` crate)                              |
| Lua config         | `mlua`                                                     |
| SSH                | `russh`                                                    |
| Accessibility      | `accesskit`                                                |

### Why Rust

- Wasmtime (WASM runtime) is a Rust project — native integration, no FFI
- One language, one build system, one contributor pool
- Alacritty and WezTerm prove Rust can hit the latency targets
- Massive ecosystem for networking, SSH, async — things we'd have to write ourselves in Zig
- Ghostty already owns the "pure Zig terminal" space
- AccessKit (a11y library) is Rust-native

## Server/Client Architecture

Inspired by Foot. One daemon process, many terminal windows.

```text
oakterm-daemon (one process)
├── Glyph atlas + font cache (shared)
├── Plugin host (shared, one WASM runtime)
├── Config state (shared)
│
├── Window 1 (AppKit/GTK4/WinUI)
│   ├── Tab 1 → Pane A, Pane B
│   └── Tab 2 → Pane C
│
├── Window 2
│   └── Tab 1 → Pane D
│
└── Remote Access (optional, plugin)
    └── WebSocket API → Web client
```

Benefits:

- Shared glyph atlas — opening a second window doesn't duplicate font data
- Shared plugin state — sidebar, harpoon list, notifications are global
- Lower memory — each window costs a few MB, not a full process
- Session persistence is natural — the daemon owns all state

The daemon starts on first window open and exits when the last window closes (or stays alive if configured for remote access).

## Data Flow

```text
Keystroke → Platform Shell → Input Handler
                                │
                ┌───────────────┼───────────────┐
                ▼               ▼               ▼
           Smart Keybinds   VT Encoder      Plugin Events
           (copy-or-int)    (send to PTY)   (harpoon, hints)
                                │
                                ▼
                            Child Process
                            (shell, agent)
                                │
                                ▼
                          PTY Output
                                │
                                ▼
                          VT Parser
                          ┌─────┼─────────────────┐
                          ▼     ▼                 ▼
                     Screen   Shell Events     Escape Sequences
                     Buffer   (OSC 133/7)      (colors, cursor)
                          │     │
                          ▼     ▼
                     Renderer  Plugin Events
                          │    (scroll-to-prompt,
                          ▼     notifications)
                     GPU Frame
                          │
                          ▼
                     Display
```

## Related Docs

- [Abstraction Layer](13-abstraction.md) — trait seams for every swappable subsystem
- [Plugin System](06-plugins.md) — the full plugin API surface
- [Performance](12-performance.md) — targets and budgets for each layer
- [Platform Support](20-platform-support.md) — per-platform implementation details
