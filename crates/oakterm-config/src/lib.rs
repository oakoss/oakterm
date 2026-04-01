//! Sandboxed Lua 5.4 configuration runtime.
//!
//! Creates a restricted Lua VM for evaluating `config.lua`. Dangerous
//! standard library functions are removed, memory and execution time
//! are bounded, and `print` is redirected to stderr.

mod event;
mod init;
mod keybind;
mod proxy;
mod schema;
mod stubs;

pub use event::{EventRegistry, HandlerResult, KNOWN_EVENTS};
pub use init::{InitResult, ensure_stubs, init_config};
pub use keybind::{Action, KeyChord, KeyName, KeybindRegistry, NamedKeyId};
pub use mlua::{self, Lua};
pub use proxy::{extract_config, register_config_table};
pub use schema::{ConfigValues, CursorStyle, Padding, UpdateCheck, WindowDecorations};

use mlua::{HookTriggers, LuaOptions, StdLib, Value, VmState};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Global appearance: 0 = dark, 1 = light.
static APPEARANCE: AtomicU8 = AtomicU8::new(0);

/// Get the current system appearance.
#[must_use]
pub fn current_appearance() -> &'static str {
    if APPEARANCE.load(Ordering::Relaxed) == 1 {
        "light"
    } else {
        "dark"
    }
}

/// Set the system appearance. Called by the GUI on startup and on
/// `WindowEvent::ThemeChanged`.
pub fn set_appearance(light: bool) {
    APPEARANCE.store(u8::from(light), Ordering::Relaxed);
}

/// Result of loading and evaluating a config file.
pub struct ConfigResult {
    /// Parsed config values (defaults on error).
    pub config: ConfigValues,
    /// Registered event handlers (empty on error or when no config file exists).
    pub registry: EventRegistry,
    /// Registered keybinds (empty on error or when no config file exists).
    pub keybinds: KeybindRegistry,
    /// The Lua VM, kept alive for handler invocation. `None` when no config
    /// file exists or when the VM could not be created.
    pub lua: Option<Lua>,
    /// Error message if config evaluation failed.
    pub error: Option<String>,
}

impl std::fmt::Debug for ConfigResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigResult")
            .field("config", &self.config)
            .field("has_lua", &self.lua.is_some())
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

/// Default memory limit for config evaluation (16 MiB).
const MEMORY_LIMIT: usize = 16 * 1024 * 1024;

/// Default wall-clock timeout for config evaluation.
const EVAL_TIMEOUT: Duration = Duration::from_millis(500);

/// Instruction hook fires every N VM instructions to check the deadline.
const HOOK_INTERVAL: u32 = 10_000;

/// Create a sandboxed Lua 5.4 VM for config evaluation.
///
/// Loads only safe standard libraries, strips dangerous functions,
/// sets memory and instruction-time limits, and overrides `print`.
///
/// The returned `print_log` collects all `print()` output from Lua
/// for display or logging by the caller.
///
/// # Errors
///
/// Returns an error if the Lua VM cannot be created or configured.
pub fn create_lua_vm() -> mlua::Result<(Lua, PrintLog)> {
    // BASE library (pairs, ipairs, type, etc.) is always loaded by new_with.
    // We load only the safe subset — IO, OS, PACKAGE, and DEBUG are excluded.
    let lua = Lua::new_with(
        StdLib::COROUTINE | StdLib::TABLE | StdLib::STRING | StdLib::UTF8 | StdLib::MATH,
        LuaOptions::new().catch_rust_panics(false),
    )?;

    strip_dangerous_functions(&lua)?;

    let print_log = install_print_override(&lua)?;

    lua.set_memory_limit(MEMORY_LIMIT)?;

    install_timeout_hook(&lua, EVAL_TIMEOUT)?;

    Ok((lua, print_log))
}

/// Shared log of `print()` output from Lua.
pub type PrintLog = Arc<Mutex<Vec<String>>>;

fn strip_dangerous_functions(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    // Code loading — can execute arbitrary files or strings.
    globals.set("dofile", Value::Nil)?;
    globals.set("loadfile", Value::Nil)?;
    globals.set("load", Value::Nil)?;
    // GC control — no config use case; allows disabling the collector.
    globals.set("collectgarbage", Value::Nil)?;
    // Raw table access — bypasses metatable protection needed for proxy
    // table validation (TREK-42).
    globals.set("rawset", Value::Nil)?;
    globals.set("rawget", Value::Nil)?;
    globals.set("rawequal", Value::Nil)?;
    globals.set("rawlen", Value::Nil)?;
    Ok(())
}

fn install_print_override(lua: &Lua) -> mlua::Result<PrintLog> {
    let log: PrintLog = Arc::new(Mutex::new(Vec::new()));
    let log_ref = log.clone();

    let print_fn = lua.create_function(move |_, args: mlua::Variadic<Value>| {
        let parts: Vec<String> = args
            .iter()
            .map(|v| {
                v.to_string().map_err(|e| {
                    mlua::Error::RuntimeError(format!("print: cannot convert value: {e}"))
                })
            })
            .collect::<mlua::Result<Vec<_>>>()?;
        let line = parts.join("\t");
        log_ref
            .lock()
            .expect("print log mutex poisoned")
            .push(line.clone());
        tracing::info!(target: "config", "{line}");
        Ok(())
    })?;

    lua.globals().set("print", print_fn)?;
    Ok(log)
}

fn install_timeout_hook(lua: &Lua, timeout: Duration) -> mlua::Result<()> {
    let start = Instant::now();
    lua.set_hook(
        HookTriggers::new().every_nth_instruction(HOOK_INTERVAL),
        move |_lua, _debug| {
            if start.elapsed() > timeout {
                Err(mlua::Error::RuntimeError(format!(
                    "config evaluation timed out ({}ms)",
                    timeout.as_millis()
                )))
            } else {
                Ok(VmState::Continue)
            }
        },
    )
}

// --- Config loading ---

use std::path::{Path, PathBuf};

/// Resolve the config directory path.
///
/// - Linux/macOS: `$XDG_CONFIG_HOME/oakterm/` or `~/.config/oakterm/`
/// - Windows: `%APPDATA%\oakterm\`
#[must_use]
pub fn config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("oakterm");
    }
    if cfg!(windows) {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("oakterm");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config").join("oakterm");
    }
    // Last resort: relative path. Only reachable if HOME is unset
    // (containers, cron). Config will likely not be found, returning defaults.
    PathBuf::from(".config").join("oakterm")
}

/// Load config from `config.lua` in the config directory.
///
/// Also ensures type stubs are up to date (writes `types/oakterm.lua`
/// if content differs). Never fails — returns defaults on error with
/// the error message in `ConfigResult.error`.
#[must_use]
pub fn load_config() -> ConfigResult {
    let dir = config_dir();
    // Best-effort stub delivery on every launch.
    if let Err(e) = ensure_stubs(&dir) {
        tracing::warn!(error = %e, "failed to update type stubs");
    }
    load_config_from(&dir.join("config.lua"))
}

/// Load config from a specific file path. Testable without touching the real config dir.
#[must_use]
pub fn load_config_from(path: &Path) -> ConfigResult {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return default_result(None);
        }
        Err(e) => {
            return default_result(Some(format!("cannot read {}: {e}", path.display())));
        }
    };

    if source.trim().is_empty() {
        return default_result(None);
    }

    let (lua, _print_log) = match create_lua_vm() {
        Ok(v) => v,
        Err(e) => {
            return default_result(Some(format!("failed to create Lua VM: {e}")));
        }
    };

    if let Err(e) = register_config_table(&lua) {
        return ConfigResult {
            config: ConfigValues::default(),
            registry: EventRegistry::new(),
            keybinds: KeybindRegistry::with_defaults(),
            lua: Some(lua),
            error: Some(format!("failed to register config API: {e}")),
        };
    }

    // Register default keybinds via Lua before user config so user can override.
    if let Err(e) = register_default_keybinds(&lua) {
        tracing::warn!(error = %e, "failed to register default keybinds via Lua");
        // Defaults are also populated in with_defaults() as a safety net.
    }

    // Install sandboxed require() for multi-file configs.
    if let Some(parent) = path.parent() {
        if let Err(e) = install_require(&lua, parent) {
            tracing::warn!(error = %e, "failed to install require()");
        }
    }

    if let Err(e) = lua.load(&source).set_name(path.to_string_lossy()).exec() {
        // Remove eval hook before returning — handlers need their own hook.
        lua.remove_hook();
        return ConfigResult {
            config: ConfigValues::default(),
            registry: EventRegistry::new(),
            keybinds: KeybindRegistry::with_defaults(),
            lua: Some(lua),
            error: Some(format_config_error(path, &e)),
        };
    }

    // Remove eval hook — handlers will install per-handler hooks.
    lua.remove_hook();

    let registry = proxy::extract_event_registry(&lua);
    let keybinds = proxy::extract_keybind_registry(&lua);

    match extract_config(&lua) {
        Ok(config) => ConfigResult {
            config,
            registry,
            keybinds,
            lua: Some(lua),
            error: None,
        },
        Err(e) => ConfigResult {
            config: ConfigValues::default(),
            registry,
            keybinds,
            lua: Some(lua),
            error: Some(format_config_error(path, &e)),
        },
    }
}

/// Registry key for the module cache table used by sandboxed `require()`.
const MODULE_CACHE_KEY: &str = "__oakterm_module_cache";

/// Registry key for the sentinel table used to detect circular requires.
/// A unique Lua table (not a string) avoids collisions with module return values.
const CIRCULAR_SENTINEL_KEY: &str = "__oakterm_circular_sentinel";

/// Install a sandboxed `require()` function that resolves modules relative
/// to the config directory. Prevents path traversal and symlink escapes.
///
/// # Errors
///
/// Returns an error if the function cannot be registered.
fn install_require(lua: &Lua, config_dir: &Path) -> mlua::Result<()> {
    lua.set_named_registry_value(MODULE_CACHE_KEY, lua.create_table()?)?;
    // Unique table as circular-require sentinel (identity comparison, no
    // string collision possible).
    lua.set_named_registry_value(CIRCULAR_SENTINEL_KEY, lua.create_table()?)?;

    let config_dir = config_dir.to_path_buf();
    let require_fn = lua.create_function(move |lua, name: mlua::String| {
        let name_str = name.to_str()?;
        validate_module_name(&name_str)?;

        // Check cache.
        let cache: mlua::Table = lua.named_registry_value(MODULE_CACHE_KEY)?;
        let sentinel: mlua::Table = lua.named_registry_value(CIRCULAR_SENTINEL_KEY)?;
        if let Some(cached) = cache.get::<Option<Value>>(name_str.as_ref())? {
            if let Value::Table(ref t) = cached {
                if t == &sentinel {
                    // Circular require: return true as placeholder.
                    return Ok(Value::Boolean(true));
                }
            }
            return Ok(cached);
        }

        // Resolve path.
        let relative = name_str.replace('.', std::path::MAIN_SEPARATOR_STR);
        let lua_path = config_dir.join(format!("{relative}.lua"));
        let init_path = config_dir.join(&relative).join("init.lua");

        let resolved = if lua_path.exists() {
            lua_path
        } else if init_path.exists() {
            init_path
        } else {
            return Err(mlua::Error::RuntimeError(format!(
                "module '{name_str}' not found"
            )));
        };

        // Security: verify resolved path stays within config directory.
        let canonical_resolved = resolved.canonicalize().map_err(|e| {
            mlua::Error::RuntimeError(format!("cannot resolve module '{name_str}': {e}"))
        })?;
        let canonical_config = config_dir.canonicalize().map_err(|e| {
            mlua::Error::RuntimeError(format!("cannot resolve config directory: {e}"))
        })?;
        if !canonical_resolved.starts_with(&canonical_config) {
            return Err(mlua::Error::RuntimeError(format!(
                "module '{name_str}' escapes config directory"
            )));
        }

        // Read source.
        let source = std::fs::read_to_string(&resolved).map_err(|e| {
            mlua::Error::RuntimeError(format!("cannot read module '{name_str}': {e}"))
        })?;

        // Set sentinel in cache before evaluation (circular require protection).
        cache.set(name_str.as_ref(), sentinel)?;

        // Evaluate module. Syntax/runtime errors propagate directly — never
        // mask as "not found" (Neovim's biggest mistake).
        let result: Value = match lua
            .load(&source)
            .set_name(resolved.to_string_lossy())
            .eval()
        {
            Ok(v) => v,
            Err(e) => {
                // Remove sentinel so future requires re-report the error.
                let _ = cache.set(name_str.as_ref(), Value::Nil);
                return Err(e);
            }
        };

        // Cache and return. Modules returning nil are cached as true
        // (Lua convention: distinguish "loaded" from "not loaded").
        // Return the same value on first and subsequent loads.
        let to_cache = if result == Value::Nil {
            Value::Boolean(true)
        } else {
            result.clone()
        };
        cache.set(name_str.as_ref(), to_cache.clone())?;

        Ok(to_cache)
    })?;

    lua.globals().set("require", require_fn)?;
    Ok(())
}

/// Validate a module name as dot-separated Lua identifiers.
fn validate_module_name(name: &str) -> mlua::Result<()> {
    if name.is_empty() {
        return Err(mlua::Error::RuntimeError(
            "invalid module name: empty string".to_string(),
        ));
    }
    for segment in name.split('.') {
        if segment.is_empty() {
            return Err(mlua::Error::RuntimeError(format!(
                "invalid module name '{name}': empty segment"
            )));
        }
        let mut chars = segment.chars();
        let first = chars.next().unwrap();
        if !first.is_ascii_alphabetic() && first != '_' {
            return Err(mlua::Error::RuntimeError(format!(
                "invalid module name '{name}': segment '{segment}' must start with a letter or underscore"
            )));
        }
        if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(mlua::Error::RuntimeError(format!(
                "invalid module name '{name}': segment '{segment}' contains invalid characters"
            )));
        }
    }
    Ok(())
}

/// Register default keybinds via Lua before user config runs.
///
/// These are the built-in keybinds that were previously hardcoded in main.rs.
/// User config can override any of these by calling `oakterm.keybind` with
/// the same key chord (last registration wins).
fn register_default_keybinds(lua: &Lua) -> mlua::Result<()> {
    lua.load(
        r#"
        oakterm.keybind("shift+pageup", oakterm.action.scroll_up(0))
        oakterm.keybind("shift+pagedown", oakterm.action.scroll_down(0))
        oakterm.keybind("shift+home", oakterm.action.scroll_up(999999))
        oakterm.keybind("shift+end", oakterm.action.scroll_down(999999))
        oakterm.keybind("super+shift+up", oakterm.action.scroll_to_prompt(-1))
        oakterm.keybind("super+shift+down", oakterm.action.scroll_to_prompt(1))
        "#,
    )
    .exec()
}

/// Create a default `ConfigResult` without a VM but with default keybinds.
fn default_result(error: Option<String>) -> ConfigResult {
    ConfigResult {
        config: ConfigValues::default(),
        registry: EventRegistry::new(),
        keybinds: KeybindRegistry::with_defaults(),
        lua: None,
        error,
    }
}

fn format_config_error(path: &Path, err: &mlua::Error) -> String {
    match err {
        mlua::Error::SyntaxError { message, .. } => {
            format!("{}: {message}", path.display())
        }
        mlua::Error::RuntimeError(msg) => {
            format!("{}: {msg}", path.display())
        }
        mlua::Error::CallbackError { traceback, cause } => {
            format!("{}: {cause}\n{traceback}", path.display())
        }
        mlua::Error::MemoryError(_) => {
            format!(
                "{}: config evaluation exceeded memory limit",
                path.display()
            )
        }
        other => format!("{}: {other}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm() -> (Lua, PrintLog) {
        create_lua_vm().expect("failed to create VM")
    }

    #[test]
    fn appearance_set_and_get() {
        // Single test to avoid parallel mutation of the global atomic.
        set_appearance(false);
        assert_eq!(current_appearance(), "dark");
        set_appearance(true);
        assert_eq!(current_appearance(), "light");
        set_appearance(false);
        assert_eq!(current_appearance(), "dark");
    }

    #[test]
    fn sandbox_blocks_dofile() {
        let (lua, _) = vm();
        let err = lua.load("dofile('x')").exec();
        assert!(err.is_err(), "dofile should be nil");
    }

    #[test]
    fn sandbox_blocks_loadfile() {
        let (lua, _) = vm();
        let err = lua.load("loadfile('x')").exec();
        assert!(err.is_err(), "loadfile should be nil");
    }

    #[test]
    fn sandbox_blocks_load() {
        let (lua, _) = vm();
        let err = lua.load("load('return 1')").exec();
        assert!(err.is_err(), "load should be nil");
    }

    #[test]
    fn sandbox_blocks_rawset() {
        let (lua, _) = vm();
        let err = lua.load("rawset({}, 'k', 'v')").exec();
        assert!(err.is_err(), "rawset should be nil");
    }

    #[test]
    fn sandbox_blocks_rawget() {
        let (lua, _) = vm();
        let err = lua.load("rawget({}, 'k')").exec();
        assert!(err.is_err(), "rawget should be nil");
    }

    #[test]
    fn sandbox_blocks_rawequal() {
        let (lua, _) = vm();
        let err = lua.load("rawequal(1, 1)").exec();
        assert!(err.is_err(), "rawequal should be nil");
    }

    #[test]
    fn sandbox_blocks_rawlen() {
        let (lua, _) = vm();
        let err = lua.load("rawlen({})").exec();
        assert!(err.is_err(), "rawlen should be nil");
    }

    #[test]
    fn sandbox_blocks_collectgarbage() {
        let (lua, _) = vm();
        let err = lua.load("collectgarbage('count')").exec();
        assert!(err.is_err(), "collectgarbage should be nil");
    }

    #[test]
    fn sandbox_blocks_debug() {
        let (lua, _) = vm();
        let err = lua.load("debug.getinfo(1)").exec();
        assert!(err.is_err(), "debug should not be loaded");
    }

    #[test]
    fn sandbox_blocks_io() {
        let (lua, _) = vm();
        let err = lua.load("io.open('x')").exec();
        assert!(err.is_err(), "io should not be loaded");
    }

    #[test]
    fn sandbox_blocks_os() {
        let (lua, _) = vm();
        let err = lua.load("os.execute('ls')").exec();
        assert!(err.is_err(), "os should not be loaded");
    }

    #[test]
    fn sandbox_blocks_require() {
        let (lua, _) = vm();
        let err = lua.load("require('foo')").exec();
        assert!(err.is_err(), "require should not be available");
    }

    #[test]
    fn sandbox_allows_safe_functions() {
        let (lua, _) = vm();
        lua.load(
            r#"
            -- BASE safe functions
            assert(type(42) == "number")
            assert(tostring(42) == "42")
            assert(tonumber("42") == 42)
            assert(select(2, "a", "b", "c") == "b")
            local ok, err = pcall(function() error("test") end)
            assert(not ok)
            for _ in pairs({a = 1}) do end
            for _ in ipairs({1, 2}) do end

            -- TABLE
            local t = {3, 1, 2}
            table.sort(t)
            assert(t[1] == 1)

            -- STRING
            assert(string.format("%d", 42) == "42")

            -- MATH
            assert(math.floor(3.7) == 3)

            -- UTF8
            assert(utf8.len("hello") == 5)
            "#,
        )
        .exec()
        .expect("safe functions should work");
    }

    #[test]
    fn memory_limit_triggers() {
        let (lua, _) = vm();
        let result = lua
            .load(
                r#"
                local t = {}
                for i = 1, 100000000 do
                    t[i] = string.rep("x", 1024)
                end
                "#,
            )
            .exec();
        assert!(result.is_err(), "should hit memory limit");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("memory") || err.contains("not enough memory"),
            "error should mention memory: {err}"
        );
    }

    #[test]
    fn timeout_triggers() {
        let (lua, _) = vm();
        let result = lua.load("while true do end").exec();
        assert!(result.is_err(), "should timeout");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("timed out"),
            "error should mention timeout: {err}"
        );
    }

    #[test]
    fn print_override_captures_output() {
        let (lua, log) = vm();
        lua.load(r#"print("hello", "world")"#)
            .exec()
            .expect("print should work");
        let buf = log.lock().unwrap();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0], "hello\tworld");
    }

    #[test]
    fn print_zero_args() {
        let (lua, log) = vm();
        lua.load("print()").exec().expect("print() with no args");
        let buf = log.lock().unwrap();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0], "");
    }

    #[test]
    fn print_non_string_values() {
        let (lua, log) = vm();
        lua.load("print(nil, true, 42)")
            .exec()
            .expect("print with mixed types");
        let buf = log.lock().unwrap();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0], "nil\ttrue\t42");
    }

    #[test]
    fn print_multiple_calls_accumulate() {
        let (lua, log) = vm();
        lua.load(r#"print("a") print("b")"#)
            .exec()
            .expect("multiple prints");
        let buf = log.lock().unwrap();
        assert_eq!(buf.len(), 2);
        assert_eq!(buf[0], "a");
        assert_eq!(buf[1], "b");
    }

    #[test]
    fn vm_consistent_after_memory_error() {
        let (lua, _) = vm();
        // Hit memory limit.
        let _ = lua
            .load(
                r#"
                local t = {}
                for i = 1, 100000000 do
                    t[i] = string.rep("x", 1024)
                end
                "#,
            )
            .exec();

        // VM should still work for simple operations.
        lua.set_memory_limit(MEMORY_LIMIT)
            .expect("should be able to reset memory limit");
        let result: i64 = lua.load("return 1 + 1").eval().expect("VM should recover");
        assert_eq!(result, 2);
    }

    // --- load_config tests ---

    fn temp_config(content: &str) -> (PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.lua");
        std::fs::write(&path, content).unwrap();
        (path, dir)
    }

    #[test]
    fn load_config_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.lua");
        let r = load_config_from(&path);
        assert!(r.error.is_none());
        assert_eq!(r.config, ConfigValues::default());
    }

    #[test]
    fn load_config_valid_file() {
        let (path, _dir) = temp_config("oakterm.config.font_size = 20.0");
        let r = load_config_from(&path);
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        assert!((r.config.font_size - 20.0).abs() < f64::EPSILON);
    }

    #[test]
    fn load_config_syntax_error() {
        let (path, _dir) = temp_config("this is not valid lua }{");
        let r = load_config_from(&path);
        assert!(r.error.is_some(), "should have error");
        assert_eq!(r.config, ConfigValues::default());
    }

    #[test]
    fn load_config_runtime_error() {
        let (path, _dir) = temp_config(r#"error("intentional")"#);
        let r = load_config_from(&path);
        assert!(r.error.is_some());
        assert!(r.error.unwrap().contains("intentional"));
        assert_eq!(r.config, ConfigValues::default());
    }

    #[test]
    fn load_config_unknown_key() {
        let (path, _dir) = temp_config("oakterm.config.font_szie = 14");
        let r = load_config_from(&path);
        assert!(r.error.is_some());
        let msg = r.error.unwrap();
        assert!(msg.contains("did you mean"), "got: {msg}");
        assert_eq!(r.config, ConfigValues::default());
    }

    #[test]
    fn load_config_empty_file() {
        let (path, _dir) = temp_config("");
        let r = load_config_from(&path);
        assert!(r.error.is_none());
        assert_eq!(r.config, ConfigValues::default());
    }

    #[test]
    fn load_config_error_includes_path() {
        let (path, _dir) = temp_config("bad syntax {{");
        let r = load_config_from(&path);
        let msg = r.error.unwrap();
        assert!(
            msg.contains(&path.to_string_lossy().to_string()),
            "error should include path: {msg}"
        );
    }

    #[test]
    fn load_config_with_event_handler() {
        let (path, _dir) = temp_config(
            r#"
            oakterm.on("config.loaded", function() end)
            oakterm.on("config.loaded", function() end)
            oakterm.on("window.focused", function(id) end)
            "#,
        );
        let r = load_config_from(&path);
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        assert_eq!(r.registry.handler_count("config.loaded"), 2);
        assert_eq!(r.registry.handler_count("window.focused"), 1);
    }

    #[test]
    fn load_config_unknown_event_is_error() {
        let (path, _dir) = temp_config(r#"oakterm.on("bogus.event", function() end)"#);
        let r = load_config_from(&path);
        assert!(r.error.is_some());
        let msg = r.error.unwrap();
        assert!(msg.contains("unknown event"), "got: {msg}");
    }

    #[test]
    fn load_config_handlers_fire_after_extract() {
        let (path, _dir) = temp_config(
            r#"
            _test_fired = false
            oakterm.on("config.loaded", function() _test_fired = true end)
            oakterm.config.font_size = 18.0
            "#,
        );
        let r = load_config_from(&path);
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        assert!((r.config.font_size - 18.0).abs() < f64::EPSILON);
        assert_eq!(r.registry.handler_count("config.loaded"), 1);
        // Fire the handler and verify it actually executes.
        let lua = r.lua.as_ref().expect("should have VM");
        let results = r.registry.fire(lua, "config.loaded", &[]);
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], HandlerResult::Ok));
        let fired: bool = lua.load("return _test_fired").eval().unwrap();
        assert!(fired);
    }

    #[test]
    fn load_config_with_keybinds() {
        let (path, _dir) = temp_config(
            r#"
            oakterm.keybind("ctrl+k", oakterm.action.reload_config())
            oakterm.keybind("ctrl+c", oakterm.action.copy())
            oakterm.keybind("super+shift+t", oakterm.action.new_tab())
            "#,
        );
        let r = load_config_from(&path);
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        // 6 defaults + 3 user = 9 total.
        assert_eq!(r.keybinds.len(), 9);
        // Verify user keybind lookup works.
        let chord = KeyChord::parse("ctrl+k").unwrap();
        let action = r.keybinds.lookup(&chord);
        assert!(action.is_some());
        assert!(matches!(action.unwrap(), Action::ReloadConfig));
    }

    #[test]
    fn load_config_keybind_with_callback() {
        let (path, _dir) = temp_config(
            r#"
            oakterm.keybind("ctrl+b", function() print("hello") end)
            "#,
        );
        let r = load_config_from(&path);
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        // 6 defaults + 1 user = 7 total.
        assert_eq!(r.keybinds.len(), 7);
        let chord = KeyChord::parse("ctrl+b").unwrap();
        let action = r.keybinds.lookup(&chord);
        assert!(matches!(action, Some(Action::Callback(_))));
    }

    #[test]
    fn load_config_invalid_keybind_chord() {
        let (path, _dir) = temp_config(r#"oakterm.keybind("hyper+x", oakterm.action.copy())"#);
        let r = load_config_from(&path);
        assert!(r.error.is_some());
        let msg = r.error.unwrap();
        assert!(msg.contains("invalid key chord"), "got: {msg}");
    }

    #[test]
    fn load_config_keybind_override() {
        let (path, _dir) = temp_config(
            r#"
            oakterm.keybind("ctrl+c", oakterm.action.copy())
            oakterm.keybind("ctrl+c", oakterm.action.reload_config())
            "#,
        );
        let r = load_config_from(&path);
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        // Last registration wins.
        let chord = KeyChord::parse("ctrl+c").unwrap();
        let action = r.keybinds.lookup(&chord).unwrap();
        assert!(matches!(action, Action::ReloadConfig));
    }

    // --- require() tests ---

    #[test]
    fn require_resolves_lua_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("helpers.lua"), "return 42").unwrap();
        let config = dir.path().join("config.lua");
        std::fs::write(&config, r#"_result = require("helpers")"#).unwrap();
        let r = load_config_from(&config);
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        let lua = r.lua.as_ref().unwrap();
        let val: i64 = lua.load("return _result").eval().unwrap();
        assert_eq!(val, 42);
    }

    #[test]
    fn require_resolves_dotted_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("modules")).unwrap();
        std::fs::write(
            dir.path().join("modules").join("theme.lua"),
            r#"return "dark""#,
        )
        .unwrap();
        let config = dir.path().join("config.lua");
        std::fs::write(&config, r#"_result = require("modules.theme")"#).unwrap();
        let r = load_config_from(&config);
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        let lua = r.lua.as_ref().unwrap();
        let val: String = lua.load("return _result").eval().unwrap();
        assert_eq!(val, "dark");
    }

    #[test]
    fn require_resolves_init_lua() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("utils")).unwrap();
        std::fs::write(
            dir.path().join("utils").join("init.lua"),
            "return { version = 1 }",
        )
        .unwrap();
        let config = dir.path().join("config.lua");
        std::fs::write(&config, r#"_result = require("utils")"#).unwrap();
        let r = load_config_from(&config);
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        let lua = r.lua.as_ref().unwrap();
        let val: i64 = lua.load("return _result.version").eval().unwrap();
        assert_eq!(val, 1);
    }

    #[test]
    fn require_file_takes_priority_over_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("foo.lua"), r#"return "file""#).unwrap();
        std::fs::create_dir_all(dir.path().join("foo")).unwrap();
        std::fs::write(dir.path().join("foo").join("init.lua"), r#"return "dir""#).unwrap();
        let config = dir.path().join("config.lua");
        std::fs::write(&config, r#"_result = require("foo")"#).unwrap();
        let r = load_config_from(&config);
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        let lua = r.lua.as_ref().unwrap();
        let val: String = lua.load("return _result").eval().unwrap();
        assert_eq!(val, "file");
    }

    #[test]
    fn require_caches_result() {
        let dir = tempfile::tempdir().unwrap();
        // Module increments a counter each time it's loaded.
        std::fs::write(
            dir.path().join("counter.lua"),
            "
            _load_count = (_load_count or 0) + 1
            return _load_count
            ",
        )
        .unwrap();
        let config = dir.path().join("config.lua");
        std::fs::write(
            &config,
            r#"
            _first = require("counter")
            _second = require("counter")
            "#,
        )
        .unwrap();
        let r = load_config_from(&config);
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        let lua = r.lua.as_ref().unwrap();
        let first: i64 = lua.load("return _first").eval().unwrap();
        let second: i64 = lua.load("return _second").eval().unwrap();
        assert_eq!(first, 1);
        assert_eq!(second, 1); // Same cached value, not 2.
    }

    #[test]
    fn require_missing_module_error() {
        let (path, _dir) = temp_config(r#"require("nonexistent")"#);
        let r = load_config_from(&path);
        assert!(r.error.is_some());
        let msg = r.error.unwrap();
        assert!(msg.contains("not found"), "got: {msg}");
    }

    #[test]
    fn require_invalid_name_error() {
        let (path, _dir) = temp_config(r#"require("../escape")"#);
        let r = load_config_from(&path);
        assert!(r.error.is_some());
        let msg = r.error.unwrap();
        assert!(msg.contains("invalid module name"), "got: {msg}");
    }

    #[test]
    fn require_empty_name_error() {
        let (path, _dir) = temp_config(r#"require("")"#);
        let r = load_config_from(&path);
        assert!(r.error.is_some());
        let msg = r.error.unwrap();
        assert!(msg.contains("invalid module name"), "got: {msg}");
    }

    #[test]
    fn require_syntax_error_propagated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("broken.lua"), "this is not valid {{").unwrap();
        let config = dir.path().join("config.lua");
        std::fs::write(&config, r#"require("broken")"#).unwrap();
        let r = load_config_from(&config);
        assert!(r.error.is_some());
        let msg = r.error.unwrap();
        // Must show the actual syntax error, NOT "module not found".
        assert!(
            !msg.contains("not found"),
            "should not say 'not found': {msg}"
        );
    }

    #[test]
    fn require_circular_no_crash() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.lua"), r#"require("b"); return "a""#).unwrap();
        std::fs::write(dir.path().join("b.lua"), r#"require("a"); return "b""#).unwrap();
        let config = dir.path().join("config.lua");
        std::fs::write(&config, r#"_result = require("a")"#).unwrap();
        let r = load_config_from(&config);
        // Should not crash. The result may be incomplete but no panic.
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
    }

    #[test]
    fn require_passes_return_value() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("data.lua"),
            r#"return { name = "oak", version = 1 }"#,
        )
        .unwrap();
        let config = dir.path().join("config.lua");
        std::fs::write(&config, r#"_data = require("data")"#).unwrap();
        let r = load_config_from(&config);
        assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        let lua = r.lua.as_ref().unwrap();
        let name: String = lua.load("return _data.name").eval().unwrap();
        let version: i64 = lua.load("return _data.version").eval().unwrap();
        assert_eq!(name, "oak");
        assert_eq!(version, 1);
    }
}
