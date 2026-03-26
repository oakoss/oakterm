---
adr: '0005'
title: Lua 5.4 Sandboxed Config
status: accepted
date: 2026-03-26
tags: [config, core]
---

# 0005. Lua 5.4 Sandboxed Config

## Context

The idea docs describe a two-tier configuration system — flat config for simple values and Lua for scripting — but disagree on the boundary:

- [01-architecture.md](../ideas/01-architecture.md): puts the "Lua config engine" inside the Extension Runtime alongside the WASM plugin host
- [09-config.md](../ideas/09-config.md): treats Lua as declarative configuration
- Config examples include event handlers (`workspace.on_create = function(ws) ... end`) which are imperative and long-lived

The review audit flagged this as a contradiction. The decision must define: what can Lua do, what can it not do, and how does it relate to WASM plugins?

Secondary concerns:

- WezTerm's Lua config gives full filesystem/process/network access, which users both appreciate (power) and criticize (complexity spiral, 500-line configs, security implications).
- Autocomplete/LSP support for config files is a known pain point. Neovim's Lua config with LLS annotations has poor callback type inference.
- Users want to organize configs across multiple files (dotfiles, per-host overrides, concern-based splitting).

## Options

### Option A: Flat config only (Ghostty/Alacritty model)

TOML or INI format. No logic, no callbacks, no conditionals.

**Pros:**

- Simplest for users who just want to change a font.
- Schema-driven autocomplete (JSON Schema + taplo) works perfectly.

**Cons:**

- Cannot express event reactions ("when workspace opens, set layout to tall").
- Cannot express conditional logic ("on macOS, use font size 14; on Linux, use 13").
- Users who need logic must use WASM plugins for trivial customizations.

### Option B: Lua config with lightweight scripting, sandboxed

Single Lua entry point (`config.lua`). Lua handles both value assignment and event handlers. Sandboxed via mlua: no `io`, `os`, `package` (system), or `debug` standard libraries. Lua can only call APIs explicitly registered by OakTerm. WASM plugins handle anything requiring capabilities.

**Pros:**

- One language for everything config-related.
- Event handlers and conditionals are natural Lua.
- Sandboxed — config files cannot access filesystem, network, or spawn processes.
- `require()` enables multi-file organization (dotfiles-friendly).
- LLS type stubs (`---@meta` definition files) provide autocomplete in any editor with Lua LSP.

**Cons:**

- Users must know basic Lua syntax even for simple values.
- LLS autocomplete for callback parameters requires manual `---@param` annotations (most users won't add them).
- Sandbox limits may frustrate power users who want WezTerm-level scripting.

### Option C: Full Lua runtime (WezTerm model)

Lua can spawn processes, read filesystem, open URLs, make network requests.

**Pros:**

- Maximum flexibility. Users can do anything in config.

**Cons:**

- Config files become programs. WezTerm configs of 500+ lines are common.
- Security: config can execute arbitrary code. No sandboxing.
- "I just want to change my font" users must still learn Lua.
- Complexity spiral: copy-paste config snippets break across versions.

### Option D: Luau instead of Lua 5.4

Better type system (inline annotations, string literal unions) and autocomplete. Designed for sandboxing.

**Pros:**

- Types in the language, not in comments. Better autocomplete for callbacks.
- Built-in sandbox model (no `io`, `os`, `package` by design).

**Cons:**

- Users must install `luau-lsp` — an unfamiliar tool for most developers.
- `.luau` file extension is unfamiliar.
- Smaller ecosystem outside Roblox. Less documentation for non-Roblox usage.
- Based on Lua 5.1, missing some Lua 5.4 features (irrelevant for config but surprising).

## Decision

**Option B — Lua 5.4 with sandboxed lightweight scripting.**

Single language, single entry point, sandboxed by default. The boundary between Lua config and WASM plugins is defined by capabilities:

| Capability                       | Lua config               | WASM plugin                     |
| -------------------------------- | ------------------------ | ------------------------------- |
| Set config values                | Yes                      | No                              |
| React to events                  | Yes                      | Yes                             |
| Manipulate panes/layouts         | Yes (via registered API) | Yes (via capability)            |
| Conditional logic (OS, hostname) | Yes                      | Yes                             |
| Read/write filesystem            | No                       | Yes (with `fs-read`/`fs-write`) |
| Network access                   | No                       | Yes (with `net`)                |
| Spawn processes                  | No                       | Yes (with `process`)            |
| Sidebar panels                   | No                       | Yes                             |
| Persistent storage               | No                       | Yes                             |
| Long-running services            | No                       | Yes                             |

**The boundary rule:** If it reads config or reacts to events with no side effects beyond the terminal itself → Lua. If it needs capabilities (I/O, network, persistent storage, UI panels) → WASM plugin.

Luau (Option D) can be revisited if autocomplete DX becomes a top user complaint. The migration cost would be moderate — Luau derives from Lua 5.1 with extensions, and mlua supports both via feature flags, but Lua 5.4 features (integers, bitwise operators, `<close>` variables) would need rework.

### Config Organization

- **Single entry point:** `~/.config/oakterm/config.lua`
- **Config directory is the require root:** `require("keybinds")` resolves to `~/.config/oakterm/keybinds.lua`
- **Modules return tables or call OakTerm APIs directly.** No prescribed module pattern — users organize by concern as they see fit.
- **Conditional/optional modules via `pcall`:** `pcall(require, "macos")` silently succeeds if the file exists, silently skips if it doesn't. Replaces Kitty's `include ${KITTY_OS}.conf` pattern with native Lua.
- **Dotfiles-friendly:** The config directory is a plain directory of `.lua` files. Works with symlinks, stow, chezmoi, or bare git repos.

Example structure:

```text
~/.config/oakterm/
├── config.lua              # Entry point
├── keybinds.lua            # Key bindings
├── appearance.lua          # Fonts, colors, padding
├── workspaces.lua          # Workspace event handlers
├── macos.lua               # Optional platform overrides
├── linux.lua               # Optional platform overrides
└── types/
    └── oakterm.lua         # LLS type stubs (shipped by OakTerm)
```

### Autocomplete

- **LLS type stubs** (`---@meta` definition files) shipped with OakTerm for Lua Language Server autocomplete. Covers all registered APIs, config fields, action enums, and event types. Installed to `~/.config/oakterm/types/` on first run and referenced via a `.luarc.json` in the config directory.
- **JSON Schema** for any flat config values exposed via a TOML-compatible surface (future consideration).
- Type stubs are generated from the same Rust config struct definitions that the runtime uses, keeping them in sync. Updated on OakTerm upgrade.

### Sandboxing

Implemented via mlua's selective standard library loading:

- Loaded: `coroutine`, `table`, `string`, `utf8`, `math`
- Not loaded: `io`, `os`, `package`, `debug`
- `require()` is reimplemented by OakTerm to resolve only within the config directory. The standard Lua `package` system (which can load C modules) is replaced entirely.
- Memory limits via `Lua::set_memory_limit()`.
- Instruction count hooks to prevent infinite loops in event handlers.

### Hot-Reload

- Watch all `.lua` files in the config directory tree.
- On any change, create a fresh Lua VM and re-execute `config.lua` from scratch.
- If the new config evaluates successfully, apply it atomically.
- If the new config errors, keep the last-known-good config and show an error notification in the terminal.
- On first launch with no config, use built-in defaults (the terminal must always start).

## Consequences

- Update [09-config.md](../ideas/09-config.md) to reflect single-language Lua config with sandboxing and multi-file organization.
- Update [01-architecture.md](../ideas/01-architecture.md) to clarify Lua config engine is sandboxed and separate from the WASM extension runtime.
- Ship LLS type stubs as part of the OakTerm install.
- The custom `require()` implementation must be part of Phase 0.
- WASM plugin API (Phase 2) must cover the capabilities that Lua intentionally lacks (I/O, network, storage).

## References

- [09-config.md](../ideas/09-config.md)
- [01-architecture.md](../ideas/01-architecture.md)
- [06-plugins.md](../ideas/06-plugins.md)
- [mlua documentation](https://docs.rs/mlua/latest/mlua/)
- [Lua Language Server](https://github.com/LuaLS/lua-language-server)
- [WezTerm configuration system](https://wezterm.org/config/lua/general.html)
