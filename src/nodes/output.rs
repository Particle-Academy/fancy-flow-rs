//! Terminal executors — capture a result, or say something on the feed.

use fancy_json::{Map, Value};

use crate::error::RunAborted;
use crate::nodes::support::expr;
use crate::runtime::{ExecutionContext, LogLevel, RunEvent};

/// `output` — returns its incoming value so it lands in `RunResult.outputs`.
///
/// # Errors
///
/// Never.
pub fn output(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    Ok(ctx.input_or_all())
}

/// `log` — emit the resolved message to the run feed at the configured level.
///
/// # Errors
///
/// Never.
pub fn log(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    let level_text = ctx.option_string("level", "info");
    let template = ctx
        .option("message")
        .cloned()
        .unwrap_or_else(|| Value::from(""));
    let message = expr::text(Some(&expr::evaluate_in(Some(&template), ctx.inputs())));

    let node_id = ctx.node().id.clone();
    ctx.emit(RunEvent::log(
        LogLevel::from_str_or_info(&level_text),
        &message,
        Some(&node_id),
    ));

    let mut out = Map::new();
    out.insert("logged", Value::from(message.as_str()));
    out.insert("level", Value::from(level_text.as_str()));
    Ok(Value::Object(out))
}
