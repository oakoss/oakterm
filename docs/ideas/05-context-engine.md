---
title: 'Context Engine'
status: reviewing
category: plugin
description: 'Smart autocomplete, typed completions, NL commands'
tags: ['autocomplete', 'ai', 'shell-awareness', 'project-detection']
---

# Context Engine

Shell-aware autocomplete that understands what command you're typing and what project you're in. Runs as a bundled plugin — disable it for a plain terminal.

## Architecture

### Sidecar daemon

Sidecar daemon (Rust), separate from the renderer's hot path:

```text
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

### Pipeline

Inside the daemon, completion runs through three stages:

1. **Tokenize** — split the input line into `Spanned<Token>` so cursor position maps to a specific token.
2. **Parse** — recursive descent over tokens, in two passes:
   - **Lite parse** groups tokens by `;` and `|` into pipelines and commands. No semantics.
   - **Typed parse** matches each command against a registered signature to identify positionals, options, and arguments.
3. **Suggest** — given the parsed token under cursor and its semantic role, ask matching providers for suggestions, then merge, dedupe, and rank.

Spans flow through every stage, so the dropdown can underline the active token and the renderer highlights the same byte range. (`Spanned<T>` is also a candidate primitive for hints mode, copy-mode word jumps, and search — those subsystems don't anticipate it today and would need to opt in.)

### Command signatures

Each command (`git`, `cd`, `kubectl`, `pnpm`, …) is described by a **signature** — a serializable schema the typed parse stage uses to interpret tokens. The sketch below paraphrases Warp's `CommandSignature`; the final shape is decided by ADR (see [Open Questions](#open-questions)):

```text
Command    { name, aliases, description, arguments, subcommands, options, priority }
Argument   { name, description, values, optional, arity }
Option     { name, short, long, description, takes_value }
Suggestion { value, display, description, priority }
Generator  { Shell | Hook }            // dynamic suggestions (see below)
TemplateType { Files, Folders, FilesAndFolders }
Priority(i32)                          // clamped [-100, 100]
```

Signatures are **data, not code**: they can be authored declaratively and loaded at startup. Plugins ship signatures the way they ship themes — as static metadata, not as WASM execution.

### Signature sources

Signatures come from two tiers:

1. **Baseline (built into the binary)** — a small set of common commands (`cd`, `ls`, `git`, `npm`/`pnpm`/`yarn`, `cargo`, `docker`, `kubectl`) so common tools complete out of the box without third-party plugin installs. Disabling the bundled `context-engine` plugin turns completion off entirely; the baseline lives inside the plugin.
2. **Plugin-contributed** — WASM plugins register signatures via the [Context Engine plugin primitive](06-plugins.md#context-engine). This is how the catalog grows.

A plugin-only model means even `cd` doesn't tab-complete until a plugin is installed. A baseline-in-core model bloats the binary with every niche tool. The split avoids both.

### Generator primitive

Most "dynamic" completion data comes from running a shell command and parsing its output: `git branch -l` for branches, `npm run` for scripts, `kubectl get pods -o name` for pods. Rather than each plugin reimplementing process spawning + parsing, the engine provides:

```text
Generator::Shell { script, parse: <hook> }    // run a shell command; parse is optional, default = one suggestion per stdout line
Generator::Hook(<hook>)                       // delegate entirely to a registered plugin callback
```

`<hook>` is the name of a callback registered by a plugin via `context.generator(name, fn)`. Signatures stay serializable: callbacks are referenced by name, never embedded, so a signature file can be JSON, TOML, or any other static format. Plugins ship the callbacks separately as code.

Most signatures only need `Generator::Shell { script }` and rely on the default line parser. Warp's `GeneratorFn::ShellCommand` uses the same shape with an optional `post_process` field (see [warp_completer/src/signatures/v2/mod.rs](https://github.com/warpdotdev/warp/blob/master/crates/warp_completer/src/signatures/v2/mod.rs)).

## Open Questions

Resolved by ADR (proposed, pending acceptance):

1. **Signature schema shape** — see [ADR-0013](../adrs/0013-fig-autocomplete-schema.md): adopt Fig's autocomplete schema with build-time conversion to JSON via a pure-Rust converter (oxc). Empirically validated against 1,484 specs.
2. **Input classifier** — see [ADR-0014](../adrs/0014-input-classifier.md): layered approach. `?` stays as explicit override; heuristic classifier ships for ambiguous inputs; ML opt-in via community plugin.
3. **Baseline location** — see [ADR-0013](../adrs/0013-fig-autocomplete-schema.md): separate `oakterm-completer-baseline` crate with `include_bytes!`-embedded JSON.
4. **Signature storage format** — see [ADR-0013](../adrs/0013-fig-autocomplete-schema.md): JSON, output of build-time converter.

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

| Command        | Shows                                                   |
| -------------- | ------------------------------------------------------- |
| `cd`           | Directories only, with icons (Warp-style visual picker) |
| `git checkout` | Branches, sorted by recent, ahead/behind counts         |
| `vim` / `code` | Files, recently edited first                            |
| `ssh`          | Hosts from ~/.ssh/config                                |
| `kill`         | Running processes with PID and CPU%                     |
| `docker exec`  | Running containers                                      |

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

| Signal                                      | Suggestion                |
| ------------------------------------------- | ------------------------- |
| `pnpm-lock.yaml` newer than `node_modules/` | `pnpm install`            |
| `.env.example` exists but `.env` doesn't    | `cp .env.example .env`    |
| Dirty git tree, finished editing            | Your usual commit pattern |
| Docker compose file, containers not running | `docker compose up -d`    |

These are deterministic rules — no AI needed.

## Natural Language (opt-in)

`?` prefix translates plain English to a command:

- `? find files over 100mb modified this week` → `find . -size +100M -mtime -7`
- Shown as ghost text for review. Tab to accept. Never auto-executes.
- Requires an AI backend (Ollama, Anthropic, OpenAI) or disable entirely.

```lua
-- In config.lua
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

- [Plugin System](06-plugins.md) — context engine primitives (`context.signature`, `context.provider`, `context.generator`, …) and `shell.on_cwd_change`
- [Shell Integration](18-shell-integration.md) — provides cwd and prompt data
- [Smart Keybinds](19-smart-keybinds.md) — hints mode uses similar pattern matching
- [Configuration](09-config.md) — plugin config syntax
