---
title: "Brainstorm — Unsorted Ideas"
status: draft
category: cross-cutting
description: "Raw ideas to be evaluated and potentially promoted to their own docs"
tags: ["brainstorm", "syntax", "tiling", "layouts", "sidebars", "scrollback", "zed"]
---
# Brainstorm — Unsorted Ideas

Raw ideas captured during discussion. Each needs evaluation — some will become features, some will fold into existing docs, some won't make the cut.

## Syntax Highlighting in Terminal Output

Built-in syntax parsing so terminal output (logs, diffs, JSON, code) gets highlighted automatically. Not just ANSI colors from programs — the terminal itself understands the content.

**How it could work:**
- Tree-sitter parsers running on pane output (same library Zed, Neovim, Helix use)
- Auto-detect content type: JSON, YAML, diffs, stack traces, log formats
- Highlight in real-time as output streams
- Configurable — enable per pane type or globally
- Works on plain `cat file.rs` output that has no ANSI colors

**Open questions:**
- Performance cost of parsing every line of output? Tree-sitter is fast but terminal output can be high-volume
- Should this be core or plugin? Leaning plugin — uses `pane.output` + a tree-sitter WASM module
- Does it conflict with programs that send their own ANSI colors? Need a priority system: program colors win over auto-highlight
- Which languages/formats to support by default?

## Auto-Tiling Layout Engine

Automatic pane tiling that arranges panes without manual splitting. Like a tiling window manager but inside the terminal.

**Modes:**
- `tiling = auto` — new panes auto-arrange (spiral, columns, main+stack)
- `tiling = manual` — you place panes yourself with splits (current default)
- `tiling = off` — single pane only, tabs for everything

**Auto-tiling algorithms:**
- **Main + stack** — one large pane on the left, new panes stack on the right (like dwm/i3 default)
- **Columns** — equal-width columns, new pane adds a column
- **Spiral** — fibonacci-style splitting (like bspwm)
- **Grid** — auto-arrange into a grid based on pane count

```
tiling-mode = auto
tiling-algorithm = main-stack
tiling-main-ratio = 0.6
```

**Interaction with pane types:** Auto-tiling only affects tiled panes. Floating, drawer, popup, modal, and sidebar panes are unaffected.

**Should be core** — it's part of the multiplexer layout engine.

## Panel Layout Presets (Quick Layouts)

Named layout configurations you can switch between instantly from the palette or keybinds. Different from saved layouts (which create tabs with specific commands) — these rearrange existing panes.

```
Cmd+Shift+P → :layout

┌──────────────────────────────────────────────────┐
│  layout:  Search layouts                         │
├──────────────────────────────────────────────────┤
│  Presets                                         │
│  ⊞ Main + Stack          Alt+1                  │
│  ⊞ Equal Columns         Alt+2                  │
│  ⊞ Grid                  Alt+3                  │
│  ⊞ Focused (one pane)    Alt+4                  │
│  ⊞ Side by Side          Alt+5                  │
│                                                  │
│  Saved                                           │
│  ⊞ dev (3 tabs, 5 panes)                        │
│  ⊞ monitoring (2 tabs, 4 panes)                 │
└──────────────────────────────────────────────────┘
```

Keybinds for quick switching:
```
keybind = alt+1 = layout-main-stack
keybind = alt+2 = layout-columns
keybind = alt+3 = layout-grid
keybind = alt+4 = layout-focused
keybind = alt+5 = layout-side-by-side
```

Presets rearrange the current tab's panes instantly. Your pane processes keep running — only the layout changes.

## Claude Code Scrollback Buffer Issue

The #1 bug report across Ghostty + Claude Code: the terminal jumps to the top or bottom of scrollback erratically when Claude is streaming output.

**Root cause:** Claude Code uses rapid terminal redraws — 4,000-6,700 scroll events per second. It also uses alternate screen + cursor movements that interact badly with scrollback management.

**Our approach (multiple layers):**

1. **Synchronized output (DEC mode 2026)** — batch terminal updates between begin/end markers. The renderer only draws complete frames, eliminating flicker and partial-render jumps. This is the standard fix and we support it.

2. **Agent-aware scroll pinning** — when a pane is marked as an agent (via the agent-manager plugin), the terminal pins the user's scroll position. If you've scrolled up to read something, the agent's new output appends below but doesn't yank your viewport. A "new output below ↓" indicator appears to jump back to the bottom.

3. **Output rate throttling for rendering** — if a pane is producing >1000 lines/second, the renderer skips intermediate frames and only draws the latest state. The scroll buffer still captures everything — only the visual rendering is throttled. This prevents the GPU from thrashing on pathological output.

4. **Separate scroll regions** — the agent's streaming output and your scrollback are managed as separate regions internally. Scrolling up enters "review mode" which freezes viewport position until you explicitly return to the bottom (press `G` in copy mode, or click the "↓" indicator).

5. **Ring buffer ceiling** — even with massive agent output, memory stays bounded (see [Memory Management](15-memory-management.md)). The ring buffer means old output rolls off, never growing unbounded.

These are all core features, not plugins. The scroll buffer, rendering pipeline, and viewport management are deeply integrated.

## Multi-Sidebar Configuration

Instead of one fixed sidebar on the left, support multiple sidebar panels that are independently configurable.

```
┌───────────┬────────────────────────────┬───────────┐
│ LEFT      │                            │ RIGHT     │
│           │                            │           │
│ AGENTS    │  Main terminal content     │ NOTES     │
│ ◉ feat/   │                            │ todo.md   │
│ ◉ tests/  │                            │ - fix auth│
│           │                            │ - add test│
│ SERVICES  │                            │           │
│ ▶ dev     │                            │ GIT       │
│ ▶ docker  │                            │ main      │
│           │                            │ 3 ahead   │
│ SHELLS    │                            │ 2 files   │
│ ● scratch │                            │           │
└───────────┴────────────────────────────┴───────────┘
```

**Configuration:**

```lua
sidebars = {
  left = {
    enabled = true,
    width = 220,
    default = "collapsed",    -- "collapsed", "expanded", "hidden"
    sections = { "agents", "services", "watchers", "shells" },
  },
  right = {
    enabled = true,
    width = 200,
    default = "hidden",
    sections = { "git-status", "notes" },
  },
  -- bottom = { ... }  -- could support bottom sidebar too
}
```

Flat config:
```
sidebar-left-enabled = true
sidebar-left-width = 220
sidebar-left-default = collapsed
sidebar-right-enabled = false
```

**Tabs within sidebars:**
Each sidebar can have tabs to cycle through different views without expanding the sidebar width:

```
┌───────────┐
│[Proc][Git] │  ← tabs at top of sidebar
│────────────│
│ AGENTS     │
│ ◉ feat/    │
│ ◉ tests/   │
│ SERVICES   │
│ ▶ dev      │
└────────────┘
```

`Ctrl+B` toggles left sidebar. `Ctrl+Shift+B` toggles right sidebar. Tabs within a sidebar cycle with clicking or a keybind.

**Plugin integration:**
Plugins register which sidebar(s) they want their sections in:

```rust
sidebar.register_section(SidebarSection {
    name: "agents",
    preferred_sidebar: "left",    // suggestion, user can override
    accessible_label: "Agent processes",
});
```

Users can drag sections between sidebars or configure placement in config.

**This is still a plugin** — the sidebar-ui plugin handles the rendering of one or more sidebars. The core provides the data model. A multi-sidebar is just the sidebar-ui plugin supporting multiple instances.

## Zed-Inspired Patterns to Adopt

From the Zed architecture research:

1. **Batched instanced GPU rendering** — typed scene graph (quads, sprites, paths), batched by type and texture, single instanced draw call per batch. This is the foundation of fast rendering.

2. **Glyph atlas with `etagere` allocator** — proven, efficient atlas packing for font glyphs.

3. **WASM Component Model + WIT** — Zed uses the WASM Component Model with WIT (WebAssembly Interface Types) for extensions, not raw WASM. This is a stricter, more type-safe plugin contract. Worth evaluating vs. raw Wasmtime.

4. **Damage tracking** — Zed rebuilds the entire element tree every frame (expensive). For terminals, most of the screen is static between frames. Track which cells changed and only re-render dirty regions. Zed doesn't do this — we should.

5. **Headless mode pattern** — Zed's `remote_server` runs the full project model without UI. Our headless daemon uses the same pattern — full multiplexer + plugins, no renderer.

6. **Pre-rasterize ASCII** — for monospaced terminal rendering, pre-rasterize the entire ASCII range (32-126) on startup into the glyph atlas. Skip font shaping for ASCII entirely. Only fall back to full OpenType shaping for non-ASCII (CJK, emoji, combining marks).

7. **Channel-based message passing** — subsystems communicate via channels, not direct calls. Terminal event loop, plugin host, and renderer are decoupled.

## Configurable Status Bar

Like tmux's status line or Neovim's lualine. Core renders the bar, plugins register widgets.

- Position: bottom, top, or hidden
- Three segments: left, center, right
- Widgets: `{mode}`, `{pane_title}`, `{git_branch}`, `{test_status}`, `{memory}`, `{ports}`, `{time}`, `{agent_count}`, etc.
- Plugins register custom widgets (watcher provides `{test_status}`, agent-manager provides `{agent_count}`)
- User places widgets wherever they want in config
- Themeable — colors from the theme's `status-bar-bg`/`status-bar-fg`
- Can be hidden completely: `status-bar = none`
- Should respect `prefers-reduced-motion` for any animated widgets
- Accessible — screen reader can read status bar content

## Error Handling Philosophy

What happens when things go wrong? The terminal should degrade gracefully, never crash.

- **GPU driver fails** → fall back to software renderer (wgpu supports this). Show a warning in `:health`.
- **Font not found** → fall back through the chain. If all fallbacks fail, use the platform default monospace. Never show blank squares silently.
- **Plugin crashes** → kill the plugin, show notification, offer restart/disable. Terminal keeps running. No pane is lost.
- **Config parse error** → boot with defaults, show a clear error banner: "Config error on line 42. Using defaults. [Fix] [Ignore]"
- **Scroll buffer full (disk archive)** → oldest lines evicted. Never OOM. Show `:health` warning if archive is >80% of budget.
- **SSH domain unreachable** → retry with backoff. Show status in sidebar. Don't block startup.
- **Remote daemon disconnected** → show "reconnecting..." in sidebar. Buffer local input. Replay on reconnect if possible.
- **Theme missing colors** → fill missing values from the default theme. Show warning in `phantom theme validate`.

Principle: **every failure mode has a fallback, every fallback has a notification.**

## Data Model

Formal relationships between core entities. Not specced yet but should be before implementation.

```
Daemon (singleton)
├── Window[] (1 or more, platform-native)
├── Workspace[] (1 or more, named)
│   └── Tab[] (1 or more per workspace)
│       └── Pane[] (1 or more per tab)
│           ├── type: tiled | floating | drawer | popup | modal | sidebar-pane
│           ├── process: PTY + child process (or surface for non-terminal)
│           ├── scroll_buffer: ring buffer + disk archive
│           ├── metadata: key-value pairs (title, status, color, branch, etc.)
│           ├── permissions: agent control API permissions
│           └── plugin_attachments: which plugins are watching this pane
├── Plugin[] (loaded WASM modules)
├── SidebarState (sections, entries, badges)
├── HarpoonList (per-workspace bookmarks)
├── NotificationHistory[]
├── Config (merged: defaults + flat file + lua + project overrides)
└── RemoteDomain[] (connections to remote daemons)
```

IDs: every pane, tab, workspace gets a stable UUID. Used by harpoon, agent control API, remote sync, session persistence.

## Contributing Guide (future)

Not needed for idea phase but worth thinking about early:

- **Decision process** — how do we decide what gets in? RFC-style proposals? Benevolent dictator? Consensus?
- **Plugin vs core** — clear criteria for what belongs in core vs plugins. The litmus test in [Plugin System](06-plugins.md) is the guide.
- **Code review** — who reviews? What standards? Rust clippy/fmt enforced in CI?
- **Release process** — who cuts releases? What's the cadence? Semver for core, independent versioning for plugins?
- **Code of conduct** — standard Contributor Covenant or similar
- **Architecture decision records (ADRs)** — document significant decisions with context, alternatives considered, and rationale

---

## Things Terminals Still Get Wrong

Common daily-use pain points that no terminal has fully solved.

### Smart Selection

Text selection in terminals is stuck in the 1980s.

**Problems:**
- Selecting wrapped lines includes trailing whitespace and newlines
- Selecting across splits grabs border characters
- Double-click selects a "word" but doesn't understand paths (`src/components/Button.tsx` is three selections, not one)
- Can't select just a command's output without getting the prompt
- No way to select a semantic object (URL, path, IP, hash) in one action

**Our approach:**
- **Semantic word boundaries** — double-click understands file paths, URLs, dot-separated identifiers. `src/components/Button.tsx:42` is one selection.
- **Command-output selection** — with shell integration, triple-click or a keybind selects the entire output of one command (semantic zone selection, from WezTerm)
- **Cross-pane selection blocked** — selecting stops at pane borders. No garbage border characters in clipboard.
- **Block/rectangular selection** — `Ctrl+V` in copy mode (vim preset) or `Alt+drag` for column selection
- **Smart trim** — auto-strip trailing whitespace and leading indentation when copying. Configurable.
- **Multi-select** — hold `Cmd/Ctrl` and click to add multiple selections. Yank copies all selections joined by newlines. (Stretch goal)

```
smart-selection = true                    # default
smart-selection-trim-whitespace = true    # strip trailing spaces
smart-selection-trim-indent = false       # strip common leading indent
```

### Paste Safety

Pasting into a terminal is the most dangerous operation most developers do daily.

**Problems:**
- Paste 500 lines? Dumped straight into the shell, every newline triggers execution
- Trailing newline in clipboard? Command executes immediately, no confirmation
- Paste contains `sudo rm -rf /`? No warning
- No preview of what you're about to paste
- Bracketed paste helps but not all shells/programs support it

**Our approach:**
- **Large paste warning** — pastes over a configurable line threshold (default: 5 lines) show a preview popup:
  ```
  ┌──────────────────────────────────────────────────┐
  │  Paste Preview (23 lines)                        │
  │                                                  │
  │  npm install                                     │
  │  npm run build                                   │
  │  npm run test                                    │
  │  ... (20 more lines)                             │
  │                                                  │
  │  ⚠ Contains newlines — will execute sequentially │
  │                                                  │
  │  [Paste] [Paste as Single Line] [Edit] [Cancel]  │
  └──────────────────────────────────────────────────┘
  ```
- **Dangerous command detection** — warn on patterns like `rm -rf`, `sudo`, `DROP TABLE`, `mkfs`, `dd if=`, `> /dev/sda`. Configurable pattern list.
- **Trailing newline strip** — option to auto-strip trailing newline so pasted commands don't auto-execute
- **Bracketed paste enforced** — always use bracketed paste mode. If the program doesn't support it, fall back gracefully.
- **Paste-as-single-line** — option to join multi-line paste into one line (useful for pasting paths with line breaks)

```
paste-warning-lines = 5          # warn on pastes > 5 lines (0 = never warn)
paste-dangerous-patterns = true  # warn on rm -rf, sudo, etc.
paste-strip-trailing-newline = false  # don't auto-strip (safety over convenience)
paste-bracketed = true           # always use bracketed paste
```

### Smart URL/Path Detection

Terminals detect URLs with dumb regex. It breaks constantly.

**Problems:**
- URLs wrapping across lines break detection
- URLs with parentheses: `https://en.wikipedia.org/wiki/Rust_(language)` — the `)` gets excluded
- URLs with trailing punctuation: `Check https://example.com.` — the `.` gets included
- File paths like `src/components/Button.tsx:42:15` aren't recognized
- Relative paths can't be resolved without cwd

**Our approach:**
- **Wrap-aware URL detection** — when a URL wraps to the next line, detect it as one continuous URL. Use soft-wrap metadata from the VT parser.
- **Balanced delimiter handling** — track parentheses, brackets, and angle brackets. `https://en.wikipedia.org/wiki/Rust_(language)` includes the closing `)`. Trailing punctuation (`.`, `,`, `:`, `;`) excluded.
- **File path detection** — recognize patterns like `file.ext:line:col`, resolve relative paths against the pane's cwd (from shell integration). Clicking opens in `$EDITOR`.
- **OSC 8 hyperlinks** — programs can emit semantic hyperlinks with custom display text. We support the standard fully.
- **All detected links get hints labels** — hints mode (from [Smart Keybinds](19-smart-keybinds.md)) labels every link/path for keyboard-only access.

### Resize Reflow

Resizing a terminal is surprisingly hard to get right.

**Problems:**
- Wrapped lines don't unwrap when you make the terminal wider
- TUI apps (vim, htop) don't always redraw after resize
- Content jumps and the viewport shifts to a random position
- Cursor position gets lost

**Our approach:**
- **Proper reflow** — when the terminal widens, soft-wrapped lines unwrap. When it narrows, lines re-wrap. Track which line breaks are soft (from wrapping) vs hard (from the program).
- **SIGWINCH delivery** — send the resize signal to the child process immediately. TUI apps that handle SIGWINCH will redraw.
- **Viewport anchoring** — during resize, keep the viewport anchored to the content you're looking at, not the top or bottom. If you're reading line 500, you're still reading line 500 after resize.
- **Smooth resize** — on platforms that support it (macOS), render at the new size progressively rather than blanking and redrawing.

### Smart Tab Close

Closing a tab should be smart about what's running.

**Problems:**
- "Are you sure?" on every tab close is annoying when it's just an idle shell
- Silently killing a running build is dangerous
- No way to tell if a process is "important" or just a shell

**Our approach:**
- **Process-aware close** — the terminal knows what's running in each pane (via PTY process tree):
  - **Idle shell** (just bash/zsh/fish, no child) → close immediately, no prompt
  - **Running foreground process** (build, test, agent) → confirm: "npm run build is running. Close? [Yes] [Cancel]"
  - **Agent pane** → warn: "Agent feat/auth is working. Close will abandon changes. [Close] [Cancel]"
  - **Background process only** (like a completed command, cursor at prompt) → close immediately

```
tab-close-confirm = smart    # default: ask only when a process is running
tab-close-confirm = always   # always ask
tab-close-confirm = never    # never ask (dangerous)
```

### Predictive Local Echo for SSH

Every keystroke on an SSH connection round-trips to the server. On high-latency connections this is miserable.

**Problems:**
- 100ms latency means you feel every keypress lag
- Mosh solved this in 2012 but requires a separate tool + UDP port
- No terminal has built-in predictive echo

**Our approach (for SSH domains):**
- **Predictive echo** — when typing in an SSH domain pane, immediately render the character locally (dimmed/italic). When the server echoes it back, replace with the confirmed character.
- **Prediction confidence** — simple predictions (typing characters at a prompt) are high confidence. Complex predictions (backspace through a completion menu) are low confidence and skipped.
- **Mismatch handling** — if the server's echo doesn't match the prediction, discard the prediction and show the server's version. Brief visual flash to indicate the correction.
- **Only for SSH domains** — local panes don't need this. Only activates on remote connections with measurable latency.

This is inspired by Mosh's approach but implemented in the terminal itself, not as a separate protocol. It works over standard SSH — no server-side component needed.

```
ssh-predictive-echo = true       # default for SSH domains
ssh-predictive-echo-style = dim  # how predicted characters look: dim, italic, underline
```

### Large Output Handling

`cat` a 100MB log file and most terminals freeze or OOM.

**Problems:**
- Terminal tries to render all output as fast as it arrives
- Scroll buffer grows unbounded (addressed by our ring buffer, but rendering is still the issue)
- No way to cancel output mid-stream without killing the process
- User intended to pipe to `less` but forgot

**Our approach:**
- **Render throttling** — if output exceeds a threshold (configurable, default: 10,000 lines/second), the renderer skips frames. Scroll buffer still captures everything. The terminal stays responsive.
- **Output rate indicator** — status bar shows output rate when it's high: `⚡ 45,000 lines/s`. Visual signal that something is flooding the terminal.
- **Ctrl+S / Ctrl+Q** — standard XOFF/XON flow control. `Ctrl+S` pauses output (the process blocks on write). `Ctrl+Q` resumes. Most terminals support this but don't surface it. We should make it discoverable.
- **Ring buffer ceiling** — old lines roll off (already specced in [Memory Management](15-memory-management.md)). The terminal never OOMs from output.
- **Suggestion after flood** — after a massive output burst finishes, show a subtle hint: "Large output detected. Consider piping to less or redirecting to a file."
