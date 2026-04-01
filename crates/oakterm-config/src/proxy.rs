//! Config proxy table: `oakterm.config` with per-key validation.
//! Also registers `oakterm.on(event, callback)` and
//! `oakterm.keybind(key, action)` with `oakterm.action.*` constructors.

use crate::event::{EVENT_REGISTRY_KEY, EventRegistry, KNOWN_EVENTS};
use crate::keybind::{Action, KeyChord, KeybindRegistry};
use crate::schema::{self, ConfigValues, CursorStyle, Padding, UpdateCheck, WindowDecorations};
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
        lua.create_function(|lua, (_, key, value): (Table, mlua::String, Value)| {
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
    let on_fn = lua.create_function(|lua, (event, callback): (mlua::String, Function)| {
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
    let keybind_fn = lua.create_function(|lua, (key, action): (mlua::String, Value)| {
        let key_str = key.to_str()?;
        if let Err(e) = KeyChord::parse(&key_str) {
            return Err(mlua::Error::RuntimeError(format!(
                "invalid key chord '{key_str}': {e}"
            )));
        }

        // Validate action is a table (from oakterm.action.*) or a function.
        match &action {
            Value::Table(t) => {
                if t.get::<Option<mlua::String>>("__action_type")?.is_none() {
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
            eprintln!("[config:warn] hostname contains non-UTF-8 bytes; using: {lossy}");
            Ok(lossy)
        }
    })?;

    // oakterm.log(level, message) — config-level logging.
    let log_fn = lua.create_function(|_, (level, message): (mlua::String, mlua::String)| {
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
        eprintln!("[config:{level_str}] {msg}");
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
        lua.create_function(|lua, data: mlua::String| {
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
            t.set("direction", opts.get::<mlua::String>("direction")?)?;
            t.set("size", opts.get::<f64>("size").unwrap_or(0.5))?;
            Ok(t)
        })?,
    )?;

    // focus_pane_direction(direction)
    action.set(
        "focus_pane_direction",
        lua.create_function(|lua, direction: mlua::String| {
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
        eprintln!("warning: failed to read event registry from Lua VM");
        return registry;
    };

    for pair in event_table.pairs::<mlua::String, Table>() {
        let (event_name, handlers) = match pair {
            Ok(p) => p,
            Err(e) => {
                eprintln!("warning: skipping malformed event registry entry: {e}");
                continue;
            }
        };
        let event_str = match event_name.to_str() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("warning: skipping event with invalid name: {e}");
                continue;
            }
        };
        for handler in handlers.sequence_values::<Function>() {
            let callback = match handler {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("warning: skipping unreadable handler for '{event_str}': {e}");
                    continue;
                }
            };
            if let Err(e) = registry.register(lua, &event_str, callback) {
                eprintln!("warning: failed to register handler for '{event_str}': {e}");
            }
        }
    }

    registry
}

/// Extract the `KeybindRegistry` from the Lua VM after config evaluation.
///
/// Converts keybind entries stored by `oakterm.keybind()` into
/// `(KeyChord, Action)` pairs. Callback functions become `RegistryKey`s.
pub(crate) fn extract_keybind_registry(lua: &Lua) -> KeybindRegistry {
    let mut registry = KeybindRegistry::new();
    let Ok(entries) = lua.named_registry_value::<Table>(KEYBIND_REGISTRY_KEY) else {
        eprintln!("warning: failed to read keybind registry from Lua VM");
        return registry;
    };

    for entry in entries.sequence_values::<Table>() {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                eprintln!("warning: skipping malformed keybind entry: {e}");
                continue;
            }
        };

        let key_str: String = match entry.get("key") {
            Ok(k) => k,
            Err(e) => {
                eprintln!("warning: skipping keybind with missing key: {e}");
                continue;
            }
        };

        let chord = match KeyChord::parse(&key_str) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("warning: skipping keybind with invalid chord '{key_str}': {e}");
                continue;
            }
        };

        let action_value: Value = match entry.get("action") {
            Ok(v) => v,
            Err(e) => {
                eprintln!("warning: skipping keybind '{key_str}' with missing action: {e}");
                continue;
            }
        };

        let action = match action_value {
            Value::Function(f) => match lua.create_registry_value(f) {
                Ok(key) => Action::Callback(key),
                Err(e) => {
                    eprintln!("warning: failed to store callback for '{key_str}': {e}");
                    continue;
                }
            },
            Value::Table(t) => match extract_action_from_table(&t) {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("warning: skipping keybind '{key_str}': {e}");
                    continue;
                }
            },
            _ => {
                eprintln!(
                    "warning: skipping keybind '{key_str}': action is not a table or function"
                );
                continue;
            }
        };

        registry.register(chord, action);
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
            let data: mlua::String = t
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
pub fn extract_config(lua: &Lua) -> mlua::Result<ConfigValues> {
    let backing: Table = lua.named_registry_value(BACKING_KEY)?;
    let defaults = ConfigValues::default();

    let font_family: String = backing
        .get::<Option<mlua::String>>("font_family")?
        .map(|s| s.to_str().map(|s| s.to_string()))
        .transpose()?
        .unwrap_or(defaults.font_family);

    let font_size: f64 = backing
        .get::<Option<f64>>("font_size")?
        .unwrap_or(defaults.font_size);

    let cursor_style: CursorStyle = match backing.get::<Option<mlua::String>>("cursor_style")? {
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
        .get::<Option<mlua::String>>("theme")?
        .map(|s| s.to_str().map(|s| s.to_string()))
        .transpose()?
        .unwrap_or(defaults.theme);

    let window_decorations = match backing.get::<Option<mlua::String>>("window_decorations")? {
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

    let check_for_updates = match backing.get::<Option<mlua::String>>("check_for_updates")? {
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
        check_for_updates,
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
