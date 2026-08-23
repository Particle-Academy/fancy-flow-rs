//! Topological execution of a [`FlowGraph`] — the Rust port of `runFlow`.
//!
//! Each node runs once, in a Kahn topological order. A node executes when **at
//! least one** incoming edge is active (its source port produced a value); that
//! is the fix for the merge-after-decision bug (#1) — requiring *all* incoming
//! edges to be active wrongly skipped a shared continuation after a decision
//! routed down one branch. Cycles are detected and abort the run.
//!
//! Port activation follows three conventions on an executor's result:
//!
//! 1. `{"__port": "x", "value": ...}` -> only port `x` emits.
//! 2. `{"branch": "x", "value": ...}` -> only port `x` emits (decision sugar).
//! 3. anything else -> the value is published on every declared output port.
//!
//! **These rules live in [`Walk`] and only there.** A queue driver must read
//! the activated ports back off the `node-output` events rather than
//! re-deriving them; a second copy of a routing table is the kind of duplicate
//! that agrees for a year and then disagrees on one branch.

mod walk;

pub use walk::{Outcome, Step, Walk};

use crate::error::RunAborted;
use crate::executors::ExecutorRegistry;
use crate::registry::NodeKindRegistry;
use crate::runtime::{RunOptions, RunResult};
use crate::schema::FlowGraph;

/// Runs a graph against an [`ExecutorRegistry`].
#[derive(Debug, Default)]
pub struct FlowRunner<'k> {
    /// Consulted for the declared-output-port fallback and for the visual-only
    /// categories. `None` means neither fallback applies, which is what the
    /// peer harnesses run with — see the note on `flow/graph-runs`.
    kinds: Option<&'k NodeKindRegistry>,
}

impl<'k> FlowRunner<'k> {
    /// A runner with no kind catalogue.
    #[must_use]
    pub fn new() -> Self {
        Self { kinds: None }
    }

    /// A runner that can consult a kind catalogue.
    ///
    /// **This changes behaviour, and the shared fixtures depend on which you
    /// pick.** With a catalogue, a node that declares no output ports falls
    /// back to its KIND's ports; without one it falls back to a lone `out`.
    /// `flow/graph-runs` is specified as a LOCAL registry that the runner does
    /// not see, matching the PHP and Python harnesses — populating it would
    /// give `for_each` its `item`/`done` ports and disagree on a case nobody
    /// changed.
    #[must_use]
    pub fn with_kinds(kinds: &'k NodeKindRegistry) -> Self {
        Self { kinds: Some(kinds) }
    }

    /// Execute the graph.
    ///
    /// # Errors
    ///
    /// [`RunAborted`] only when a **host signal** cancelled the run. An
    /// executor's abort is not an error here — it ends the run with `ok: false`
    /// and the reason in [`RunResult::error`], because that is also how a human
    /// gate pauses.
    pub fn run(
        &self,
        graph: &FlowGraph,
        executors: &ExecutorRegistry,
        options: &RunOptions<'_>,
    ) -> Result<RunResult, RunAborted> {
        let mut walk = Walk::start(graph, executors, self.kinds, options);

        while let Some(step) = walk.next_step() {
            let mut ctx = walk.context_for(&step);
            let outcome = match step.executor.execute(&mut ctx) {
                Ok(value) => Outcome::Value(value),
                Err(aborted) => Outcome::Aborted(aborted),
            };
            walk.resume(ctx, outcome);
        }

        walk.finish()
    }
}
