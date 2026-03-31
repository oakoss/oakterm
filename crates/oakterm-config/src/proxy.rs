//! Config proxy table: `oakterm.config` with per-key validation.

use crate::schema::{self, ConfigValues, CursorStyle, Padding};
use mlua::{Lua, Table, Value};

/// Registry key for the hidden backing table that stores validated config values.
const BACKING_KEY: &str = "__oakterm_config_backing";

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

            (def.validate)(lua, &value)?;

            let backing: Table = lua.named_registry_value(BACKING_KEY)?;
            backing.set(key_str, value)?;
            Ok(())
        })?,
    )?;

    // __metatable: block getmetatable/setmetatable introspection.
    meta.set("__metatable", "oakterm.config")?;

    proxy.set_metatable(Some(meta))?;

    // Register as oakterm.config.
    let oakterm = lua.create_table()?;
    oakterm.set("config", proxy)?;
    lua.globals().set("oakterm", oakterm)?;

    Ok(())
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
            CursorStyle::from_config_str(&s)
                .ok_or_else(|| mlua::Error::RuntimeError(format!("invalid cursor_style '{s}'")))?
        }
        None => defaults.cursor_style,
    };

    let cursor_blink: bool = backing
        .get::<Option<bool>>("cursor_blink")?
        .unwrap_or(defaults.cursor_blink);

    let scrollback_limit = extract_scrollback_limit(&backing, defaults.scrollback_limit)?;

    let save_alternate_scrollback: bool = backing
        .get::<Option<bool>>("save_alternate_scrollback")?
        .unwrap_or(defaults.save_alternate_scrollback);

    let padding = extract_padding(&backing, defaults.padding)?;

    Ok(ConfigValues {
        font_family,
        font_size,
        cursor_style,
        cursor_blink,
        scrollback_limit,
        save_alternate_scrollback,
        padding,
    })
}

fn extract_scrollback_limit(backing: &Table, default: u64) -> mlua::Result<u64> {
    let value: Value = backing.get("scrollback_limit")?;
    match value {
        Value::Nil => Ok(default),
        Value::Integer(n) => u64::try_from(n).map_err(|_| {
            mlua::Error::RuntimeError(format!("scrollback_limit must be non-negative, got {n}"))
        }),
        Value::Number(n) if n.is_finite() && n >= 0.0 =>
        {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Ok(n as u64)
        }
        Value::Number(n) => Err(mlua::Error::RuntimeError(format!(
            "scrollback_limit must be a finite non-negative number, got {n}"
        ))),
        Value::String(s) => {
            let s = s.to_str()?;
            schema::parse_byte_size(&s).map_err(mlua::Error::RuntimeError)
        }
        _ => Err(mlua::Error::RuntimeError(
            "unexpected type for scrollback_limit".to_string(),
        )),
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
}
