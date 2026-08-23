//! Who is running, which step this is, and how many times it has been tried.
//!
//! # Why an engine needs this at all
//!
//! A node that WRITES to somebody else's system — charge a card, send a
//! message, open a pull request — can only survive a retry if the retry carries
//! the same idempotency key the first attempt did. Otherwise the provider
//! treats the second call as a new request and the customer is charged twice.
//!
//! Both obvious fallbacks are worse than sending no key at all:
//!
//! - the **node id alone** is stable across retries, and also across RUNS — two
//!   legitimate payments share a key and the provider silently collapses the
//!   second into the first: a payment that never happened, reported as success;
//! - a **fresh random value** is unique per run, and also per ATTEMPT — a retry
//!   creates a second charge, which is the thing being avoided.
//!
//! # What actually identifies a step
//!
//! Not `(run, node)`. A node legitimately executes more than once inside one
//! run: once per subflow invocation, once per iteration of a loop an executor
//! drives itself. So a step is identified by the **path of invocations that led
//! to it**, plus an optional **occurrence** for repetition at the same level:
//!
//! ```text
//! runKey ":" segment ("/" segment)*     segment := escape(id) ["#" occurrence]
//! ```
//!
//! And the part that is easy to get backwards: **`attempt` is NOT in the key.**
//! It is carried for logging and for [`RunIdentity::is_replay_safe`], and
//! putting it in the key would restore the exact bug the key exists to prevent.
//!
//! Pinned cross-runtime by `shared/flow-run-identity` in `fancy-conformance`.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Escape one segment so the composition is injective.
///
/// `%` FIRST, or the escaping is not reversible: escaping `/` before `%` turns
/// a literal `a%2Fb` into the same text as the escaped form of `a/b`, which is
/// the collision this exists to prevent, reintroduced by its own fix.
#[must_use]
pub fn escape_segment(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('/', "%2F")
        .replace('#', "%23")
}

fn render(value: &str, occurrence: Option<u64>) -> String {
    let escaped = escape_segment(value);
    // `Some(0)` is a REAL occurrence. A truthiness check here silently
    // collapses iteration 0 into the un-iterated key.
    match occurrence {
        Some(occurrence) => alloc::format!("{escaped}#{occurrence}"),
        None => escaped,
    }
}

/// A run, a position inside it, and how many times this position was tried.
///
/// Immutable: [`descend`](RunIdentity::descend) returns a new identity rather
/// than mutating, so an executor cannot change what its siblings see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunIdentity {
    /// Stable for the whole run: same across retries, resumes, workers and hosts.
    run_key: String,
    /// Enclosing invocation segments, outermost first, ALREADY RENDERED.
    ///
    /// Empty at the top level; a subflow pushes the invoking node's id.
    path: Vec<String>,
    /// 1-based attempt of THIS logical step. Never part of the key.
    attempt: u32,
    /// Milliseconds since the epoch, of attempt 1 of this step.
    ///
    /// **Required, not defaulted** — deliberate divergence D1. The Python twin
    /// defaults it from the wall clock; a silently-minted timestamp is a
    /// nondeterminism, and this port's consumer cannot tolerate one.
    first_attempt_at: i64,
}

impl RunIdentity {
    /// A top-level identity.
    ///
    /// # Panics
    ///
    /// Never — an empty `run_key` is normalised rather than rejected, because
    /// this constructor is on the hot path of every durable resume. Use
    /// [`RunIdentity::try_new`] when the key came from a user.
    #[must_use]
    pub fn new(run_key: &str, first_attempt_at: i64) -> Self {
        Self::try_new(run_key, first_attempt_at).unwrap_or_else(|| Self {
            run_key: "run".to_string(),
            path: Vec::new(),
            attempt: 1,
            first_attempt_at,
        })
    }

    /// A top-level identity, refusing a blank run key.
    #[must_use]
    pub fn try_new(run_key: &str, first_attempt_at: i64) -> Option<Self> {
        if run_key.trim().is_empty() {
            return None;
        }
        Some(Self {
            run_key: run_key.to_string(),
            path: Vec::new(),
            attempt: 1,
            first_attempt_at,
        })
    }

    /// The run key.
    #[must_use]
    pub fn run_key(&self) -> &str {
        &self.run_key
    }

    /// The enclosing invocation segments, outermost first.
    #[must_use]
    pub fn path(&self) -> &[String] {
        &self.path
    }

    /// Which attempt of this step is running. 1-based.
    #[must_use]
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// When attempt 1 of this step happened, in epoch milliseconds.
    #[must_use]
    pub fn first_attempt_at(&self) -> i64 {
        self.first_attempt_at
    }

    /// Set the attempt number, clamped to at least 1.
    #[must_use]
    pub fn with_attempt(mut self, attempt: u32) -> Self {
        self.attempt = attempt.max(1);
        self
    }

    /// Replace the enclosing invocation path with ALREADY-RENDERED segments.
    ///
    /// The segments a durable store holds are rendered — an invocation that
    /// repeats rendered its own occurrence before it was pushed, so a segment
    /// may legitimately contain `#`. Re-escaping them through
    /// [`descend`](RunIdentity::descend) turns `dunning#2` into `dunning%232`
    /// and quietly changes every idempotency key under it.
    ///
    /// Pinned by `shared/flow-run-identity` case `0005`.
    #[must_use]
    pub fn with_rendered_path(mut self, path: Vec<String>) -> Self {
        self.path = path;
        self
    }

    /// Set when attempt 1 happened.
    ///
    /// **Never move this once set.** It is the retry clock: a store that
    /// refreshed it per attempt would report a retry 25 hours late as seconds
    /// old, and a connector would reuse a key the provider forgot yesterday.
    #[must_use]
    pub fn with_first_attempt_at(mut self, millis: i64) -> Self {
        self.first_attempt_at = millis;
        self
    }

    /// The identity a child graph runs under.
    ///
    /// A subflow pushes the invoking node's id, so a node inside the child is
    /// distinguishable from the same node inside a different invocation.
    #[must_use]
    pub fn descend(&self, node_id: &str, occurrence: Option<u64>) -> Self {
        let mut path = self.path.clone();
        path.push(render(node_id, occurrence));
        Self {
            run_key: self.run_key.clone(),
            path,
            // A child's attempt starts over: it is a different logical step.
            attempt: 1,
            first_attempt_at: self.first_attempt_at,
        }
    }

    /// The identity of one execution of one node.
    ///
    /// Stable across retries of that execution, distinct from every other
    /// execution of the same node. Pass `occurrence` when an executor runs the
    /// same node more than once at the same level (a loop body, one item of a
    /// fan-out it drives itself).
    #[must_use]
    pub fn step_key(&self, node_id: &str, occurrence: Option<u64>) -> String {
        let mut out = self.run_key.clone();
        out.push(':');
        for (index, segment) in self.path.iter().enumerate() {
            if index > 0 {
                out.push('/');
            }
            out.push_str(segment);
        }
        if !self.path.is_empty() {
            out.push('/');
        }
        out.push_str(&render(node_id, occurrence));
        out
    }

    /// Whether a retry may still reuse the first attempt's key.
    ///
    /// `window_seconds` is the provider's dedup window — Stripe's is 24 hours.
    /// Past it the caller must **refuse**, because resending the key and
    /// minting a fresh one both write twice.
    ///
    /// **Attempt 1 is always replay-safe**, however long the run was parked:
    /// nothing was sent for the provider to forget. Without that rule an
    /// implementation "helpfully" refuses the first write of every
    /// long-running approval workflow.
    #[must_use]
    pub fn is_replay_safe(&self, now_millis: i64, window_seconds: Option<i64>) -> bool {
        if self.attempt <= 1 {
            return true;
        }
        let Some(window) = window_seconds else {
            return true;
        };

        // Zero is a WINDOW, not an absent one: a provider that remembers
        // nothing must never be handed a reused key. An inclusive comparison
        // alone makes `0 <= 0` true and does exactly that, and an
        // implementation treating 0 as null turns "this provider does not
        // deduplicate" into "it deduplicates forever".
        if window <= 0 {
            return false;
        }

        // The boundary is INCLUSIVE: a retry exactly at the window is still
        // inside it. And `saturating_sub` clamps clock skew to zero, so two
        // workers a few seconds apart do not turn a legitimate retry into a
        // refusal.
        let elapsed_ms = now_millis.saturating_sub(self.first_attempt_at);
        elapsed_ms <= window.saturating_mul(1000)
    }
}
