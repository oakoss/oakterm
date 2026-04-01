//! Event handler registry for `oakterm.on(event, callback)`.
//!
//! Stores Lua callbacks as `RegistryKey`s, keyed by event name.
//! Handlers fire in registration order. A handler returning `false`
//! cancels subsequent handlers for that event.

use mlua::{Function, Lua, RegistryKey, Value};
use std::collections::HashMap;
use std::time::Duration;

/// Known event names per Spec-0005.
pub const KNOWN_EVENTS: &[&str] = &[
    "appearance.changed",
    "config.loaded",
    "config.reloaded",
    "window.created",
    "window.focused",
    "window.resized",
    "pane.created",
    "pane.focused",
    "pane.closed",
    "pane.title_changed",
    "pane.cwd_changed",
];

/// Per-handler execution timeout.
const HANDLER_TIMEOUT: Duration = Duration::from_millis(100);

/// Instruction hook interval for handler timeout checks.
const HANDLER_HOOK_INTERVAL: u32 = 10_000;

/// Result of invoking a single event handler.
#[derive(Debug)]
pub enum HandlerResult {
    /// Handler completed normally.
    Ok,
    /// Handler returned `false`, cancelling subsequent handlers.
    Cancelled,
    /// Handler raised an error.
    Error(String),
    /// Handler exceeded the per-handler timeout.
    Timeout,
}

/// Registry of event handlers stored as Lua `RegistryKey`s.
pub struct EventRegistry {
    handlers: HashMap<String, Vec<RegistryKey>>,
}

impl EventRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// Register a callback for an event. The callback must be a Lua `Function`.
    ///
    /// # Errors
    ///
    /// Returns an error if `event` is not a known event name or if the
    /// callback cannot be stored in the Lua registry.
    pub fn register(&mut self, lua: &Lua, event: &str, callback: Function) -> mlua::Result<()> {
        if !KNOWN_EVENTS.contains(&event) {
            let suggestion = suggest_event(event);
            let msg = if let Some(s) = suggestion {
                format!("unknown event '{event}' (did you mean '{s}'?)")
            } else {
                format!("unknown event '{event}'")
            };
            return Err(mlua::Error::RuntimeError(msg));
        }
        let key = lua.create_registry_value(callback)?;
        self.handlers
            .entry(event.to_string())
            .or_default()
            .push(key);
        Ok(())
    }

    /// Fire all handlers for an event with the given arguments.
    ///
    /// Handlers execute in registration order. If a handler returns `false`,
    /// subsequent handlers are skipped. Errors and timeouts are caught per
    /// handler; they do not prevent remaining handlers from running.
    #[must_use]
    pub fn fire(&self, lua: &Lua, event: &str, args: &[Value]) -> Vec<HandlerResult> {
        let Some(keys) = self.handlers.get(event) else {
            return Vec::new();
        };

        let mut results = Vec::with_capacity(keys.len());
        for key in keys {
            let result = invoke_handler(lua, key, args);
            let cancelled = matches!(result, HandlerResult::Cancelled);
            results.push(result);
            if cancelled {
                break;
            }
        }
        results
    }

    /// Number of handlers registered for an event.
    #[must_use]
    pub fn handler_count(&self, event: &str) -> usize {
        self.handlers.get(event).map_or(0, Vec::len)
    }

    /// Remove all registry keys and clear the handler map.
    ///
    /// Must be called before dropping the associated Lua VM to prevent
    /// registry leaks.
    pub fn cleanup(&mut self, lua: &Lua) {
        for (event, keys) in self.handlers.drain() {
            for key in keys {
                if let Err(e) = lua.remove_registry_value(key) {
                    eprintln!("warning: failed to clean up handler for '{event}': {e}");
                }
            }
        }
    }
}

impl Default for EventRegistry {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    static HANDLER_TIMED_OUT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn invoke_handler(lua: &Lua, key: &RegistryKey, args: &[Value]) -> HandlerResult {
    // Install per-handler timeout hook with thread-local flag.
    let start = std::time::Instant::now();
    let timeout = HANDLER_TIMEOUT;
    HANDLER_TIMED_OUT.set(false);
    if lua
        .set_hook(
            mlua::HookTriggers::new().every_nth_instruction(HANDLER_HOOK_INTERVAL),
            move |_lua, _debug| {
                if start.elapsed() > timeout {
                    HANDLER_TIMED_OUT.set(true);
                    Err(mlua::Error::RuntimeError(format!(
                        "event handler timed out ({}ms)",
                        timeout.as_millis()
                    )))
                } else {
                    Ok(mlua::VmState::Continue)
                }
            },
        )
        .is_err()
    {
        return HandlerResult::Error("failed to install handler timeout hook".to_string());
    }

    let func: Function = match lua.registry_value(key) {
        Ok(f) => f,
        Err(e) => {
            lua.remove_hook();
            return HandlerResult::Error(format!("failed to retrieve handler: {e}"));
        }
    };

    let result = func.call::<Value>(args.iter().cloned().collect::<mlua::MultiValue>());
    lua.remove_hook();
    let timed_out = HANDLER_TIMED_OUT.replace(false);

    match result {
        Ok(Value::Boolean(false)) => HandlerResult::Cancelled,
        Ok(_) => HandlerResult::Ok,
        Err(e) => {
            if timed_out {
                HandlerResult::Timeout
            } else {
                HandlerResult::Error(e.to_string())
            }
        }
    }
}

/// Suggest a known event name similar to the given input.
fn suggest_event(input: &str) -> Option<&'static str> {
    KNOWN_EVENTS
        .iter()
        .filter(|&&e| strsim::jaro(input, e) > 0.8)
        .max_by(|a, b| {
            strsim::jaro(input, a)
                .partial_cmp(&strsim::jaro(input, b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .copied()
}

/// Registry key for storing the `EventRegistry` in the Lua named registry.
pub(crate) const EVENT_REGISTRY_KEY: &str = "__oakterm_event_registry";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_lua_vm;
    use crate::proxy::register_config_table;

    fn setup() -> Lua {
        let (lua, _) = create_lua_vm().expect("VM creation failed");
        register_config_table(&lua).expect("registration failed");
        // Remove the eval timeout hook so it doesn't interfere with tests.
        lua.remove_hook();
        lua
    }

    #[test]
    fn register_and_count() {
        let lua = setup();
        let mut reg = EventRegistry::new();
        let cb: Function = lua
            .load("return function() end")
            .eval()
            .expect("create function");
        reg.register(&lua, "config.loaded", cb).unwrap();
        assert_eq!(reg.handler_count("config.loaded"), 1);
        assert_eq!(reg.handler_count("config.reloaded"), 0);
    }

    #[test]
    fn register_multiple_same_event() {
        let lua = setup();
        let mut reg = EventRegistry::new();
        for _ in 0..3 {
            let cb: Function = lua
                .load("return function() end")
                .eval()
                .expect("create function");
            reg.register(&lua, "config.loaded", cb).unwrap();
        }
        assert_eq!(reg.handler_count("config.loaded"), 3);
    }

    #[test]
    fn unknown_event_error() {
        let lua = setup();
        let mut reg = EventRegistry::new();
        let cb: Function = lua
            .load("return function() end")
            .eval()
            .expect("create function");
        let err = reg.register(&lua, "config.bogus", cb);
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("unknown event"), "got: {msg}");
    }

    #[test]
    fn unknown_event_suggests_close_match() {
        let lua = setup();
        let mut reg = EventRegistry::new();
        let cb: Function = lua
            .load("return function() end")
            .eval()
            .expect("create function");
        let err = reg.register(&lua, "config.reload", cb);
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("did you mean"), "got: {msg}");
    }

    #[test]
    fn fire_handler_receives_args() {
        let lua = setup();
        let mut reg = EventRegistry::new();
        // Handler stores its argument in a global.
        lua.load("_test_arg = nil").exec().expect("init global");
        let cb: Function = lua
            .load("return function(x) _test_arg = x end")
            .eval()
            .expect("create function");
        reg.register(&lua, "window.focused", cb).unwrap();
        let args = [Value::Integer(42)];
        let results = reg.fire(&lua, "window.focused", &args);
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], HandlerResult::Ok));
        let stored: i64 = lua.load("return _test_arg").eval().expect("read global");
        assert_eq!(stored, 42);
    }

    #[test]
    fn fire_handler_cancel_stops_chain() {
        let lua = setup();
        let mut reg = EventRegistry::new();
        lua.load("_call_count = 0").exec().unwrap();
        let cb1: Function = lua
            .load("return function() _call_count = _call_count + 1; return false end")
            .eval()
            .unwrap();
        let cb2: Function = lua
            .load("return function() _call_count = _call_count + 1 end")
            .eval()
            .unwrap();
        reg.register(&lua, "config.loaded", cb1).unwrap();
        reg.register(&lua, "config.loaded", cb2).unwrap();
        let results = reg.fire(&lua, "config.loaded", &[]);
        assert_eq!(results.len(), 1); // second handler not reached
        assert!(matches!(results[0], HandlerResult::Cancelled));
        let count: i64 = lua.load("return _call_count").eval().unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn fire_handler_error_continues_chain() {
        let lua = setup();
        let mut reg = EventRegistry::new();
        lua.load("_reached = false").exec().unwrap();
        let bad: Function = lua
            .load(r#"return function() error("boom") end"#)
            .eval()
            .unwrap();
        let good: Function = lua
            .load("return function() _reached = true end")
            .eval()
            .unwrap();
        reg.register(&lua, "config.loaded", bad).unwrap();
        reg.register(&lua, "config.loaded", good).unwrap();
        let results = reg.fire(&lua, "config.loaded", &[]);
        assert_eq!(results.len(), 2);
        assert!(matches!(results[0], HandlerResult::Error(_)));
        assert!(matches!(results[1], HandlerResult::Ok));
        let reached: bool = lua.load("return _reached").eval().unwrap();
        assert!(reached);
    }

    #[test]
    fn fire_handler_timeout() {
        let lua = setup();
        let mut reg = EventRegistry::new();
        let infinite: Function = lua
            .load("return function() while true do end end")
            .eval()
            .unwrap();
        reg.register(&lua, "config.loaded", infinite).unwrap();
        let results = reg.fire(&lua, "config.loaded", &[]);
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], HandlerResult::Timeout));
    }

    #[test]
    fn fire_no_handlers_is_noop() {
        let lua = setup();
        let reg = EventRegistry::new();
        let results = reg.fire(&lua, "config.loaded", &[]);
        assert!(results.is_empty());
    }

    #[test]
    fn cleanup_clears_all() {
        let lua = setup();
        let mut reg = EventRegistry::new();
        let cb: Function = lua.load("return function() end").eval().unwrap();
        reg.register(&lua, "config.loaded", cb).unwrap();
        assert_eq!(reg.handler_count("config.loaded"), 1);
        reg.cleanup(&lua);
        assert_eq!(reg.handler_count("config.loaded"), 0);
    }
}
