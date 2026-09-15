//! How many times a single node may be attempted.
//!
//! # Why this cannot be one number
//!
//! A run-wide `tries` setting forces every workflow to pick between two bad
//! answers. At 1, a single flaky LLM or HTTP call takes the whole run down.
//! Above 1, the retry replays from the last checkpoint and everything already
//! done runs again -- including the nodes that must not: `git_pr_open` opens a
//! second pull request.
//!
//! Per-node jobs make the question per node, which is where it always belonged.
//! A node declaring `sideEffects: unsafe-to-replay` is pinned to ONE attempt and
//! no backoff. Everything else takes the configured tries, or a per-kind
//! override.
//!
//! Undeclared side effects are treated as the configured default rather than
//! assumed safe: this decides retries, and inventing a safety claim on a node
//! author's behalf is how a retry loop ends up posting the same webhook twice.
//!
//! # Backoff is reported, never slept
//!
//! Nothing in this crate sleeps -- a node inside a blockchain node has no
//! business reading a clock to wait on it. [`RetryPolicy::backoff_ms_for`] is
//! what a queue adapter schedules the next attempt with; the in-process
//! [`Coordinator::run_to_completion`](super::Coordinator::run_to_completion)
//! retries immediately.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::registry::{dedup_push, kind_id, NodeKind, NodeKindRegistry};
use crate::schema::FlowNode;

/// A node that is not safe to run twice. Same vocabulary as the node manifest.
pub const UNSAFE_TO_REPLAY: &str = "unsafe-to-replay";

/// Run-wide defaults plus per-kind overrides.
///
/// **Backoff is integer milliseconds**, where the peers carry float seconds
/// (`backoff_seconds` / `backoffSeconds`): every other duration in this crate --
/// the [`Clock`](crate::runtime::Clock), `first_attempt_at`, a run's timeout --
/// is integer milliseconds, and a float here would be the one place a rounding
/// question could enter a deterministic run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts every node gets unless overridden. Below 1 is read as 1.
    pub tries: u32,
    /// How long a queue adapter should wait before the next attempt.
    pub backoff_ms: u64,
    /// Kind id -> tries. Keyed by any spelling; every id the kind answers to is
    /// checked.
    pub per_kind: BTreeMap<String, u32>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            tries: 1,
            backoff_ms: 0,
            per_kind: BTreeMap::new(),
        }
    }
}

impl RetryPolicy {
    /// One attempt per node, no backoff -- what an unset policy means.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the run-wide tries, builder-style.
    #[must_use]
    pub fn with_tries(mut self, tries: u32) -> Self {
        self.tries = tries;
        self
    }

    /// Set the run-wide backoff, builder-style.
    #[must_use]
    pub fn with_backoff_ms(mut self, backoff_ms: u64) -> Self {
        self.backoff_ms = backoff_ms;
        self
    }

    /// Override the tries for one kind, under any of its spellings.
    #[must_use]
    pub fn with_kind_tries(mut self, kind: &str, tries: u32) -> Self {
        self.per_kind.insert(kind.to_string(), tries);
        self
    }

    /// How many attempts `node` gets, resolving its kind against `kinds`.
    #[must_use]
    pub fn tries_for(&self, node: &FlowNode, kinds: &NodeKindRegistry) -> u32 {
        self.tries_for_kind(node, Self::kind_in(node, kinds))
    }

    /// How long to wait before `node`'s next attempt, in milliseconds.
    #[must_use]
    pub fn backoff_ms_for(&self, node: &FlowNode, kinds: &NodeKindRegistry) -> u64 {
        self.backoff_ms_for_kind(Self::kind_in(node, kinds))
    }

    /// Whether `node`'s kind declares [`UNSAFE_TO_REPLAY`].
    #[must_use]
    pub fn is_unsafe_to_replay(node: &FlowNode, kinds: &NodeKindRegistry) -> bool {
        Self::kind_is_unsafe_to_replay(Self::kind_in(node, kinds))
    }

    /// [`tries_for`](Self::tries_for), given the kind already resolved -- for a
    /// caller whose kind lookup is a chain of catalogues rather than one.
    #[must_use]
    pub fn tries_for_kind(&self, node: &FlowNode, kind: Option<&NodeKind>) -> u32 {
        if Self::kind_is_unsafe_to_replay(kind) {
            return 1;
        }

        for id in ids(node, kind) {
            if let Some(&tries) = self.per_kind.get(&id) {
                return tries.max(1);
            }
        }

        self.tries.max(1)
    }

    /// [`backoff_ms_for`](Self::backoff_ms_for), given the kind already
    /// resolved.
    #[must_use]
    pub fn backoff_ms_for_kind(&self, kind: Option<&NodeKind>) -> u64 {
        // Nothing to back off from: the node gets one attempt.
        if Self::kind_is_unsafe_to_replay(kind) {
            return 0;
        }
        self.backoff_ms
    }

    fn kind_is_unsafe_to_replay(kind: Option<&NodeKind>) -> bool {
        kind.is_some_and(|kind| kind.side_effects.as_deref() == Some(UNSAFE_TO_REPLAY))
    }

    fn kind_in<'k>(node: &FlowNode, kinds: &'k NodeKindRegistry) -> Option<&'k NodeKind> {
        node.kind.as_deref().and_then(|name| kinds.get(name))
    }
}

/// Every id a per-kind override for this node could be keyed under.
///
/// Canonical ids are namespaced while a host almost certainly writes the bare
/// one; keying on only the literal string would make the override silently stop
/// applying the day a kind is renamed.
fn ids(node: &FlowNode, kind: Option<&NodeKind>) -> Vec<String> {
    let Some(name) = node.kind.as_deref() else {
        return Vec::new();
    };

    let mut ordered: Vec<String> = alloc::vec![name.to_string()];
    for id in kind.map(NodeKind::ids).unwrap_or_default() {
        dedup_push(&mut ordered, id);
    }
    for variant in kind_id::variants(name) {
        dedup_push(&mut ordered, variant);
    }
    ordered
}
