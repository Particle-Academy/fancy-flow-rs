//! The HOST seam.
//!
//! Two contracts the engine declares and never implements. Core stays
//! framework-free and dependency-free; a host plugs in whatever it actually
//! uses.
//!
//! On AI this engine is a **shuttle, not an engine**: it declares a decision
//! contract and never imports a provider SDK.

use alloc::string::String;
use alloc::vec::Vec;

use crate::schema::FlowGraph;

/// Where workflows live — `subflow` resolves a name through this.
pub trait WorkflowResolver {
    /// Find a workflow by the reference a `subflow` node names.
    fn resolve(&self, reference: &str) -> Option<FlowGraph>;
}

/// A routing decision, with no prompt and no provider.
///
/// One method, deliberately. `llm_router` needs to know WHICH branch, not how
/// the host decided — so a rules engine, a cached classifier and a frontier
/// model all satisfy the same contract, and core never learns which one ran.
pub trait LlmClient {
    /// Choose one of `routes` for `input`, or `None` to fall through.
    fn choose_route(&self, input: &str, routes: &[String]) -> Option<String>;
}

/// A resolver holding a fixed set of workflows — enough for a test, and for a
/// host whose workflows are compiled in.
#[derive(Debug, Default)]
pub struct StaticWorkflowResolver {
    entries: alloc::collections::BTreeMap<String, FlowGraph>,
}

impl StaticWorkflowResolver {
    /// An empty resolver.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a workflow under a reference.
    #[must_use]
    pub fn with(mut self, reference: &str, graph: FlowGraph) -> Self {
        self.entries.insert(String::from(reference), graph);
        self
    }
}

impl WorkflowResolver for StaticWorkflowResolver {
    fn resolve(&self, reference: &str) -> Option<FlowGraph> {
        self.entries.get(reference).cloned()
    }
}

/// The first declared route, always.
///
/// Deterministic by construction, which is what lets a graph containing an
/// `llm_router` run in a test — and on a chain, where a model call is not
/// available and would not be reproducible if it were.
#[derive(Debug, Default)]
pub struct FirstRouteClient;

impl LlmClient for FirstRouteClient {
    fn choose_route(&self, _input: &str, routes: &[String]) -> Option<String> {
        routes.first().cloned()
    }
}

/// The message a host sees when an LLM-backed node has no client.
///
/// **No auto-detection**, deliberately — divergence from the PHP twin, which
/// probes for Prism / laravel-ai with `class_exists()` because that is free in
/// PHP. The Rust equivalent would be a feature flag silently changing
/// behaviour, or a provider the author never named.
#[must_use]
pub fn llm_unavailable_message(node_id: &str) -> String {
    alloc::format!(
        "Node {node_id} needs an LlmClient and none is configured. Core declares the contract \
         and never imports a provider; wire one through ExecutorDeps."
    )
}

/// Every route name declared on a node, in order.
#[must_use]
pub fn declared_routes(routes: Option<&fancy_json::Value>) -> Vec<String> {
    routes
        .and_then(fancy_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(alloc::string::ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}
