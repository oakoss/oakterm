---
title: 'Shell Integration'
status: draft
category: core
description: 'Prompt markers, semantic zones, scroll-to-prompt, notifications'
tags: ['shell', 'osc-133', 'prompt-markers', 'scroll-to-prompt']
---

# Shell Integration

The layer between the terminal and the shell that makes smart features possible. Without this, the terminal is blind — it can't tell prompts from output, doesn't know when commands start or finish, and can't resolve relative paths.

## Ownership

Shell integration spans three layers:

1. **Shell scripts** (shipped with the terminal) — lightweight scripts sourced by bash/zsh/fish that emit escape sequences at key moments
2. **VT parser** (core) — parses the OSC 133 / OSC 7 escape sequences and stores prompt/command markers in the scroll buffer
3. **Plugin API** (core) — exposes shell events (`shell.on_prompt_start`, `shell.on_command_finish`, etc.) so plugins can react

The core owns parsing and storage. Plugins consume the events. Features like scroll-to-prompt use the stored markers directly in the multiplexer (core). Features like process notifications use the plugin API events.

## What Shell Integration Provides

The terminal installs a lightweight shell script (bash, zsh, fish) that emits escape sequences at key moments:

```text
OSC 133;A ST  → prompt started
OSC 133;B ST  → command started (user pressed enter)
OSC 133;C ST  → command output started
OSC 133;D;{exit_code} ST  → command finished with exit code
OSC 7;{cwd} ST  → current working directory changed
```

This is the same protocol iTerm2, Ghostty, and WezTerm use. We follow the standard — no custom escape sequences.

## What This Enables

### Scroll-to-Prompt

`Ctrl+Up` / `Ctrl+Down` — jump between command prompts in scrollback instead of scrolling line by line through output. The terminal knows where every prompt is because the shell told it.

### Click-to-Position Cursor

Click anywhere in the current prompt line to place the cursor there. The terminal knows which line is the prompt and can calculate the cursor offset.

### Command Exit Code Visualization

Failed commands get a visual marker — a red left border, a dimmed `✗` in the margin, or a colored prompt. The terminal knows the exit code.

```bash
  $ npm test                    ← prompt
  14 tests passed               ← output
✗ $ npm run build               ← failed (exit code 1)
  ERROR: Module not found       ← output
  $ _                           ← current prompt
```

### Semantic Zone Selection

Triple-click or a keybind selects the entire output of a single command — not just one line. The terminal knows where output starts and ends.

### Process Completion Notifications

When a command finishes in a background pane/tab:

- Sidebar badge updates
- OS notification if configured: "npm run build finished (exit 0)"
- `Cmd+Shift+U` includes it in the attention cycle

Only triggers for commands that took longer than a threshold (configurable, default 10s) — you don't want a notification for every `ls`.

```lua
notifications = {
  command_complete = true,
  min_duration = 10,  -- seconds, only notify for long commands
  notify_on_success = false,  -- only notify on failure by default
  notify_on_failure = true,
}
```

### Context Engine Integration

The context engine needs shell integration to:

- Know the current working directory (for file/dir completions)
- Know what command is being typed (for typed completions)
- Know recent command history with exit codes (for suggestions)
- Distinguish prompt from output (for proactive suggestions)

### Smart Pane Titles

Auto-name tabs/panes based on the running command or cwd:

- Typing a command → tab shows the command
- Running `npm run dev` → tab shows "npm run dev"
- Idle → tab shows the directory name

## Installation

Shell integration should be opt-in but trivial:

```bash
phantom shell-integration install
```

Adds one line to `.zshrc` / `.bashrc` / `config.fish`:

```bash
# .zshrc
source ~/.config/phantom/shell-integration.zsh
```

Or auto-inject without modifying shell config (like Ghostty does):

```lua
shell_integration = "auto"  -- inject at launch, no file modification
-- or "manual"              -- user sources the script themselves
-- or "none"                -- disabled
```

## Shell Support

| Shell      | Status                                   |
| ---------- | ---------------------------------------- |
| zsh        | Full support                             |
| bash       | Full support                             |
| fish       | Full support                             |
| nushell    | Partial (structured output is different) |
| powershell | Future                                   |

## What Shell Integration Is Not

- Not a shell replacement — it's a thin layer on top of your existing shell
- Not required — everything works without it, you just lose the smart features
- Not a custom prompt — it works alongside Starship, p10k, or whatever you use

## Related Docs

- [Plugin System](06-plugins.md) — shell integration events API
- [Context Engine](05-context-engine.md) — consumes cwd and prompt data
- [Smart Keybinds](19-smart-keybinds.md) — scroll-to-prompt uses prompt markers
- [Agent Management](07-agent-management.md) — process completion notifications
