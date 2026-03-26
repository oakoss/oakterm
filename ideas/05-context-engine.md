---
title: "Context Engine"
status: draft
category: plugin
description: "Smart autocomplete, typed completions, NL commands"
tags: ["autocomplete", "ai", "shell-awareness", "project-detection"]
---
# Context Engine


Shell-aware autocomplete that understands what command you're typing and what project you're in. Runs as a bundled plugin — disable it for a plain terminal.

## Architecture

Sidecar daemon (Rust), separate from the renderer's hot path:

```
Terminal (renderer)           Context Daemon
     │                            │
     │── keystroke stream ──────→ │
     │                            ├── query fs/git/history
     │                            ├── rank completions
     │◄── completion candidates ──┤
     │                            │
     │   (optional AI call) ────→ │──→ local LLM / API
```

If the daemon is slow, you just don't see ghost text for that keystroke. Zero degradation to typing feel.

## Context Sources

- Current working directory
- Recent command history (session + global)
- Active environment variables
- Git branch / status
- Files/dirs in cwd
- Executables on $PATH
- Man page / --help parsing (cached)
- Project type detection (package.json → node, Cargo.toml → rust, etc.)

## Typed Completions

Different commands get different completion UIs:

| Command | Shows |
|---------|-------|
| `cd` | Directories only, with icons (Warp-style visual picker) |
| `git checkout` | Branches, sorted by recent, ahead/behind counts |
| `vim` / `code` | Files, recently edited first |
| `ssh` | Hosts from ~/.ssh/config |
| `kill` | Running processes with PID and CPU% |
| `docker exec` | Running containers |

Each is a **completion provider** — a module that registers which command and argument it handles. Bundled providers cover common tools. WASM plugins add more.

## Presentation

- **Ghost text** — most likely completion inline, dimmed. Tab to accept.
- **Dropdown** — Tab or partial match triggers a floating popup. Entries ranked by frequency + context, each with a one-line description from man/help.
- **Fuzzy by default** — typing `comp` matches `components/`, `computed/`, `compat/`.

## Project Awareness

The engine detects project type and weights suggestions:
- In a pnpm project, `pnpm` ranks over `npm`
- In a Rust project, `cargo` commands surface first
- Learns per-project command frequency

## Proactive Suggestions

Context-aware suggestions on directory change or after specific events:

| Signal | Suggestion |
|--------|-----------|
| `pnpm-lock.yaml` newer than `node_modules/` | `pnpm install` |
| `.env.example` exists but `.env` doesn't | `cp .env.example .env` |
| Dirty git tree, finished editing | Your usual commit pattern |
| Docker compose file, containers not running | `docker compose up -d` |

These are deterministic rules — no AI needed.

## Natural Language (opt-in)

`?` prefix translates plain English to a command:
- `? find files over 100mb modified this week` → `find . -size +100M -mtime -7`
- Shown as ghost text for review. Tab to accept. Never auto-executes.
- Requires an AI backend (Ollama, Anthropic, OpenAI) or disable entirely.

Flat config:
```
context-engine.enabled = true
context-engine.ai-backend = ollama
context-engine.ai-model = codellama:7b
context-engine.natural-language-prefix = ?
context-engine.learn-per-project = true
```

Lua config:
```lua
plugins["context-engine"] = {
  enabled = true,
  ai = {
    backend = "ollama",        -- or "anthropic", "openai", "none"
    model = "codellama:7b",
  },
  natural_language_prefix = "?",
  learn_per_project = true,
}
```

## What This Is Not

- Not a shell replacement (works with bash, zsh, fish, nushell)
- Not a chatbot
- Not required — disable the plugin and it's gone

## Related Docs

- [Plugin System](06-plugins.md) — `context.provider` and `shell.on_cwd_change` APIs
- [Shell Integration](18-shell-integration.md) — provides cwd and prompt data
- [Smart Keybinds](19-smart-keybinds.md) — hints mode uses similar pattern matching
- [Configuration](09-config.md) — plugin config syntax
