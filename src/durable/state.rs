//! What a durable run remembers, and the seam a real database plugs into.
//!
//! A durable run is bookkeeping plus one hard requirement: **the claim is a
//! unique constraint, not a check.** Two workers racing for the same node must
//! produce a no-op, not a double run, and only the storage layer can promise
//! that. So [`NodeClaimStore`] is a trait with exactly the operations a driver
//! needs, and an adapter implements it over `INSERT ... ON CONFLICT DO NOTHING`
//! (or its dialect's spelling).
//!
//! [`InMemoryClaimStore`] is the reference implementation and is genuinely
//! useful: it makes the whole per-node driver testable, and it is correct for a
//! single-process durable run.
//!
//! The twin of `fancy_flow.durable.state` (Python), `src/durable/state.ts`
//! (TypeScript) and fancy-flow-php's `NodeClaims` + `workflow_run_nodes`.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use fancy_json::Value;

/// Where one node of one run has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRunStatus {
    /// A worker holds it.
    Claimed,
    /// It ran to an output, and its ports are stored.
    Completed,
    /// It will never run: every inbound edge is dead, or it is a note.
    Skipped,
    /// It ran out of attempts.
    Failed,
    /// It is parked on a person.
    Paused,
}

impl NodeRunStatus {
    /// The wire spelling, shared with every peer's store.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::Completed => "completed",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
            Self::Paused => "paused",
        }
    }

    /// A node the frontier may treat as decided.
    ///
    /// A FAILED node is settled too -- it will never publish, so its successors
    /// skip rather than wait forever.
    #[must_use]
    pub const fn is_settled(self) -> bool {
        matches!(self, Self::Completed | Self::Skipped | Self::Failed)
    }

    /// A node that occupies one of the run's dispatch slots: a worker holds it,
    /// or it is parked for a person.
    ///
    /// A paused gate keeps its slot, so a serial run hands out nothing else
    /// while the person decides.
    #[must_use]
    pub const fn is_held(self) -> bool {
        matches!(self, Self::Claimed | Self::Paused)
    }
}

/// One node's row.
///
/// `ports` are the ports the engine's own `node-output` events reported. They
/// are STORED, never recomputed: a second copy of the routing table would agree
/// for a year and then disagree on one branch.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeState {
    /// Where the node has got to.
    pub status: NodeRunStatus,
    /// The ports its output activated, in publication order.
    pub ports: Vec<String>,
    /// The checkpointed output, once COMPLETED.
    pub output: Option<Value>,
    /// Why it failed, or the encoded pause it parked on.
    pub error: Option<String>,
    /// The token of the worker holding the claim.
    pub owner: Option<String>,
    /// 1-based, incremented each time an owner re-enters its own claim. `0` for
    /// a row nobody ever claimed -- a skip the frontier settled.
    pub attempts: u32,
    /// Epoch milliseconds of the FIRST claim, from the coordinator's injected
    /// [`Clock`](crate::runtime::Clock).
    ///
    /// This is the retry CLOCK, and it must never move. It is what an
    /// idempotency window is measured from, so a store that refreshed it on each
    /// reclaim would report a retry 25 hours late as seconds old -- and a
    /// connector would reuse a key the provider forgot yesterday, creating the
    /// second charge the mechanism exists to prevent. fancy-flow-php's AGENTS.md
    /// rule 4 is the same rule.
    ///
    /// `None` for a row no attempt ever started: Python mints the wall clock
    /// here, which is exactly the silently-minted timestamp this crate refuses
    /// (D1).
    pub first_attempt_at: Option<i64>,
}

impl NodeState {
    /// A row with `status` and nothing else recorded.
    #[must_use]
    pub fn new(status: NodeRunStatus) -> Self {
        Self {
            status,
            ports: Vec::new(),
            output: None,
            error: None,
            owner: None,
            attempts: 0,
            first_attempt_at: None,
        }
    }

    /// A COMPLETED row that published `ports`, builder-style.
    #[must_use]
    pub fn completed(ports: &[&str]) -> Self {
        let mut state = Self::new(NodeRunStatus::Completed);
        state.ports = ports.iter().map(ToString::to_string).collect();
        state
    }

    /// Set the checkpointed output, builder-style.
    #[must_use]
    pub fn with_output(mut self, output: Value) -> Self {
        self.output = Some(output);
        self
    }

    /// Set the error, builder-style.
    #[must_use]
    pub fn with_error(mut self, error: &str) -> Self {
        self.error = Some(error.to_string());
        self
    }
}

/// A run's rows, keyed by node id.
///
/// Sorted, never hashed: nothing in this crate iterates a randomly-seeded map.
/// Nothing reads its ORDER either -- every rule that cares about order walks the
/// graph's own node list.
pub type RunState = BTreeMap<String, NodeState>;

/// The persistence a per-node driver needs.
///
/// Six operations. An adapter over Postgres, `SQLite` or a chain's own state
/// implements these and nothing else; every rule about WHICH node may run lives
/// in [`Frontier`](super::Frontier), which reads only [`state`](Self::state).
///
/// Every method takes `&self`: a store is shared by whoever drives the run, so
/// it owns its interior mutability. [`InMemoryClaimStore`] uses a `RefCell`
/// because this crate is `no_std` and single-threaded by design (D4).
pub trait NodeClaimStore {
    /// Take exclusive ownership of one node of one run.
    ///
    /// MUST be atomic against concurrent callers. MUST return `true` for a
    /// caller re-entering its OWN claim -- CLAIMED or PAUSED -- which is what
    /// lets a job's retry resume instead of deadlocking against the row it wrote
    /// itself. Anything else -- another owner, or a settled node -- is a lost
    /// race, and returns `false` having changed nothing.
    ///
    /// `now_millis` is stamped as `first_attempt_at` on the FIRST claim only,
    /// and never on a re-entry.
    fn claim(&self, run_key: &str, node_id: &str, owner: &str, now_millis: i64) -> bool;

    /// Every row of one run.
    fn state(&self, run_key: &str) -> RunState;

    /// Checkpoint an output and the ports the engine said it activated.
    fn complete(&self, run_key: &str, node_id: &str, output: Value, ports: Vec<String>);

    /// Settle one node as skipped.
    ///
    /// Return `true` when THIS call settled it and `false` when the node was
    /// already settled, leaving that row as it was. It is what lets the
    /// coordinator deliver a skipped node's diagnostics exactly once when two
    /// callers reach the same skip decision -- and a stale skip landing on a
    /// COMPLETED node must never erase its output.
    fn skip(&self, run_key: &str, node_id: &str) -> bool;

    /// Settle one node as failed.
    fn fail(&self, run_key: &str, node_id: &str, error: &str);

    /// Park one node on a person. `reason` is the encoded pause, verbatim.
    fn pause(&self, run_key: &str, node_id: &str, reason: &str);
}

impl<S: NodeClaimStore + ?Sized> NodeClaimStore for &S {
    fn claim(&self, run_key: &str, node_id: &str, owner: &str, now_millis: i64) -> bool {
        (**self).claim(run_key, node_id, owner, now_millis)
    }

    fn state(&self, run_key: &str) -> RunState {
        (**self).state(run_key)
    }

    fn complete(&self, run_key: &str, node_id: &str, output: Value, ports: Vec<String>) {
        (**self).complete(run_key, node_id, output, ports);
    }

    fn skip(&self, run_key: &str, node_id: &str) -> bool {
        (**self).skip(run_key, node_id)
    }

    fn fail(&self, run_key: &str, node_id: &str, error: &str) {
        (**self).fail(run_key, node_id, error);
    }

    fn pause(&self, run_key: &str, node_id: &str, reason: &str) {
        (**self).pause(run_key, node_id, reason);
    }
}

/// A correct, single-process [`NodeClaimStore`].
///
/// It is NOT durable across a restart, which is the honest limit: use it for
/// tests, for a CLI run, and for a worker that genuinely owns the whole run.
///
/// No method holds its borrow across a call out of the store, so no sequence of
/// calls through the trait can trip the `RefCell`.
#[derive(Debug, Default)]
pub struct InMemoryClaimStore {
    runs: RefCell<BTreeMap<String, RunState>>,
}

impl InMemoryClaimStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop a node's row -- a paused gate's, so a recorded answer can run it
    /// again.
    ///
    /// Not part of the trait: resuming a human gate is the host's decision and
    /// its storage's business. Provided here because the in-memory store is also
    /// what the tests resume through.
    pub fn release(&self, run_key: &str, node_id: &str) {
        if let Some(run) = self.runs.borrow_mut().get_mut(run_key) {
            run.remove(node_id);
        }
    }

    /// Apply `change` to one node's row, creating it as CLAIMED if a driver
    /// never claimed it.
    fn with_entry(&self, run_key: &str, node_id: &str, change: impl FnOnce(&mut NodeState)) {
        let mut runs = self.runs.borrow_mut();
        let entry = runs
            .entry(run_key.to_string())
            .or_default()
            .entry(node_id.to_string())
            .or_insert_with(|| NodeState::new(NodeRunStatus::Claimed));
        change(entry);
    }
}

impl NodeClaimStore for InMemoryClaimStore {
    fn claim(&self, run_key: &str, node_id: &str, owner: &str, now_millis: i64) -> bool {
        let mut runs = self.runs.borrow_mut();
        let run = runs.entry(run_key.to_string()).or_default();

        let Some(existing) = run.get_mut(node_id) else {
            let mut state = NodeState::new(NodeRunStatus::Claimed);
            state.owner = Some(owner.to_string());
            state.attempts = 1;
            state.first_attempt_at = Some(now_millis);
            run.insert(node_id.to_string(), state);
            return true;
        };

        // Re-entering our own claim is how a retry gets back in. Anything else
        // -- another owner, or a settled node -- is a lost race, and a lost race
        // is a NO-OP rather than a duplicate run.
        let own_claim = existing.status.is_held() && existing.owner.as_deref() == Some(owner);
        if !own_claim {
            return false;
        }

        existing.attempts = existing.attempts.saturating_add(1);
        existing.status = NodeRunStatus::Claimed;
        // `first_attempt_at` is deliberately untouched, and `now_millis` unread:
        // the retry clock is the FIRST attempt, not the latest claim.
        true
    }

    fn state(&self, run_key: &str) -> RunState {
        self.runs.borrow().get(run_key).cloned().unwrap_or_default()
    }

    fn complete(&self, run_key: &str, node_id: &str, output: Value, ports: Vec<String>) {
        self.with_entry(run_key, node_id, |entry| {
            entry.status = NodeRunStatus::Completed;
            entry.output = Some(output);
            entry.ports = ports;
            entry.error = None;
        });
    }

    fn skip(&self, run_key: &str, node_id: &str) -> bool {
        let already_settled = self
            .runs
            .borrow()
            .get(run_key)
            .and_then(|run| run.get(node_id))
            .is_some_and(|entry| entry.status.is_settled());
        // A settled row is a decision already made. Re-settling it would report
        // a second skip, and a stale skip landing on a COMPLETED node would erase
        // its output.
        if already_settled {
            return false;
        }

        self.with_entry(run_key, node_id, |entry| {
            entry.status = NodeRunStatus::Skipped;
            entry.ports.clear();
        });
        true
    }

    fn fail(&self, run_key: &str, node_id: &str, error: &str) {
        self.with_entry(run_key, node_id, |entry| {
            entry.status = NodeRunStatus::Failed;
            entry.error = Some(error.to_string());
            entry.ports.clear();
        });
    }

    fn pause(&self, run_key: &str, node_id: &str, reason: &str) {
        self.with_entry(run_key, node_id, |entry| {
            entry.status = NodeRunStatus::Paused;
            entry.error = Some(reason.to_string());
            entry.ports.clear();
        });
    }
}
