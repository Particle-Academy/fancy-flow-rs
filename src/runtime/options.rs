//! Run inputs and outputs.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use fancy_json::{Map, Value};

use super::clock::Clock;
use super::events::RunEvent;
use super::identity::RunIdentity;

/// Cooperative cancellation — the analogue of the DOM `AbortSignal`.
///
/// The runner checks it before each node. Deliberately **not** `Sync`: this
/// engine is synchronous and single-threaded by design, so a `Cell` is honest
/// where an atomic would imply a guarantee the rest of the crate does not make.
/// A host that needs cross-thread cancellation owns its own flag and trips this
/// one from the thread that drives the run.
#[derive(Debug, Default)]
pub struct AbortSignal {
    aborted: Cell<bool>,
    reason: RefCell<Option<String>>,
}

impl AbortSignal {
    /// A signal that has not been tripped.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Trip it. The run stops before its next node.
    pub fn abort(&self, reason: Option<&str>) {
        self.aborted.set(true);
        *self.reason.borrow_mut() = Some(reason.unwrap_or("aborted").to_string());
    }

    /// Whether it has been tripped.
    #[must_use]
    pub fn aborted(&self) -> bool {
        self.aborted.get()
    }

    /// Why, if it has.
    #[must_use]
    pub fn reason(&self) -> Option<String> {
        self.reason.borrow().clone()
    }
}

/// A wall-clock budget for a run.
///
/// One field, not two, so a budget cannot be set without a clock to measure it
/// against. The Python twin reads `time.monotonic()` directly; here the clock
/// is the host's, because a run inside a blockchain node must not consult the
/// machine it happens to be executing on.
pub struct Timeout<'a> {
    /// How long the run may take, in milliseconds.
    pub budget_ms: i64,
    /// What "now" means.
    pub clock: &'a dyn Clock,
}

impl core::fmt::Debug for Timeout<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Timeout")
            .field("budget_ms", &self.budget_ms)
            .finish_non_exhaustive()
    }
}

/// Options for a single run.
#[derive(Debug, Default)]
pub struct RunOptions<'a> {
    /// Stop the run after a budget. Nothing interrupts an executor mid-call on
    /// any runtime; the budget is observed **between** nodes.
    pub timeout: Option<Timeout<'a>>,
    /// Cooperative cancellation, checked before each node.
    pub signal: Option<&'a AbortSignal>,
    /// Inputs seeded to entry nodes, keyed by node id then port.
    pub initial_inputs: BTreeMap<String, Map>,
    /// Outputs of nodes already completed in a prior run, keyed by node id.
    ///
    /// A node present here is **republished, not re-executed** — its stored
    /// value goes back onto the same ports, so downstream routing reproduces
    /// exactly what it did the first time. This is the primitive every durable
    /// driver is built on, and the reason a per-node queue driver never has to
    /// re-implement routing.
    pub resume_outputs: BTreeMap<String, Value>,
    /// How deep this run is nested. `subflow` passes `depth + 1` to the child
    /// graph it runs, so runaway recursion is reported BY NAME rather than as a
    /// stack overflow from somewhere unrelated.
    pub depth: usize,
    /// Who is running, so a writing node can derive a stable idempotency key.
    ///
    /// **Deliberately not defaulted:** a key minted per call would change on
    /// every whole-run retry, which is exactly the failure an idempotency key
    /// exists to prevent — so a host that has not supplied one gets
    /// `ctx.run() == None` and a connector that declines to write blind, rather
    /// than a plausible-looking key that double-charges.
    pub run: Option<RunIdentity>,
}

impl<'a> RunOptions<'a> {
    /// Default options: no timeout, no signal, nothing seeded.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed one entry node's inputs, builder-style.
    #[must_use]
    pub fn seed(mut self, node_id: &str, inputs: Map) -> Self {
        self.initial_inputs.insert(node_id.to_string(), inputs);
        self
    }

    /// Give the run an identity, builder-style.
    #[must_use]
    pub fn with_run(mut self, run: RunIdentity) -> Self {
        self.run = Some(run);
        self
    }

    /// Set a wall-clock budget, builder-style.
    #[must_use]
    pub fn with_timeout(mut self, budget_ms: i64, clock: &'a dyn Clock) -> Self {
        self.timeout = Some(Timeout { budget_ms, clock });
        self
    }

    /// Republish an already-completed node instead of re-executing it.
    #[must_use]
    pub fn resume(mut self, node_id: &str, output: Value) -> Self {
        self.resume_outputs.insert(node_id.to_string(), output);
        self
    }
}

/// The result of a run.
#[derive(Debug, Clone, PartialEq)]
pub struct RunResult {
    /// Whether every node that ran succeeded.
    pub ok: bool,
    /// Outputs collected per node, keyed by node id.
    pub outputs: BTreeMap<String, Value>,
    /// The first error, **verbatim**.
    ///
    /// Call [`Pause::decode`] on it before treating it as a failure: a human
    /// gate pauses through this same field.
    ///
    /// [`Pause::decode`]: super::pause::Pause::decode
    pub error: Option<String>,
    /// The whole event stream.
    ///
    /// Retained in full — the peer runtimes make it opt-in — so a caller that
    /// passed no sink can still inspect it afterwards. That is what a per-node
    /// durable driver reads activated ports from, rather than re-deriving them.
    pub events: Vec<RunEvent>,
}

impl RunResult {
    /// One node's output.
    #[must_use]
    pub fn output(&self, node_id: &str) -> Option<&Value> {
        self.outputs.get(node_id)
    }

    /// The ports a node published on, read back off the event stream.
    ///
    /// **Read them; never recompute them.** A second copy of the activation
    /// rules agrees for a year and then disagrees on one branch.
    #[must_use]
    pub fn activated_ports(&self, node_id: &str) -> Vec<&str> {
        self.events
            .iter()
            .filter(|event| {
                event.kind == RunEvent::NODE_OUTPUT && event.node_id.as_deref() == Some(node_id)
            })
            .filter_map(|event| event.port_id.as_deref())
            .collect()
    }
}
