//! Sandboxed Lua 5.4 configuration runtime.
//!
//! Creates a restricted Lua VM for evaluating `config.lua`. Dangerous
//! standard library functions are removed, memory and execution time
//! are bounded, and `print` is redirected to stderr.

mod proxy;
mod schema;

pub use proxy::{extract_config, register_config_table};
pub use schema::{ConfigValues, CursorStyle, Padding};

use mlua::{HookTriggers, Lua, LuaOptions, StdLib, Value, VmState};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
        eprintln!("[config] {line}");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn vm() -> (Lua, PrintLog) {
        create_lua_vm().expect("failed to create VM")
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
}
