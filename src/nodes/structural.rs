//! Nesting executors — a graph inside a node.
//!
//! `subgraph` carries its child graph inline in config; `subflow` NAMES one the
//! host resolves. They differ in where the graph comes from, not in how it gets
//! its inputs, so [`seed_entry_nodes`] stays one implementation.

use alloc::collections::BTreeSet;
use alloc::rc::Rc;
use alloc::string::ToString;

use fancy_json::{Map, Value};

use crate::capabilities::WorkflowResolver;
use crate::engine::FlowRunner;
use crate::error::RunAborted;
use crate::executors::ExecutorRegistry;
use crate::nodes::support::clients::ExecutorDeps;
use crate::registry::{builtin, NodeKindRegistry};
use crate::runtime::{ExecutionContext, RunOptions};
use crate::schema::FlowGraph;
use crate::workflow::import_workflow;

/// How deep a `subflow` chain may go before it is reported by name.
///
/// A workflow referencing itself, directly or through a chain, would otherwise
/// recurse until the stack gives up — which surfaces as an abort with no
/// explanation rather than "you built a loop".
pub const DEFAULT_MAX_DEPTH: usize = 8;

/// Hand a parent node's inputs to every entry point of a child graph.
#[must_use]
pub fn seed_entry_nodes(
    graph: &FlowGraph,
    inputs: &Map,
) -> alloc::collections::BTreeMap<alloc::string::String, Map> {
    let has_incoming: BTreeSet<&str> = graph.edges.iter().map(|e| e.target.as_str()).collect();
    graph
        .nodes
        .iter()
        .filter(|node| !has_incoming.contains(node.id.as_str()))
        .map(|node| (node.id.clone(), inputs.clone()))
        .collect()
}

/// Run a child graph through the very same runner, and bring its outputs home.
fn run_child(
    ctx: &ExecutionContext<'_>,
    graph: &FlowGraph,
    deps: &ExecutorDeps,
) -> Result<Value, RunAborted> {
    let mut kinds = NodeKindRegistry::new();
    builtin::register(&mut kinds, true);
    let executors = builtin::executors(deps);

    let options = RunOptions {
        initial_inputs: seed_entry_nodes(graph, ctx.inputs()),
        depth: ctx.depth() + 1,
        // A child's identity DESCENDS through the invoking node, so a node
        // inside the child is a different logical step from the same node
        // inside a different invocation — which is what stops two subflow
        // invocations sharing one idempotency key.
        run: ctx.run().map(|run| run.descend(&ctx.node().id, None)),
        ..RunOptions::new()
    };

    let result = FlowRunner::with_kinds(&kinds).run(graph, &executors, &options)?;

    if !result.ok {
        // The child's reason travels up VERBATIM, so a human gate inside a
        // subflow still decodes at the top.
        return Err(RunAborted::new(
            result.error.unwrap_or_else(|| "subflow failed".to_string()),
        ));
    }

    let mut out = Map::new();
    for (node_id, value) in result.outputs {
        out.insert(node_id, value);
    }
    Ok(Value::Object(out))
}

/// `subgraph` — runs the nested `WorkflowSchema` held in the node's config.
///
/// With no nested graph the input passes through, so a half-built node does not
/// take a run down.
pub struct Subgraph {
    deps: Rc<ExecutorDeps>,
}

impl Subgraph {
    /// Bind the executor to a set of host clients.
    #[must_use]
    pub fn new(deps: Rc<ExecutorDeps>) -> Self {
        Self { deps }
    }
}

impl crate::executors::Executor for Subgraph {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
        let Some(document) = ctx.option("graph").filter(|v| v.as_object().is_some()) else {
            return Ok(ctx.input_or_all());
        };

        if ctx.depth() >= DEFAULT_MAX_DEPTH {
            return Err(ctx.abort(&alloc::format!(
                "subgraph nesting exceeded {DEFAULT_MAX_DEPTH} levels at node {}",
                ctx.node().id
            )));
        }

        let mut kinds = NodeKindRegistry::new();
        builtin::register(&mut kinds, true);
        let imported = import_workflow(document, true, &kinds);

        run_child(ctx, &imported.graph, &self.deps)
    }
}

/// `subflow` — run another workflow and bring its result home.
///
/// Core, not marketplace: it introduces no third-party dependency. It runs a
/// child graph through the very same [`FlowRunner`], so the only thing it needs
/// from the host is WHERE workflows live — a [`WorkflowResolver`].
pub struct Subflow {
    deps: Rc<ExecutorDeps>,
    resolver: Option<Rc<dyn WorkflowResolver>>,
}

impl Subflow {
    /// Bind the executor to host clients and, optionally, a workflow resolver.
    #[must_use]
    pub fn new(deps: Rc<ExecutorDeps>, resolver: Option<Rc<dyn WorkflowResolver>>) -> Self {
        Self { deps, resolver }
    }
}

impl crate::executors::Executor for Subflow {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
        let reference = ctx.option_string("workflow", "");
        if reference.trim().is_empty() {
            return Err(ctx.abort("subflow has no workflow reference configured"));
        }

        if ctx.depth() >= DEFAULT_MAX_DEPTH {
            return Err(ctx.abort(&alloc::format!(
                "subflow nesting exceeded {DEFAULT_MAX_DEPTH} levels at node {}",
                ctx.node().id
            )));
        }

        // A missing resolver ABORTS rather than passing the input through. A
        // subflow that quietly did nothing would look like a workflow that ran,
        // and the run would report success having skipped the work.
        let Some(resolver) = &self.resolver else {
            return Err(ctx.abort(&alloc::format!(
                "subflow \"{reference}\" cannot run: no WorkflowResolver is configured"
            )));
        };

        let Some(graph) = resolver.resolve(&reference) else {
            return Err(ctx.abort(&alloc::format!("subflow \"{reference}\" was not found")));
        };

        run_child(ctx, &graph, &self.deps)
    }
}

/// The registry binding a host uses when it has its own executor set.
#[must_use]
pub fn with_structural(mut registry: ExecutorRegistry, deps: Rc<ExecutorDeps>) -> ExecutorRegistry {
    registry.bind("subgraph", Rc::new(Subgraph::new(Rc::clone(&deps))));
    registry.bind("subflow", Rc::new(Subflow::new(deps, None)));
    registry
}
