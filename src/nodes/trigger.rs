//! Trigger executors — the entry points a run starts from.
//!
//! A trigger does not decide WHEN it fires; the host does (a click, an inbound
//! request, a scheduler tick). What it owns is the shape of the payload that
//! reaches the rest of the graph.

use fancy_json::{Map, Value};

use crate::error::RunAborted;
use crate::runtime::ExecutionContext;

/// `manual_trigger` — passes the seeded payload straight through on `out`.
///
/// # Errors
///
/// Never.
pub fn manual_trigger(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    Ok(Value::Object(ctx.inputs().clone()))
}

/// `webhook_trigger` — emits the request payload.
///
/// Seeded under `payload` when the host separates the body from its envelope,
/// otherwise the whole seed.
///
/// # Errors
///
/// Never.
pub fn webhook_trigger(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    Ok(match ctx.input("payload") {
        Some(payload) => payload.clone(),
        None => Value::Object(ctx.inputs().clone()),
    })
}

/// `schedule_trigger` — the schedule context merged with any seeded payload.
///
/// The seed wins on a key collision, which is what lets a host inject the tick
/// it actually fired for.
///
/// # Errors
///
/// Never.
pub fn schedule_trigger(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    let mut out = Map::new();
    out.insert("cron", ctx.option("cron").cloned().unwrap_or(Value::Null));
    out.insert("timezone", Value::from(ctx.option_str("timezone", "UTC")));
    for (key, value) in ctx.inputs().iter() {
        out.insert(key, value.clone());
    }
    Ok(Value::Object(out))
}
