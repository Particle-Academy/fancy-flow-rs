//! Run ONE node of a graph -- through the real engine, not around it.
//!
//! # The problem this solves
//!
//! A per-node driver has to hand a node exactly the inputs it would have
//! received mid-run: the right values, on the right target handles, from the
//! right *active* edges. Those rules are the engine's (`collect_inputs`,
//! `activated_ports`, the merge-after-decision contract, the `out` fallbacks),
//! and they are the reason the four runtimes agree. Re-implementing them here
//! would be a second engine wearing a driver's clothes, and the two would
//! drift.
//!
//! # What it does instead
//!
//! It replays the graph through [`FlowRunner::run`] untouched -- the same
//! [`Walk`](crate::engine::Walk) every driver drives:
//!
//! - every node already completed is fed back as
//!   [`RunOptions::resume_outputs`], so the engine republishes it on the same
//!   ports and routes exactly as it did the first time;
//! - every node EXCEPT the target is bound, by node id on a FORKED registry, to
//!   a FENCE that runs nothing and publishes only [`FENCE_PORT`], which no edge
//!   reads;
//! - so the engine walks its own topological order, skips its own dead
//!   branches, collects the target's inputs its own way, and runs the target.
//!
//! # Why the fence does not stop the walk
//!
//! It used to abort the run, in the peers. The target's own inputs never
//! depend on a fenced node: the frontier dispatches a node only once every
//! source is settled. A COMPLETED source is resumed, not fenced. A SKIPPED or
//! FAILED source lit no ports in the frontier and lights none in the replay:
//! the engine skips it again or fences it, and a fence publishes only a port no
//! edge reads.
//!
//! But an UNRELATED node can precede the target in topological order, and two
//! siblings dispatched together are exactly that. When `b`'s job started while
//! `a` was still running, an aborting replay stopped at `a` and never reached
//! `b`; the coordinator read "the replay ended without running me" as "the
//! engine decided I am unreachable", so `b` was recorded skipped, never ran,
//! and the run completed as a success. That needs no second worker: the
//! frontier reports ready nodes in the order the graph declares them, while the
//! engine walks siblings in the order their edges are listed.
//!
//! Walking past fences makes that inference honest again: when the replay
//! finishes without an output for the target, it is because the engine found
//! every inbound edge dead.
//!
//! # The cost, stated plainly
//!
//! Replaying the completed prefix is O(nodes) per node, so a run is O(nodes^2)
//! in bookkeeping. The republish executes nothing -- it re-publishes stored
//! values -- so for the graph sizes workflows actually have this is noise next
//! to a single queue round trip. It buys exact fidelity to the engine, which is
//! not negotiable, and one implementation of the routing rules instead of two.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use fancy_json::Value;

use crate::engine::FlowRunner;
use crate::error::RunAborted;
use crate::executors::{ExecutorRegistry, SharedExecutor};
use crate::registry::NodeKindRegistry;
use crate::runtime::{ExecutionContext, Port, RunEvent, RunOptions, RunResult};
use crate::schema::FlowGraph;

/// The abort reason a boundary used to report.
///
/// Nothing aborts with it -- see "Why the fence does not stop the walk" -- and
/// this crate never shipped a fence that did. [`is_boundary`] still recognises
/// it, as every peer does, so a run recorded by one of them reads the same here.
pub const BOUNDARY: &str = "fancy-flow:node-boundary";

/// The port a fenced node publishes on. No edge reads it, so everything
/// downstream of a fenced node is dark in the replay -- which never matters to
/// the target, whose sources are all settled.
pub const FENCE_PORT: &str = "fancy-flow:fenced";

/// What a replay produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayResult {
    /// The engine's own result, with every FENCED node's placeholder output
    /// removed: a fence executes nothing, so what the engine recorded for it is
    /// not an output.
    pub result: RunResult,
    /// Node id -> the ports its output activated, from the engine's own
    /// `node-output` events, in publication order. Fenced nodes are absent.
    pub ports: BTreeMap<String, Vec<String>>,
}

impl ReplayResult {
    /// One node's output, if it produced one.
    #[must_use]
    pub fn output_of(&self, node_id: &str) -> Option<&Value> {
        self.result.outputs.get(node_id)
    }

    /// The ports one node's output activated.
    #[must_use]
    pub fn ports_of(&self, node_id: &str) -> &[String] {
        self.ports.get(node_id).map_or(&[], Vec::as_slice)
    }
}

/// Replay `graph` up to and through `node_id`.
///
/// Pass `None` to PROBE: every node is fenced, so nothing executes and the
/// engine reports only what it can determine structurally -- a cycle, and the
/// ports each resumed output republishes on.
///
/// `options` carries what the run carries: `resume_outputs` (the completed
/// prefix), `initial_inputs`, `depth` and the `run` identity of THIS attempt.
///
/// `kinds` is the catalogue the engine resolves ports against, exactly as
/// [`FlowRunner::with_kinds`] takes it; `None` is [`FlowRunner::new`]. **A driver
/// must pass the catalogue its run was configured with.** Replaying against a
/// different one publishes a node with no declared outputs on THAT catalogue's
/// idea of its kind -- `for_each` on `out` instead of `item` / `done` -- so the
/// durable run routes differently from a single-process run of the same graph.
///
/// # Errors
///
/// [`RunAborted`] only when a host signal in `options` cancelled the replay,
/// exactly as [`FlowRunner::run`].
pub fn replay_up_to(
    graph: &FlowGraph,
    node_id: Option<&str>,
    executors: &ExecutorRegistry,
    kinds: Option<&NodeKindRegistry>,
    options: &RunOptions<'_>,
) -> Result<ReplayResult, RunAborted> {
    let fence: SharedExecutor = Rc::new(fence);
    let mut fork = executors.fork();
    let mut fenced: Vec<&str> = Vec::new();
    for node in &graph.nodes {
        if Some(node.id.as_str()) != node_id {
            // `bind_node` outranks kind bindings AND the `*` fallback, so this
            // fences off the whole graph regardless of what a host bound.
            fork.bind_node(&node.id, Rc::clone(&fence));
            fenced.push(node.id.as_str());
        }
    }

    let runner = kinds.map_or_else(FlowRunner::new, FlowRunner::with_kinds);
    let mut result = runner.run(graph, &fork, options)?;

    let mut ports: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for event in &result.events {
        if event.kind != RunEvent::NODE_OUTPUT {
            continue;
        }
        if let (Some(id), Some(port)) = (event.node_id.as_deref(), event.port_id.as_deref()) {
            ports
                .entry(id.to_string())
                .or_default()
                .push(port.to_string());
        }
    }

    // A fence that RAN left a placeholder output. A resumed node never reaches
    // its fence -- the engine republishes it first -- so its output stays.
    for id in fenced {
        if !options.resume_outputs.contains_key(id) {
            result.outputs.remove(id);
            ports.remove(id);
        }
    }

    Ok(ReplayResult { result, ports })
}

/// True when a run ended because the replay reached a node it does not own.
#[must_use]
pub fn is_boundary(error: Option<&str>) -> bool {
    error == Some(BOUNDARY)
}

/// Runs nothing and publishes only a port no edge reads.
#[expect(
    clippy::unnecessary_wraps,
    reason = "an executor's signature; a fence never aborts, and that is the point"
)]
fn fence(_ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    Ok(Port::only(FENCE_PORT, Value::Null))
}
