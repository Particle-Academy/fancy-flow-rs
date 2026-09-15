//! Which ports a node CAN publish — the one answer, for everybody.
//!
//! The twin of `FancyFlow\Registry\PortResolution` (PHP) and of
//! `fancy_flow.registry.port_resolution` (Python).
//!
//! Three kinds decide their ports from their own CONFIG rather than from a fixed
//! declaration: `switch_case` publishes one port per entry in its `cases` map,
//! `llm_router` one per declared route, and `subflow` gains `stream` in the
//! streaming modes. Their [`NodeKind`] can only carry a representative default.
//!
//! In the PHP twin that answer was computed in two places that did not agree:
//! the authoring API derived it from config and offered an agent a third case,
//! while the engine read only the kind's static declaration and called that same
//! port impossible. **The authoring API invited an edge and the runtime then
//! reported it as a mistake.** This crate never consulted config-derived ports
//! until the undelivered-edge warning needed them, so it starts from the fixed
//! version.
//!
//! Deliberately NOT what the walk's port activation computes. That answers
//! "which ports did this RESULT light up"; this answers "which ports could this
//! node light up on SOME run". A `branch` that took `true` activated no `false`,
//! and `false` is still possible.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use fancy_json::{Map, Value};

use super::{dedup_push, kind_id, NodeKind};
use crate::schema::FlowNode;

/// Every port `node` could publish, given its kind and its own config.
///
/// Precedence, and it is deliberate:
///
/// 1. the NODE's own declared `outputs` — the document is more specific than
///    the kind;
/// 2. the kind's CONFIG-DERIVED ports, for the three kinds that have them;
/// 3. the kind's declared ports;
/// 4. `out`.
///
/// An unregistered kind (`kind` is `None`) is NOT ambiguous, though it looks as
/// though it should be: port activation falls back to exactly `out` for a kind
/// it cannot resolve, so that is what such a node publishes.
#[must_use]
pub fn possible_ports(node: &FlowNode, kind: Option<&NodeKind>) -> Vec<String> {
    if let Some(outputs) = &node.outputs {
        return outputs.iter().map(|port| port.id.clone()).collect();
    }

    let Some(kind) = kind else {
        return alloc::vec!["out".to_string()];
    };

    let declared: Vec<String> = kind
        .outputs
        .iter()
        .flatten()
        .map(|port| port.id.clone())
        .collect();

    let derived = match kind_id::bare(&kind.name) {
        "switch_case" => switch_case_ports(&node.config),
        "llm_router" => llm_router_ports(&node.config),
        "subflow" => subflow_ports(&node.config, &declared),
        _ => Vec::new(),
    };

    if !derived.is_empty() {
        return derived;
    }
    if declared.is_empty() {
        return alloc::vec!["out".to_string()];
    }
    declared
}

/// `cases` maps VALUE -> PORT ID, so the ports are its values.
///
/// `default` is always added: the executor falls back to it for any value with
/// no matching entry, so it can publish even when no case names it. Empty when
/// nothing is configured yet, so an unconfigured node falls back to the kind's
/// representative ports rather than to nothing.
///
/// An OBJECT only. PHP also iterates a list here, because its `isset` can index
/// one; this crate's `switch_case` routes only through an object, so a list's
/// elements could never be published and calling them possible would silence a
/// warning that is true.
fn switch_case_ports(config: &Map) -> Vec<String> {
    let Some(cases) = config.get("cases").and_then(Value::as_object) else {
        return Vec::new();
    };

    let mut ports: Vec<String> = Vec::new();
    for (_, port) in cases.iter() {
        if let Some(port) = port.as_str().filter(|port| !port.is_empty()) {
            dedup_push(&mut ports, port.to_string());
        }
    }
    if ports.is_empty() {
        return ports;
    }

    dedup_push(&mut ports, "default".to_string());
    ports
}

/// One port per declared route, plus `fallback` unless it is switched off.
///
/// A route is `{ "port": "..." }`, the shape every peer authors. A bare string
/// is accepted too, and that is a divergence rather than a courtesy: this
/// crate's offline `llm_router` executor reads `routes` as a list of port names,
/// so without it an ordinary graph written for that executor would have its
/// untaken routes reported as impossible.
fn llm_router_ports(config: &Map) -> Vec<String> {
    let Some(routes) = config.get("routes").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut ports: Vec<String> = Vec::new();
    for route in routes {
        let port = route
            .as_str()
            .or_else(|| route.get("port").and_then(Value::as_str));
        if let Some(port) = port.filter(|port| !port.is_empty()) {
            dedup_push(&mut ports, port.to_string());
        }
    }
    if ports.is_empty() {
        return ports;
    }

    // `fallback` defaults ON: it is where a run goes when the model returns a
    // port that was never offered. Only an explicit `false` switches it off.
    if config.get("fallback") != Some(&Value::Bool(false)) {
        dedup_push(&mut ports, "fallback".to_string());
    }
    ports
}

/// `subflow` gains `stream` in the `stream` and `both` modes.
fn subflow_ports(config: &Map, declared: &[String]) -> Vec<String> {
    let mode = config
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("output");
    if mode != "stream" && mode != "both" {
        return Vec::new();
    }

    let mut ports: Vec<String> = Vec::new();
    if declared.is_empty() {
        ports.push("out".to_string());
    }
    for port in declared {
        dedup_push(&mut ports, port.clone());
    }
    dedup_push(&mut ports, "stream".to_string());
    ports
}
