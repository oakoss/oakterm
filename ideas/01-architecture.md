# Architecture

## Layer Stack

```
┌───────────────────────────────────────────────────────┐
│  Native Platform Shell (AppKit / GTK)                 │
├───────┬───────────────────────────────────────────────┤
│       │  GPU Renderer                                 │
│ Side- │  - Platform-native text shaping               │
│ bar   │  - Kitty + Sixel graphics protocols           │
│       │───────────────────────────────────────────────│
│ Ctrl+B│  Multiplexer                                  │
│ toggle│  - Splits, tabs, workspaces, floating panes   │
│       │  - SSH domains, session persistence           │
│       │───────────────────────────────────────────────│
│       │  Context Engine (plugin)                      │
│       │  - Smart autocomplete, ? NL commands          │
│       │───────────────────────────────────────────────│
│       │  Extension Runtime (Lua config + WASM plugins)│
├───────┴───────────────────────────────────────────────┤
│  Agent Lifecycle Manager (plugin)                     │
│  - Worktree create/cleanup, process supervision       │
│  - Notification routing, diff/merge shortcuts         │
└───────────────────────────────────────────────────────┘
```

## Language

**Pure Rust** is the pragmatic choice.

| Component | Crate / Approach |
|-----------|-----------------|
| GPU renderer | `wgpu` (WebGPU — Metal/Vulkan/DX12) |
| Text shaping | Core Text (macOS), HarfBuzz (Linux) |
| Async / networking | `tokio` |
| WASM plugin host | `wasmtime` |
| Platform native | `objc2` (AppKit), `gtk4-rs` (GTK) |
| VT parser | Custom or `vte` crate |
| Lua config | `mlua` |
| SSH | `russh` |

### Why Rust over Zig

- Wasmtime (WASM runtime) is a Rust project — native integration, no FFI
- One language, one build system, one contributor pool
- Alacritty and WezTerm prove Rust can hit the latency targets
- Massive ecosystem for networking, SSH, async — things we'd have to write ourselves in Zig
- Ghostty already owns the "pure Zig terminal" space

### Server/Client Architecture (from Foot)

One daemon process, many terminal windows. Shared font cache and glyph atlas across all windows. Dramatically lower memory when running multiple windows.
