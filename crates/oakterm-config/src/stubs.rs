//! Embedded type stubs, `.luarc.json`, and config template for `--init-config`.
//!
//! These are compiled into the binary. On `--init-config` or first launch,
//! the stubs are written to the user's config directory.

/// LLS type stub for `oakterm` API. Written to `types/oakterm.lua`.
pub(crate) const OAKTERM_LUA_STUB: &str = r#"---@meta _

-- OakTerm Lua API type definitions for Lua Language Server.
-- This file is auto-generated — do not edit. It will be overwritten on upgrade.

---@alias oakterm.CursorStyle "block"|"underline"|"bar"
---@alias oakterm.WindowDecorations "full"|"none"
---@alias oakterm.UpdateCheck "off"|"check"
---@alias oakterm.LogLevel "debug"|"info"|"warn"|"error"
---@alias oakterm.Platform "macos"|"linux"|"windows"
---@alias oakterm.Appearance "dark"|"light"
---@alias oakterm.PaneDirection "left"|"right"|"up"|"down"
---@alias oakterm.EventName
---| "appearance.changed"
---| "config.loaded"
---| "config.reloaded"
---| "window.created"
---| "window.focused"
---| "window.resized"
---| "pane.created"
---| "pane.focused"
---| "pane.closed"
---| "pane.title_changed"
---| "pane.cwd_changed"

---@class oakterm.Padding
---@field top integer Non-negative pixel value (default: 8)
---@field bottom integer Non-negative pixel value (default: 8)
---@field left integer Non-negative pixel value (default: 12)
---@field right integer Non-negative pixel value (default: 12)

---@class oakterm.SplitPaneOpts
---@field direction oakterm.PaneDirection Split direction
---@field size? number Size ratio 0.0-1.0 (default: 0.5)

--- Opaque action value returned by oakterm.action.* constructors.
---@class oakterm.Action

---@class oakterm.ActionModule
local ActionModule = {}

--- Scroll up by lines (0 = one page).
---@param lines? integer Lines to scroll (default: 0)
---@return oakterm.Action
function ActionModule.scroll_up(lines) end

--- Scroll down by lines (0 = one page).
---@param lines? integer Lines to scroll (default: 0)
---@return oakterm.Action
function ActionModule.scroll_down(lines) end

--- Jump to previous (-1) or next (1) shell prompt.
---@param direction integer -1 for previous, 1 for next
---@return oakterm.Action
function ActionModule.scroll_to_prompt(direction) end

--- Send raw bytes to the PTY.
---@param data string Raw byte string (e.g., "\x1b[A")
---@return oakterm.Action
function ActionModule.send_string(data) end

--- Copy selection to clipboard.
---@return oakterm.Action
function ActionModule.copy() end

--- Paste from clipboard.
---@return oakterm.Action
function ActionModule.paste() end

--- Toggle fullscreen mode.
---@return oakterm.Action
function ActionModule.toggle_fullscreen() end

--- Trigger config reload.
---@return oakterm.Action
function ActionModule.reload_config() end

--- Split the focused pane.
---@param opts oakterm.SplitPaneOpts
---@return oakterm.Action
function ActionModule.split_pane(opts) end

--- Close the focused pane.
---@return oakterm.Action
function ActionModule.close_pane() end

--- Focus pane in a direction.
---@param direction oakterm.PaneDirection
---@return oakterm.Action
function ActionModule.focus_pane_direction(direction) end

--- Open a new tab.
---@return oakterm.Action
function ActionModule.new_tab() end

--- Close the focused tab.
---@return oakterm.Action
function ActionModule.close_tab() end

--- Show the command palette.
---@return oakterm.Action
function ActionModule.show_command_palette() end

--- Config fields. Unknown keys raise errors with "did you mean?" suggestions.
---@class oakterm.Config
---@field font_family string Font family name (default: platform default)
---@field font_size number Font size in points, 0-200 exclusive (default: 14.0)
---@field cursor_style oakterm.CursorStyle Cursor visual style (default: "block")
---@field cursor_blink boolean Cursor blink enabled (default: true)
---@field scrollback_limit integer|string Scrollback size in bytes or "50MB" (default: "50MB")
---@field save_alternate_scrollback boolean Save alternate screen content (default: true)
---@field scroll_indicator boolean Show scroll position indicator (default: true)
---@field padding oakterm.Padding Window padding in pixels
---@field theme string Theme name (default: built-in)
---@field window_decorations oakterm.WindowDecorations Window chrome style (default: "full")
---@field confirm_close_process boolean Confirm before closing pane with running process (default: true)
---@field scrollback_archive boolean Enable scrollback disk archive (default: true)
---@field scrollback_archive_limit integer|string Archive size limit in bytes or "1GB" (default: "1GB")
---@field daemon_persist boolean Keep daemon alive after last window closes (default: false)
---@field check_for_updates oakterm.UpdateCheck Update check policy (default: "off")

---@class oakterm
---@field config oakterm.Config Configuration table
---@field action oakterm.ActionModule Action constructors for keybindings
oakterm = {}

--- Register a keybinding.
---
--- Key chord format: `modifier+modifier+key`
--- Modifiers: `ctrl`/`control`, `alt`/`option`/`opt`, `shift`, `super`/`cmd`/`command`/`win`
--- Keys: single characters, or named keys (`up`, `down`, `left`, `right`,
--- `home`, `end`, `pageup`, `pagedown`, `tab`, `enter`, `backspace`,
--- `escape`/`esc`, `delete`, `insert`, `space`, `f1`-`f12`)
---@param key string Key chord (e.g., "ctrl+d", "super+shift+t")
---@param action oakterm.Action|function Action from oakterm.action.* or a callback
function oakterm.keybind(key, action) end

--- Register an event handler. Multiple handlers per event fire in order.
--- Return `false` from a handler to cancel subsequent handlers.
---@param event oakterm.EventName Event name
---@param callback function Handler function
function oakterm.on(event, callback) end

--- Get the current OS name.
---@return oakterm.Platform
function oakterm.os() end

--- Get the current system appearance (dark or light mode).
---@return oakterm.Appearance
function oakterm.appearance() end

--- Get the system hostname.
---@return string
function oakterm.hostname() end

--- Log a message (appears in `oakterm --log` output, not in the terminal).
---@param level oakterm.LogLevel Log level
---@param message string Log message
function oakterm.log(level, message) end
"#;

/// `.luarc.json` for LLS workspace configuration. Written to config root.
pub(crate) const LUARC_JSON: &str = r#"{
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
"#;

/// Starter config template. Written to `config.lua` on `--init-config`.
pub(crate) const CONFIG_TEMPLATE: &str = r#"-- OakTerm configuration
-- Type hints require Lua Language Server (LuaLS) in your editor.
-- Run `oakterm --init-config` to regenerate type stubs after an upgrade.

-- Font
-- oakterm.config.font_family = "JetBrains Mono"
-- oakterm.config.font_size = 14.0

-- Appearance
-- oakterm.config.theme = "catppuccin"
-- oakterm.config.cursor_style = "block"    -- "block", "underline", "bar"
-- oakterm.config.cursor_blink = true
-- oakterm.config.window_decorations = "full" -- "full", "none"
-- oakterm.config.padding = { top = 8, bottom = 8, left = 12, right = 12 }
-- oakterm.config.scroll_indicator = true

-- Scrollback
-- oakterm.config.scrollback_limit = "50MB"
-- oakterm.config.save_alternate_scrollback = true
-- oakterm.config.scrollback_archive = true
-- oakterm.config.scrollback_archive_limit = "1GB"

-- Behavior
-- oakterm.config.confirm_close_process = true
-- oakterm.config.daemon_persist = false
-- oakterm.config.check_for_updates = "off"  -- "off", "check"

-- Keybindings
-- oakterm.keybind("super+shift+t", oakterm.action.new_tab())
-- oakterm.keybind("super+shift+w", oakterm.action.close_tab())
-- oakterm.keybind("ctrl+shift+c", oakterm.action.copy())
-- oakterm.keybind("ctrl+shift+v", oakterm.action.paste())

-- Platform-specific overrides
-- if oakterm.os() == "macos" then
--     oakterm.config.font_size = 15.0
-- end

-- Event handlers
-- oakterm.on("config.loaded", function()
--     oakterm.log("info", "Config loaded!")
-- end)

-- Dark/light mode
-- oakterm.on("appearance.changed", function(appearance)
--     oakterm.log("info", "Appearance: " .. appearance)
-- end)
"#;
