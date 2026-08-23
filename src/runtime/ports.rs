//! Branching sugar for executor return values.
//!
//! The engine inspects a result and decides which output ports fire. Three
//! conventions, and they live in the engine and only there:
//!
//! 1. [`Port::only`] -> `{"__port": "true", "value": ...}` — only that port
//!    emits, carrying `value`.
//! 2. [`Port::branch`] -> `{"branch": "true", "value": ...}` — decision sugar.
//!    With `value` omitted the whole result object is carried, matching the
//!    peer runtimes' `r.value ?? r` rule.
//! 3. Anything else — published on every declared output port.
//!
//! These mirror fancy-flow's `__port` / `branch` conventions exactly, so an
//! identical graph branches identically on Node, PHP, Python and Rust.

use fancy_json::{Map, Value};

/// Constructors for the two port conventions.
pub struct Port;

impl Port {
    /// Publish `value` on exactly one named port.
    #[must_use]
    pub fn only(port_id: &str, value: Value) -> Value {
        let mut map = Map::new();
        map.insert("__port", Value::from(port_id));
        map.insert("value", value);
        Value::Object(map)
    }

    /// Take one branch, carrying `value`.
    #[must_use]
    pub fn branch(port_id: &str, value: Value) -> Value {
        let mut map = Map::new();
        map.insert("branch", Value::from(port_id));
        map.insert("value", value);
        Value::Object(map)
    }
}
