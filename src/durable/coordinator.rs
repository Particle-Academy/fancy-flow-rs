//! The per-node driver, with the queue left out.
//!
//! This is the whole of "how a queued run branches", minus the transport. It
//! owns two operations and nothing else:
//!
//! [`advance`](Coordinator::advance)
//! : Ask the frontier what is unblocked, settle the skip cascade, and report
//!   the node ids that may be dispatched NOW. A queue adapter dispatches one job
//!   per id, and calls `advance()` again whenever a job settles. A node settled
//!   as skipped gets no job, so its run diagnostics are delivered here instead.
//!
//!   **Serial by default.** `max_concurrent` caps how many of the run's nodes
//!   are held -- claimed by a worker, or paused for a person -- at once, and it
//!   defaults to `1`: a node goes on the queue only after the node before it
//!   has settled, in declaration order. See [`dispatch`](super::select_dispatch).
//!
//! [`run_node`](Coordinator::run_node)
//! : Claim one node, replay the graph through the real engine fenced to that
//!   node, and checkpoint the output plus the ports the ENGINE said it
//!   activated.
//!
//! Everything a queue library would add -- enqueue, retry scheduling, worker
//! lifecycle -- sits outside. That separation is the point: it makes the subtle
//! part (which node may run, and with what inputs) testable in-process, with no
//! broker, and identical under every adapter. **A queue adapter therefore
//! contains no workflow logic at all.**
//!
//! [`run_to_completion`](Coordinator::run_to_completion) drives both in one
//! process. It is a real durable runner, not a toy: with a persistent
//! [`NodeClaimStore`] it survives a crash exactly as a queued run does, because
//! the crash-resume behaviour lives in the checkpoints rather than in the loop.
//!
//! # Determinism
//!
//! Nothing here reads a wall clock and nothing is random -- the two places the
//! peers do both:
//!
//! - **`first_attempt_at` comes from an injected [`Clock`]**, read once per
//!   claim and stamped by the store on the FIRST claim only. Python and
//!   TypeScript stamp the host's wall clock.
//! - **Owner tokens are deterministic.** The peers mint a random UUID per
//!   worker. Here [`run_node`](Coordinator::run_node) takes the token from the
//!   caller -- a queue adapter already has a job id, and only the caller can
//!   make a token unique ACROSS workers, which is what makes a lost race a
//!   no-op -- and [`run_to_completion`](Coordinator::run_to_completion), the
//!   single in-process driver, mints `"{run_key}:{node_id}:{n}"` from a
//!   per-coordinator counter. Its tokens are unique within ONE coordinator; two
//!   drivers over one run must each pass their own tokens to `run_node`.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::Cell;

use fancy_json::{Map, Value};

use super::dispatch::{check_max_concurrent, select_dispatch};
use super::frontier::Frontier;
use super::replay::{is_boundary, replay_up_to, ReplayResult};
use super::retry::RetryPolicy;
use super::state::{InMemoryClaimStore, NodeClaimStore, NodeRunStatus, NodeState, RunState};
use crate::engine::undelivered_edges::{undelivered_edge_warnings, KindLookup, PublishedPorts};
use crate::error::FlowError;
use crate::executors::ExecutorRegistry;
use crate::registry::NodeKindRegistry;
use crate::runtime::{Clock, Pause, PauseSignal, RunEvent, RunIdentity, RunOptions, RunResult};
use crate::schema::{FlowEdge, FlowGraph, FlowNode};

/// The fewest passes [`Coordinator::run_to_completion`] allows by default.
/// Raised to one per node for a larger graph -- see that method.
pub const DEFAULT_MAX_PASSES: usize = 10_000;

/// What [`Coordinator::run_node`] did with one node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeOutcomeStatus {
    /// Another worker holds the claim, or the node already settled. A NO-OP.
    NotClaimed,
    /// It ran, and its output and ports are checkpointed.
    Completed,
    /// The engine found no live inbound edge for it.
    Skipped,
    /// It failed. See [`NodeOutcome::retryable`].
    Failed,
    /// It is parked on a person.
    Paused,
}

impl NodeOutcomeStatus {
    /// The peers' spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotClaimed => "not-claimed",
            Self::Completed => NodeRunStatus::Completed.as_str(),
            Self::Skipped => NodeRunStatus::Skipped.as_str(),
            Self::Failed => NodeRunStatus::Failed.as_str(),
            Self::Paused => NodeRunStatus::Paused.as_str(),
        }
    }
}

/// What happened to one node.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeOutcome {
    /// The node.
    pub node_id: String,
    /// What happened.
    pub status: NodeOutcomeStatus,
    /// The checkpointed output, when COMPLETED.
    pub output: Option<Value>,
    /// The ports the engine said the output activated, when COMPLETED.
    pub ports: Vec<String>,
    /// Why it failed, or why the engine skipped it.
    pub error: Option<String>,
    /// The pause, when PAUSED.
    pub pause: Option<PauseSignal>,
    /// 1-based attempt this execution ran as. `0` when the claim was lost.
    pub attempt: u32,
    /// `true` when this attempt failed and the policy still allows another.
    ///
    /// The claim row is left CLAIMED in that case, deliberately: a queue
    /// adapter re-dispatches the job with the SAME owner token and the retry
    /// re-enters the claim it already holds. Recording FAILED here instead
    /// would settle the node, which SKIPS everything downstream -- a run
    /// reporting a tidy finish having done half its work.
    pub retryable: bool,
}

impl NodeOutcome {
    fn new(node_id: &str, status: NodeOutcomeStatus, attempt: u32) -> Self {
        Self {
            node_id: node_id.to_string(),
            status,
            output: None,
            ports: Vec::new(),
            error: None,
            pause: None,
            attempt,
            retryable: false,
        }
    }

    /// `false` when another worker got there first. A lost race is a NO-OP.
    #[must_use]
    pub fn claimed(&self) -> bool {
        self.status != NodeOutcomeStatus::NotClaimed
    }
}

/// How a durable run stands after [`Coordinator::run_to_completion`].
#[derive(Debug, Clone, PartialEq)]
pub struct DurableRunResult {
    /// Every node settled and none failed.
    pub ok: bool,
    /// Checkpointed outputs, in the graph's own NODE order.
    pub outputs: Map,
    /// Why the run did not finish, when it did not and is not paused.
    pub error: Option<String>,
    /// The gate the run is parked on.
    pub pause: Option<PauseSignal>,
}

impl DurableRunResult {
    /// Whether the run is parked on a person.
    #[must_use]
    pub fn paused(&self) -> bool {
        self.pause.is_some()
    }
}

/// Drives a graph across per-node checkpoints.
///
/// ```
/// use fancy_flow::durable::{Coordinator, NodeClaimStore, NodeRunStatus};
/// use fancy_flow::executors::executor;
/// use fancy_flow::{ExecutorRegistry, FixedClock, FlowEdge, FlowGraph, FlowNode, RunIdentity};
///
/// let graph = FlowGraph {
///     nodes: vec![FlowNode::new("a", "step"), FlowNode::new("b", "step")],
///     edges: vec![FlowEdge::new("e", "a", "b")],
/// };
/// let mut executors = ExecutorRegistry::new();
/// executors.bind("step", executor(|ctx| Ok(ctx.node().id.as_str().into())));
///
/// let clock = FixedClock::new(1_700_000_000_000);
/// let coordinator = Coordinator::new(&graph, &executors, RunIdentity::new("run-1", 0), &clock);
///
/// // Serial: one node handed out at a time, in declaration order.
/// assert_eq!(coordinator.advance(), ["a"]);
/// assert_eq!(coordinator.advance(), ["a"], "nothing is claimed until a worker claims it");
///
/// let result = coordinator.run_to_completion(None);
/// assert!(result.ok);
/// assert_eq!(coordinator.store().state("run-1")["b"].status, NodeRunStatus::Completed);
/// ```
pub struct Coordinator<'a, S: NodeClaimStore = InMemoryClaimStore> {
    graph: &'a FlowGraph,
    executors: &'a ExecutorRegistry,
    /// The run's stable identity. Required, not defaulted: a durable run without
    /// a stable key cannot key an idempotent write, and minting one per
    /// construction would hand a retrying host a different key each time.
    run: RunIdentity,
    clock: &'a dyn Clock,
    store: S,
    initial_inputs: BTreeMap<String, Map>,
    retry: RetryPolicy,
    /// The catalogue the replay's runner resolves ports against -- the RUN's
    /// own. See [`replay_up_to`].
    kinds: Option<&'a NodeKindRegistry>,
    /// The same kind lookup the walk uses for its diagnostics: `kinds`, then the
    /// executor registry's catalogue, then the built-in one.
    kind_lookup: KindLookup<'a>,
    on_event: Option<&'a dyn Fn(&RunEvent)>,
    max_concurrent: usize,
    /// How many owner tokens `run_to_completion` has minted.
    minted: Cell<u64>,
}

impl<'a> Coordinator<'a, InMemoryClaimStore> {
    /// A serial coordinator over an in-memory store.
    ///
    /// `clock` stamps `first_attempt_at` on each node's first claim. Required:
    /// there is no wall clock to default to, and a deterministic host hands it
    /// block time.
    #[must_use]
    pub fn new(
        graph: &'a FlowGraph,
        executors: &'a ExecutorRegistry,
        run: RunIdentity,
        clock: &'a dyn Clock,
    ) -> Self {
        Self {
            graph,
            executors,
            run,
            clock,
            store: InMemoryClaimStore::new(),
            initial_inputs: BTreeMap::new(),
            retry: RetryPolicy::new(),
            kinds: None,
            kind_lookup: KindLookup::new(None, executors.kinds()),
            on_event: None,
            max_concurrent: 1,
            minted: Cell::new(0),
        }
    }
}

impl<'a, S: NodeClaimStore> Coordinator<'a, S> {
    // -- configuration ---------------------------------------------------

    /// Checkpoint into `store`. Pass `&store` to keep a handle of your own.
    #[must_use]
    pub fn with_store<T: NodeClaimStore>(self, store: T) -> Coordinator<'a, T> {
        Coordinator {
            graph: self.graph,
            executors: self.executors,
            run: self.run,
            clock: self.clock,
            store,
            initial_inputs: self.initial_inputs,
            retry: self.retry,
            kinds: self.kinds,
            kind_lookup: self.kind_lookup,
            on_event: self.on_event,
            max_concurrent: self.max_concurrent,
            minted: self.minted,
        }
    }

    /// Resolve ports and kinds against `kinds` -- the catalogue this run is
    /// configured with. It is handed to the replay's runner exactly as
    /// [`FlowRunner::with_kinds`](crate::FlowRunner::with_kinds) takes it.
    #[must_use]
    pub fn with_kinds(mut self, kinds: &'a NodeKindRegistry) -> Self {
        self.kinds = Some(kinds);
        self.kind_lookup = KindLookup::new(Some(kinds), self.executors.kinds());
        self
    }

    /// Seed entry nodes' inputs, keyed by node id then port.
    #[must_use]
    pub fn with_initial_inputs(mut self, initial_inputs: BTreeMap<String, Map>) -> Self {
        self.initial_inputs = initial_inputs;
        self
    }

    /// How many attempts each node gets.
    #[must_use]
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Where each job's events go -- only the events the job's OWN node
    /// produced, plus the run-level ones, and each skipped node's diagnostics.
    #[must_use]
    pub fn on_event(mut self, sink: &'a dyn Fn(&RunEvent)) -> Self {
        self.on_event = Some(sink);
        self
    }

    /// How many of this run's nodes may be HELD at once -- claimed by a worker,
    /// or paused for a person. `1`, the default, is serial;
    /// [`UNLIMITED_CONCURRENCY`](super::UNLIMITED_CONCURRENCY) (`0`) hands out
    /// the whole ready frontier.
    ///
    /// # Errors
    ///
    /// [`FlowError::Contract`], naming `max_concurrent`, for a negative value --
    /// refused HERE, where it is set, rather than on a worker's first advance.
    /// Under a serial default, a typo that silently turned a run parallel is the
    /// failure to avoid.
    pub fn with_max_concurrent(mut self, max_concurrent: i64) -> Result<Self, FlowError> {
        self.max_concurrent = check_max_concurrent(max_concurrent)?;
        Ok(self)
    }

    // -- reading ---------------------------------------------------------

    /// The run key.
    #[must_use]
    pub fn run_key(&self) -> &str {
        self.run.run_key()
    }

    /// The run identity every attempt's identity is derived from.
    #[must_use]
    pub fn identity(&self) -> &RunIdentity {
        &self.run
    }

    /// The store this run checkpoints into.
    #[must_use]
    pub fn store(&self) -> &S {
        &self.store
    }

    /// The dispatch cap [`advance`](Self::advance) applies. `0` is unlimited.
    #[must_use]
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    // -- the two operations ----------------------------------------------

    /// Which nodes may be dispatched right now.
    ///
    /// The ready frontier, in declaration order, cut to what the run's
    /// `max_concurrent` budget has room for. The budget is measured against
    /// nodes already HELD, never the size of one call's batch: with the default
    /// of `1`, a node claimed by a worker -- or paused for a person -- means this
    /// returns nothing at all, and whichever job settles next calls it again. A
    /// paused gate keeps its slot because a pause does not park the run here, so
    /// without it this would hand out the gate's siblings while the person is
    /// still deciding.
    ///
    /// Also settles the skip cascade, because a skip is a decision the frontier
    /// just made and a second caller must not make it again.
    ///
    /// And delivers the skipped nodes' undelivered-edge warnings, because this is
    /// the only place a skipped node is ever looked at. It never gets a job, so
    /// its warning was emitted inside OTHER jobs' replays and filtered out there.
    /// Only for the nodes THIS call settled: a second caller reaching the same
    /// skip decision must not report it twice.
    #[must_use]
    pub fn advance(&self) -> Vec<String> {
        let run_key = self.run_key();
        let mut state = self.store.state(run_key);
        let frontier = Frontier::compute(self.graph, &state);
        let settled = Frontier::settle_skips(&self.store, run_key, &frontier.skipped);
        self.deliver_skip_diagnostics(&settled, &state);

        // Held work is counted against the state AFTER the skips were written,
        // as the PHP driver counts it. A skip never takes a slot.
        if !frontier.skipped.is_empty() {
            state = self.store.state(run_key);
        }
        select_dispatch(&frontier.ready, &state, self.max_concurrent)
    }

    /// Claim, execute and checkpoint one node.
    ///
    /// The claim is taken FIRST. Two workers racing for the same node produce
    /// one execution and one no-op, and the loser learns that from the store
    /// rather than from a duplicate side effect.
    ///
    /// `owner` is the token that lets a job's own retry -- or a paused gate's
    /// re-dispatch -- re-enter its claim instead of losing the race to itself:
    /// pass the SAME token across a job's attempts, and a DIFFERENT one from
    /// every other worker. It is required rather than minted because this crate
    /// mints nothing at random, and a deterministic default could not be unique
    /// across workers -- two workers sharing a token would both "win" the claim.
    #[must_use]
    pub fn run_node(&self, node_id: &str, owner: &str) -> NodeOutcome {
        let run_key = self.run_key();
        if !self
            .store
            .claim(run_key, node_id, owner, self.clock.now_millis())
        {
            return NodeOutcome::new(node_id, NodeOutcomeStatus::NotClaimed, 0);
        }

        let state = self.store.state(run_key);
        // Per NODE, off the claim row -- not per run. This is the only place the
        // attempt and the first-attempt clock are EXACT rather than conservative,
        // and they are what a writing connector checks a provider's idempotency
        // window against. The step key does not change: `attempt` is not in it.
        let identity = match state.get(node_id) {
            Some(row) => self
                .run
                .clone()
                .with_attempt(row.attempts)
                .with_first_attempt_at(row.first_attempt_at.unwrap_or(self.run.first_attempt_at())),
            None => self.run.clone(),
        };
        let attempt = identity.attempt();

        let options = RunOptions {
            initial_inputs: self.initial_inputs.clone(),
            resume_outputs: completed_outputs(&state),
            run: Some(identity),
            ..RunOptions::default()
        };
        let replay = replay_up_to(
            self.graph,
            Some(node_id),
            self.executors,
            self.kinds,
            &options,
        )
        .unwrap_or_else(|cancelled| cancelled_replay(cancelled.reason));
        self.forward(node_id, &replay.result.events);

        let result = replay.result;
        if let Some(output) = result.outputs.get(node_id) {
            let ports = replay.ports.get(node_id).cloned().unwrap_or_default();
            self.store
                .complete(run_key, node_id, output.clone(), ports.clone());
            let mut outcome = NodeOutcome::new(node_id, NodeOutcomeStatus::Completed, attempt);
            outcome.output = Some(output.clone());
            outcome.ports = ports;
            return outcome;
        }

        // The node did not produce an output. Three reasons, and they are NOT
        // interchangeable.
        if let Some(pause) =
            Pause::decode(result.error.as_deref()).filter(|pause| pause.node_id == node_id)
        {
            self.store
                .pause(run_key, node_id, result.error.as_deref().unwrap_or(""));
            let mut outcome = NodeOutcome::new(node_id, NodeOutcomeStatus::Paused, attempt);
            outcome.pause = Some(pause);
            return outcome;
        }

        if is_boundary(result.error.as_deref()) || result.ok {
            // The replay walked the whole graph -- fences publish a dead port
            // rather than aborting -- and never ran the target. So the engine
            // decided it was unreachable: every inbound edge dead. The frontier
            // normally settles that first; honouring the engine's verdict here
            // too means the two can never disagree about a branch. It must not be
            // recorded as a failure: a FAILED node fails the run.
            self.store.skip(run_key, node_id);
            let mut outcome = NodeOutcome::new(node_id, NodeOutcomeStatus::Skipped, attempt);
            outcome.error =
                Some("the engine skipped this node: no inbound edge was active".to_string());
            return outcome;
        }

        let error = result
            .error
            .unwrap_or_else(|| alloc::format!("node {node_id} produced no output and no error"));
        let mut outcome = NodeOutcome::new(node_id, NodeOutcomeStatus::Failed, attempt);

        if attempt < self.tries_for(node_id) {
            // Leave the row CLAIMED so the same owner can re-enter it. This is
            // what fancy-flow-php does by only marking FAILED from the job's
            // `failed()` hook -- a row a worker still holds must not settle
            // mid-retry, because settling it skips everything downstream.
            outcome.error = Some(error);
            outcome.retryable = true;
            return outcome;
        }

        self.store.fail(run_key, node_id, &error);
        outcome.error = Some(error);
        outcome
    }

    // -- an in-process driver over the two ------------------------------

    /// Drive the graph here, in this process.
    ///
    /// Each pass runs what [`advance`](Self::advance) hands out, so under the
    /// default serial budget a pass is ONE node, chosen in declaration order
    /// among what is ready at that moment.
    ///
    /// Every checkpoint is written exactly as a queued run writes it, so a crash
    /// mid-loop resumes from the same place a crashed worker would.
    ///
    /// Retries honour the [`RetryPolicy`]: a node declaring `unsafe-to-replay`
    /// gets one attempt whatever the policy says, and a retry re-enters the SAME
    /// claim with the SAME owner token -- so the step key it derives is
    /// unchanged, which is what makes the retry idempotent rather than
    /// duplicative. Backoff is not slept.
    ///
    /// Nothing here sleeps, polls or waits on a person: a paused node RETURNS,
    /// and so does a failed one.
    ///
    /// `max_passes` bounds the loop. `None` allows at least one pass per node --
    /// `max(DEFAULT_MAX_PASSES, nodes + 1)`: a serial pass is ONE node, so a flat
    /// limit would stop a serial run of a larger graph short and report it as
    /// unable to progress.
    ///
    /// Owner tokens are minted here, deterministically -- see the module docs.
    #[must_use]
    pub fn run_to_completion(&self, max_passes: Option<usize>) -> DurableRunResult {
        let passes = max_passes
            .unwrap_or_else(|| default_passes(self.graph.nodes.len(), DEFAULT_MAX_PASSES));

        for _ in 0..passes {
            let ready = self.advance();
            if ready.is_empty() {
                break;
            }

            for node_id in &ready {
                let outcome = self.run_node_with_retries(node_id);
                match outcome.status {
                    // A pause parks THIS DRIVER, not just the node: continuing
                    // would run the human gate's siblings while a person is still
                    // deciding.
                    NodeOutcomeStatus::Paused => {
                        return self.stopped(None, outcome.pause);
                    }
                    NodeOutcomeStatus::Failed => return self.stopped(outcome.error, None),
                    _ => {}
                }
            }
        }

        let state = self.store.state(self.run_key());
        if !Frontier::is_complete(self.graph, &state) {
            if Frontier::has_work_in_flight(&state) {
                return self.stopped(
                    Some("the run is waiting on work held elsewhere".to_string()),
                    None,
                );
            }
            let unsettled: Vec<&str> = self
                .graph
                .nodes
                .iter()
                .filter(|node| {
                    !state
                        .get(&node.id)
                        .is_some_and(|entry| entry.status.is_settled())
                })
                .map(|node| node.id.as_str())
                .collect();
            return self.stopped(
                Some(alloc::format!(
                    "the run cannot progress; unsettled nodes: {}",
                    unsettled.join(", ")
                )),
                None,
            );
        }

        // In graph order, so the error reported is the same on every run.
        let failed = self.graph.nodes.iter().find_map(|node| {
            state
                .get(&node.id)
                .filter(|entry| entry.status == NodeRunStatus::Failed)
        });
        DurableRunResult {
            ok: failed.is_none(),
            outputs: outputs_in_graph_order(self.graph, &state),
            error: failed.map(|entry| {
                entry
                    .error
                    .clone()
                    .unwrap_or_else(|| "node failed".to_string())
            }),
            pause: None,
        }
    }

    /// Checkpointed outputs, in the graph's own node order.
    ///
    /// Ordered by the graph rather than by completion so two runs of the same
    /// workflow produce comparable output maps even when nodes finished in a
    /// different order.
    #[must_use]
    pub fn outputs(&self) -> Map {
        outputs_in_graph_order(self.graph, &self.store.state(self.run_key()))
    }

    /// Run to completion, in the shape a single-process run returns. `events`
    /// is empty: they went to the sink as each job ran.
    #[must_use]
    pub fn as_run_result(&self) -> RunResult {
        let outcome = self.run_to_completion(None);
        RunResult {
            ok: outcome.ok,
            outputs: outcome
                .outputs
                .iter()
                .map(|(node_id, value)| (node_id.to_string(), value.clone()))
                .collect(),
            error: outcome.error,
            events: Vec::new(),
        }
    }

    // -- internals -------------------------------------------------------

    /// One owner token for every attempt of this node.
    ///
    /// The token is what re-enters the claim rather than losing the race to
    /// itself -- and it is why the step key a retrying node derives is the same
    /// one its first attempt sent.
    ///
    /// Bounded by the node's tries. A store that counts attempts reaches the
    /// bound exactly as `retryable` turns false, so this changes nothing for
    /// one; a store that never reports a higher attempt would otherwise retry
    /// forever, and a hang is no better than an abort in a node.
    fn run_node_with_retries(&self, node_id: &str) -> NodeOutcome {
        let owner = self.mint_owner(node_id);
        let tries = self.tries_for(node_id);

        let mut outcome = self.run_node(node_id, &owner);
        let mut attempts = 1;
        while outcome.retryable && attempts < tries {
            outcome = self.run_node(node_id, &owner);
            attempts += 1;
        }
        outcome
    }

    /// `"{run_key}:{node_id}:{n}"`. The counter alone is unique per coordinator,
    /// so two tokens this coordinator mints never collide, whatever the ids hold.
    fn mint_owner(&self, node_id: &str) -> String {
        let next = self.minted.get().wrapping_add(1);
        self.minted.set(next);
        alloc::format!("{}:{node_id}:{next}", self.run_key())
    }

    /// How many attempts this node gets, from the policy the host configured,
    /// resolving its kind the way the walk's diagnostics do.
    fn tries_for(&self, node_id: &str) -> u32 {
        self.graph.node(node_id).map_or(1, |node| {
            self.retry
                .tries_for_kind(node, self.kind_lookup.kind_of(node))
        })
    }

    fn stopped(&self, error: Option<String>, pause: Option<PauseSignal>) -> DurableRunResult {
        DurableRunResult {
            ok: false,
            outputs: self.outputs(),
            error,
            pause,
        }
    }

    /// Send each skipped node's undelivered-edge warnings to the sink.
    ///
    /// The check is the engine's own [`undelivered_edge_warnings`], fed the
    /// durable spelling of what the walk holds at that node: the ports every
    /// COMPLETED node stored, in stored order. `state` is the snapshot the
    /// frontier decided from, and the frontier skips a node only once every
    /// source has settled, so every edge it asks about is decided -- exactly as
    /// in the walk's topological order.
    ///
    /// A skipped node is the ONLY case handled here. A target that runs, beside
    /// a live edge, gets its warning from its own job's replay, which carries its
    /// node id and is forwarded; delivering it here too would report it twice.
    fn deliver_skip_diagnostics(&self, skipped: &[String], state: &RunState) {
        let Some(sink) = self.on_event else {
            return;
        };
        if skipped.is_empty() {
            return;
        }

        let nodes_by_id: BTreeMap<&str, &FlowNode> = self
            .graph
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect();
        let rows = ClaimRows(state);

        for node_id in skipped {
            let Some(&target) = nodes_by_id.get(node_id.as_str()) else {
                continue;
            };
            let incoming: Vec<&FlowEdge> = self
                .graph
                .edges
                .iter()
                .filter(|edge| edge.target == *node_id)
                .collect();
            for warning in
                undelivered_edge_warnings(target, &incoming, &rows, &nodes_by_id, &self.kind_lookup)
            {
                sink(&warning);
            }
        }
    }

    /// Forward only the events the target node produced, plus run-level ones.
    ///
    /// A replay re-emits the whole completed prefix. Passing that through would
    /// show a consumer every node running again on every job -- the run feed
    /// would report a 20-node workflow as 200 status changes.
    fn forward(&self, node_id: &str, events: &[RunEvent]) {
        let Some(sink) = self.on_event else {
            return;
        };
        for event in events {
            if event.node_id.as_deref().is_none_or(|id| id == node_id) {
                sink(event);
            }
        }
    }
}

/// The claim rows, answering the undelivered-edge rule's questions the way the
/// walk answers them from its own bookkeeping.
struct ClaimRows<'s>(&'s RunState);

impl ClaimRows<'_> {
    fn completed_row(&self, node_id: &str) -> Option<&NodeState> {
        self.0
            .get(node_id)
            .filter(|entry| entry.status == NodeRunStatus::Completed)
    }
}

impl PublishedPorts for ClaimRows<'_> {
    fn completed(&self, node_id: &str) -> bool {
        self.completed_row(node_id).is_some()
    }

    fn published(&self, node_id: &str, port: &str) -> bool {
        self.completed_row(node_id)
            .is_some_and(|entry| entry.ports.iter().any(|stored| stored == port))
    }

    fn in_publication_order(&self, node_id: &str) -> Vec<&str> {
        let mut ports: Vec<&str> = Vec::new();
        for port in self
            .completed_row(node_id)
            .into_iter()
            .flat_map(|entry| &entry.ports)
        {
            if !ports.contains(&port.as_str()) {
                ports.push(port.as_str());
            }
        }
        ports
    }
}

/// The outputs to resume: every COMPLETED row's.
fn completed_outputs(state: &RunState) -> BTreeMap<String, Value> {
    state
        .iter()
        .filter(|(_, entry)| entry.status == NodeRunStatus::Completed)
        .map(|(node_id, entry)| (node_id.clone(), entry.output.clone().unwrap_or(Value::Null)))
        .collect()
}

fn outputs_in_graph_order(graph: &FlowGraph, state: &RunState) -> Map {
    let mut outputs = Map::new();
    for node in &graph.nodes {
        if let Some(entry) = state
            .get(&node.id)
            .filter(|entry| entry.status == NodeRunStatus::Completed)
        {
            outputs.insert(
                node.id.as_str(),
                entry.output.clone().unwrap_or(Value::Null),
            );
        }
    }
    outputs
}

/// A replay a host signal cancelled, as a failed run. The coordinator passes no
/// signal, so nothing reaches this today; it exists so that could never become a
/// panic.
fn cancelled_replay(reason: String) -> ReplayResult {
    ReplayResult {
        result: RunResult {
            ok: false,
            outputs: BTreeMap::new(),
            error: Some(reason),
            events: Vec::new(),
        },
        ports: BTreeMap::new(),
    }
}

/// `max(floor, nodes + 1)` -- one pass per node at least.
fn default_passes(node_count: usize, floor: usize) -> usize {
    floor.max(node_count.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::default_passes;

    #[test]
    fn the_default_pass_limit_is_at_least_one_pass_per_node() {
        // Python shrinks DEFAULT_MAX_PASSES with monkeypatch to prove this on a
        // four-node graph; a Rust const cannot be patched, so the rule is tested
        // with the floor as a parameter. `tests/durable.rs` pins that an
        // explicit limit is honoured exactly.
        assert_eq!(default_passes(4, 2), 5, "a floor below the node count");
        assert_eq!(
            default_passes(4, 10_000),
            10_000,
            "the floor for a small graph"
        );
        assert_eq!(
            default_passes(20_000, 10_000),
            20_001,
            "one per node beyond it"
        );
        assert_eq!(
            default_passes(usize::MAX, 10),
            usize::MAX,
            "never overflows"
        );
    }
}
