//! Durable, resumable runs -- checkpoint per node, keyed by node id.
//!
//! A JSON-graph engine wants **checkpoint-per-node keyed by node id**, not
//! Temporal-style event-sourced replay. Replay exists to police arbitrary user
//! code for non-determinism; an interpreter over a declarative graph is
//! deterministic by construction. And node-id keying survives a graph being
//! edited while a run is parked on an approval, where an ordinal-keyed
//! checkpoint cannot.
//!
//! The port of `fancy_flow.durable` (Python), `src/durable/` in
//! `@particle-academy/fancy-flow`, and fancy-flow-php's `per_node` driver:
//!
//! - [`state`](NodeClaimStore): what a run remembers, and the claim contract a
//!   database implements
//! - [`Frontier`]: which nodes are unblocked, restated from the engine's own
//!   rule
//! - [`replay_up_to`]: run one node THROUGH the engine, never around it
//! - [`RetryPolicy`]: how many attempts a node gets, per node
//! - [`DurableUserInput`] / [`DurableApproval`]: gates that pause and cannot be
//!   walked past
//! - [`select_dispatch`]: how many of a run's nodes may be held at once -- ONE
//!   by default
//! - [`Coordinator`]: the two operations a queue adapter dispatches
//!
//! A queue adapter supplies transport and nothing else.
//!
//! # What is different here
//!
//! The coordinator reads no wall clock and mints nothing at random: an
//! injected [`Clock`](crate::runtime::Clock) stamps `first_attempt_at`, and owner
//! tokens are caller-supplied or counted. See [`Coordinator`]'s module docs.

mod coordinator;
mod dispatch;
mod frontier;
mod human;
mod replay;
mod retry;
mod state;

pub use coordinator::{
    Coordinator, DurableRunResult, NodeOutcome, NodeOutcomeStatus, DEFAULT_MAX_PASSES,
};
pub use dispatch::{
    check_max_concurrent, select_dispatch, DEFAULT_MAX_CONCURRENT, UNLIMITED_CONCURRENCY,
};
pub use frontier::{Frontier, FrontierResult};
pub use human::{DurableApproval, DurableUserInput, NotAwaitingHuman, Submissions};
pub use replay::{is_boundary, replay_up_to, ReplayResult, BOUNDARY, FENCE_PORT};
pub use retry::{RetryPolicy, UNSAFE_TO_REPLAY};
pub use state::{InMemoryClaimStore, NodeClaimStore, NodeRunStatus, NodeState, RunState};
