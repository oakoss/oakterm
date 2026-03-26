---
title: "Health Check"
status: draft
category: core
description: "Neovim-style :health with actionable diagnostics"
tags: ["diagnostics", "health", "doctor", "plugin-health"]
---
# Health Check


A single `:health` command that runs every diagnostic and shows a complete picture. Inspired by Neovim's `:checkhealth`.

## Why Core, Not Plugin

Health checks need to verify the core itself — renderer, VT parser, platform integration, font loading, plugin runtime. A plugin can't diagnose problems in the system that hosts it. This lives in the core alongside `:debug`.

## :health

```
Cmd+Shift+P → :health

┌──────────────────────────────────────────────────────┐
│  Phantom Health Check                                │
├──────────────────────────────────────────────────────┤
│                                                      │
│  ## Core                                             │
│  ✓ Version           0.7.0 (up to date)              │
│  ✓ GPU Backend       wgpu (Metal)                    │
│  ✓ Text Shaper       Core Text                       │
│  ✓ VT Parser         built-in                        │
│  ✓ Plugin Runtime    Wasmtime 25.0                   │
│  ✓ Config            ~/.config/phantom/config (valid) │
│                                                      │
│  ## Performance                                      │
│  ✓ Input Latency     6.2ms avg (target: <8ms)        │
│  ✓ FPS               120 (vsync)                     │
│  ✓ Memory            48 MB terminal / 1.2 GB children│
│  ✓ Idle CPU          0%                              │
│  ⚠ Scroll Buffer     82% of memory budget            │
│                                                      │
│  ## Fonts                                            │
│  ✓ Primary           JetBrains Mono (loaded)         │
│  ✓ Fallback 1        Symbols Nerd Font (loaded)      │
│  ✗ Fallback 2        Noto Color Emoji (NOT FOUND)    │
│    → Install: brew install font-noto-color-emoji      │
│    → Or remove from fallback chain in config          │
│  ✓ Ligatures         enabled, 23 substitutions active│
│                                                      │
│  ## Shell Integration                                │
│  ✓ Shell             /bin/zsh                        │
│  ✓ Integration       loaded (auto)                   │
│  ✓ TERM              xterm-256color                  │
│  ✓ COLORTERM         truecolor                       │
│  ✓ Prompt markers    detected (OSC 133)              │
│                                                      │
│  ## Plugins                                          │
│  ✓ agent-manager     v1.0.0  healthy  0.12ms/frame   │
│  ✓ context-engine    v1.0.0  healthy  0.31ms/frame   │
│  ✓ service-monitor   v1.0.0  healthy  0.05ms/frame   │
│  ⚠ docker-manager    v1.2.0  3 errors in last 5m     │
│    → network timeout: unix:///var/run/docker.sock     │
│    → Is Docker running? Check: docker info            │
│  ✓ harpoon           v1.0.0  healthy  0.01ms/frame   │
│  ⚠ docker-manager    v1.2.0  update available (v1.3.0)│
│                                                      │
│  ## Accessibility                                    │
│  ✓ AccessKit         loaded                          │
│  ✓ Screen reader     not detected (tree: lazy mode)  │
│  ✓ System theme      dark                            │
│  ✓ Reduced motion    not requested                   │
│  ✓ High contrast     not requested                   │
│  ✓ Bundled themes    all pass WCAG AA                │
│                                                      │
│  ## Platform                                         │
│  ✓ OS                macOS 15.4 (Sequoia)            │
│  ✓ Display           Retina (2x), P3 color space     │
│  ✓ Clipboard         NSPasteboard (working)          │
│  ✓ Notifications     NSUserNotification (permitted)  │
│  ✓ Secure input      available                       │
│  ✓ Keyboard access   Full Keyboard Access: off       │
│                                                      │
│  ## SSH Domains                                      │
│  ✓ homelab           proxmox.local (reachable)       │
│  ✗ prod              prod.example.com (unreachable)  │
│    → Connection refused. Check: ssh prod.example.com  │
│                                                      │
│  ## Security                                         │
│  ✓ Bracketed paste   enabled                         │
│  ✓ Clipboard read    blocked                         │
│  ✓ Title reporting   blocked                         │
│  ✓ Lua sandbox       restricted (no os/io)           │
│  ✓ Plugin checksums  all verified                    │
│  ✓ No sideloaded plugins                             │
│                                                      │
│  Summary: 26 passed, 3 warnings, 1 error             │
│                                                      │
│  [Copy Report]  [Open Full Log]                      │
└──────────────────────────────────────────────────────┘
```

## Key Design Decisions

### Actionable

Every warning and error includes:
- What's wrong (the symptom)
- Why it matters (what breaks)
- How to fix it (specific command or config change)

No cryptic error codes. No "check the docs." The fix is right there.

### Sections

| Section | What it checks |
|---------|---------------|
| Core | Version, GPU, text shaping, VT parser, plugin runtime, config validity |
| Performance | Input latency, FPS, memory, idle CPU, scroll buffer usage |
| Fonts | Primary + all fallbacks loaded, ligature support |
| Shell Integration | Shell detected, integration loaded, TERM/COLORTERM set, prompt markers |
| Plugins | Health, performance budget, errors, available updates |
| Accessibility | AccessKit status, screen reader detection, system preferences, theme contrast |
| Platform | OS version, display, clipboard, notifications, secure input |
| SSH Domains | Connectivity test for each configured domain |
| Security | All security settings, plugin verification |

### Plugin Health

Plugins can register their own health checks via the API:

```rust
health.register(HealthCheck {
    name: "Docker connection",
    check: || {
        // Try connecting to Docker socket
        // Return Ok or Err with message
    },
    fix_hint: "Is Docker running? Check: docker info",
});
```

This means the docker-manager plugin can check if Docker is actually running, the agent-manager can verify git is installed, etc.

### Relationship to Existing Debug Commands

| Command | Purpose |
|---------|---------|
| `:health` | Full diagnostic — "is everything working?" Run when something's wrong. |
| `:debug` | System info — "what's my current state?" Quick reference. |
| `:debug perf` | Live overlay — "is it slow right now?" Real-time monitoring. |
| `:debug memory` | Memory attribution — "what's using RAM?" |
| `:debug plugins` | Plugin performance — "which plugin is slow?" |
| `:debug plugin X` | Deep dive — "why is this plugin slow?" |
| `:debug input` | Input inspector — "what did I press?" |
| `:debug escape` | Escape inspector — "what's the program sending?" |
| `:debug security` | Security state — "what's locked down?" |
| `phantom doctor` | CLI-only health check — same as `:health` but for outside the terminal |

`:health` is the "run everything" option. The specific `:debug` commands are for targeted investigation after `:health` tells you where to look.

### CLI Equivalent

```
$ phantom doctor

Same output as :health, but on the command line.
Useful when the terminal itself won't start.
```

`phantom doctor` and `:health` run the same checks, produce the same output. One is for inside the terminal, one is for outside.

## Related Docs

- [Debugging](14-debugging.md) — relationship between `:health` and `:debug` commands
- [Plugin System](06-plugins.md) — plugins register custom health checks
- [Testing](25-testing.md) — health checks verified in CI
- [Security](21-security.md) — security section in health output
