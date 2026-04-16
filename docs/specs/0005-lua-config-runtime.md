---
spec: '0005'
title: Lua Config Runtime
status: complete
date: 2026-03-27
adrs: ['0005']
tags: [config, core]
---

# 0005. Lua Config Runtime

## Overview

Defines the sandboxed Lua 5.4 runtime for OakTerm configuration: the API surface registered into Lua, the sandboxing mechanism, custom module resolution, hot-reload lifecycle, event handler registration, error handling, and LLS type stub delivery. Implements ADR-0005.

## Contract

### Lua VM Initialization

The Lua VM is created with selective standard library loading via mlua:

```rust
let lua = Lua::new_with(
    StdLib::BASE | StdLib::COROUTINE | StdLib::TABLE
    | StdLib::STRING | StdLib::UTF8 | StdLib::MATH,
    LuaOptions::default(),
)?;
```

**Loaded libraries:**

| Library   | Purpose                                                                                                                                                        |
| --------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| BASE      | `pairs`, `ipairs`, `pcall`, `xpcall`, `error`, `type`, `tostring`, `tonumber`, `select`, `assert`, `rawget`, `rawset`, `setmetatable`, `getmetatable`, `print` |
| COROUTINE | Coroutine operations                                                                                                                                           |
| TABLE     | `table.insert`, `table.remove`, `table.sort`, `table.concat`, `table.unpack`                                                                                   |
| STRING    | String manipulation, pattern matching                                                                                                                          |
| UTF8      | `utf8.char`, `utf8.codes`, `utf8.len`, `utf8.offset`                                                                                                           |
| MATH      | Math functions                                                                                                                                                 |

**Not loaded:**

| Library | Reason                                                                                           |
| ------- | ------------------------------------------------------------------------------------------------ |
| IO      | Filesystem access (sandboxed out)                                                                |
| OS      | System calls, process spawning (sandboxed out)                                                   |
| PACKAGE | Default `require()` loads C modules and arbitrary paths (replaced with custom sandboxed require) |
| DEBUG   | Debug hooks can escape sandboxes                                                                 |

**Dangerous BASE functions removed after creation:**

```rust
let globals = lua.globals();
globals.set("dofile", Value::Nil)?;
globals.set("loadfile", Value::Nil)?;
globals.set("load", Value::Nil)?;
```

These are removed because they can load and execute arbitrary code from the filesystem, bypassing the sandboxed require.

**`print` override:** The default Lua `print` is replaced with a function that redirects output to `oakterm.log("info", ...)`. In a daemon context, stdout is not meaningful. This ensures `print()` debugging in config files produces visible output in `oakterm --log`.

### Resource Limits

**Memory limit:**

```rust
lua.set_memory_limit(16 * 1024 * 1024)?; // 16 MiB
```

When exceeded, Lua raises a memory error. The VM remains consistent. The daemon catches the error, keeps the last-known-good config, and displays the error.

Default: 16 MiB. This is generous for config scripts (typical configs use < 1 MiB). The limit prevents pathological configs (infinite table growth) from exhausting daemon memory.

**Instruction limit:**

```rust
lua.set_hook(
    HookTriggers::new().every_nth_instruction(10_000),
    move |_lua, _debug| {
        if start.elapsed() > Duration::from_millis(500) {
            Err(mlua::Error::RuntimeError(
                "config evaluation timed out (500ms)".into(),
            ))
        } else {
            Ok(())
        }
    },
)?;
```

The hook fires every 10,000 VM instructions and checks wall-clock time. If config evaluation exceeds 500ms, it is aborted.

**Event handler timeout:** The instruction hook is re-installed before each event handler invocation with a per-handler timeout of 100ms. Event handlers should complete in < 10ms; the 100ms limit catches infinite loops without interfering with legitimate work. A timed-out handler is logged as an error and skipped; subsequent handlers for the same event still fire.

Default timeout: 500ms. Config scripts should evaluate in < 50ms. The timeout catches infinite loops in event handlers or recursive requires.

### OakTerm API Surface

The following tables and functions are registered into Lua's global scope before config evaluation.

#### `oakterm` Module

```lua
---@class oakterm
oakterm = {
    config = { ... },   -- Config fields (see below)
    action = { ... },   -- Action constructors (see below)
    layout = { ... },   -- Layout definitions (see Layout API below)
}

--- Register a keybinding.
---@param key string Key chord (e.g., "ctrl+d", "super+shift+t")
---@param action oakterm.Action|function Action or callback
function oakterm.keybind(key, action) end

--- Register an event handler.
---@param event string Event name
---@param callback function Handler function
function oakterm.on(event, callback) end

--- Get the current OS name.
---@return "macos"|"linux"|"windows"
function oakterm.os() end

--- Get the hostname.
---@return string
function oakterm.hostname() end

--- Log a message (appears in oakterm --log output, not in the terminal).
---@param level "debug"|"info"|"warn"|"error"
---@param message string
function oakterm.log(level, message) end
```

#### `oakterm.config` Table

Config values. Writing to this table sets configuration. Unknown keys raise a Lua error (via `__newindex` metatable).

**Naming convention:** All config keys use snake_case. Lua is the only config format (ADR-0005). There is no separate flat config file.

```lua
oakterm.config.font_family = "JetBrains Mono"
oakterm.config.font_size = 14.0
oakterm.config.theme = "catppuccin"
oakterm.config.cursor_style = "block"
oakterm.config.cursor_blink = true
oakterm.config.scrollback_limit = "50MB"
oakterm.config.scrollback_archive = true
oakterm.config.scrollback_archive_limit = "1GB"
oakterm.config.save_alternate_scrollback = false  -- ADR-0006: default off; opt in for CLI-agent workflows
oakterm.config.daemon_persist = false
oakterm.config.check_for_updates = "off"
oakterm.config.padding = { top = 8, bottom = 8, left = 12, right = 12 }
oakterm.config.window_decorations = "full"
oakterm.config.confirm_close_process = true

-- Phase 1: Multiplexer config (ADR-0011, Spec-0007, Spec-0008, Spec-0010)
oakterm.config.oak_mod = "ctrl+shift"       -- Linux default; "super" on macOS (super = Cmd key)
oakterm.config.leader = nil                  -- optional: { key = "ctrl+b", timeout = 1000 }
oakterm.config.copy_mode_keybinds = "vim"    -- "vim", "emacs", or "basic"
oakterm.config.status_bar = true
oakterm.config.status_bar_position = "bottom" -- "top" or "bottom"
oakterm.config.restartable_commands = {}     -- list of command prefixes to restore on session load
```

The `oakterm.config` table is implemented as a **proxy table**: an empty table with `__newindex` and `__index` on its metatable, backed by a hidden storage table. This means `rawset(oakterm.config, key, value)` writes to the empty proxy (discarded), not the backing store. The metatable has `__metatable` set to a string, preventing `getmetatable`/`setmetatable` from inspecting or replacing the protection.

The `__newindex` metamethod validates keys against the known config schema. Setting an unknown key (e.g., `oakterm.config.font_szie = 14`) raises an immediate error with a "did you mean?" suggestion if a close match exists.

#### `oakterm.action` Table

Action constructors for keybindings. Each returns an opaque action value.

```lua
oakterm.action.split_pane({ direction = "right", size = 0.5 })
oakterm.action.close_pane()
oakterm.action.focus_pane_direction("left")
oakterm.action.new_tab()
oakterm.action.close_tab()
oakterm.action.copy()
oakterm.action.paste()
oakterm.action.scroll_up(5)
oakterm.action.scroll_down(5)
oakterm.action.scroll_to_prompt(-1)  -- previous prompt
oakterm.action.scroll_to_prompt(1)   -- next prompt
oakterm.action.send_string("\x1b[A") -- raw escape sequence
oakterm.action.show_command_palette()
oakterm.action.toggle_fullscreen()
oakterm.action.reload_config()

-- Phase 1: Multiplexer actions (Spec-0007, Spec-0008, Spec-0009)
oakterm.action.enter_copy_mode()
oakterm.action.enter_resize_mode()
oakterm.action.toggle_floating_pane()
oakterm.action.zoom_pane()               -- toggle pane fullscreen within tab
oakterm.action.next_tab()
oakterm.action.previous_tab()
oakterm.action.switch_tab(1)             -- switch to tab by index
oakterm.action.rename_tab("name")
oakterm.action.new_workspace("name")
oakterm.action.switch_workspace("name")
oakterm.action.promote_to_floating()     -- tiled → floating
oakterm.action.demote_to_tiled()         -- floating → tiled
oakterm.action.swap_pane(pane_id)        -- swap focused pane with target
oakterm.action.move_tab(to_index)        -- reorder tab within workspace
oakterm.action.close_workspace()
oakterm.action.rename_workspace("name")
oakterm.action.load_layout("name")       -- apply a named layout (see Layout API)
```

#### Events

Events registered via `oakterm.on(event, callback)`:

| Event                | Callback Signature                                        | When Fired                                                 |
| -------------------- | --------------------------------------------------------- | ---------------------------------------------------------- |
| `config.loaded`      | `function()`                                              | After config evaluation completes successfully             |
| `config.reloaded`    | `function()`                                              | After hot-reload succeeds                                  |
| `window.created`     | `function(window_id: number)`                             | New GUI window opened                                      |
| `window.focused`     | `function(window_id: number)`                             | Window gains focus                                         |
| `window.resized`     | `function(window_id: number, cols: number, rows: number)` | Window resized                                             |
| `pane.created`       | `function(pane_id: number)`                               | New pane spawned                                           |
| `pane.focused`       | `function(pane_id: number)`                               | Pane gains focus                                           |
| `pane.closed`        | `function(pane_id: number, exit_code: number)`            | Pane's process exited                                      |
| `pane.title_changed` | `function(pane_id: number, title: string)`                | Pane title updated                                         |
| `pane.cwd_changed`   | `function(pane_id: number, cwd: string)`                  | Working directory changed (OSC 7)                          |
| `tab.created`        | `function(tab_id: number)`                                | New tab created                                            |
| `tab.closed`         | `function(tab_id: number)`                                | Tab closed                                                 |
| `tab.switched`       | `function(tab_id: number)`                                | Active tab changed                                         |
| `workspace.created`  | `function(workspace_id: number, name: string)`            | New workspace created                                      |
| `workspace.switched` | `function(workspace_id: number, name: string)`            | Active workspace changed                                   |
| `mode.changed`       | `function(pane_id: number, mode: string)`                 | Mode changed ("normal", "copy", "resize") for focused pane |

Multiple handlers can be registered for the same event. Handlers fire in registration order. A handler returning `false` cancels subsequent handlers for that event.

#### Layout API

Declarative layout definitions. Layouts can be loaded by name from the command palette (`# layout_name`) or via `oakterm.action.load_layout("name")`.

```lua
oakterm.layout.define("dev", {
    tabs = {
        { name = "code", panes = {
            { command = "nvim", split = "left", size = 0.65 },
            { split = "right", children = {
                { command = "npm run dev", split = "top" },
                { split = "bottom" },
            }},
        }},
        { name = "git", panes = { { command = "lazygit" } } },
    },
})
```

Each `panes` list maps to a container's children in the layout tree (Spec-0007). `split` indicates the child's position within its parent container: `"left"` / `"right"` create horizontal containers, `"top"` / `"bottom"` create vertical containers. `size` is the proportional weight as a float (0.0-1.0); omitted children share remaining weight equally. The idea doc uses `"65%"` string syntax, which is accepted as sugar for `0.65`. `children` nests a sub-container. `command` is optional (default: shell).

### Custom `require()`

The standard Lua `package` library is not loaded. A custom `require()` function replaces it.

**Resolution:**

1. Convert dot-separated module names to path separators: `require("keybinds")` → `keybinds.lua`, `require("modules.theme")` → `modules/theme.lua`.
2. Also check `<name>/init.lua` for directory-style modules.
3. Resolve the path relative to the config directory (`~/.config/oakterm/`).
4. **Security:** Canonicalize the resolved path and verify it starts with the canonicalized config directory. Reject paths that escape via `..` or symlinks.
5. Check the loaded-module cache. If already loaded, return the cached value.
6. Read the file, evaluate it as a Lua chunk, cache the return value.

**Error on missing module:** `require("nonexistent")` raises a Lua error: `module 'nonexistent' not found`.

**Circular requires:** Standard Lua behavior. If A requires B and B requires A, B gets an incomplete table for A (whatever A had returned before B was required). This is a known Lua footgun documented in user-facing docs.

### Hot-Reload

File watching and config re-evaluation on save.

**Mechanism:**

1. Watch all `.lua` files in the config directory tree using the `notify` crate with a 300ms debounce (via `notify-debouncer-full`).
2. On debounced change event, create a **new Lua VM** (fresh state, no carryover from previous evaluation).
3. Register the OakTerm API surface into the new VM.
4. Evaluate `config.lua` in the new VM.
5. If evaluation succeeds:
   - Extract the config values and event handlers from the new VM.
   - Atomically swap the new config and event registry into the running daemon.
   - Drop the old VM and its registry keys.
   - Fire `config.reloaded` event on the new handlers.
   - Auto-dismiss any error overlay.
6. If evaluation fails:
   - Keep the current (last-known-good) config and event handlers.
   - Display the error in the terminal (see Error Display below).
   - Log the error.

**First launch with no config:** Use built-in defaults. The terminal must always start. If `~/.config/oakterm/config.lua` does not exist, all config values are defaults and no event handlers are registered.

**First launch with broken config:** Use built-in defaults and display the error. The user can fix the config; hot-reload picks up the fix.

### Event Handler Storage

Lua callback functions are stored using mlua's `RegistryKey` mechanism.

```rust
struct EventRegistry {
    handlers: HashMap<String, Vec<RegistryKey>>,
}
```

`RegistryKey` is `!Send` — all Lua interaction happens on a single dedicated thread. The daemon communicates with the Lua thread via channels.

On hot-reload, all old `RegistryKey`s are removed from the Lua registry (preventing leaks) before the old VM is dropped.

### Error Display

Config errors are displayed as a **banner bar** at the top of the terminal window.

**Requirements:**

- The banner is a thin bar (1-2 lines) at the top of the window. Terminal content shifts down to make room — nothing is hidden behind the banner.
- It shows: error type, message, file path, line number. Long messages truncate with a reference to `oakterm --log` for the full traceback.
- The banner is persistent — it does not auto-dismiss on a timer.
- It auto-dismisses when the config is successfully reloaded.
- The terminal remains fully functional below the banner. The user can type commands, switch panes, and edit their config file.
- The banner uses a distinct background color (red/orange for errors, yellow for warnings).
- Dismissible via keybind or click.

**Error types displayed:**

| Error              | Example                                                                    |
| ------------------ | -------------------------------------------------------------------------- |
| Syntax error       | `config.lua:12: unexpected symbol near 'end'`                              |
| Runtime error      | `keybinds.lua:5: attempt to index a nil value (field 'action')`            |
| Unknown config key | `config.lua:3: unknown config key 'font_szie' (did you mean 'font_size'?)` |
| Memory exceeded    | `config evaluation exceeded 16 MiB memory limit`                           |
| Timeout            | `config evaluation timed out (500ms)`                                      |
| Module not found   | `module 'missing_module' not found`                                        |

### LLS Type Stubs

OakTerm ships type stub files for Lua Language Server autocomplete.

**Files installed to the config directory:**

```text
~/.config/oakterm/
├── types/
│   └── oakterm.lua         # ---@meta stub with all API types
└── .luarc.json             # LLS configuration pointing to types/
```

**`.luarc.json`:**

```json
{
  "runtime": {
    "version": "Lua 5.4"
  },
  "workspace": {
    "library": ["types"]
  },
  "diagnostics": {
    "globals": ["oakterm"]
  }
}
```

**Stub delivery:**

- On every launch, `types/oakterm.lua` is written if the content differs from the embedded version. This keeps stubs current after upgrades without user intervention.
- `oakterm --init-config` creates the full config directory: `config.lua` (commented template), `.luarc.json`, and `types/oakterm.lua`. User files (`config.lua`, `.luarc.json`) are created only if absent and never overwritten.
- `.luarc.json` is not created automatically on launch — it changes editor behavior and requires explicit opt-in via `--init-config`.
- The stubs are generated from the same Rust config type definitions that the runtime uses, keeping them in sync. Initially hand-written; codegen from `schemars` JSON Schema is a future optimization.

## Behavior

### Config Evaluation Order

1. Create new Lua VM with sandboxed stdlib.
2. Remove dangerous BASE functions (`dofile`, `loadfile`, `load`).
3. Set memory limit (16 MiB) and instruction hook (500ms timeout).
4. Register `oakterm` module (config table, action constructors, utility functions).
5. Install custom `require()` with config-directory-only resolution.
6. Evaluate `~/.config/oakterm/config.lua`.
7. Remove instruction hook.
8. Extract config values from `oakterm.config` table.
9. Validate config values (type checking, range checking).
10. Return config + event registry, or error.

### Config Value Validation

After evaluation, config values are validated:

- **Type checking:** `font_size` must be a number, `font_family` must be a string, etc.
- **Range checking:** `font_size` must be > 0 and < 200. `padding` values must be >= 0.
- **Size parsing:** Values like `"50MB"` are parsed to byte counts. Invalid suffixes raise errors.
- **Enum validation:** `cursor_style` must be one of the known values. Invalid values raise errors with the list of valid options.

Validation errors are treated the same as evaluation errors — the last-known-good config is kept and the error is displayed.

### Interaction with Wire Protocol

Config changes (from hot-reload or initial load) are communicated to GUI clients via the [ConfigChanged](0001-daemon-wire-protocol.md) message (msg_type `0x84`). The payload contains the serialized config values needed by the renderer (fonts, colors, padding, cursor style).

## Constraints

- **Evaluation time:** Config evaluation should complete in < 50ms for typical configs. The 500ms timeout is a safety net for pathological cases, not a target.
- **Memory usage:** The Lua VM (including config state and event handlers) should use < 2 MiB for typical configs. The 16 MiB limit is a safety net.
- **Reload latency:** From file save to config applied should be < 500ms (300ms debounce + < 200ms evaluation + atomic swap).
- **Module cache:** `require()` caches return values. The cache is per-VM-instance and cleared on reload (new VM = empty cache).
- **Config directory:** `~/.config/oakterm/` on Linux/macOS, `%APPDATA%\oakterm\` on Windows. Follows XDG Base Directory specification on Linux.

## References

- [ADR 0005: Lua 5.4 Sandboxed Config](../adrs/0005-lua-sandboxed-config.md)
- [Spec 0001: Daemon Wire Protocol](0001-daemon-wire-protocol.md) — ConfigChanged message
- [09-config.md](../ideas/09-config.md)
- [mlua crate](https://docs.rs/mlua/latest/mlua/)
- [Lua Language Server](https://github.com/LuaLS/lua-language-server)
- [notify crate](https://docs.rs/notify/latest/notify/)
