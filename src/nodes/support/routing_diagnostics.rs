//! Warn when a routing decision was made on a path that DID NOT RESOLVE.
//!
//! The twin of `FancyFlow\Nodes\Support\RoutingDiagnostics` (PHP).
//!
//! `branch` resolves its `condition` and asks [`expr::truthy`]. An unresolvable
//! path yields null, null is falsy, and the run takes the `false` port —
//! **silently, and for the wrong reason.** `switch_case` has the identical shape
//! one step over: a `value` that does not resolve falls through to `default`.
//! From the outside that is indistinguishable from a condition that was
//! legitimately false, except that half the graph never runs and the run
//! reports success.
//!
//! **Routing is deliberately unchanged.** Changing it would silently re-route
//! graphs that have been running for months; the warning supplies the part that
//! was missing, which is the REASON.
//!
//! Found by `flabs`: an agent built a correct triage graph whose urgency check
//! named a field that did not resolve, so every request — including one
//! reporting total payment failure — was routed as non-urgent.

use alloc::string::ToString;

use fancy_json::{Map, Value};

use super::expr;
use crate::runtime::{ExecutionContext, LogLevel, RunEvent};

/// Emit a `warn` when config `config_key` is a single WHOLE `{{ path }}` whose
/// path does not resolve against the node's inputs.
///
/// Only a whole expression. A condition mixing literal text with expressions is
/// being used as a string, and an unresolved fragment there is the
/// interpolation case rather than a routing one.
///
/// ABSENT is not NULL. A key that exists holding null RESOLVED, and is silent;
/// testing the resolved value instead of whether the path resolved would warn
/// on it, which is the collapse this exists to prevent.
pub fn warn_if_unresolved(ctx: &mut ExecutionContext<'_>, config_key: &str, took_port: &str) {
    let Some(condition) = ctx.option(config_key).and_then(Value::as_str) else {
        return;
    };

    let trimmed = condition.trim();
    if trimmed.len() < 4 || !trimmed.starts_with("{{") || !trimmed.ends_with("}}") {
        return;
    }

    let path = trimmed[2..trimmed.len() - 2].trim();

    // `{{ a }}{{ b }}` would otherwise read as one path spanning `}}{{`. That
    // is two references, not a missing field, and reporting it as a missing
    // field sends the reader somewhere useless.
    if path.is_empty() || path.contains("}}") {
        return;
    }

    let context = Value::Object(ctx.inputs().clone());
    if expr::resolve_path(path, &context).is_some() {
        return;
    }

    let path = path.to_string();
    let node_id = ctx.node().id.clone();

    let message = alloc::format!(
        "Node {node_id} took the \"{took_port}\" port because `{config_key}` resolved to \
         NOTHING \u{2014} the path {path} names no field on this node's inputs. That is not the \
         same as a false condition: the route was decided by an absent value rather than by \
         the data."
    );

    let mut detail = Map::new();
    detail.insert("node", Value::from(node_id.as_str()));
    detail.insert("configKey", Value::from(config_key));
    detail.insert("path", Value::from(path.as_str()));
    detail.insert("tookPort", Value::from(took_port));

    ctx.emit(RunEvent::log_with_detail(
        LogLevel::Warn,
        &message,
        Some(&node_id),
        Value::Object(detail),
    ));
}
