# Abstraction Layer

The core defines interfaces, not implementations. Every major subsystem sits behind a trait so it can be swapped without rewriting the terminal.

## Seams

```
Core
├── trait GpuBackend        → wgpu (default), raw Metal, raw Vulkan, software
├── trait TextShaper         → Core Text (macOS), HarfBuzz (Linux), custom
├── trait FontRasterizer     → Core Text (macOS), FreeType (Linux)
├── trait PlatformShell      → AppKit (macOS), GTK (Linux), future: Windows
├── trait PluginRuntime      → Wasmtime (default), Wasmer, WasmEdge
├── trait VtParser           → Built-in (default), custom/third-party
├── trait ScrollBuffer       → Ring buffer (default), memory-mapped, disk-backed
├── trait SshTransport       → russh (default), libssh2, custom
├── trait ConfigLoader       → Flat file, Lua, both
└── trait ClipboardProvider  → Platform native, OSC-52, custom
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

A Windows port doesn't require rethinking the architecture — it implements:
- `PlatformShell` → Win32/WinUI
- `TextShaper` → DirectWrite
- `FontRasterizer` → DirectWrite
- `ClipboardProvider` → Win32 clipboard

Everything else (multiplexer, plugins, config, VT parser) stays the same.

## Rules

1. **No subsystem calls another subsystem's concrete type.** Always go through the trait.
2. **Defaults are compile-time features, not runtime switches.** You pick your backends at build time. No runtime dispatch overhead in the hot path.
3. **Traits are narrow.** A `GpuBackend` doesn't know about fonts. A `TextShaper` doesn't know about the GPU. Keep interfaces focused.
4. **Don't abstract prematurely.** If there's only one possible implementation today and no clear reason for a second, skip the trait. Add it when the second implementation appears. The seams listed above are ones where alternatives already exist or are clearly coming.
