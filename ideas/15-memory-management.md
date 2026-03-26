---
title: "Memory Management"
status: draft
category: cross-cutting
description: "Tiered scroll buffer, per-pane budgets, memory attribution"
tags: ["memory", "scroll-buffer", "ring-buffer", "disk-archive", "agents"]
---
# Memory Management


Memory is the #1 complaint in terminal + AI agent workflows. Ghostty hit 71 GB with Claude Code. iTerm2 routinely sits at 3 GB. WezTerm pre-allocates scrollback it may never use. This is a problem we solve from day one.

## The Two Memory Problems

There are two separate memory consumers and users conflate them:

1. **Terminal process** — our memory (renderer, scrollback buffer, glyph atlas, plugins)
2. **Child processes** — their memory (Claude Code, dev servers, shells)

We need to clearly separate and surface both.

### Memory attribution in :debug

```
Cmd+Shift+P → :debug memory

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
│    ● scratch (zsh)            12 MB              │
│                                                  │
│  Total system impact         1.25 GB             │
└──────────────────────────────────────────────────┘
```

This answers "is my terminal leaking or is Claude Code leaking?" instantly. The sidebar can also show per-pane memory in the expanded view.

### Memory alerts

When a child process grows abnormally:

```
⚠ feat/auth (claude) is using 890 MB and growing at ~18 MB/min
  [Ignore] [Restart Process] [Kill]
```

Configurable thresholds:

```lua
memory = {
  alert_threshold = "2GB",     -- warn when a child process exceeds this
  alert_growth_rate = "50MB/min", -- warn on sustained growth
}
```

## Scroll Buffer Strategy

The scroll buffer is the biggest terminal-side memory consumer. Every terminal gets this wrong in a different way.

| Terminal | Problem |
|----------|---------|
| iTerm2 | Unlimited scrollback = unbounded memory growth |
| WezTerm | Pre-allocates full scrollback on tab open |
| Ghostty pre-1.3 | Non-standard page leak under heavy Unicode output |
| Kitty | 64K scrollback = ~350 MB |

### Our approach: tiered scroll buffer

```
Active region (in memory)
├── Last N lines (configurable, default 10,000)
├── Ring buffer — zero-copy, fixed memory ceiling
├── Lazy allocation — memory grows with content, not config
└── Budget: ~2.5 KB per 1,000 lines (200 columns)

Archive region (on disk)
├── Older lines compressed and written to disk
├── Memory-mapped for fast access when scrolling back
├── Configurable: unlimited history with bounded memory
└── Transparent — scrolling back into archived region loads seamlessly
```

Flat config:
```
scrollback-memory-lines = 10000
scrollback-archive = true
scrollback-archive-path = ~/.local/state/phantom/scrollback/
scrollback-archive-max = 1GB
scrollback-compress = true
```

Lua config:
```lua
scrollback = {
  memory_lines = 10000,         -- kept in RAM (ring buffer)
  archive = true,               -- overflow goes to disk
  archive_path = "~/.local/state/phantom/scrollback/",
  archive_max = "1GB",          -- total disk budget across all panes
  compress = true,              -- zstd compression on archived lines
}
```

This gives you effectively unlimited scrollback with bounded memory. The ring buffer has a hard ceiling. Old content goes to disk, compressed.

### Per-pane scroll budgets

Agent panes produce way more output than interactive shells. Different panes can have different limits:

```lua
-- Agent panes get less in-memory scrollback since they produce tons of output
pane_defaults = {
  agent = { memory_lines = 5000 },
  shell = { memory_lines = 10000 },
  service = { memory_lines = 2000 },
  watcher = { memory_lines = 1000 },
}
```

### Blank line compression

Ghostty stores ~12.5 bytes per cell, even for blank cells. A 200-column terminal with 10K lines of mostly-empty scrollback wastes megabytes on whitespace.

Our approach: run-length encode blank regions. A line of 200 spaces costs one entry, not 200 cells.

## Glyph Atlas Management

The shared glyph atlas (from the server/client architecture) needs bounds:

- LRU eviction for glyphs not rendered in the last N frames
- Maximum atlas size configurable (default: 64 MB)
- Nerd Font symbols and emoji cached separately from text glyphs
- Font fallback resolution cached permanently (codepoint → font mapping)

## Plugin Memory Budgets

WASM plugins run in sandboxed linear memory. Each plugin has a configurable ceiling:

```lua
plugins = {
  ["agent-manager"] = { enabled = true, memory_limit = "32MB" },
  ["docker-manager"] = { enabled = true, memory_limit = "16MB" },
}
```

If a plugin exceeds its budget, it's killed and restarted (or disabled with a notification). The terminal never goes down because a plugin leaked.

## What We Guarantee

| Metric | Guarantee |
|--------|-----------|
| Terminal idle memory | <30 MB (no plugins), <50 MB (all bundled) |
| Memory per empty pane | <1 MB |
| Scrollback memory ceiling | Hard cap via ring buffer size |
| Plugin memory | Capped per-plugin via WASM linear memory |
| No pre-allocation | Memory grows with actual content, never with config values |
| Glyph atlas ceiling | LRU eviction, configurable max |
| Zero memory leaks | Continuous fuzzing + leak detection in CI |

## CI Memory Tests

Every PR runs:
- Valgrind / AddressSanitizer on the core
- 24-hour soak test: open 10 panes, stream random output, verify memory stays bounded
- "Claude Code simulation": rapid Unicode + escape sequence output for 1 hour, measure RSS delta
- Plugin lifecycle test: load/unload plugins 1000 times, verify no growth
- Scrollback archival test: 1M lines of output, verify memory stays at ring buffer size

## Related Pain Points Addressed

- Ghostty 71 GB leak with Claude Code → ring buffer with hard ceiling, no non-standard page allocation
- iTerm2 3 GB with unlimited scrollback → disk-backed archive with bounded memory
- WezTerm pre-allocating scrollback → lazy allocation, grow with content
- Kitty 350 MB at 64K lines → tiered buffer, most lines on disk
- Claude Code's own memory leaks → surfaced in :debug memory with alerts, clearly attributed to the child process (not us)
