//! Is this graph safe to ACCEPT?
//!
//! Distinct from [`workflow`](crate::workflow), which answers "is this graph
//! COHERENT?". Conflating the two is how a payload gets treated as a document —
//! and this port's consumer accepts graphs that arrived over a wire and
//! executes them, so the distinction is not academic.

use alloc::collections::BTreeSet;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use fancy_json::Value;

use crate::error::UnsafeGraph;
use crate::registry::kind_id;
use crate::schema::{FlowGraph, ImportIssue};

/// Caps and kind rules a graph must satisfy before it is run.
///
/// Immutable: every `with` / `allow` / `deny` returns a new policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphPolicy {
    /// Most nodes permitted.
    pub max_nodes: usize,
    /// Most edges permitted.
    pub max_edges: usize,
    /// Deepest nesting permitted inside a config value.
    pub max_depth: usize,
    /// Longest string permitted anywhere in the document.
    pub max_string_length: usize,
    /// Largest serialized document permitted.
    pub max_bytes: usize,
    /// Bare kind names. `None` means "no allowlist".
    pub allowed: Option<BTreeSet<String>>,
    /// Bare kind names.
    pub denied: BTreeSet<String>,
}

impl Default for GraphPolicy {
    fn default() -> Self {
        Self {
            max_nodes: 60,
            max_edges: 120,
            max_depth: 12,
            max_string_length: 20_000,
            max_bytes: 256_000,
            allowed: None,
            denied: BTreeSet::new(),
        }
    }
}

impl GraphPolicy {
    /// The posture for a graph you did not write.
    ///
    /// Deliberately strict, and deliberately an ALLOWLIST: a denylist of
    /// dangerous kinds is a list you have to keep complete forever, and the
    /// first kind added to the package after you wrote it is permitted by
    /// default. An allowlist fails the other way, which is the correct way.
    ///
    /// The caller names what it wants to permit, because only the caller knows
    /// — this crate cannot guess which of its own kinds are safe in someone
    /// else's app.
    ///
    /// # Divergence from the PHP twin, on purpose
    ///
    /// `GraphPolicy::untrusted()` there returns a policy whose allowlist is
    /// ABSENT rather than empty, and an absent allowlist permits every kind. A
    /// caller who writes `untrusted()->assert()` and forgets `allowKinds()`
    /// gets size caps and byte hygiene with **no kind restriction at all**,
    /// from a method named `untrusted`.
    ///
    /// Here `untrusted()` starts with an EMPTY allowlist: nothing is permitted
    /// until something is named. That changes no verdict for a correctly
    /// configured policy, and turns a silent fail-open into a loud rejection.
    /// `fancy-flow-py` made the same call.
    #[must_use]
    pub fn untrusted() -> Self {
        Self {
            allowed: Some(BTreeSet::new()),
            ..Self::default()
        }
    }

    /// Caps only, no kind policy — for graphs your own code produced.
    #[must_use]
    pub fn trusted() -> Self {
        Self {
            max_nodes: 5_000,
            max_edges: 10_000,
            max_depth: 32,
            max_string_length: 1_000_000,
            max_bytes: 8_000_000,
            allowed: None,
            denied: BTreeSet::new(),
        }
    }

    /// Permit ONLY these kinds.
    ///
    /// Any spelling: every id each kind answers to is permitted with it,
    /// because the allowlist is keyed on the bare name.
    #[must_use]
    pub fn allow_kinds(mut self, kinds: &[&str]) -> Self {
        self.allowed = Some(kinds.iter().map(|k| kind_id::bare(k).to_string()).collect());
        self
    }

    /// Refuse these kinds, whatever else is permitted.
    #[must_use]
    pub fn deny_kinds(mut self, kinds: &[&str]) -> Self {
        self.denied = kinds.iter().map(|k| kind_id::bare(k).to_string()).collect();
        self
    }

    /// Whether one kind id is permitted.
    ///
    /// Keyed on the BARE name, so `branch`, `@fancy/branch` and
    /// `@particle-academy/branch` are one decision. A policy that matched
    /// literal strings would permit a kind under one spelling and refuse it
    /// under another, which is an allowlist with a hole in it.
    #[must_use]
    pub fn permits(&self, kind_id_text: &str) -> bool {
        let bare = kind_id::bare(kind_id_text);
        if self.denied.contains(bare) {
            return false;
        }
        match &self.allowed {
            Some(allowed) => allowed.contains(bare),
            None => true,
        }
    }

    /// Everything wrong with a graph, or an empty list.
    ///
    /// Returns them ALL, not the first: a caller fixing a rejected graph wants
    /// the whole list, and a validator that reveals one problem per attempt
    /// turns a five-minute fix into five round trips.
    #[must_use]
    pub fn inspect(&self, graph: &FlowGraph) -> Vec<ImportIssue> {
        let mut issues = Vec::new();

        if graph.nodes.len() > self.max_nodes {
            issues.push(ImportIssue::error(alloc::format!(
                "Graph has {} nodes; the policy permits {}.",
                graph.nodes.len(),
                self.max_nodes
            )));
        }
        if graph.edges.len() > self.max_edges {
            issues.push(ImportIssue::error(alloc::format!(
                "Graph has {} edges; the policy permits {}.",
                graph.edges.len(),
                self.max_edges
            )));
        }

        for node in &graph.nodes {
            let kind = node.kind.as_deref().unwrap_or("");
            if !self.permits(kind) {
                issues.push(
                    ImportIssue::error(alloc::format!("Kind \"{kind}\" is not permitted."))
                        .at_node(&node.id),
                );
            }

            let config = Value::Object(node.config.clone());
            if let Some(problem) = self.inspect_value(&config, 1) {
                issues.push(ImportIssue::error(problem).at_node(&node.id));
            }
        }

        let serialized = crate::workflow::to_json(graph, None);
        if serialized.len() > self.max_bytes {
            issues.push(ImportIssue::error(alloc::format!(
                "Graph serializes to {} bytes; the policy permits {}.",
                serialized.len(),
                self.max_bytes
            )));
        }

        issues
    }

    /// Depth and string caps, walked iteratively.
    ///
    /// **Iteratively**, because the thing being checked is untrusted nesting:
    /// a recursive walk would blow the stack on exactly the input the depth cap
    /// exists to refuse, before the cap could report it.
    fn inspect_value(&self, root: &Value, start_depth: usize) -> Option<String> {
        let mut stack: Vec<(&Value, usize)> = alloc::vec![(root, start_depth)];

        while let Some((value, depth)) = stack.pop() {
            if depth > self.max_depth {
                return Some(alloc::format!(
                    "Config nests deeper than {} levels.",
                    self.max_depth
                ));
            }
            match value {
                Value::String(text) if text.len() > self.max_string_length => {
                    return Some(alloc::format!(
                        "A config string is {} characters; the policy permits {}.",
                        text.len(),
                        self.max_string_length
                    ));
                }
                Value::Array(items) => {
                    for item in items {
                        stack.push((item, depth + 1));
                    }
                }
                Value::Object(map) => {
                    for (_, item) in map.iter() {
                        stack.push((item, depth + 1));
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Whether a graph satisfies the policy.
    #[must_use]
    pub fn accepts(&self, graph: &FlowGraph) -> bool {
        self.inspect(graph).is_empty()
    }

    /// Refuse a graph that does not satisfy the policy.
    ///
    /// # Errors
    ///
    /// [`UnsafeGraph`], carrying every issue.
    pub fn assert_safe(&self, graph: &FlowGraph) -> Result<(), UnsafeGraph> {
        let issues = self.inspect(graph);
        if issues.is_empty() {
            Ok(())
        } else {
            Err(UnsafeGraph { issues })
        }
    }
}
