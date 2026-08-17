//! Config proxy table: `oakterm.config` with per-key validation.
//! Also registers `oakterm.on(event, callback)` and
//! `oakterm.keybind(key, action)` with `oakterm.action.*` constructors.

use crate::event::{EVENT_REGISTRY_KEY, EventRegistry, KNOWN_EVENTS};
use crate::keybind::{Action, KeyChord, KeybindRegistry};
use crate::schema::{
    self, ConfigValues, CursorStyle, Padding, StatusBarPosition, TextBlending, UpdateCheck,
    WindowDecorations,
};
use mlua::{Function, Lua, Table, Value};

/// Registry key for the hidden backing table that stores validated config values.
const BACKING_KEY: &str = "__oakterm_config_backing";

/// Registry key for keybind entries stored during config evaluation.
const KEYBIND_REGISTRY_KEY: &str = "__oakterm_keybind_registry";

/// Register the `oakterm.config` proxy table into the Lua VM.
///
/// Creates the `oakterm` global table with a `config` subtable that validates
/// every assignment via `__newindex`. Must be called after `create_lua_vm()`.
///
/// # Errors
///
/// Returns an error if table registration fails.
pub fn register_config_table(lua: &Lua) -> mlua::Result<()> {
    let backing = lua.create_table()?;
    lua.set_named_registry_value(BACKING_KEY, backing)?;

    let proxy = lua.create_table()?;
    let meta = lua.create_table()?;

    // __index: read from backing table.
    let backing_ref: Table = lua.named_registry_value(BACKING_KEY)?;
    meta.set("__index", backing_ref)?;

    // __newindex: validate key + value, then write to backing.
    meta.set(
        "__newindex",
        lua.create_function(|lua, (_, key, value): (Table, mlua::LuaString, Value)| {
            let key_str = key.to_str()?;

            let Some(def) = schema::find_key(&key_str) else {
                let msg = if let Some(suggestion) = schema::suggest_key(&key_str) {
                    format!("unknown config key '{key_str}' (did you mean '{suggestion}'?)")
                } else {
                    format!("unknown config key '{key_str}'")
                };
                return Err(mlua::Error::RuntimeError(msg));
            };

            (def.validate)(lua, &value)
                .map_err(|e| mlua::Error::RuntimeError(format!("{}: {e}", def.name)))?;

            let backing: Table = lua.named_registry_value(BACKING_KEY)?;
            backing.set(key_str, value)?;
            Ok(())
        })?,
    )?;

    // __metatable: block getmetatable/setmetatable introspection.
    meta.set("__metatable", "oakterm.config")?;

    proxy.set_metatable(Some(meta))?;

    // Initialize event handler table in the Lua named registry.
    // During eval, oakterm.on() appends callbacks here as Lua functions.
    // After eval, extract_event_registry() converts them to RegistryKeys.
    lua.set_named_registry_value(EVENT_REGISTRY_KEY, lua.create_table()?)?;

    // oakterm.on(event, callback)
    let on_fn = lua.create_function(|lua, (event, callback): (mlua::LuaString, Function)| {
        let event_str = event.to_str()?;

        if !KNOWN_EVENTS.contains(&event_str.as_ref()) {
            let suggestion = KNOWN_EVENTS
                .iter()
                .filter(|&&e| strsim::jaro(&event_str, e) > 0.8)
                .max_by(|a, b| {
                    strsim::jaro(&event_str, a)
                        .partial_cmp(&strsim::jaro(&event_str, b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .copied();
            let msg = if let Some(s) = suggestion {
                format!("unknown event '{event_str}' (did you mean '{s}'?)")
            } else {
                format!("unknown event '{event_str}'")
            };
            return Err(mlua::Error::RuntimeError(msg));
        }

        let event_table: Table = lua.named_registry_value(EVENT_REGISTRY_KEY)?;
        let handlers: Table =
            if let Some(t) = event_table.get::<Option<Table>>(event_str.as_ref())? {
                t
            } else {
                let t = lua.create_table()?;
                event_table.set(event_str.as_ref(), t.clone())?;
                t
            };
        handlers.push(callback)?;
        Ok(())
    })?;

    // Initialize keybind registry table.
    lua.set_named_registry_value(KEYBIND_REGISTRY_KEY, lua.create_table()?)?;

    // oakterm.action.* constructors — return tagged tables.
    let action = lua.create_table()?;
    register_action_constructors(lua, &action)?;

    // oakterm.keybind(key, action_or_callback)
    let keybind_fn = lua.create_function(|lua, (key, action): (mlua::LuaString, Value)| {
        let key_str = key.to_str()?;
        if let Err(e) = validate_keybind_chord(&key_str) {
            return Err(mlua::Error::RuntimeError(format!(
                "invalid key chord '{key_str}': {e}"
            )));
        }

        // Validate action is a table (from oakterm.action.*) or a function.
        match &action {
            Value::Table(t) => {
                if t.get::<Option<mlua::LuaString>>("__action_type")?.is_none() {
                    return Err(mlua::Error::RuntimeError(
                        "keybind action must be from oakterm.action.* or a function".to_string(),
                    ));
                }
            }
            Value::Function(_) => {}
            _ => {
                return Err(mlua::Error::RuntimeError(
                    "keybind action must be from oakterm.action.* or a function".to_string(),
                ));
            }
        }

        let registry: Table = lua.named_registry_value(KEYBIND_REGISTRY_KEY)?;
        let entry = lua.create_table()?;
        entry.set("key", key_str.as_ref())?;
        entry.set("action", action)?;
        registry.push(entry)?;
        Ok(())
    })?;

    // Register oakterm global with all subtables and utility functions.
    let oakterm = lua.create_table()?;
    oakterm.set("config", proxy)?;
    oakterm.set("on", on_fn)?;
    oakterm.set("action", action)?;
    oakterm.set("keybind", keybind_fn)?;
    register_platform_utilities(lua, &oakterm)?;
    lua.globals().set("oakterm", oakterm)?;

    Ok(())
}

/// Split a keybind string into its `leader+` routing prefix (if any) and
/// the chord to parse. Leading/trailing whitespace is trimmed first.
fn split_leader_prefix(key_str: &str) -> (bool, &str) {
    let trimmed = key_str.trim();
    match trimmed.strip_prefix("leader+") {
        Some(rest) => (true, rest),
        None => (false, trimmed),
    }
}

/// Syntax-check a keybind chord at `oakterm.keybind()` call time.
/// `oak_mod` expands at extraction with the final config value, so it
/// validates against the empty set (an `oak_mod` expansion can never
/// introduce a parse error); a `leader+` prefix routes to the leader
/// table at extraction, so the chord after it is what's validated.
fn validate_keybind_chord(key_str: &str) -> Result<(), String> {
    let (_, to_validate) = split_leader_prefix(key_str);
    KeyChord::parse_with_oak_mod(to_validate, crate::keybind::ModifierSet::default()).map(|_| ())
}

/// Register `oakterm.os()`, `oakterm.hostname()`, and `oakterm.log()`.
fn register_platform_utilities(lua: &Lua, oakterm: &Table) -> mlua::Result<()> {
    // oakterm.os() — compile-time platform detection.
    let platform_fn = lua.create_function(|_, ()| {
        let name = if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(target_os = "windows") {
            "windows"
        } else {
            "unknown"
        };
        Ok(name)
    })?;

    // oakterm.hostname() — system hostname (writes to stderr on non-UTF-8).
    let hostname_fn = lua.create_function(|_, ()| {
        let raw = gethostname::gethostname();
        if let Some(s) = raw.to_str() {
            Ok(s.to_string())
        } else {
            let lossy = raw.to_string_lossy().into_owned();
            tracing::warn!(hostname = %lossy, "hostname contains non-UTF-8 bytes");
            Ok(lossy)
        }
    })?;

    // oakterm.log(level, message) — config-level logging.
    let log_fn =
        lua.create_function(|_, (level, message): (mlua::LuaString, mlua::LuaString)| {
            let level_str = level.to_str()?;
            match level_str.as_ref() {
                "debug" | "info" | "warn" | "error" => {}
                _ => {
                    return Err(mlua::Error::RuntimeError(format!(
                        "invalid log level '{level_str}' (expected: debug, info, warn, error)"
                    )));
                }
            }
            let msg = message.to_str()?;
            match level_str.as_ref() {
                "debug" => tracing::debug!(target: "config", "{msg}"),
                "info" => tracing::info!(target: "config", "{msg}"),
                "warn" => tracing::warn!(target: "config", "{msg}"),
                "error" => tracing::error!(target: "config", "{msg}"),
                _ => unreachable!(),
            }
            Ok(())
        })?;

    // oakterm.appearance() — current system dark/light mode.
    let appearance_fn = lua.create_function(|_, ()| Ok(crate::current_appearance()))?;

    oakterm.set("os", platform_fn)?;
    oakterm.set("hostname", hostname_fn)?;
    oakterm.set("log", log_fn)?;
    oakterm.set("appearance", appearance_fn)?;
    Ok(())
}

/// Register `oakterm.action.*` constructor functions.
fn register_action_constructors(lua: &Lua, action: &Table) -> mlua::Result<()> {
    // Parameterless actions.
    for name in [
        "copy",
        "paste",
        "toggle_fullscreen",
        "reload_config",
        "close_pane",
        "new_tab",
        "close_tab",
        "next_tab",
        "previous_tab",
        "show_command_palette",
    ] {
        let n = name.to_string();
        action.set(
            name,
            lua.create_function(move |lua, ()| {
                let t = lua.create_table()?;
                t.set("__action_type", n.as_str())?;
                Ok(t)
            })?,
        )?;
    }

    // scroll_up(lines) / scroll_down(lines)
    for name in ["scroll_up", "scroll_down"] {
        let n = name.to_string();
        action.set(
            name,
            lua.create_function(move |lua, lines: Option<i64>| {
                let t = lua.create_table()?;
                t.set("__action_type", n.as_str())?;
                t.set("lines", lines.unwrap_or(0))?;
                Ok(t)
            })?,
        )?;
    }

    // scroll_to_prompt(direction)
    action.set(
        "scroll_to_prompt",
        lua.create_function(|lua, direction: i64| {
            let t = lua.create_table()?;
            t.set("__action_type", "scroll_to_prompt")?;
            t.set("direction", direction)?;
            Ok(t)
        })?,
    )?;

    // send_string(data)
    action.set(
        "send_string",
        lua.create_function(|lua, data: mlua::LuaString| {
            let t = lua.create_table()?;
            t.set("__action_type", "send_string")?;
            t.set("data", data)?;
            Ok(t)
        })?,
    )?;

    // split_pane({ direction, size })
    action.set(
        "split_pane",
        lua.create_function(|lua, opts: Table| {
            let t = lua.create_table()?;
            t.set("__action_type", "split_pane")?;
            t.set("direction", opts.get::<mlua::LuaString>("direction")?)?;
            t.set("size", opts.get::<f64>("size").unwrap_or(0.5))?;
            Ok(t)
        })?,
    )?;

    // switch_tab(index) — 1-based strip index
    action.set(
        "switch_tab",
        lua.create_function(|lua, index: i64| {
            if !(1..=i64::from(u32::MAX)).contains(&index) {
                return Err(mlua::Error::RuntimeError(format!(
                    "switch_tab index must be between 1 and {}, got {index}",
                    u32::MAX
                )));
            }
            let t = lua.create_table()?;
            t.set("__action_type", "switch_tab")?;
            t.set("index", index)?;
            Ok(t)
        })?,
    )?;

    // focus_pane_direction(direction)
    action.set(
        "focus_pane_direction",
        lua.create_function(|lua, direction: mlua::LuaString| {
            let t = lua.create_table()?;
            t.set("__action_type", "focus_pane_direction")?;
            t.set("direction", direction)?;
            Ok(t)
        })?,
    )?;

    Ok(())
}

/// Extract the `EventRegistry` from the Lua VM after config evaluation.
///
/// Converts Lua function references stored by `oakterm.on()` into
/// `RegistryKey`s owned by the `EventRegistry`.
pub(crate) fn extract_event_registry(lua: &Lua) -> EventRegistry {
    let mut registry = EventRegistry::new();
    let Ok(event_table) = lua.named_registry_value::<Table>(EVENT_REGISTRY_KEY) else {
        tracing::warn!("failed to read event registry from Lua VM");
        return registry;
    };

    for pair in event_table.pairs::<mlua::LuaString, Table>() {
        let (event_name, handlers) = match pair {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "skipping malformed event registry entry");
                continue;
            }
        };
        let event_str = match event_name.to_str() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "skipping event with invalid name");
                continue;
            }
        };
        for handler in handlers.sequence_values::<Function>() {
            let callback = match handler {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(event = %event_str, error = %e, "skipping unreadable handler");
                    continue;
                }
            };
            if let Err(e) = registry.register(lua, &event_str, callback) {
                tracing::warn!(event = %event_str, error = %e, "failed to register handler");
            }
        }
    }

    registry
}

/// Extract the `KeybindRegistry` from the Lua VM after config evaluation.
///
/// Converts keybind entries stored by `oakterm.keybind()` into
/// `(KeyChord, Action)` pairs. Callback functions become `RegistryKey`s.
pub(crate) fn extract_keybind_registry(
    lua: &Lua,
    oak_mod: crate::keybind::ModifierSet,
    leader_configured: bool,
) -> KeybindRegistry {
    // Seed with the built-in defaults; user binds append after and win
    // on conflict (lookup is last-registration). This is the single
    // source of the default table — the no-config path also uses it.
    let mut registry = KeybindRegistry::with_defaults_for(oak_mod);
    let Ok(entries) = lua.named_registry_value::<Table>(KEYBIND_REGISTRY_KEY) else {
        tracing::warn!("failed to read keybind registry from Lua VM");
        return registry;
    };

    for entry in entries.sequence_values::<Table>() {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "skipping malformed keybind entry");
                continue;
            }
        };

        let key_str: String = match entry.get("key") {
            Ok(k) => k,
            Err(e) => {
                tracing::warn!(error = %e, "skipping keybind with missing key");
                continue;
            }
        };

        // A `leader+` prefix routes the binding into the leader table.
        let (is_leader, chord_str) = split_leader_prefix(&key_str);
        let chord = match KeyChord::parse_with_oak_mod(chord_str, oak_mod) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(chord = %key_str, error = %e, "skipping keybind with invalid chord");
                continue;
            }
        };

        let action_value: Value = match entry.get("action") {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(chord = %key_str, error = %e, "skipping keybind with missing action");
                continue;
            }
        };

        let action = match action_value {
            Value::Function(f) => match lua.create_registry_value(f) {
                Ok(key) => Action::Callback(key),
                Err(e) => {
                    tracing::warn!(chord = %key_str, error = %e, "failed to store callback");
                    continue;
                }
            },
            Value::Table(t) => match extract_action_from_table(&t) {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!(chord = %key_str, error = %e, "skipping keybind");
                    continue;
                }
            },
            _ => {
                tracing::warn!(chord = %key_str, "skipping keybind: action is not a table or function");
                continue;
            }
        };

        if is_leader {
            registry.register_leader(chord, action);
        } else {
            registry.register(chord, action);
        }
    }

    if registry.has_leader_bindings() && !leader_configured {
        tracing::warn!(
            "leader+ keybinds registered but oakterm.config.leader is unset; they cannot fire"
        );
    }

    registry
}

/// Convert an action table `{ __action_type = "...", ... }` to an `Action`.
fn extract_action_from_table(t: &Table) -> Result<Action, String> {
    let action_type: String = t
        .get::<Option<String>>("__action_type")
        .map_err(|e| format!("failed to read __action_type: {e}"))?
        .ok_or_else(|| "missing __action_type field".to_string())?;

    match action_type.as_str() {
        "copy" => Ok(Action::Copy),
        "paste" => Ok(Action::Paste),
        "toggle_fullscreen" => Ok(Action::ToggleFullscreen),
        "reload_config" => Ok(Action::ReloadConfig),
        "close_pane" => Ok(Action::ClosePane),
        "new_tab" => Ok(Action::NewTab),
        "close_tab" => Ok(Action::CloseTab),
        "next_tab" => Ok(Action::NextTab),
        "previous_tab" => Ok(Action::PreviousTab),
        "switch_tab" => {
            let index: i64 = t
                .get("index")
                .map_err(|e| format!("switch_tab missing index: {e}"))?;
            let index = u32::try_from(index)
                .ok()
                .and_then(std::num::NonZeroU32::new)
                .ok_or_else(|| {
                    format!(
                        "switch_tab index must be between 1 and {}, got {index}",
                        u32::MAX
                    )
                })?;
            Ok(Action::SwitchTab(index))
        }
        "show_command_palette" => Ok(Action::ShowCommandPalette),
        "scroll_up" => {
            let lines: i64 = t.get("lines").unwrap_or(0);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Ok(Action::ScrollUp(lines.clamp(0, i64::from(u32::MAX)) as u32))
        }
        "scroll_down" => {
            let lines: i64 = t.get("lines").unwrap_or(0);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Ok(Action::ScrollDown(
                lines.clamp(0, i64::from(u32::MAX)) as u32
            ))
        }
        "scroll_to_prompt" => {
            let direction: i64 = t
                .get("direction")
                .map_err(|e| format!("scroll_to_prompt missing direction: {e}"))?;
            #[allow(clippy::cast_possible_truncation)]
            Ok(Action::ScrollToPrompt(direction as i32))
        }
        "send_string" => {
            let data: mlua::LuaString = t
                .get("data")
                .map_err(|e| format!("send_string missing data: {e}"))?;
            Ok(Action::SendString(data.as_bytes().to_vec()))
        }
        "split_pane" => {
            let direction: String = t
                .get("direction")
                .map_err(|e| format!("split_pane missing direction: {e}"))?;
            let size: f64 = t.get("size").unwrap_or(0.5);
            Ok(Action::SplitPane { direction, size })
        }
        "focus_pane_direction" => {
            let direction: String = t
                .get("direction")
                .map_err(|e| format!("focus_pane_direction missing direction: {e}"))?;
            Ok(Action::FocusPaneDirection(direction))
        }
        other => Err(format!("unknown action type '{other}'")),
    }
}

/// Extract a `ConfigValues` struct from the Lua VM after config evaluation.
///
/// Unset keys use their default values.
///
/// # Errors
///
/// Returns an error if a set value cannot be converted to the expected Rust type.
#[allow(clippy::too_many_lines)] // One field extraction per config key.
pub fn extract_config(lua: &Lua) -> mlua::Result<ConfigValues> {
    let backing: Table = lua.named_registry_value(BACKING_KEY)?;
    let defaults = ConfigValues::default();

    let font_family: String = backing
        .get::<Option<mlua::LuaString>>("font_family")?
        .map(|s| s.to_str().map(|s| s.to_string()))
        .transpose()?
        .unwrap_or(defaults.font_family);

    let font_size: f64 = backing
        .get::<Option<f64>>("font_size")?
        .unwrap_or(defaults.font_size);

    let cursor_style: CursorStyle = match backing.get::<Option<mlua::LuaString>>("cursor_style")? {
        Some(s) => {
            let s = s.to_str()?;
            CursorStyle::from_config_str(&s).ok_or_else(|| {
                mlua::Error::RuntimeError(format!(
                    "invalid cursor_style '{s}' (expected: {})",
                    CursorStyle::ALL.join(", ")
                ))
            })?
        }
        None => defaults.cursor_style,
    };

    let cursor_blink: bool = backing
        .get::<Option<bool>>("cursor_blink")?
        .unwrap_or(defaults.cursor_blink);

    let scrollback_limit =
        extract_byte_size_field(&backing, "scrollback_limit", defaults.scrollback_limit)?;

    let save_alternate_scrollback: bool = backing
        .get::<Option<bool>>("save_alternate_scrollback")?
        .unwrap_or(defaults.save_alternate_scrollback);

    let scroll_indicator: bool = backing
        .get::<Option<bool>>("scroll_indicator")?
        .unwrap_or(defaults.scroll_indicator);

    let padding = extract_padding(&backing, defaults.padding)?;

    let theme: String = backing
        .get::<Option<mlua::LuaString>>("theme")?
        .map(|s| s.to_str().map(|s| s.to_string()))
        .transpose()?
        .unwrap_or(defaults.theme);

    let window_decorations = match backing.get::<Option<mlua::LuaString>>("window_decorations")? {
        Some(s) => {
            let s = s.to_str()?;
            WindowDecorations::from_config_str(&s).ok_or_else(|| {
                mlua::Error::RuntimeError(format!(
                    "invalid window_decorations '{s}' (expected: {})",
                    WindowDecorations::ALL.join(", ")
                ))
            })?
        }
        None => defaults.window_decorations,
    };

    let confirm_close_process: bool = backing
        .get::<Option<bool>>("confirm_close_process")?
        .unwrap_or(defaults.confirm_close_process);

    let scrollback_archive: bool = backing
        .get::<Option<bool>>("scrollback_archive")?
        .unwrap_or(defaults.scrollback_archive);

    let scrollback_archive_limit = extract_byte_size_field(
        &backing,
        "scrollback_archive_limit",
        defaults.scrollback_archive_limit,
    )?;

    let daemon_persist: bool = backing
        .get::<Option<bool>>("daemon_persist")?
        .unwrap_or(defaults.daemon_persist);

    let oak_mod = match backing.get::<Option<mlua::LuaString>>("oak_mod")? {
        Some(s) => {
            let s = s.to_str()?;
            if let Err(e) = crate::keybind::ModifierSet::parse(&s) {
                return Err(mlua::Error::RuntimeError(format!(
                    "invalid oak_mod '{s}': {e}"
                )));
            }
            s.to_string()
        }
        None => defaults.oak_mod,
    };

    let leader = match backing.get::<Value>("leader")? {
        Value::Nil => defaults.leader,
        Value::Table(t) => {
            let key: mlua::LuaString = t.get("key")?;
            let key = key.to_str()?;
            let chord = crate::keybind::KeyChord::parse(&key)
                .map_err(|e| mlua::Error::RuntimeError(format!("invalid leader key: {e}")))?;
            let timeout_ms = t
                .get::<Option<u64>>("timeout")?
                .unwrap_or(schema::DEFAULT_LEADER_TIMEOUT_MS);
            let leader = crate::schema::LeaderKey::new(chord, timeout_ms)
                .map_err(mlua::Error::RuntimeError)?;
            Some(leader)
        }
        other => {
            return Err(mlua::Error::RuntimeError(format!(
                "invalid leader value: expected table or nil, got {}",
                other.type_name()
            )));
        }
    };

    let status_bar: bool = backing
        .get::<Option<bool>>("status_bar")?
        .unwrap_or(defaults.status_bar);

    let status_bar_position = match backing.get::<Option<mlua::LuaString>>("status_bar_position")? {
        Some(s) => {
            let s = s.to_str()?;
            StatusBarPosition::from_config_str(&s).ok_or_else(|| {
                mlua::Error::RuntimeError(format!(
                    "invalid status_bar_position '{s}' (expected: {})",
                    StatusBarPosition::ALL.join(", ")
                ))
            })?
        }
        None => defaults.status_bar_position,
    };

    let check_for_updates = match backing.get::<Option<mlua::LuaString>>("check_for_updates")? {
        Some(s) => {
            let s = s.to_str()?;
            UpdateCheck::from_config_str(&s).ok_or_else(|| {
                mlua::Error::RuntimeError(format!(
                    "invalid check_for_updates '{s}' (expected: {})",
                    UpdateCheck::ALL.join(", ")
                ))
            })?
        }
        None => defaults.check_for_updates,
    };

    let text_blending = match backing.get::<Option<mlua::LuaString>>("text_blending")? {
        Some(s) => {
            let s = s.to_str()?;
            TextBlending::from_config_str(&s).ok_or_else(|| {
                mlua::Error::RuntimeError(format!(
                    "invalid text_blending '{s}' (expected: {})",
                    TextBlending::ALL.join(", ")
                ))
            })?
        }
        None => defaults.text_blending,
    };

    let text_gamma: f64 = backing
        .get::<Option<f64>>("text_gamma")?
        .unwrap_or(defaults.text_gamma);

    Ok(ConfigValues {
        font_family,
        font_size,
        cursor_style,
        cursor_blink,
        scrollback_limit,
        save_alternate_scrollback,
        scroll_indicator,
        padding,
        theme,
        window_decorations,
        confirm_close_process,
        scrollback_archive,
        scrollback_archive_limit,
        daemon_persist,
        oak_mod,
        leader,
        status_bar,
        status_bar_position,
        check_for_updates,
        text_blending,
        text_gamma,
    })
}

fn extract_byte_size_field(backing: &Table, field: &str, default: u64) -> mlua::Result<u64> {
    let value: Value = backing.get(field)?;
    match value {
        Value::Nil => Ok(default),
        Value::Integer(n) => u64::try_from(n).map_err(|_| {
            mlua::Error::RuntimeError(format!("{field} must be non-negative, got {n}"))
        }),
        Value::Number(n) if n.is_finite() && n >= 0.0 =>
        {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Ok(n as u64)
        }
        Value::Number(n) => Err(mlua::Error::RuntimeError(format!(
            "{field} must be a finite non-negative number, got {n}"
        ))),
        Value::String(s) => {
            let s = s.to_str()?;
            schema::parse_byte_size(&s)
                .map_err(|e| mlua::Error::RuntimeError(format!("{field}: {e}")))
        }
        _ => Err(mlua::Error::RuntimeError(format!(
            "expected number or size string for {field}, got {}",
            value.type_name()
        ))),
    }
}

fn extract_u32_field(t: &Table, field: &str) -> mlua::Result<u32> {
    let n: i64 = t.get(field)?;
    u32::try_from(n).map_err(|_| {
        mlua::Error::RuntimeError(format!(
            "padding.{field} must be between 0 and {}, got {n}",
            u32::MAX
        ))
    })
}

fn extract_padding(backing: &Table, default: Padding) -> mlua::Result<Padding> {
    let value: Value = backing.get("padding")?;
    match value {
        Value::Nil => Ok(default),
        Value::Table(t) => Ok(Padding {
            top: extract_u32_field(&t, "top")?,
            bottom: extract_u32_field(&t, "bottom")?,
            left: extract_u32_field(&t, "left")?,
            right: extract_u32_field(&t, "right")?,
        }),
        _ => Err(mlua::Error::RuntimeError(
            "unexpected type for padding".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_lua_vm;

    fn setup() -> Lua {
        let (lua, _) = create_lua_vm().expect("VM creation failed");
        register_config_table(&lua).expect("registration failed");
        lua
    }

    #[test]
    fn set_valid_font_size() {
        let lua = setup();
        lua.load("oakterm.config.font_size = 16.0").exec().unwrap();
        let cfg = extract_config(&lua).unwrap();
        assert!((cfg.font_size - 16.0).abs() < f64::EPSILON);
    }

    #[test]
    fn set_invalid_font_size_type() {
        let lua = setup();
        let err = lua.load(r#"oakterm.config.font_size = "big""#).exec();
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("expected number"), "got: {msg}");
    }

    #[test]
    fn set_invalid_font_size_range() {
        let lua = setup();
        let err = lua.load("oakterm.config.font_size = 0").exec();
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("greater than 0 and less than 200"),
            "got: {msg}"
        );
    }

    #[test]
    fn set_valid_font_family() {
        let lua = setup();
        lua.load(r#"oakterm.config.font_family = "Fira Code""#)
            .exec()
            .unwrap();
        let cfg = extract_config(&lua).unwrap();
        assert_eq!(cfg.font_family, "Fira Code");
    }

    #[test]
    fn set_valid_cursor_style() {
        let lua = setup();
        lua.load(r#"oakterm.config.cursor_style = "bar""#)
            .exec()
            .unwrap();
        let cfg = extract_config(&lua).unwrap();
        assert_eq!(cfg.cursor_style, CursorStyle::Bar);
    }

    #[test]
    fn set_invalid_cursor_style() {
        let lua = setup();
        let err = lua.load(r#"oakterm.config.cursor_style = "beam""#).exec();
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("block, underline, bar"), "got: {msg}");
    }

    #[test]
    fn set_valid_cursor_blink() {
        let lua = setup();
        lua.load("oakterm.config.cursor_blink = false")
            .exec()
            .unwrap();
        let cfg = extract_config(&lua).unwrap();
        assert!(!cfg.cursor_blink);
    }

    #[test]
    fn set_invalid_cursor_blink() {
        let lua = setup();
        let err = lua.load(r#"oakterm.config.cursor_blink = "yes""#).exec();
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("expected boolean"), "got: {msg}");
    }

    #[test]
    fn set_scrollback_limit_string() {
        let lua = setup();
        lua.load(r#"oakterm.config.scrollback_limit = "100MB""#)
            .exec()
            .unwrap();
        let cfg = extract_config(&lua).unwrap();
        assert_eq!(cfg.scrollback_limit, 100 * 1024 * 1024);
    }

    #[test]
    fn set_scrollback_limit_number() {
        let lua = setup();
        lua.load("oakterm.config.scrollback_limit = 1048576")
            .exec()
            .unwrap();
        let cfg = extract_config(&lua).unwrap();
        assert_eq!(cfg.scrollback_limit, 1_048_576);
    }

    #[test]
    fn set_scrollback_limit_invalid() {
        let lua = setup();
        let err = lua
            .load(r#"oakterm.config.scrollback_limit = "50XB""#)
            .exec();
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("unknown size suffix"), "got: {msg}");
    }

    #[test]
    fn set_valid_padding() {
        let lua = setup();
        lua.load("oakterm.config.padding = { top = 4, bottom = 4, left = 8, right = 8 }")
            .exec()
            .unwrap();
        let cfg = extract_config(&lua).unwrap();
        assert_eq!(cfg.padding.top, 4);
        assert_eq!(cfg.padding.left, 8);
    }

    #[test]
    fn set_invalid_padding_missing_field() {
        let lua = setup();
        let err = lua.load("oakterm.config.padding = { top = 4 }").exec();
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("missing required field"), "got: {msg}");
    }

    #[test]
    fn unknown_key_raises_error() {
        let lua = setup();
        let err = lua.load("oakterm.config.font_szie = 14").exec();
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("unknown config key"), "got: {msg}");
    }

    #[test]
    fn unknown_key_suggests_match() {
        let lua = setup();
        let err = lua.load("oakterm.config.font_szie = 14").exec();
        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("did you mean 'font_size'"),
            "should suggest font_size: {msg}"
        );
    }

    #[test]
    fn unknown_key_no_suggestion() {
        let lua = setup();
        let err = lua.load("oakterm.config.zzzzz = 1").exec();
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("unknown config key"), "got: {msg}");
        assert!(!msg.contains("did you mean"), "should not suggest: {msg}");
    }

    #[test]
    fn read_config_value() {
        let lua = setup();
        lua.load("oakterm.config.font_size = 18.0").exec().unwrap();
        let result: f64 = lua.load("return oakterm.config.font_size").eval().unwrap();
        assert!((result - 18.0).abs() < f64::EPSILON);
    }

    #[test]
    fn unset_keys_return_nil() {
        let lua = setup();
        let result: Value = lua.load("return oakterm.config.font_size").eval().unwrap();
        assert!(matches!(result, Value::Nil));
    }

    #[test]
    fn extract_defaults() {
        let lua = setup();
        let cfg = extract_config(&lua).unwrap();
        assert_eq!(cfg, ConfigValues::default());
    }

    #[test]
    fn extract_after_set() {
        let lua = setup();
        lua.load(
            r#"
            oakterm.config.font_size = 20.0
            oakterm.config.cursor_style = "underline"
            "#,
        )
        .exec()
        .unwrap();
        let cfg = extract_config(&lua).unwrap();
        assert!((cfg.font_size - 20.0).abs() < f64::EPSILON);
        assert_eq!(cfg.cursor_style, CursorStyle::Underline);
        // Unset keys use defaults.
        assert!(cfg.cursor_blink);
        assert_eq!(cfg.font_family, "");
    }

    #[test]
    fn metatable_protected() {
        let lua = setup();
        let result: Value = lua
            .load("return getmetatable(oakterm.config)")
            .eval()
            .unwrap();
        match result {
            Value::String(s) => assert_eq!(s.to_str().unwrap(), "oakterm.config"),
            _ => panic!("expected string from protected metatable, got {result:?}"),
        }
    }

    #[test]
    fn set_valid_theme() {
        let lua = setup();
        lua.load(r#"oakterm.config.theme = "catppuccin-mocha""#)
            .exec()
            .unwrap();
        let cfg = extract_config(&lua).unwrap();
        assert_eq!(cfg.theme, "catppuccin-mocha");
    }

    #[test]
    fn set_valid_window_decorations() {
        let lua = setup();
        lua.load(r#"oakterm.config.window_decorations = "none""#)
            .exec()
            .unwrap();
        let cfg = extract_config(&lua).unwrap();
        assert_eq!(cfg.window_decorations, WindowDecorations::None);
    }

    #[test]
    fn set_invalid_window_decorations() {
        let lua = setup();
        let err = lua
            .load(r#"oakterm.config.window_decorations = "borderless""#)
            .exec();
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("full, none"), "got: {msg}");
    }

    #[test]
    fn oak_mod_defaults_to_the_platform_value() {
        let lua = setup();
        let cfg = extract_config(&lua).unwrap();
        assert_eq!(cfg.oak_mod, crate::keybind::default_oak_mod());
    }

    #[test]
    fn set_valid_oak_mod() {
        let lua = setup();
        lua.load(r#"oakterm.config.oak_mod = "ctrl+alt""#)
            .exec()
            .unwrap();
        let cfg = extract_config(&lua).unwrap();
        assert_eq!(cfg.oak_mod, "ctrl+alt");
    }

    #[test]
    fn set_invalid_oak_mod() {
        let lua = setup();
        let err = lua.load(r#"oakterm.config.oak_mod = "ctrl+x""#).exec();
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("unknown modifier"), "got: {msg}");
    }

    #[test]
    fn keybind_accepts_the_oak_mod_token() {
        let lua = setup();
        lua.load(r#"oakterm.keybind("oak_mod+d", oakterm.action.close_pane())"#)
            .exec()
            .unwrap();
        let mods = crate::keybind::ModifierSet::parse("ctrl+alt").unwrap();
        let reg = extract_keybind_registry(&lua, mods, false);
        let chord = KeyChord::parse("ctrl+alt+d").unwrap();
        assert!(matches!(reg.lookup(&chord), Some(Action::ClosePane)));
    }

    #[test]
    fn leader_defaults_to_none_and_parses_when_set() {
        let lua = setup();
        assert_eq!(extract_config(&lua).unwrap().leader, None);
        lua.load(r#"oakterm.config.leader = { key = "ctrl+b", timeout = 500 }"#)
            .exec()
            .unwrap();
        let leader = extract_config(&lua).unwrap().leader.unwrap();
        assert_eq!(leader.chord, KeyChord::parse("ctrl+b").unwrap());
        assert_eq!(leader.timeout_ms, 500);
    }

    #[test]
    fn leader_timeout_defaults_when_omitted() {
        let lua = setup();
        lua.load(r#"oakterm.config.leader = { key = "ctrl+a" }"#)
            .exec()
            .unwrap();
        let leader = extract_config(&lua).unwrap().leader.unwrap();
        assert_eq!(leader.timeout_ms, 1000);
    }

    #[test]
    fn leader_rejects_bad_shapes() {
        let lua = setup();
        assert!(
            lua.load(r#"oakterm.config.leader = "ctrl+b""#)
                .exec()
                .is_err()
        );
        assert!(
            lua.load("oakterm.config.leader = { timeout = 500 }")
                .exec()
                .is_err(),
            "missing key"
        );
        assert!(
            lua.load(r#"oakterm.config.leader = { key = "nope+x" }"#)
                .exec()
                .is_err()
        );
        assert!(
            lua.load(r#"oakterm.config.leader = { key = "ctrl+b", timeout = -5 }"#)
                .exec()
                .is_err()
        );
    }

    #[test]
    fn leader_rejects_zero_and_fractional_timeouts() {
        let lua = setup();
        assert!(
            lua.load(r#"oakterm.config.leader = { key = "ctrl+b", timeout = 0 }"#)
                .exec()
                .is_err()
        );
        assert!(
            lua.load(r#"oakterm.config.leader = { key = "ctrl+b", timeout = 500.5 }"#)
                .exec()
                .is_err()
        );
        assert!(
            lua.load(r#"oakterm.config.leader = { key = "oak_mod+b" }"#)
                .exec()
                .is_err(),
            "the leader chord itself takes no oak_mod token"
        );
    }

    #[test]
    fn leader_binds_compose_with_oak_mod_in_the_rest() {
        let lua = setup();
        lua.load(r#"oakterm.keybind("leader+oak_mod+d", oakterm.action.new_tab())"#)
            .exec()
            .unwrap();
        let mods = crate::keybind::ModifierSet::parse("ctrl+alt").unwrap();
        let reg = extract_keybind_registry(&lua, mods, true);
        let chord = KeyChord::parse("ctrl+alt+d").unwrap();
        assert!(reg.lookup(&chord).is_none());
        let idx = reg.lookup_leader_index(&chord).unwrap();
        assert!(matches!(reg.get_leader(idx), Some(Action::NewTab)));
    }

    #[test]
    fn leader_prefixed_binds_land_in_the_leader_table() {
        let lua = setup();
        lua.load(r#"oakterm.keybind("leader+5", oakterm.action.close_pane())"#)
            .exec()
            .unwrap();
        let mods = crate::keybind::ModifierSet::parse("ctrl+alt").unwrap();
        let reg = extract_keybind_registry(&lua, mods, true);
        let chord = KeyChord::parse("5").unwrap();
        assert!(
            reg.lookup(&chord).is_none(),
            "leader binds stay out of the main table"
        );
        let idx = reg.lookup_leader_index(&chord).unwrap();
        assert!(matches!(reg.get_leader(idx), Some(Action::ClosePane)));
    }

    #[test]
    fn status_bar_defaults_on_at_bottom() {
        let lua = setup();
        let cfg = extract_config(&lua).unwrap();
        assert!(cfg.status_bar);
        assert_eq!(cfg.status_bar_position, StatusBarPosition::Bottom);
    }

    #[test]
    fn set_status_bar_keys() {
        let lua = setup();
        lua.load("oakterm.config.status_bar = false")
            .exec()
            .unwrap();
        lua.load(r#"oakterm.config.status_bar_position = "top""#)
            .exec()
            .unwrap();
        let cfg = extract_config(&lua).unwrap();
        assert!(!cfg.status_bar);
        assert_eq!(cfg.status_bar_position, StatusBarPosition::Top);
    }

    #[test]
    fn set_invalid_status_bar_position() {
        let lua = setup();
        let err = lua
            .load(r#"oakterm.config.status_bar_position = "left""#)
            .exec();
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("bottom, top"), "got: {msg}");
    }

    #[test]
    fn set_valid_confirm_close_process() {
        let lua = setup();
        lua.load("oakterm.config.confirm_close_process = false")
            .exec()
            .unwrap();
        let cfg = extract_config(&lua).unwrap();
        assert!(!cfg.confirm_close_process);
    }

    #[test]
    fn set_valid_scrollback_archive() {
        let lua = setup();
        lua.load("oakterm.config.scrollback_archive = false")
            .exec()
            .unwrap();
        let cfg = extract_config(&lua).unwrap();
        assert!(!cfg.scrollback_archive);
    }

    #[test]
    fn set_valid_scrollback_archive_limit() {
        let lua = setup();
        lua.load(r#"oakterm.config.scrollback_archive_limit = "2GB""#)
            .exec()
            .unwrap();
        let cfg = extract_config(&lua).unwrap();
        assert_eq!(cfg.scrollback_archive_limit, 2 * 1024 * 1024 * 1024);
    }

    #[test]
    fn set_valid_daemon_persist() {
        let lua = setup();
        lua.load("oakterm.config.daemon_persist = true")
            .exec()
            .unwrap();
        let cfg = extract_config(&lua).unwrap();
        assert!(cfg.daemon_persist);
    }

    #[test]
    fn set_valid_check_for_updates() {
        let lua = setup();
        lua.load(r#"oakterm.config.check_for_updates = "check""#)
            .exec()
            .unwrap();
        let cfg = extract_config(&lua).unwrap();
        assert_eq!(cfg.check_for_updates, UpdateCheck::Check);
    }

    #[test]
    fn set_invalid_check_for_updates() {
        let lua = setup();
        let err = lua
            .load(r#"oakterm.config.check_for_updates = "auto""#)
            .exec();
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("off, check"), "got: {msg}");
    }

    #[test]
    fn os_returns_known_platform() {
        let lua = setup();
        let result: String = lua.load("return oakterm.os()").eval().unwrap();
        assert!(
            ["macos", "linux", "windows", "unknown"].contains(&result.as_str()),
            "unexpected os: {result}"
        );
    }

    #[test]
    fn appearance_returns_and_reflects_changes() {
        // Single test to avoid parallel mutation of the global atomic.
        let lua = setup();
        let result: String = lua.load("return oakterm.appearance()").eval().unwrap();
        assert!(
            ["dark", "light"].contains(&result.as_str()),
            "unexpected appearance: {result}"
        );
        crate::set_appearance(true);
        let result: String = lua.load("return oakterm.appearance()").eval().unwrap();
        assert_eq!(result, "light");
        crate::set_appearance(false);
        let result: String = lua.load("return oakterm.appearance()").eval().unwrap();
        assert_eq!(result, "dark");
    }

    #[test]
    fn hostname_returns_nonempty_string() {
        let lua = setup();
        let result: String = lua.load("return oakterm.hostname()").eval().unwrap();
        assert!(!result.is_empty(), "hostname should not be empty");
    }

    #[test]
    fn log_valid_levels() {
        let lua = setup();
        for level in ["debug", "info", "warn", "error"] {
            lua.load(format!(r#"oakterm.log("{level}", "test message")"#))
                .exec()
                .unwrap();
        }
    }

    #[test]
    fn log_invalid_level() {
        let lua = setup();
        let err = lua.load(r#"oakterm.log("trace", "msg")"#).exec();
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("invalid log level"), "got: {msg}");
    }
}
