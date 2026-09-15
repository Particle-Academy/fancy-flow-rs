//! AN EDGE THAT DELIVERS NOTHING MUST SAY SO -- the rule, in one place.
//!
//! An edge whose source COMPLETED without publishing the port it reads, and
//! which could NEVER publish it, binds nothing on any run. If it is the
//! target's only inbound edge the target is skipped; if not, the target runs
//! with that input missing and a correct template renders empty. Both are
//! silent, so the engine warns against the target.
//!
//! # Why it is not a method on the walk
//!
//! Two drivers ask it, at different moments:
//!
//! - [`Walk`](super::Walk) asks it of every node it reaches, just before
//!   deciding whether the node runs;
//! - the durable [`Coordinator`](crate::durable::Coordinator) asks it of each
//!   node its frontier SKIPS. A skipped node never gets a job, so the warning
//!   its target id carries was raised inside OTHER jobs' replays and filtered
//!   out there -- a host driving the run durably never saw a warning that a
//!   single-process run of the same graph delivers.
//!
//! The walk answers from its own bookkeeping; the coordinator answers from the
//! ports stored on the claim rows, which are the engine's own `node-output`
//! events. Both hand the SAME function the same four facts -- the target, its
//! inbound edges, what has completed and published, and how to look a kind up
//! -- and emit what comes back. It is the twin of TypeScript's
//! `undeliveredEdgeWarnings`, PHP's `UndeliveredEdges::warnings` and Python's
//! `undelivered_edge_warnings`, and a second copy in the durable layer would
//! agree for a year and then disagree on one config-derived port.
//!
//! Pinned by fancy-conformance `flow/run-diagnostics`, which this crate runs
//! through BOTH drivers.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::OnceCell;

use fancy_json::{Map, Value};

use crate::registry::{builtin, possible_ports, NodeKind, NodeKindRegistry};
use crate::runtime::{LogLevel, RunEvent};
use crate::schema::{FlowEdge, FlowNode};

/// What a driver has recorded about the run so far -- the facts the rule reads.
///
/// A trait rather than the `"<node>:<port>"` map the peers pass, because this
/// crate's walk keeps its port values in a SORTED map: the peers read
/// publication order off their map's insertion order, and a `BTreeMap` has
/// none. Each driver answers from what it already holds instead of building a
/// second copy per node.
pub(crate) trait PublishedPorts {
    /// Whether `node_id` ran to an output -- a resumed node included.
    fn completed(&self, node_id: &str) -> bool;

    /// Whether `node_id` published `port`.
    fn published(&self, node_id: &str, port: &str) -> bool;

    /// Every port `node_id` published, in PUBLICATION order, each once.
    ///
    /// Only asked when a warning fires, so it may be slow.
    fn in_publication_order(&self, node_id: &str) -> Vec<&str>;
}

/// The kind declaration for a node, for the questions a run cannot answer from
/// its result alone.
///
/// The runner's catalogue first, then the executor registry's, then the
/// built-in one. PHP always has a catalogue to ask -- its executor registry
/// falls back to the default registry -- while this crate's runner may hold
/// none at all; stopping there would call `false` impossible for every `branch`
/// run through [`FlowRunner::new`](crate::FlowRunner::new), and warn on
/// ordinary branching.
///
/// Port ACTIVATION does not use this fallback and must not start to: which
/// ports a result lights up without a catalogue is pinned by `flow/graph-runs`.
pub(crate) struct KindLookup<'a> {
    runner: Option<&'a NodeKindRegistry>,
    executors: Option<&'a NodeKindRegistry>,
    /// Built on first use: most runs never ask.
    builtin: OnceCell<NodeKindRegistry>,
}

impl<'a> KindLookup<'a> {
    /// A lookup over the runner's catalogue and the executor registry's.
    pub(crate) fn new(
        runner: Option<&'a NodeKindRegistry>,
        executors: Option<&'a NodeKindRegistry>,
    ) -> Self {
        Self {
            runner,
            executors,
            builtin: OnceCell::new(),
        }
    }

    /// The kind `node` names, from the first catalogue that knows it.
    pub(crate) fn kind_of(&self, node: &FlowNode) -> Option<&NodeKind> {
        let name = node.kind.as_deref()?;
        self.runner
            .and_then(|kinds| kinds.get(name))
            .or_else(|| self.executors.and_then(|kinds| kinds.get(name)))
            .or_else(|| {
                self.builtin
                    .get_or_init(|| {
                        let mut kinds = NodeKindRegistry::new();
                        builtin::register(&mut kinds, true);
                        kinds
                    })
                    .get(name)
            })
    }
}

/// The `log` / `warn` events for each inbound edge of `target` that can never
/// deliver. Returns them and emits nothing; the caller decides where they go.
///
/// Keyed on the source having COMPLETED, which is what separates the two
/// reasons a port can be absent. A branch that was not taken is ordinary and
/// must never warn; a source that finished and CANNOT publish this port is a
/// misconfiguration that will never work on any run.
///
/// So the question is whether the port is POSSIBLE for the source, never
/// whether it was published: an untaken `false` and an impossible handle are
/// both absent, and asking "did it publish?" would warn on every branching
/// graph. A warning that fires on ordinary branching is noise, and noise is how
/// a real warning stops being read.
pub(crate) fn undelivered_edge_warnings(
    target: &FlowNode,
    incoming: &[&FlowEdge],
    run: &dyn PublishedPorts,
    nodes_by_id: &BTreeMap<&str, &FlowNode>,
    kinds: &KindLookup<'_>,
) -> Vec<RunEvent> {
    let mut warnings: Vec<RunEvent> = Vec::new();

    for edge in incoming {
        let handle = edge.source_port();

        if run.published(&edge.source, handle) || !run.completed(&edge.source) {
            continue;
        }
        let Some(&source) = nodes_by_id.get(edge.source.as_str()) else {
            continue;
        };

        let kind = kinds.kind_of(source);
        if possible_ports(source, kind)
            .iter()
            .any(|port| port == handle)
        {
            continue;
        }

        let mut detail = Map::new();
        detail.insert("edge", Value::from(edge.id.as_str()));
        detail.insert("source", Value::from(edge.source.as_str()));
        detail.insert("sourceHandle", Value::from(handle));

        warnings.push(RunEvent::log_with_detail(
            LogLevel::Warn,
            &undelivered_edge_message(edge, source, kind, target, run),
            Some(&target.id),
            Value::Object(detail),
        ));
    }

    warnings
}

/// The warning's text: the edge, the consequence in run-time terms, what the
/// source DID publish, and the remedy for the common case.
fn undelivered_edge_message(
    edge: &FlowEdge,
    source: &FlowNode,
    kind: Option<&NodeKind>,
    target: &FlowNode,
    run: &dyn PublishedPorts,
) -> String {
    let handle = edge.source_port();
    let mut message = alloc::format!(
        "Edge {} reads port \"{handle}\" from node {}, which never publishes it \u{2014} \
         nothing would reach {} at run time.",
        edge.id,
        source.id,
        target.id,
    );

    // In PUBLICATION order, not sorted: a `for_each` that published `item`
    // first must print `item, done`.
    let available = run.in_publication_order(&source.id);
    if !available.is_empty() {
        message.push_str(" Available: ");
        message.push_str(&available.join(", "));
        message.push('.');
    }

    // The near-miss: a FIELD of that name where a PORT was expected, which is
    // nearly always an agent reaching for a field. Naming it turns a correction
    // into an explanation.
    //
    // A config-dependent shape (`OutputShape::Dynamic`) cannot be resolved
    // in-process, so such a kind never gets the note here. PHP resolves it from
    // config; the warning itself still fires on both.
    let is_field = kind
        .and_then(NodeKind::output_fields)
        .is_some_and(|fields| fields.iter().any(|field| field.path == handle));
    if is_field {
        for part in [
            " Note: \"",
            handle,
            "\" is a FIELD this node emits, not a port \u{2014} read it downstream as {{ in.",
            handle,
            " }} rather than naming it as a source handle.",
        ] {
            message.push_str(part);
        }
    }

    // Only when there IS a handle to remove. A handle-less edge reads `out`,
    // and advice that cannot be followed is worse than none.
    if edge.source_handle.is_some() {
        message.push_str(" Leave sourceHandle off to read the node's output.");
    }

    message
}
