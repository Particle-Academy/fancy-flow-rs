//! Human-gate executors.
//!
//! `user_input` and `human_approval` are the two places a workflow stops for a
//! person. The framework-free defaults here are **pass-throughs**, exactly as
//! in the PHP and Python twins: they let a graph be exercised end to end
//! offline. A durable host replaces them with pausing variants — and *that* is
//! where the fail-closed rule lives, because only a durable runner has
//! somewhere to park.
//!
//! The rule, restated so nobody re-derives it wrongly: a gate pauses because it
//! **is** a human node, not because its input port happens to be empty.
//! Pre-filled inputs — initial inputs, an upstream edge, a submission recorded
//! before the node ran — never satisfy the gate. Only a recorded answer for
//! *that node* does.

use alloc::rc::Rc;

use fancy_json::{Map, Value};

use crate::error::RunAborted;
use crate::nodes::support::clients::Notifier;
use crate::nodes::support::expr;
use crate::runtime::{ExecutionContext, LogLevel, Port, RunEvent};

/// `user_input` — offline default: treat the incoming values as the submission.
///
/// # Errors
///
/// Never. The pausing variant lives in the durable layer.
pub fn user_input(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    Ok(match ctx.input("values") {
        Some(values) => values.clone(),
        None => ctx.input_or_all(),
    })
}

/// `human_approval` — offline default: read an `approved` flag, defaulting to yes.
///
/// # Errors
///
/// Never. The pausing variant lives in the durable layer.
pub fn human_approval(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    let approved = ctx.input("approved").is_none_or(expr::truthy);
    let port = if approved { "approved" } else { "denied" };
    Ok(Port::branch(port, ctx.input_or_all()))
}

/// `notify` — send a message through a host [`Notifier`].
pub struct Notify {
    notifier: Rc<dyn Notifier>,
}

impl Notify {
    /// Bind the executor to a notifier.
    #[must_use]
    pub fn new(notifier: Rc<dyn Notifier>) -> Self {
        Self { notifier }
    }
}

impl crate::executors::Executor for Notify {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
        let channel = ctx.option_string("channel", "slack");
        let to = ctx.option_string("to", "");
        let template = ctx
            .option("message")
            .cloned()
            .unwrap_or_else(|| Value::from(""));
        let message = expr::text(Some(&expr::evaluate_in(Some(&template), ctx.inputs())));

        self.notifier.notify(&channel, &to, &message);

        let node_id = ctx.node().id.clone();
        ctx.emit(RunEvent::log(
            LogLevel::Info,
            &alloc::format!("notify -> {channel}:{to}"),
            Some(&node_id),
        ));

        let mut out = Map::new();
        out.insert("sent", Value::Bool(true));
        out.insert("channel", Value::from(channel.as_str()));
        out.insert("to", Value::from(to.as_str()));
        out.insert("message", Value::from(message.as_str()));
        Ok(Value::Object(out))
    }
}
