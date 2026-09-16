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
//! 3. [`Port::many`] -> `{"__ports": ["a","c"], "value": ...}` — exactly those
//!    ports emit, each carrying `value`. [`Port::many_with`] takes a map of port
//!    id -> its OWN payload. An empty list lights nothing, deliberately.
//! 4. Anything else — published on every declared output port.
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

    /// Publish `value` on a CHOSEN SUBSET of the ports (#18).
    ///
    /// An empty list lights nothing, deliberately — the same answer an
    /// explicitly empty `outputs` gives, and the honest one for a router that
    /// matched no rule. Before this a node could light one port or all of them,
    /// so a router that matched two of five had to drop work or wake lanes
    /// nobody asked for.
    #[must_use]
    pub fn many(port_ids: &[&str], value: Value) -> Value {
        let mut map = Map::new();
        map.insert(
            "__ports",
            Value::Array(port_ids.iter().map(|id| Value::from(*id)).collect()),
        );
        map.insert("value", value);
        Value::Object(map)
    }

    /// Publish a DIFFERENT payload on each of a chosen subset of ports (#18).
    #[must_use]
    pub fn many_with(per_port: Map) -> Value {
        let mut map = Map::new();
        map.insert("__ports", Value::Object(per_port));
        Value::Object(map)
    }
}
