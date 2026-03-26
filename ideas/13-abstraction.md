---
title: "Abstraction Layer"
status: draft
category: cross-cutting
description: "Trait seams for swappable backends"
tags: ["traits", "interfaces", "cross-platform", "testing"]
---
# Abstraction Layer


The core defines interfaces, not implementations. Every major subsystem sits behind a trait so it can be swapped without rewriting the terminal.

## Seams

```
Core
├── trait GpuBackend        → wgpu (default), raw Metal, raw Vulkan, software
├── trait TextShaper         → Core Text (macOS), HarfBuzz (Linux), DirectWrite (Windows)
├── trait FontRasterizer     → Core Text (macOS), FreeType (Linux), DirectWrite (Windows)
├── trait PlatformShell      → AppKit (macOS), GTK4 (Linux), WinUI 3 (Windows)
├── trait AccessibilityBridge → NSAccessibility (macOS), AT-SPI (Linux), UIA (Windows)
├── trait PluginRuntime      → Wasmtime (default), Wasmer, WasmEdge
├── trait VtParser           → Built-in (default), custom/third-party
├── trait ScrollBuffer       → Ring buffer (default), memory-mapped, disk-backed
├── trait SshTransport       → russh (default), libssh2, custom
├── trait ConfigLoader       → Flat file, Lua, both
├── trait ClipboardProvider  → NSPasteboard (macOS), Wayland/X11 (Linux), Win32 (Windows), OSC-52
└── trait NotificationProvider → NSUserNotification (macOS), libnotify (Linux), Windows Toast
```

## Why This Matters

### Today: sensible defaults

Ship with wgpu, Wasmtime, russh, Core Text/HarfBuzz. These are the best choices right now.

### Tomorrow: swap without rewriting

- A faster WASM runtime appears? Implement `trait PluginRuntime`, swap it in.
- WebGPU spec changes and a better GPU crate emerges? Implement `trait GpuBackend`.
- Someone wants a software renderer for headless/CI use? Implement `trait GpuBackend` with CPU rasterization.
- Windows port needs DirectWrite for text? Implement `trait TextShaper` + `trait FontRasterizer`.
- Want disk-backed scroll buffer for massive logs? Implement `trait ScrollBuffer`.

No feature code changes. Just the backend.

### Testing

Abstractions enable testing without real hardware:
- Mock `GpuBackend` for CI — verify frame output without a GPU
- Mock `PlatformShell` for headless testing
- Mock `SshTransport` for multiplexer integration tests
- Test plugins against a mock `PluginRuntime`

### Platform ports

All three platforms implement the same traits:

| Trait | macOS | Linux | Windows |
|-------|-------|-------|---------|
| `PlatformShell` | AppKit | GTK4 | WinUI 3 |
| `TextShaper` | Core Text | HarfBuzz | DirectWrite |
| `FontRasterizer` | Core Text | FreeType | DirectWrite |
| `AccessibilityBridge` | NSAccessibility | AT-SPI | UIA |
| `ClipboardProvider` | NSPasteboard | Wayland/X11 | Win32 |
| `NotificationProvider` | NSUserNotification | libnotify | Windows Toast |

Everything above these traits (multiplexer, plugins, config, VT parser) is shared cross-platform code.

## Rules

1. **No subsystem calls another subsystem's concrete type.** Always go through the trait.
2. **Defaults are compile-time features, not runtime switches.** You pick your backends at build time. No runtime dispatch overhead in the hot path.
3. **Traits are narrow.** A `GpuBackend` doesn't know about fonts. A `TextShaper` doesn't know about the GPU. Keep interfaces focused.
4. **Don't abstract prematurely.** If there's only one possible implementation today and no clear reason for a second, skip the trait. Add it when the second implementation appears. The seams listed above are ones where alternatives already exist or are clearly coming.

## Related Docs

- [Architecture](01-architecture.md) — layer stack built on these traits
- [Platform Support](20-platform-support.md) — per-platform trait implementations
- [Remote Access](29-remote-access.md) — Null implementations for headless mode
