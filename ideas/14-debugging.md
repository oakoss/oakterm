# Debugging

Debugging is a first-class citizen. When something goes wrong — in the terminal, in a plugin, in a pane — you shouldn't have to guess.

## Terminal Self-Diagnostics

### :debug command

```
Cmd+Shift+P → :debug

┌──────────────────────────────────────────────────┐
│  Phantom Debug                                   │
├──────────────────────────────────────────────────┤
│  Version          0.5.0                          │
│  GPU Backend      wgpu (Metal)                   │
│  Text Shaper      Core Text                      │
│  Plugin Runtime   Wasmtime 25.0                  │
│  WASM Plugins     5 loaded, 3 active             │
│  Panes            4 open (2 shell, 2 agent)      │
│  Memory           62MB RSS                       │
│  Render FPS       120 (vsync)                    │
│  Input Latency    6.2ms avg (last 100 frames)    │
│  Scroll Buffer    12.4MB across all panes        │
│  TERM             xterm-256color                 │
│  COLORTERM        truecolor                      │
│  Shell            /bin/zsh                       │
│  Config           ~/.config/phantom/config.lua   │
│                                                  │
│  [Copy to Clipboard]  [Open Debug Log]           │
└──────────────────────────────────────────────────┘
```

One command, full picture. Paste it in a bug report and we have everything we need.

### :debug memory

Full memory attribution — see `ideas/15-memory-management.md` for the complete spec.

```
┌──────────────────────────────────────────────────┐
│  Memory Overview                                 │
├──────────────────────────────────────────────────┤
│  Terminal (ours)              48 MB              │
│    Renderer / glyph atlas     12 MB              │
│    Scroll buffers (all panes) 18 MB              │
│    Plugin runtime              8 MB              │
│    Multiplexer state           2 MB              │
│    Other                       8 MB              │
│                                                  │
│  Child Processes (theirs)    1.2 GB              │
│    ◉ feat/auth (claude)     890 MB  ⚠ growing   │
│    ▶ next dev                142 MB              │
│    👁 vitest --watch           68 MB              │
│    ● scratch (zsh)            14 MB              │
│                                                  │
│  Total system impact         1.25 GB             │
└──────────────────────────────────────────────────┘
```

Clearly separates terminal memory from child process memory — answers "is it us or them?" instantly.

### :debug pane

Per-pane diagnostics — focus a pane and run `:debug pane`:

```
┌──────────────────────────────────────────────────┐
│  Pane Debug: feat/auth                           │
├──────────────────────────────────────────────────┤
│  Type              agent (claude)                │
│  PID               48291                         │
│  Working Dir       ~/project/.worktrees/feat-auth│
│  Scroll Lines      2,481                         │
│  Scroll Buffer     1.2MB                         │
│  PTY Size          120x40                        │
│  Encoding          UTF-8                         │
│  Mouse Mode        SGR                           │
│  Bracketed Paste   enabled                       │
│  Attached Plugins  agent-manager, watcher        │
│  Last Output       2s ago                        │
│  Exit Code         (running)                     │
└──────────────────────────────────────────────────┘
```

### :debug plugins

```
┌──────────────────────────────────────────────────┐
│  Plugin Debug                                    │
├──────────────────────────────────────────────────┤
│  agent-manager     v1.0.0  loaded  2.1MB  active │
│    Avg response:   0.3ms                         │
│    Last event:     notify (feat/auth ❓)          │
│    Errors:         0                             │
│                                                  │
│  context-engine    v1.0.0  loaded  1.8MB  active │
│    Avg response:   1.2ms                         │
│    Last event:     completion (cd)               │
│    Errors:         0                             │
│                                                  │
│  docker-manager    v1.2.0  loaded  0.9MB  idle   │
│    Avg response:   0.8ms                         │
│    Last event:     sidebar update                │
│    Errors:         1 (network timeout 5m ago)    │
└──────────────────────────────────────────────────┘
```

See which plugins are slow, which are erroring, and how much memory each uses.

### :debug input

Live input debugger — shows exactly what the terminal receives and sends for each keystroke:

```
┌──────────────────────────────────────────────────┐
│  Input Debug (press keys, Esc to exit)           │
├──────────────────────────────────────────────────┤
│  Key: Ctrl+Shift+A                               │
│  Raw: \x01                                       │
│  Mod: ctrl+shift                                 │
│  Bound to: (none)                                │
│                                                  │
│  Key: →                                          │
│  Escape: \e[C                                    │
│  Bound to: cursor-right                          │
│                                                  │
│  Key: Cmd+Shift+P                                │
│  Bound to: command-palette                       │
└──────────────────────────────────────────────────┘
```

Essential for diagnosing keybind conflicts, escape sequence issues, and SSH passthrough problems.

### :debug escape

Live escape sequence debugger — shows what programs are sending to the terminal:

```
┌──────────────────────────────────────────────────┐
│  Escape Sequence Debug (watching active pane)    │
├──────────────────────────────────────────────────┤
│  ← \e[38;2;255;100;50m   Set fg: RGB(255,100,50)│
│  ← \e[1m                 Bold on                 │
│  ← \e[?25l               Cursor hide             │
│  ← \e]52;c;SGVsbG8=\a    OSC-52: clipboard set   │
│  ← \e[?1049h             Alt screen on            │
└──────────────────────────────────────────────────┘
```

Decoded in real time with human-readable descriptions. Critical for debugging programs that misbehave, theme issues, and clipboard problems.

## Logging

### Structured debug log

```
phantom --log-level=debug
```

Writes structured logs (JSON) to `~/.local/state/phantom/debug.log`:

```json
{"ts":"...","level":"debug","component":"renderer","msg":"frame","fps":120,"latency_ms":6.2}
{"ts":"...","level":"debug","component":"plugin","plugin":"agent-manager","msg":"event","event":"notify","pane":"feat/auth"}
{"ts":"...","level":"warn","component":"vt","msg":"unrecognized escape","seq":"\\e[?8888h"}
{"ts":"...","level":"error","component":"plugin","plugin":"docker-manager","msg":"network timeout","url":"unix:///var/run/docker.sock"}
```

Each log entry includes the component, so you can filter:

```
phantom --log-level=debug --log-filter=plugin
phantom --log-level=debug --log-filter=renderer
phantom --log-level=debug --log-filter=vt
```

### Plugin crash reporting

When a WASM plugin crashes (panic, OOM, infinite loop timeout):
- The plugin is killed, not the terminal
- A notification appears: "Plugin docker-manager crashed. [Restart] [Disable] [View Error]"
- The crash log includes the WASM stack trace
- The terminal continues running — no pane is lost

## Health Checks

### phantom doctor

CLI diagnostic that checks the environment:

```
$ phantom doctor

✓ GPU:            Metal (Apple M2 Max)
✓ Font:           JetBrains Mono found
✓ Fallback Font:  Symbols Nerd Font found
✗ Fallback Font:  Noto Color Emoji NOT found (emoji may not render)
✓ Shell:          /bin/zsh
✓ TERM:           xterm-256color
✓ Config:         ~/.config/phantom/config.lua (valid)
✓ Plugins:        5 installed, all valid WASM
⚠ Plugin:         docker-manager v1.2.0 (update available: v1.3.0)
✓ SSH:            ~/.ssh/config readable, 3 hosts
✓ Permissions:    PTY access OK
```

Run it when something's wrong, paste in a bug report.

## Performance Profiling

### :debug perf

Live performance overlay (like a game FPS counter):

```
┌──────────────────────┐
│ FPS: 120 │ 6.2ms avg │
│ GPU: 2.1ms │ CPU: 0.8ms│
│ Plugins: 0.3ms       │
│ Mem: 62MB             │
└──────────────────────┘
```

Toggle with `:debug perf` — shows in a corner, updates per frame. Quick way to see if something is causing frame drops.

### phantom benchmark

Automated performance test suite:

```
$ phantom benchmark

Input latency:     6.4ms avg / 9.1ms p99
Throughput:        1.2 GB/s (cat /dev/urandom | head -c 100M)
Cold start:        4.2ms (no plugins) / 11.8ms (all bundled)
Scroll FPS:        120 (100k lines buffer)
Memory idle:       28MB
Memory 10 panes:   74MB
Plugin overhead:   +0.2ms/frame (5 active plugins)

All targets met ✓
```

## Plugin Debugging & Performance Attribution

The goal: when something is slow, leaking memory, or broken — know instantly whether it's us or a plugin, and which plugin.

### :debug plugins (extended)

```
┌───────────────────────────────────────────────────────────────┐
│  Plugin Performance                                           │
├───────────────────────────────────────────────────────────────┤
│  Plugin             CPU/frame  Mem     Events/s  Errors  │
│  ──────────────────────────────────────────────────────────── │
│  agent-manager      0.12ms     2.1MB   4.2       0       │
│  context-engine     0.31ms     1.8MB   12.8      0       │
│  service-monitor    0.05ms     0.6MB   0.3       0       │
│  docker-manager     0.82ms ⚠  3.2MB   1.1       3 ⚠     │
│  harpoon            0.01ms     0.2MB   0.0       0       │
│  ──────────────────────────────────────────────────────────── │
│  Total plugins      1.31ms     7.9MB                      │
│  Core (no plugins)  4.89ms    40.1MB                      │
│  ──────────────────────────────────────────────────────────── │
│  Total              6.20ms    48.0MB                      │
│                                                               │
│  ⚠ docker-manager: 0.82ms/frame (budget: 0.5ms)              │
│    3 errors in last 5m (network timeout → unix socket)        │
│    [View Logs]  [Disable]  [Report Issue]                     │
└───────────────────────────────────────────────────────────────┘
```

Every plugin's CPU time, memory, event throughput, and error count — separated from core. At a glance you see: "docker-manager is using 0.82ms/frame and erroring — that's the problem, not the terminal."

### :debug plugin <name>

Deep dive into a single plugin:

```
:debug plugin docker-manager

┌───────────────────────────────────────────────────────────────┐
│  docker-manager v1.2.0                                        │
├───────────────────────────────────────────────────────────────┤
│  Status:          active                                      │
│  Loaded:          14m ago                                     │
│  Memory:          3.2MB (limit: 16MB)                         │
│  Memory trend:    ↑ +0.4MB in last 5m ⚠                      │
│                                                               │
│  CPU per frame:   0.82ms avg / 2.1ms p99                      │
│  Event loop:      1.1 events/sec                              │
│                                                               │
│  API Calls (last 5m):                                         │
│    sidebar.update      312 calls   0.05ms avg                 │
│    process.spawn         2 calls   1.20ms avg                 │
│    pane.output          48 calls   0.08ms avg                 │
│    network.request      24 calls   45ms avg  ⚠                │
│    notify                3 calls   0.01ms avg                 │
│                                                               │
│  Errors (last 5m):                                            │
│    12:04:32  network.request timeout (unix:///var/run/docker)  │
│    12:06:15  network.request timeout (unix:///var/run/docker)  │
│    12:08:44  network.request timeout (unix:///var/run/docker)  │
│                                                               │
│  Capabilities:                                                │
│    ✓ sidebar.section  ✓ process.spawn  ✓ notify               │
│    ✓ pane.create      ✓ fs.read        ✓ network              │
│                                                               │
│  [View Full Logs]  [Restart]  [Disable]  [Report Issue]       │
└───────────────────────────────────────────────────────────────┘
```

This tells the plugin author exactly what's wrong: network requests to the Docker socket are averaging 45ms and timing out. That's not our problem — it's their network call strategy.

### Plugin Performance Budgets

Each plugin gets a frame-time budget. Exceed it and we flag it:

```lua
-- config.lua (defaults, overridable per-plugin)
plugin_defaults = {
  frame_budget = "0.5ms",      -- max CPU time per frame
  memory_limit = "16MB",       -- max WASM linear memory
  event_timeout = "100ms",     -- max time to respond to an event
  error_threshold = 10,        -- errors in 5m before auto-disable warning
}

plugins = {
  -- Override for a specific plugin that needs more room
  ["context-engine"] = { frame_budget = "1ms" },
}
```

When a plugin exceeds its budget:
1. First: `⚠` indicator in `:debug plugins` and the perf overlay
2. Sustained: notification — "docker-manager is slowing down your terminal (0.82ms/frame, budget 0.5ms). [View Details] [Disable]"
3. Critical: if a plugin blocks for >1s, it's killed and restarted with a notification

### The Blame Chain

When the user notices something slow, the diagnostic path is:

```
User: "My terminal feels slow"
        │
        ▼
:debug perf  →  "6.2ms total, 1.3ms plugins, 4.9ms core"
        │
        ├── Plugins > 50% of frame time?
        │   YES → :debug plugins → identify the slow plugin
        │         → :debug plugin <name> → see which API calls are slow
        │         → Plugin author's problem
        │
        └── Core > 5ms?
            YES → Our problem
            → :debug → check renderer, scroll buffer, glyph atlas
            → phantom benchmark → identify which subsystem
```

Every step in this chain is a command the user can run. No guessing. Paste the output in a bug report and we (or the plugin author) know exactly where to look.

### Plugin Profiling CLI

For plugin authors during development:

```
phantom plugin profile docker-manager --duration 30s

Profiling docker-manager for 30s...

Results:
  Total CPU:         24.6ms over 3,600 frames (0.68ms/frame avg)
  Peak CPU:          4.2ms (frame 2,847 — sidebar.update after docker event)
  Memory start:      2.8MB
  Memory end:        3.2MB (+0.4MB, possible slow leak)
  API call breakdown:
    network.request:   73% of CPU time (avg 45ms per call)
    sidebar.update:    18% of CPU time (avg 0.05ms but 312 calls)
    pane.output:        6% of CPU time
    other:              3%

Recommendation: network.request calls are dominating. Consider caching
Docker state and polling less frequently.
```

### Plugin Logs

Every plugin can emit structured logs via the API:

```rust
// In plugin code
log::info!("Refreshing container list");
log::warn!("Docker socket timeout, retrying");
log::error!("Failed to connect to Docker daemon");
```

These are visible via:
- `phantom plugin logs docker-manager` — CLI, tail style
- `:debug plugin docker-manager` — in the palette, last N entries
- `~/.local/state/phantom/plugins/docker-manager/log` — on disk
- `:debug plugins` — error count summary

Logs are namespaced per-plugin. The core's logs and each plugin's logs are separate streams. No interleaving, no confusion about who logged what.

### Crash Attribution

When a WASM plugin crashes:
- The plugin is killed, not the terminal
- Notification: "Plugin docker-manager crashed. [Restart] [Disable] [View Error]"
- The crash log includes the WASM stack trace, mapped to source if debug symbols present
- The crash is logged with full context: which API call triggered it, memory state at crash, last N log entries
- Terminal continues running — no panes are lost

When the core crashes (shouldn't happen, but if it does):
- Crash report saved to `~/.local/state/phantom/crash.log`
- On next launch: "Phantom crashed. [View Crash Report] [Send Report]" (sending is opt-in, never automatic)
- Session restore offers to recover panes from before the crash
