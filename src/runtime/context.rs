//! Everything an executor gets when it runs.

use alloc::string::String;
use alloc::vec::Vec;

use fancy_json::{Map, Value};

use super::events::RunEvent;
use super::identity::RunIdentity;
use super::pause::{Pause, PauseSignal};
use crate::error::RunAborted;
use crate::schema::FlowNode;

/// The executor's handle on the run.
///
/// # Events are buffered, not streamed
///
/// [`emit`](ExecutionContext::emit) appends to a buffer the engine drains when
/// the node returns, rather than calling the host's sink directly. That keeps
/// an executor from borrowing the engine's sink and is why an executor can be a
/// plain `&self` method.
///
/// The cost is that a host's `on_event` sees a node's log lines when the node
/// finishes rather than as it runs. Ordering is preserved. It matters only for
/// a live progress UI, which is not what this port's consumer is.
pub struct ExecutionContext<'a> {
    node: &'a FlowNode,
    inputs: Map,
    depth: usize,
    run: Option<&'a RunIdentity>,
    emitted: Vec<RunEvent>,
}

impl<'a> ExecutionContext<'a> {
    pub(crate) fn new(
        node: &'a FlowNode,
        inputs: Map,
        depth: usize,
        run: Option<&'a RunIdentity>,
    ) -> Self {
        Self {
            node,
            inputs,
            depth,
            run,
            emitted: Vec::new(),
        }
    }

    pub(crate) fn take_emitted(&mut self) -> Vec<RunEvent> {
        core::mem::take(&mut self.emitted)
    }

    /// The node being executed.
    #[must_use]
    pub fn node(&self) -> &FlowNode {
        self.node
    }

    /// Values arriving on each input port, keyed by port id.
    ///
    /// The default port is `in`, merged over any seeded initial inputs.
    #[must_use]
    pub fn inputs(&self) -> &Map {
        &self.inputs
    }

    /// How deep this run is nested.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Who is running, and which attempt of which step this is.
    ///
    /// `ctx.run().map(|r| r.step_key(&ctx.node().id, None))` is the idempotency
    /// key for a node that writes to somebody else's system — stable across
    /// retries of this step, distinct for every other execution of the same
    /// node.
    ///
    /// `None` when the host supplied no identity, **and that is a real answer**:
    /// a write with no key must decline or accept exactly one attempt, never
    /// invent a key.
    #[must_use]
    pub fn run(&self) -> Option<&RunIdentity> {
        self.run
    }

    /// Stop the run.
    ///
    /// Returns the error to hand back with `Err(..)`. Rust has no raise, so an
    /// executor writes `return Err(ctx.abort("no"))` — which has the useful
    /// side effect of making the abort visible in the signature.
    #[must_use]
    pub fn abort(&self, reason: &str) -> RunAborted {
        RunAborted::new(reason)
    }

    /// Halt the run to wait for a person.
    ///
    /// Reach for this rather than hand-encoding a reason, so the format stays
    /// ours to change:
    ///
    /// ```
    /// use fancy_flow::executors::executor;
    /// use fancy_flow::{
    ///     ExecutionContext, ExecutorRegistry, FlowGraph, FlowNode, FlowRunner, Pause,
    ///     RunOptions,
    /// };
    ///
    /// let gate = executor(|ctx: &mut ExecutionContext<'_>| {
    ///     match ctx.input("values") {
    ///         Some(values) => Ok(values.clone()),
    ///         // Absent, not falsy. An empty submission is a real answer.
    ///         None => Err(ctx.pause_for_human("input", None)),
    ///     }
    /// });
    ///
    /// let graph = FlowGraph {
    ///     nodes: vec![FlowNode::new("ask", "user_input")],
    ///     edges: vec![],
    /// };
    /// let mut executors = ExecutorRegistry::new();
    /// executors.bind("user_input", gate);
    ///
    /// let result = FlowRunner::new().run(&graph, &executors, &RunOptions::new())?;
    ///
    /// // The run did not FAIL — it is waiting. That distinction is the whole
    /// // contract, and it survives only because the reason travels verbatim.
    /// assert!(!result.ok);
    /// let signal = Pause::decode(result.error.as_deref()).expect("a pause, not a failure");
    /// assert_eq!(signal.node_id, "ask");
    /// assert!(signal.is_input());
    /// # Ok::<(), fancy_flow::RunAborted>(())
    /// ```
    ///
    /// Note the *absent* check rather than a truthiness test — an empty
    /// submission (`{}` / `[]`) is a real answer and must resume. A truthiness
    /// test pauses forever on an empty form.
    #[must_use]
    pub fn pause_for_human(&self, awaiting: &str, detail: Option<Value>) -> RunAborted {
        RunAborted::new(Pause::encode(&PauseSignal::new(
            &self.node.id,
            awaiting,
            detail,
        )))
    }

    /// Stream a status update or partial output to the run feed.
    pub fn emit(&mut self, event: RunEvent) {
        self.emitted.push(event);
    }

    /// Read one input port's value.
    ///
    /// Absent **and** null both yield `None` — the peer runtimes spell this
    /// `??`, and matching them is what keeps a graph's behaviour identical when
    /// a dead branch contributes nothing.
    #[must_use]
    pub fn input(&self, port: &str) -> Option<&Value> {
        // Key PRESENCE, not null-ness. A port BOUND to null is not an ABSENT
        // port, and callers pairing this with a fallback rely on the
        // difference: the fallback is for an entry node with no `in` edge, not
        // for a port that genuinely holds null.
        //
        // Collapsing them substituted a PLAUSIBLE value -- the inputs map looks
        // exactly like real data, so a downstream node read fields from the
        // wrong place and nothing looked wrong. `??` (and `is None`, and
        // `unwrap_or`) is safe only where null is not a legal value.
        self.inputs.get(port)
    }

    /// Read the default `in` port, falling back to the whole input map.
    ///
    /// The peers spell this `ctx.input("in", ctx.inputs)`: a node with one
    /// unnamed upstream reads `in`, and a node seeded with initial inputs reads
    /// whatever it was handed.
    #[must_use]
    pub fn input_or_all(&self) -> Value {
        self.input("in")
            .cloned()
            .unwrap_or_else(|| Value::Object(self.inputs.clone()))
    }

    /// The node's resolved config.
    #[must_use]
    pub fn config(&self) -> &Map {
        &self.node.config
    }

    /// Read one config key, with the same `??` semantics as [`input`].
    ///
    /// [`input`]: ExecutionContext::input
    #[must_use]
    pub fn option(&self, key: &str) -> Option<&Value> {
        match self.node.config.get(key) {
            Some(Value::Null) | None => None,
            Some(value) => Some(value),
        }
    }

    /// Read one config key as a string, with a fallback.
    #[must_use]
    pub fn option_str<'b>(&'b self, key: &str, default: &'b str) -> &'b str {
        self.option(key).and_then(Value::as_str).unwrap_or(default)
    }

    /// Read one config key as an owned string, with a fallback.
    #[must_use]
    pub fn option_string(&self, key: &str, default: &str) -> String {
        String::from(self.option_str(key, default))
    }
}
