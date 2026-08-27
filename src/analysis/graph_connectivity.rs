//! Refuse a graph whose nodes cannot take part in the workflow's dataflow.
//!
//! Two shapes, both of which import cleanly and then quietly do nothing.
//! Neither *fails* — which is what makes them worth refusing at authoring time,
//! because a run that reports success is the worst way for a workflow to be
//! wrong. Both were measured against the engine before any runtime implemented
//! this:
//!
//! 1. **A floating node** — no inbound edge and no outbound edge. It is not
//!    skipped: a node with no incoming edge is a root, so the topological sort
//!    runs it. A three-node graph with one stray `log` executed `t,lonely,o`.
//!    It runs disconnected, receiving nothing from the graph and reaching
//!    nobody in it, which is exactly the state an author cannot see on a canvas.
//!
//! 2. **An edge leaving a terminator.** A terminal kind — `output`, `log` —
//!    declares an *empty* output port list; it ends a chain. Measured:
//!    `t -> output -> log` imported clean and the `log` ran, with
//!    `{{ input }}` resolving to `""`. Inputs bind only when
//!    `"<source_id>:<handle>"` exists, and a node publishing no ports never
//!    creates that key — so the edge does not fail, it delivers nothing, and
//!    the node downstream operates on a hole.
//!
//! Both are errors rather than warnings because both are unambiguous: no data
//! at run time makes a floating node participate, and none makes an edge out of
//! a terminator deliver. That is the test for refusing at authoring time
//! instead of warning about it.
//!
//! The twin of `FancyFlow\Analysis\GraphConnectivity` (PHP 0.48),
//! `checkGraphConnectivity` (TypeScript 0.64) and
//! `fancy_flow.analysis.check_graph_connectivity` (Python 0.16).

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::vec::Vec;

use crate::registry::kind_id;
use crate::registry::NodeKindRegistry;
use crate::schema::{FlowEdge, FlowNode, ImportIssue};

/// Every connectivity problem in the graph, as import issues.
#[must_use]
pub fn check_graph_connectivity(
    nodes: &[FlowNode],
    edges: &[FlowEdge],
    registry: &NodeKindRegistry,
) -> Vec<ImportIssue> {
    let mut has_incoming: BTreeSet<&str> = BTreeSet::new();
    let mut has_outgoing: BTreeSet<&str> = BTreeSet::new();
    for edge in edges {
        has_incoming.insert(edge.target.as_str());
        has_outgoing.insert(edge.source.as_str());
    }

    let mut issues = Vec::new();

    // A single-node graph is not "floating" -- it is a graph with one step,
    // which is a legitimate (if small) workflow and what every graph looks like
    // on the way to a bigger one. Refusing it would make an editor unusable
    // from the first node placed.
    let single = nodes.len() == 1;

    for node in nodes {
        if single || may_float(node, registry) {
            continue;
        }

        if !has_incoming.contains(node.id.as_str()) && !has_outgoing.contains(node.id.as_str()) {
            issues.push(
                ImportIssue::error(format!(
                    "Node \"{}\" is connected to nothing - no inbound edge and no outbound edge. \
                     It still RUNS (a node with no inbound edge is a root), but it receives \
                     nothing from the graph and reaches nobody in it, so it is either unwired or \
                     left behind by a deletion. Only a note, an annotation or a lane may float.",
                    node.id
                ))
                .at_node(node.id.clone()),
            );
        }
    }

    let by_id: BTreeMap<&str, &FlowNode> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    for edge in edges {
        let Some(source) = by_id.get(edge.source.as_str()) else {
            continue;
        };
        if !is_terminator(source, registry) {
            continue;
        }

        issues.push(
            ImportIssue::error(format!(
                "Edge \"{}\" reads from \"{}\", which is a TERMINAL node and publishes no output \
                 ports at all. Nothing can ever travel this edge: it does not fail at run time, \
                 it delivers nothing, and \"{}\" runs anyway with an empty input.",
                edge.id, edge.source, edge.target
            ))
            .at_edge(edge.id.clone()),
        );
    }

    issues
}

/// Whether this node is allowed to sit unconnected.
///
/// Three answers, and the third is the one that took a second pass — it was
/// missed in the PHP twin's first release and shipped as 0.48.1:
///
/// 1. `note`, matched across every id the kind answers to, so a graph saved
///    with the canonical `@particle-academy/note` stays an annotation rather
///    than becoming an unwireable node.
/// 2. Any kind categorised `annotation` or `layout`. A host may register its
///    own note, and the TypeScript runtime ships `@particle-academy/lane` — a
///    swimlane its engine walks straight past. Neither is a step, and neither
///    is ever wired to anything.
/// 3. A kind this registry has never heard of. Not a loophole, the honest
///    answer: an unknown kind already produces its own issue, and we cannot
///    know whether it is a step, an annotation or a lane. Claiming it must be
///    wired would assert something unverifiable — and it lands hardest on the
///    graphs that deserve it least, since a laned graph loaded by a runtime
///    without `lane` registered would report every swimlane twice, the second
///    time wrongly.
#[must_use]
pub fn may_float(node: &FlowNode, registry: &NodeKindRegistry) -> bool {
    let Some(kind_name) = node.kind.as_deref() else {
        return false;
    };
    if kind_name.is_empty() {
        return false;
    }

    if kind_id::matches(kind_name, "note") {
        return true;
    }

    match registry.get(kind_name) {
        None => true,
        Some(kind) => kind.category == "annotation" || kind.category == "layout",
    }
}

/// Whether this node ends a chain and can never be an edge's source.
///
/// `Some(vec![])` and `None` are different answers and only the first means
/// this. `None` is "nobody declared what this publishes", which resolves to
/// `out` and describes most nodes in most graphs; an empty list is an explicit
/// claim that there is nothing to connect from. Reading them alike would refuse
/// nearly every workflow ever written.
fn is_terminator(node: &FlowNode, registry: &NodeKindRegistry) -> bool {
    // A node declaring its own ports overrides its kind, so an author who has
    // said what this node publishes is believed -- the same way the engine
    // believes it.
    //
    // Reachable only for a hand-built graph here: `import_workflow` drops
    // node-level ports (as the PHP and Python twins do, and unlike the
    // TypeScript one, which preserves them). A real divergence between the
    // importers, recorded rather than smoothed over.
    if let Some(own) = node.outputs.as_ref() {
        return own.is_empty();
    }

    let Some(kind_name) = node.kind.as_deref() else {
        return false;
    };

    // An unregistered kind falls back to `out` in the engine, so it is not a
    // terminator. Refusing here would break a host mid-registration, and would
    // use "I do not know" as evidence.
    match registry.get(kind_name) {
        None => false,
        Some(kind) => kind.outputs.as_ref().is_some_and(Vec::is_empty),
    }
}
