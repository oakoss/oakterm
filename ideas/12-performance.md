# Performance

Performance is a core principle, not an optimization pass. Every architectural decision is made with latency and resource usage as constraints.

## Targets

| Metric | Target | Reference |
|--------|--------|-----------|
| Input latency (keystroke → pixel) | <8ms | Foot ~7ms, Alacritty ~10ms |
| Cold start (no plugins) | <5ms | Alacritty-class |
| Cold start (all bundled plugins) | <15ms | Faster than WezTerm |
| Memory (single empty pane) | <30MB | Foot ~15MB, Alacritty ~80MB |
| Memory (10 panes, sidebar, plugins) | <100MB | Less than WezTerm (~170MB) |
| Idle CPU | 0% | No polling, no timers, pure event-driven |
| Scrollback (100k lines) | No typing lag | iTerm2 fails this |

These are budgets, not aspirations. CI runs performance benchmarks on every commit. A regression fails the build.

## Architecture Decisions Driven by Performance

### The renderer never waits

The GPU render loop is the highest-priority thread. Nothing blocks it:
- Plugin responses are async — if a plugin is slow, the frame renders without its data
- Context engine runs in a separate process — completions arrive when ready
- Font fallback lookups are cached in the glyph atlas after first resolution
- Config changes hot-reload without restarting the render loop

### Server/client architecture (from Foot)

One daemon process, many terminal windows. The glyph atlas and font cache are shared across all windows. Opening a second window costs memory for the PTY and scroll buffer — not another copy of every rendered glyph.

### Plugins run in their own threads

WASM plugins execute on a thread pool, never on the render thread. The plugin host communicates with the renderer via a lock-free message queue. A slow or blocked plugin cannot cause a frame drop.

### Lazy plugin loading

Plugins aren't loaded until first use or first relevant event. The `docker-manager` plugin doesn't load until you're in a directory with a `docker-compose.yml` or you run `:docker`. This keeps cold start fast regardless of how many plugins are installed.

### Zero-copy scroll buffer

The scroll buffer is a ring buffer with zero-copy access for rendering. Scrolling through 100k lines of output doesn't allocate or copy — it adjusts a viewport offset.

### GPU text rendering

Text is rendered via a glyph atlas on the GPU. Each frame uploads only new/changed glyphs. Static text (most of the screen, most of the time) costs zero CPU per frame.

## What We Measure

Automated benchmarks on every PR:

- **Input latency** — time from keypress event to pixel change on screen (typometer-style)
- **Throughput** — bytes/second for raw output (cat large file)
- **Memory** — RSS at idle, after 1k lines, after 100k lines, with N panes
- **Startup** — cold start to first frame, with and without plugins
- **Scroll performance** — FPS while scrolling through large scroll buffer
- **Plugin overhead** — frame time delta with 0, 5, 10 active plugins

Results are tracked over time. Performance dashboard is public.

## Performance Anti-Patterns We Avoid

| Anti-pattern | Who does it | Our approach |
|-------------|-------------|-------------|
| Electron/web rendering | Hyper, Tabby | Native GPU rendering |
| No GPU acceleration | iTerm2 | wgpu from day one |
| Polling for events | Various | Pure event-driven (epoll/kqueue) |
| Single-threaded plugin execution | Kitty (Python GIL) | Thread pool + WASM |
| Unbounded scroll buffer allocation | iTerm2 (3GB RSS) | Ring buffer with configurable cap |
| Loading all plugins at startup | — | Lazy loading on first use |
| Synchronous font fallback | — | Cached in glyph atlas, async resolve |
| Full redraw on every frame | — | Damage tracking, only redraw changed regions |

## Performance vs Features

When performance and features conflict, performance wins. Specific rules:

1. **No feature may add >0.5ms to input latency.** If it does, it runs async or it doesn't ship.
2. **No plugin can block the render thread.** The API makes this architecturally impossible — plugins communicate via async messages.
3. **Idle means idle.** 0% CPU when nothing is happening. No background polling, no animation timers when nothing is animating.
4. **Memory scales with content, not features.** 10 installed but inactive plugins should cost near-zero memory. Only loaded plugins consume resources.
5. **Benchmarks are tests.** A performance regression is a bug, same as a crash.
