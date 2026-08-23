//! Logic executors — the nodes that decide a graph's SHAPE.
//!
//! Worth precision, because everything downstream depends on which port lights
//! up. See `.ai/knowledge/flow-engine-spec.md` section 4.

use alloc::string::ToString;
use alloc::vec::Vec;

use fancy_json::{Map, Value};

use crate::error::RunAborted;
use crate::nodes::support::expr;
use crate::runtime::{ExecutionContext, LogLevel, Port, RunEvent};

/// `branch` — two ports, exactly one taken.
///
/// The condition resolves through [`expr`] against the node's inputs and
/// [`expr::truthy`] decides. The incoming value passes through unchanged down
/// whichever side is taken, and the other edge stays dead for the rest of the
/// run.
///
/// # Errors
///
/// Never.
pub fn branch(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    let resolved = expr::evaluate_in(ctx.option("condition"), ctx.inputs());
    let port = if expr::truthy(&resolved) {
        "true"
    } else {
        "false"
    };
    Ok(Port::branch(port, ctx.input_or_all()))
}

/// `switch_case` — N ports, one taken.
///
/// Routes on a key: `value` is resolved and looked up in the `cases` map
/// (value -> port id), falling back to `default`.
///
/// # Errors
///
/// Never.
pub fn switch_case(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    let resolved = expr::evaluate_in(ctx.option("value"), ctx.inputs());
    let key = expr::text(Some(&resolved));

    let port = ctx
        .option("cases")
        .and_then(Value::as_object)
        .and_then(|cases| cases.get(&key))
        .and_then(Value::as_str)
        .unwrap_or("default")
        .to_string();

    Ok(Port::only(&port, ctx.input_or_all()))
}

/// `for_each` — fan-out as DATA, not as jobs.
///
/// Publishes the resolved collection and its size. It does **not** spawn one
/// job per item, and that is deliberate: on a durable run a `for_each` over
/// 10,000 rows is one node, one claim, one checkpoint — not 10,000. Hosts that
/// want true per-item iteration override this executor.
///
/// # Errors
///
/// Never.
pub fn for_each(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    let source = expr::evaluate_in(ctx.option("source"), ctx.inputs());

    let items: Vec<Value> = match &source {
        // An object fans out over its VALUES, matching PHP's `array_values`
        // and Python's `.values()`. Its keys are not the collection.
        Value::Object(map) => map.values().cloned().collect(),
        Value::Array(items) => items.clone(),
        Value::Null => Vec::new(),
        other => alloc::vec![other.clone()],
    };

    let mut out = Map::new();
    out.insert("count", Value::from(items.len() as u64));
    out.insert("items", Value::Array(items));
    // `items` before `count` on the peers; key ORDER is not part of equality in
    // any conformance loader, so this is presentation only.
    Ok(Value::Object(out))
}

/// `merge` — several inputs, one value.
///
/// `merge` (default) combines inputs into one object: a mapping is merged in by
/// key, anything else is keyed by its PORT id. `concat` flattens everything
/// into one list.
///
/// Null inputs are skipped, and because dead edges never reach `collect_inputs`
/// at all, a merge downstream of a branch receives only the side that actually
/// ran.
///
/// # Errors
///
/// Never.
pub fn merge(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    let mode = ctx.option_string("mode", "merge");

    if mode == "concat" {
        let mut out: Vec<Value> = Vec::new();
        for (_, value) in ctx.inputs().iter() {
            match value {
                Value::Null => {}
                Value::Array(items) => out.extend(items.iter().cloned()),
                other => out.push(other.clone()),
            }
        }
        return Ok(Value::Array(out));
    }

    let mut merged = Map::new();
    for (port, value) in ctx.inputs().iter() {
        match value {
            Value::Null => {}
            Value::Object(map) => {
                for (key, inner) in map.iter() {
                    merged.insert(key, inner.clone());
                }
            }
            other => {
                merged.insert(port, other.clone());
            }
        }
    }
    Ok(Value::Object(merged))
}

/// `wait` — a pause point.
///
/// The framework-free default does NOT sleep: it records the requested wait and
/// passes the input through, so tests stay fast and deterministic. A durable
/// adapter overrides this to schedule the run's continuation rather than block
/// a worker for an hour.
///
/// # Errors
///
/// Never.
pub fn wait(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    let mode = ctx.option_string("mode", "duration");
    let duration = ctx.option("duration").cloned().unwrap_or(Value::Null);

    let node_id = ctx.node().id.clone();
    ctx.emit(RunEvent::log(
        LogLevel::Info,
        &alloc::format!("wait ({mode}) - not sleeping in framework-free mode"),
        Some(&node_id),
    ));

    let mut out = Map::new();
    out.insert("waited", Value::from(mode.as_str()));
    out.insert("duration", duration);
    out.insert("input", ctx.input_or_all());
    Ok(Value::Object(out))
}

/// `transform` — reshape in place.
///
/// With no expression the input passes through untouched. One `out` port,
/// always active.
///
/// # Errors
///
/// Never.
pub fn transform(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    let Some(expression) = ctx.option("expression") else {
        return Ok(ctx.input_or_all());
    };
    if expression.as_str() == Some("") {
        return Ok(ctx.input_or_all());
    }
    let expression = expression.clone();
    Ok(expr::evaluate_in(Some(&expression), ctx.inputs()))
}
