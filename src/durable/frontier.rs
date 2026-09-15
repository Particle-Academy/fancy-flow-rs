//! Which nodes can run RIGHT NOW, given what has already settled.
//!
//! # Why this is not a second engine
//!
//! [`FlowRunner`](crate::FlowRunner) walks a Kahn topological order and, at each
//! node, runs it when at least one incoming edge is active. That is a total
//! order because one process executes every node. Split the graph across jobs
//! and the same rule has to be asked the other way round -- not "what is next"
//! but "what is unblocked" -- which is this module.
//!
//! The rule is the engine's, restated:
//!
//! - every direct predecessor has SETTLED (in topological order the engine has
//!   already settled all of them by the time it reaches a node);
//! - and either the node has no incoming edges, or at least one incoming edge
//!   is ACTIVE -- its source completed and published on that edge's source
//!   handle.
//!
//! A node whose predecessors have all settled with no active edge is what the
//! engine reports as `idle/skipped`. Skipping SETTLES it, which can in turn
//! unblock -- or skip -- its own successors, so the pass repeats until nothing
//! changes. That cascade is how a dead branch collapses without leaving the run
//! stuck.
//!
//! # The one thing it does NOT decide
//!
//! Which ports a result activated. Those rules (`__port`, `branch`, declared
//! outputs, the kind's ports, the `out` fallback) live in the engine and stay
//! there: this reads the ports back off the `node-output` events the engine
//! emitted when the node ran, stored on the claim row.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use super::state::{NodeClaimStore, NodeRunStatus, RunState};
use crate::registry::kind_id;
use crate::schema::{FlowEdge, FlowGraph};

/// What [`Frontier::compute`] decided.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FrontierResult {
    /// The nodes that may run now: declaration order within each pass of the
    /// skip cascade, exactly as the peers order it.
    pub ready: Vec<String>,
    /// The nodes this pass settled as skipped, in cascade order.
    pub skipped: Vec<String>,
}

/// The frontier of a durable run. Stateless: every answer is a function of the
/// graph and the rows.
pub struct Frontier;

impl Frontier {
    /// The ready nodes and the skip cascade, for `state`.
    ///
    /// Iterative, and bounded: each pass either settles or readies at least one
    /// node or stops, so there are at most `nodes + 1` passes.
    #[must_use]
    pub fn compute(graph: &FlowGraph, state: &RunState) -> FrontierResult {
        let mut incoming: BTreeMap<&str, Vec<&FlowEdge>> = BTreeMap::new();
        for edge in &graph.edges {
            incoming.entry(edge.target.as_str()).or_default().push(edge);
        }

        // Settled nodes and the ports they lit. A skipped node is settled with
        // NO ports, which is precisely what makes its successors skip too.
        let mut settled: BTreeMap<&str, &[String]> = BTreeMap::new();
        let mut held: BTreeSet<&str> = BTreeSet::new();
        for (node_id, entry) in state {
            match entry.status {
                NodeRunStatus::Completed => {
                    settled.insert(node_id.as_str(), entry.ports.as_slice());
                }
                NodeRunStatus::Skipped | NodeRunStatus::Failed => {
                    settled.insert(node_id.as_str(), &[]);
                }
                NodeRunStatus::Claimed | NodeRunStatus::Paused => {
                    held.insert(node_id.as_str());
                }
            }
        }

        let mut ready: Vec<String> = Vec::new();
        let mut ready_ids: BTreeSet<&str> = BTreeSet::new();
        let mut skipped: Vec<String> = Vec::new();

        let mut changed = true;
        while changed {
            changed = false;

            for node in &graph.nodes {
                let node_id = node.id.as_str();
                if settled.contains_key(node_id)
                    || held.contains(node_id)
                    || ready_ids.contains(node_id)
                {
                    continue;
                }

                let edges = incoming.get(node_id).map_or(&[][..], Vec::as_slice);
                let mut blocked = false;
                let mut active = false;

                for edge in edges {
                    let Some(ports) = settled.get(edge.source.as_str()) else {
                        blocked = true;
                        break;
                    };
                    // The engine's port key: an edge with no source handle reads
                    // the source's `out` port.
                    let handle = edge.source_port();
                    if ports.iter().any(|port| port == handle) {
                        active = true;
                    }
                }

                if blocked {
                    continue;
                }

                // Reached, but down a branch that never lit.
                if !edges.is_empty() && !active {
                    settled.insert(node_id, &[]);
                    skipped.push(node.id.clone());
                    changed = true;
                    continue;
                }

                // Annotations are never executed. Settling them here rather than
                // dispatching a job saves a queue round trip per sticky note --
                // and a graph can carry a lot of sticky notes.
                if node
                    .kind
                    .as_deref()
                    .is_some_and(|kind| kind_id::matches(kind, "note"))
                {
                    settled.insert(node_id, &[]);
                    skipped.push(node.id.clone());
                    changed = true;
                    continue;
                }

                ready_ids.insert(node_id);
                ready.push(node.id.clone());
            }
        }

        // In the order nodes became ready: declaration order within a pass,
        // and a node readied only once a LATER-declared skip settled comes after
        // the pass that readied the rest. That is what PHP's, TypeScript's and
        // Python's frontiers return, and `flow/durable-dispatch` was generated
        // from PHP's, so it is reproduced rather than re-sorted.
        FrontierResult { ready, skipped }
    }

    /// Has every node settled? The run is finished when it has.
    #[must_use]
    pub fn is_complete(graph: &FlowGraph, state: &RunState) -> bool {
        graph.nodes.iter().all(|node| {
            state
                .get(&node.id)
                .is_some_and(|entry| entry.status.is_settled())
        })
    }

    /// Is any node still held by a worker, or parked for a person?
    ///
    /// An empty frontier means something different depending on this: with
    /// work in flight the run is simply waiting, and whichever job finishes will
    /// advance it. With nothing in flight and nodes still unsettled, the graph
    /// cannot progress at all -- which is a stuck run, and must be reported
    /// rather than waited on.
    #[must_use]
    pub fn has_work_in_flight(state: &RunState) -> bool {
        state.values().any(|entry| entry.status.is_held())
    }

    /// Persist the skip cascade so the next pass does not recompute it.
    ///
    /// Returns the nodes THIS call settled, in cascade order. A node another
    /// caller had already settled is left out.
    pub fn settle_skips<S: NodeClaimStore + ?Sized>(
        store: &S,
        run_key: &str,
        skipped: &[String],
    ) -> Vec<String> {
        skipped
            .iter()
            .filter(|node_id| store.skip(run_key, node_id))
            .cloned()
            .collect()
    }
}
