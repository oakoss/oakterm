//! Config schema: types, defaults, and per-key validation.

use mlua::{Lua, Value};

/// Cursor visual style for config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorStyle {
    #[default]
    Block,
    Underline,
    Bar,
}

impl CursorStyle {
    /// Parse from a Lua config string.
    #[must_use]
    pub fn from_config_str(s: &str) -> Option<Self> {
        match s {
            "block" => Some(Self::Block),
            "underline" => Some(Self::Underline),
            "bar" => Some(Self::Bar),
            _ => None,
        }
    }

    /// Config string representation.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Underline => "underline",
            Self::Bar => "bar",
        }
    }

    const ALL: &[&str] = &["block", "underline", "bar"];
}

/// Parsed configuration values extracted from Lua state.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigValues {
    /// Font family name. Empty means "use platform default".
    pub font_family: String,
    pub font_size: f64,
    pub cursor_style: CursorStyle,
    pub cursor_blink: bool,
    pub scrollback_limit: u64,
    pub save_alternate_scrollback: bool,
    pub padding: Padding,
}

impl Default for ConfigValues {
    fn default() -> Self {
        Self {
            font_family: String::new(),
            font_size: 14.0,
            cursor_style: CursorStyle::default(),
            cursor_blink: true,
            scrollback_limit: 50 * 1024 * 1024,
            save_alternate_scrollback: true,
            padding: Padding::default(),
        }
    }
}

/// Window padding in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Padding {
    pub top: u32,
    pub bottom: u32,
    pub left: u32,
    pub right: u32,
}

impl Default for Padding {
    fn default() -> Self {
        Self {
            top: 8,
            bottom: 8,
            left: 12,
            right: 12,
        }
    }
}

/// Schema entry for a single config key.
pub(crate) struct ConfigKeyDef {
    pub name: &'static str,
    pub validate: fn(&Lua, &Value) -> mlua::Result<()>,
}

/// All known config keys and their validators.
pub(crate) static SCHEMA: &[ConfigKeyDef] = &[
    ConfigKeyDef {
        name: "font_family",
        validate: validate_string,
    },
    ConfigKeyDef {
        name: "font_size",
        validate: validate_font_size,
    },
    ConfigKeyDef {
        name: "cursor_style",
        validate: validate_cursor_style,
    },
    ConfigKeyDef {
        name: "cursor_blink",
        validate: validate_bool,
    },
    ConfigKeyDef {
        name: "scrollback_limit",
        validate: validate_byte_size,
    },
    ConfigKeyDef {
        name: "save_alternate_scrollback",
        validate: validate_bool,
    },
    ConfigKeyDef {
        name: "padding",
        validate: validate_padding,
    },
];

/// Find the config key definition for the given name.
pub(crate) fn find_key(name: &str) -> Option<&'static ConfigKeyDef> {
    SCHEMA.iter().find(|k| k.name == name)
}

/// Suggest the closest known key name, or `None` if nothing is close.
pub(crate) fn suggest_key(name: &str) -> Option<&'static str> {
    let mut best: Option<(&str, f64)> = None;
    for def in SCHEMA {
        let score = strsim::jaro(name, def.name);
        if score > 0.7 && (best.is_none() || score > best.unwrap().1) {
            best = Some((def.name, score));
        }
    }
    best.map(|(name, _)| name)
}

/// Parse a byte-size string like "50MB" or "1GB" into a byte count.
/// Also accepts a raw integer.
pub(crate) fn parse_byte_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if let Ok(n) = s.parse::<u64>() {
        return Ok(n);
    }

    let (num_part, suffix) = split_size_suffix(s)?;
    let num: f64 = num_part
        .trim()
        .parse()
        .map_err(|_| format!("invalid number in size: '{num_part}'"))?;
    if num < 0.0 || !num.is_finite() {
        return Err(format!(
            "size must be a finite non-negative number, got '{s}'"
        ));
    }

    let multiplier: i32 = match suffix.to_uppercase().as_str() {
        "KB" | "K" => 1024,
        "MB" | "M" => 1024 * 1024,
        "GB" | "G" => 1024 * 1024 * 1024,
        _ => {
            return Err(format!(
                "unknown size suffix '{suffix}' (expected KB, MB, or GB)"
            ));
        }
    };

    let bytes = num * f64::from(multiplier);
    // 1 TiB is a sane upper bound; catches overflow without precision-loss cast.
    if bytes > 1_099_511_627_776.0 {
        return Err(format!("size '{s}' exceeds maximum (1 TiB)"));
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let result = bytes as u64;
    if result == 0 && num > 0.0 {
        return Err(format!("size '{s}' is too small (rounds to 0 bytes)"));
    }
    Ok(result)
}

fn split_size_suffix(s: &str) -> Result<(&str, &str), String> {
    let i = s
        .find(|c: char| c.is_alphabetic())
        .ok_or_else(|| format!("missing size suffix in '{s}' (expected e.g. '50MB')"))?;
    Ok((&s[..i], &s[i..]))
}

// --- Validators ---

fn validate_string(_lua: &Lua, value: &Value) -> mlua::Result<()> {
    match value {
        Value::String(_) => Ok(()),
        _ => Err(mlua::Error::RuntimeError(format!(
            "expected string, got {}",
            value_type_name(value)
        ))),
    }
}

fn validate_bool(_lua: &Lua, value: &Value) -> mlua::Result<()> {
    match value {
        Value::Boolean(_) => Ok(()),
        _ => Err(mlua::Error::RuntimeError(format!(
            "expected boolean, got {}",
            value_type_name(value)
        ))),
    }
}

fn validate_font_size(_lua: &Lua, value: &Value) -> mlua::Result<()> {
    let n = as_number(value)?;
    if n > 0.0 && n < 200.0 {
        Ok(())
    } else {
        Err(mlua::Error::RuntimeError(format!(
            "font_size must be greater than 0 and less than 200, got {n}"
        )))
    }
}

fn validate_cursor_style(_lua: &Lua, value: &Value) -> mlua::Result<()> {
    let s = as_str(value)?;
    if CursorStyle::from_config_str(&s).is_some() {
        Ok(())
    } else {
        Err(mlua::Error::RuntimeError(format!(
            "invalid cursor_style '{}' (expected: {})",
            s,
            CursorStyle::ALL.join(", ")
        )))
    }
}

fn validate_byte_size(_lua: &Lua, value: &Value) -> mlua::Result<()> {
    match value {
        Value::Integer(_) | Value::Number(_) => {
            let n = as_number(value)?;
            if n >= 0.0 && n.is_finite() {
                Ok(())
            } else {
                Err(mlua::Error::RuntimeError(
                    "scrollback_limit must be a finite non-negative number".to_string(),
                ))
            }
        }
        Value::String(s) => {
            let s = s
                .to_str()
                .map_err(|e| mlua::Error::RuntimeError(e.to_string()))?;
            parse_byte_size(&s).map_err(mlua::Error::RuntimeError)?;
            Ok(())
        }
        _ => Err(mlua::Error::RuntimeError(format!(
            "expected number or size string (e.g. \"50MB\"), got {}",
            value_type_name(value)
        ))),
    }
}

fn validate_padding(_lua: &Lua, value: &Value) -> mlua::Result<()> {
    let Value::Table(t) = value else {
        return Err(mlua::Error::RuntimeError(format!(
            "expected table with top/bottom/left/right, got {}",
            value_type_name(value)
        )));
    };
    let known_fields = ["top", "bottom", "left", "right"];
    for field in known_fields {
        let v: Value = t.get(field)?;
        match v {
            Value::Integer(n) if n >= 0 => {}
            Value::Nil => {
                return Err(mlua::Error::RuntimeError(format!(
                    "padding is missing required field '{field}'"
                )));
            }
            _ => {
                return Err(mlua::Error::RuntimeError(format!(
                    "padding.{field} must be a non-negative integer"
                )));
            }
        }
    }
    Ok(())
}

// --- Helpers ---

fn as_number(value: &Value) -> mlua::Result<f64> {
    match value {
        #[allow(clippy::cast_precision_loss)] // config values are small integers
        Value::Integer(n) => Ok(*n as f64),
        Value::Number(n) => Ok(*n),
        _ => Err(mlua::Error::RuntimeError(format!(
            "expected number, got {}",
            value_type_name(value)
        ))),
    }
}

fn as_str(value: &Value) -> mlua::Result<String> {
    match value {
        Value::String(s) => s
            .to_str()
            .map(|s| s.to_string())
            .map_err(|e| mlua::Error::RuntimeError(e.to_string())),
        _ => Err(mlua::Error::RuntimeError(format!(
            "expected string, got {}",
            value_type_name(value)
        ))),
    }
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Nil => "nil",
        Value::Boolean(_) => "boolean",
        Value::Integer(_) | Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Table(_) => "table",
        Value::Function(_) => "function",
        Value::Thread(_) => "thread",
        _ => "userdata",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_byte_size_mb() {
        assert_eq!(parse_byte_size("50MB").unwrap(), 50 * 1024 * 1024);
    }

    #[test]
    fn parse_byte_size_gb() {
        assert_eq!(parse_byte_size("1GB").unwrap(), 1024 * 1024 * 1024);
    }

    #[test]
    fn parse_byte_size_kb() {
        assert_eq!(parse_byte_size("512KB").unwrap(), 512 * 1024);
    }

    #[test]
    fn parse_byte_size_raw_number() {
        assert_eq!(parse_byte_size("1048576").unwrap(), 1_048_576);
    }

    #[test]
    fn parse_byte_size_with_space() {
        assert_eq!(parse_byte_size("50 MB").unwrap(), 50 * 1024 * 1024);
    }

    #[test]
    fn parse_byte_size_invalid_suffix() {
        assert!(parse_byte_size("50XB").is_err());
    }

    #[test]
    fn parse_byte_size_tiny_rounds_to_zero() {
        assert!(parse_byte_size("0.0001KB").is_err());
    }

    #[test]
    fn suggest_key_close_match() {
        assert_eq!(suggest_key("font_szie"), Some("font_size"));
    }

    #[test]
    fn suggest_key_no_match() {
        assert_eq!(suggest_key("zzzzzzzzz"), None);
    }

    #[test]
    fn find_key_exists() {
        assert!(find_key("font_size").is_some());
    }

    #[test]
    fn find_key_missing() {
        assert!(find_key("nonexistent").is_none());
    }

    #[test]
    fn default_config_values() {
        let d = ConfigValues::default();
        assert!((d.font_size - 14.0).abs() < f64::EPSILON);
        assert_eq!(d.cursor_style, CursorStyle::Block);
        assert!(d.cursor_blink);
        assert_eq!(d.scrollback_limit, 50 * 1024 * 1024);
        assert_eq!(d.padding.top, 8);
        assert_eq!(d.padding.left, 12);
    }

    #[test]
    fn cursor_style_round_trip() {
        for s in CursorStyle::ALL {
            let style = CursorStyle::from_config_str(s).unwrap();
            assert_eq!(style.as_str(), *s);
        }
    }

    #[test]
    fn schema_covers_all_keys() {
        // Safety net: if a config key is added to ConfigValues, add it to SCHEMA too.
        assert_eq!(
            SCHEMA.len(),
            7,
            "SCHEMA must match ConfigValues field count"
        );
    }
}
