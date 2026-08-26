//! The one graph walk.
//!
//! TypeScript executors may be `async`; PHP's are synchronous; Python drives
//! both by making the walk a generator. Rust has no stable generators, and
//! writing the loop twice would put two copies of the routing rules in the file
//! that exists to have exactly one.
//!
//! So the walk is an explicit **state machine**: [`Walk::next_step`] yields the
//! node to execute, [`Walk::resume`] is handed the outcome, and
//! [`Walk::finish`] produces the result. `FlowRunner::run` drives it
//! synchronously; a future async driver, and the per-node durable driver, drive
//! the **same** `Walk` and never re-derive topology, branching, skipping or
//! port activation.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use fancy_json::{Map, Value};

use crate::error::RunAborted;
use crate::executors::{ExecutorRegistry, SharedExecutor};
use crate::registry::{kind_id, NodeKindRegistry};
use crate::runtime::{ExecutionContext, NodeStatus, RunEvent, RunOptions, RunResult};
use crate::schema::{FlowEdge, FlowGraph, FlowNode};

/// One unit of work the walk hands back to whichever driver is running it.
pub struct Step {
    /// Which node in the walk's order this is.
    index: usize,
    /// The executor to call.
    pub executor: SharedExecutor,
}

/// What a driver hands back.
pub enum Outcome {
    /// The executor returned a value.
    Value(Value),
    /// The executor aborted. The reason travels **verbatim**.
    Aborted(RunAborted),
}

enum State {
    /// Ready to look at `order[cursor]`.
    Walking,
    /// Waiting for the driver to report on the node at `order[cursor]`.
    AwaitingOutcome,
    /// Nothing left to do.
    Finished,
}

/// The graph walk, mid-flight.
pub struct Walk<'a> {
    graph: &'a FlowGraph,
    executors: &'a ExecutorRegistry,
    kinds: Option<&'a NodeKindRegistry>,
    options: &'a RunOptions<'a>,

    /// Topological order, as indices into `graph.nodes`.
    order: Vec<usize>,
    cursor: usize,
    state: State,

    /// key: `"{node_id}:{port_id}"`.
    port_values: BTreeMap<String, Value>,
    outputs: BTreeMap<String, Value>,
    events: Vec<RunEvent>,
    errors: Vec<String>,

    /// Set when a host signal was tripped — distinct from an executor's abort.
    cancelled: Option<String>,

    started_at: Option<i64>,
}

impl<'a> Walk<'a> {
    /// Begin a walk. A cyclic graph is finished immediately, with the error set.
    pub(crate) fn start(
        graph: &'a FlowGraph,
        executors: &'a ExecutorRegistry,
        kinds: Option<&'a NodeKindRegistry>,
        options: &'a RunOptions<'a>,
    ) -> Self {
        let mut walk = Self {
            graph,
            executors,
            kinds,
            options,
            order: Vec::new(),
            cursor: 0,
            state: State::Walking,
            port_values: BTreeMap::new(),
            outputs: BTreeMap::new(),
            events: Vec::new(),
            errors: Vec::new(),
            cancelled: None,
            started_at: options.timeout.as_ref().map(|t| t.clock.now_millis()),
        };

        if let Some(order) = topo_sort(graph) {
            walk.order = order;
            walk.emit(RunEvent::run_start());
        } else {
            // The em dash is not decoration. PHP and TypeScript emit this exact
            // string; the Python twin emitted an ASCII hyphen and nothing
            // reported it for two releases, because the shared fixture asserted
            // a SUBSTRING that stopped before the character they disagreed on.
            // `flow/graph-runs` 0021 now pins the whole string on four runtimes.
            let message = "Cycle detected in flow graph \u{2014} aborting.";
            walk.emit(RunEvent::run_error(message));
            walk.errors.push(message.to_string());
            walk.state = State::Finished;
        }

        walk
    }

    fn emit(&mut self, event: RunEvent) {
        self.events.push(event);
    }

    fn node(&self, index: usize) -> &'a FlowNode {
        &self.graph.nodes[index]
    }

    /// Advance until a node needs executing, or the walk is over.
    ///
    /// # Panics
    ///
    /// If called again before [`resume`](Walk::resume) has been given the
    /// outcome of the previous step. That is a driver bug, and a silent one
    /// otherwise: the walk would run the same node twice.
    pub fn next_step(&mut self) -> Option<Step> {
        assert!(
            !matches!(self.state, State::AwaitingOutcome),
            "next_step() called before resume(); the previous node has no outcome yet"
        );

        loop {
            if matches!(self.state, State::Finished) || self.cursor >= self.order.len() {
                self.state = State::Finished;
                return None;
            }

            // Host cancellation propagates out of the run. That is distinct
            // from an executor's abort(), which ends the run with ok=false — a
            // cancelled run has no result to report, a failed one does.
            if let Some(signal) = self.options.signal {
                if signal.aborted() {
                    self.cancelled = Some(signal.reason().unwrap_or_else(|| "aborted".to_string()));
                    self.state = State::Finished;
                    return None;
                }
            }

            // A budget is recorded as an error and observed BETWEEN nodes,
            // mirroring the TypeScript timer that pushes an error the loop then
            // sees. Nothing interrupts an executor mid-call on any runtime.
            if let (Some(timeout), Some(started)) = (&self.options.timeout, self.started_at) {
                if self.errors.is_empty()
                    && timeout.clock.now_millis().saturating_sub(started) > timeout.budget_ms
                {
                    self.errors.push(alloc::format!(
                        "Run timed out after {}ms",
                        timeout.budget_ms
                    ));
                }
            }

            if !self.errors.is_empty() {
                self.state = State::Finished;
                return None;
            }

            let index = self.order[self.cursor];
            let node = self.node(index);

            // Resume: a node completed in a prior run is NOT re-executed. Its
            // stored output is republished on its ports, reproducing the same
            // routing, so downstream nodes see identical inputs. This is the
            // primitive every durable driver is built on.
            if let Some(stored) = self.options.resume_outputs.get(&node.id) {
                let stored = stored.clone();
                self.publish(index, &stored, true);
                self.cursor += 1;
                continue;
            }

            let incoming = self.incoming(&node.id);

            // Run once ANY upstream branch reaches this node. In topological
            // order every upstream node is already settled, so each incoming
            // edge is active or dead — never pending. Requiring ALL active
            // wrongly skipped merge points (#1): when a decision routes down one
            // branch, the other branch's edge stays dead forever, so an `every`
            // check skipped the shared continuation and halted the run after the
            // first branch. `collect_inputs` reads only the active ones.
            if !incoming.is_empty() {
                let any_active = incoming.iter().any(|edge| {
                    self.port_values
                        .contains_key(&port_key(&edge.source, edge.source_port()))
                });
                if !any_active {
                    self.emit(RunEvent::node_status(
                        &node.id,
                        NodeStatus::Idle,
                        Some("skipped"),
                    ));
                    self.cursor += 1;
                    continue;
                }
            }

            // Notes and layout nodes are visual only — never executed and never
            // fed to runners. Their config (a note's text, a lane's title) stays
            // in the document for editors and MCP tools, but the engine walks
            // straight past them. Edges cross lanes freely, so grouping never
            // affects topology.
            if let Some(text) = self.visual_only(node) {
                self.emit(RunEvent::node_status(
                    &node.id,
                    NodeStatus::Idle,
                    Some(text),
                ));
                self.cursor += 1;
                continue;
            }

            self.emit(RunEvent::node_status(&node.id, NodeStatus::Running, None));

            let Some(executor) = self.executors.resolve_for(node) else {
                // An unregistered kind fails CLOSED — no executor, no outputs,
                // and the run stops. That silence is the right default and is
                // exactly what made a kind-keyed registry miss go unnoticed in
                // the TypeScript engine until 0.48.1.
                let message = alloc::format!(
                    "No executor registered for kind={}",
                    node.kind.as_deref().unwrap_or("")
                );
                self.emit(RunEvent::node_status(
                    &node.id,
                    NodeStatus::Error,
                    Some(&message),
                ));
                self.emit(RunEvent::log(
                    crate::runtime::LogLevel::Error,
                    &message,
                    Some(&node.id),
                ));
                self.errors.push(message);
                self.state = State::Finished;
                return None;
            };

            self.state = State::AwaitingOutcome;
            return Some(Step { index, executor });
        }
    }

    /// Build the context for a step. Separate from [`next_step`](Walk::next_step)
    /// so the driver owns the context and can hand `&mut` to the executor.
    #[must_use]
    pub fn context_for(&self, step: &Step) -> ExecutionContext<'a> {
        let node = self.node(step.index);
        let incoming = self.incoming(&node.id);
        let inputs = self.collect_inputs(node, &incoming);
        ExecutionContext::new(node, inputs, self.options.depth, self.options.run.as_ref())
    }

    /// Report what an executor did, and absorb whatever it emitted.
    ///
    /// # Panics
    ///
    /// If no step is in flight. A driver calling this out of order would
    /// otherwise attribute one node's outcome to another.
    pub fn resume(&mut self, mut ctx: ExecutionContext<'a>, outcome: Outcome) {
        assert!(
            matches!(self.state, State::AwaitingOutcome),
            "resume() called without a step in flight"
        );
        self.state = State::Walking;

        let index = self.order[self.cursor];
        let node_id = self.node(index).id.clone();

        self.events.extend(ctx.take_emitted());

        match outcome {
            Outcome::Value(value) => {
                self.publish(index, &value, false);
            }
            Outcome::Aborted(aborted) => {
                // VERBATIM. A human gate pauses through this exact string and
                // the durable layer decodes it back out; decorating it here is
                // what broke 72 tests in the PHP twin.
                let reason = aborted.reason;
                self.emit(RunEvent::node_status(
                    &node_id,
                    NodeStatus::Error,
                    Some(&reason),
                ));
                self.emit(RunEvent::log(
                    crate::runtime::LogLevel::Error,
                    &reason,
                    Some(&node_id),
                ));
                self.errors.push(reason);
                self.state = State::Finished;
                return;
            }
        }

        self.cursor += 1;
    }

    /// Finish the walk and produce the result.
    ///
    /// # Errors
    ///
    /// [`RunAborted`] when a **host signal** cancelled the run. That is not a
    /// failed run: a cancelled run has no result to report, a failed one does.
    pub fn finish(mut self) -> Result<RunResult, RunAborted> {
        if let Some(reason) = self.cancelled.take() {
            return Err(RunAborted::new(reason));
        }

        let ok = self.errors.is_empty();
        self.emit(RunEvent::run_end(ok));

        Ok(RunResult {
            ok,
            outputs: self.outputs,
            error: self.errors.into_iter().next(),
            events: self.events,
        })
    }

    // -- publishing ------------------------------------------------------

    /// Record a result, publish it on the activated ports, mark the node done.
    fn publish(&mut self, index: usize, result: &Value, resumed: bool) {
        let node = self.node(index);
        let node_id = node.id.clone();
        self.outputs.insert(node_id.clone(), result.clone());

        let (ports, value) = self.activated_ports(node, result);
        for port_id in ports {
            self.port_values
                .insert(port_key(&node_id, &port_id), value.clone());
            self.emit(RunEvent::node_output(&node_id, &port_id, value.clone()));
        }

        self.emit(RunEvent::node_status(
            &node_id,
            NodeStatus::Done,
            if resumed { Some("resumed") } else { None },
        ));
    }

    /// Which output ports a result activates, and the value carried.
    ///
    /// **These rules live here and only here.** A queue driver must read the
    /// activated ports back off the `node-output` events this emits rather than
    /// re-deriving them; a second copy of a routing table is the kind of
    /// duplicate that agrees for a year and then disagrees on one branch.
    fn activated_ports(&self, node: &FlowNode, result: &Value) -> (Vec<String>, Value) {
        if let Some(map) = result.as_object() {
            if let Some(port) = map.get("__port").and_then(Value::as_str) {
                return (
                    alloc::vec![port.to_string()],
                    map.get("value").cloned().unwrap_or(Value::Null),
                );
            }
            if let Some(port) = map.get("branch").and_then(Value::as_str) {
                // Key PRESENCE, not null-ness. Two different questions:
                //   no `value` key at all -> the whole result IS the payload
                //   `value` present, null -> the payload is null; pass it on
                // Matching `Some(Value::Null) | None` collapsed them, so a
                // branch whose payload was null leaked the WRAPPER downstream --
                // every following node received `{ branch, value }`, two fields
                // no kind declares. The reachable path is an upstream
                // `transform` whose dot-path did not resolve. All four runtimes
                // shared this identically, so no parity table could catch it:
                // they agreed on being wrong.
                let value = match map.get("value") {
                    None => result.clone(),
                    Some(value) => value.clone(),
                };
                return (alloc::vec![port.to_string()], value);
            }
        }

        let mut declared: Option<Vec<String>> = node
            .outputs
            .as_ref()
            .map(|ports| ports.iter().map(|p| p.id.clone()).collect());

        // When the node declares none, fall back to the KIND's ports before
        // falling back to `out`. This covers hand-written schemas that omit
        // them; the TypeScript side resolves ports through its kind and
        // serializes the resolved ports into the document.
        if declared.is_none() {
            if let (Some(kinds), Some(kind_name)) = (self.kinds, node.kind.as_deref()) {
                if let Some(kind) = kinds.get(kind_name) {
                    // Only adopt NON-EMPTY kind ports. A terminal kind declares
                    // an empty list, and consuming that literally would publish
                    // zero ports where the historical fallback published `out`
                    // — silently cutting every chain through such a node.
                    if let Some(ports) = kind.outputs.as_ref().filter(|ports| !ports.is_empty()) {
                        declared = Some(ports.iter().map(|p| p.id.clone()).collect());
                    }
                }
            }
        }

        match declared {
            // `Some(vec![])` is "explicitly no ports" and is honoured as such.
            // Collapsing it into the `out` fallback is how a terminal node
            // starts publishing.
            Some(ports) => (ports, result.clone()),
            None => (alloc::vec!["out".to_string()], result.clone()),
        }
    }

    // -- inputs ----------------------------------------------------------

    fn incoming(&self, node_id: &str) -> Vec<&'a FlowEdge> {
        self.graph
            .edges
            .iter()
            .filter(|edge| edge.target == node_id)
            .collect()
    }

    /// Gather a node's inputs, keyed by target-port id (default `in`).
    ///
    /// Only **active** incoming edges contribute. An edge whose source port
    /// never produced a value — a dead branch — is skipped, so it cannot
    /// clobber a live value arriving on the same handle.
    ///
    /// This was a REAL divergence once: TypeScript assigned unconditionally, so
    /// a trailing dead edge overwrote a live one with `undefined` whenever two
    /// branches rejoined on the same handle. PHP implemented the documented
    /// contract, TypeScript implemented the code, and the two disagreed
    /// silently since both still reported success. TypeScript was fixed in
    /// fancy-flow 0.27.1; `flow/graph-runs` 0023 pins it on all four sides.
    fn collect_inputs(&self, node: &FlowNode, incoming: &[&FlowEdge]) -> Map {
        let mut inputs = self
            .options
            .initial_inputs
            .get(&node.id)
            .cloned()
            .unwrap_or_default();

        for edge in incoming {
            let key = port_key(&edge.source, edge.source_port());
            if let Some(value) = self.port_values.get(&key) {
                inputs.insert(edge.target_port(), value.clone());
            }
        }
        inputs
    }

    /// `Some(label)` when the engine must walk straight past this node.
    fn visual_only(&self, node: &FlowNode) -> Option<&'static str> {
        let kind_name = node.kind.as_deref()?;

        // Matched across every id the kind answers to: a graph saved with the
        // canonical `@particle-academy/note` must stay an annotation, not
        // become an unrunnable node.
        if kind_id::matches(kind_name, "note") {
            return Some("annotation");
        }

        let kind = self.kinds?.get(kind_name)?;
        match kind.category.as_str() {
            crate::registry::category::LAYOUT => Some("lane"),
            crate::registry::category::ANNOTATION => Some("annotation"),
            _ => None,
        }
    }
}

fn port_key(node_id: &str, port_id: &str) -> String {
    alloc::format!("{node_id}:{port_id}")
}

/// Kahn's algorithm. `None` when a cycle is present.
///
/// Iteration order matches the peer engines so runs are comparable node for
/// node, not merely equal at the end.
fn topo_sort(graph: &FlowGraph) -> Option<Vec<usize>> {
    let mut index_of: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, node) in graph.nodes.iter().enumerate() {
        index_of.insert(node.id.as_str(), index);
    }

    let mut in_degree: Vec<usize> = alloc::vec![0; graph.nodes.len()];
    for edge in &graph.edges {
        if let Some(&target) = index_of.get(edge.target.as_str()) {
            in_degree[target] += 1;
        }
    }

    // Seeded in DOCUMENT order, not sorted-by-id: the peers push entry nodes in
    // the order they appear, and a run is compared node for node.
    let mut queue: Vec<usize> = (0..graph.nodes.len())
        .filter(|&index| in_degree[index] == 0)
        .collect();

    let mut ordered: Vec<usize> = Vec::with_capacity(graph.nodes.len());
    let mut at = 0;
    while at < queue.len() {
        let index = queue[at];
        at += 1;
        ordered.push(index);

        let node_id = graph.nodes[index].id.as_str();
        for edge in &graph.edges {
            if edge.source != node_id {
                continue;
            }
            if let Some(&target) = index_of.get(edge.target.as_str()) {
                in_degree[target] = in_degree[target].saturating_sub(1);
                if in_degree[target] == 0 {
                    queue.push(target);
                }
            }
        }
    }

    // A duplicate node id makes `index_of` smaller than `nodes`, which would
    // also fail this check — correctly, since a graph with two nodes of one id
    // has no well-defined topology.
    let unique: BTreeSet<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
    if ordered.len() != graph.nodes.len() || unique.len() != graph.nodes.len() {
        return None;
    }
    Some(ordered)
}
