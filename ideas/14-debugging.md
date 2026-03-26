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

## Plugin Debugging

Plugins get their own debug tools:

- `phantom plugin logs docker-manager` — tail plugin logs
- `phantom plugin inspect docker-manager` — show capabilities, memory, state
- Plugins can emit structured logs via the plugin API, visible in `:debug plugins`
- WASM stack traces on crash, mapped to source if debug symbols are present
