//! How many of ONE run's nodes may be held at once, and which ready nodes go
//! next.
//!
//! # Serial is the default
//!
//! A queued run hands out **one node at a time**: a node is dispatched only
//! once the node before it has settled, in the graph's own declaration order.
//! Handing out the whole ready frontier at once is something a host ASKS for,
//! with [`UNLIMITED_CONCURRENCY`] or a positive cap.
//!
//! Several nodes of one run sitting on the queue together is exactly the
//! condition the Python twin's 0.22.1 sibling-order bug needed, and "what ran,
//! in what order" has to be the same answer on every run of the same graph --
//! which a frontier racing across workers cannot give.
//!
//! # The limit
//!
//! | value | meaning |
//! |---|---|
//! | `1` (the default) | serial: one node of the run held at a time |
//! | `N >= 1` | up to N held at once |
//! | [`UNLIMITED_CONCURRENCY`] (`0`) | the whole ready frontier |
//! | negative | refused, naming `max_concurrent` |
//!
//! A negative number is refused rather than read as unlimited -- and rather
//! than cast: `-1 as usize` is a cap no run ever reaches, which is a typo that
//! silently turned a serial run parallel, the one failure this must not have.
//! The peers' other refusals (a bool, a float, a string, null) are type errors
//! here and never reach a running program.
//!
//! # Held means CLAIMED or PAUSED
//!
//! A node parked on a person keeps its slot. The coordinator does not park the
//! RUN on a pause, so without that a queue adapter calling `advance()` when
//! another job settled would hand out the gate's siblings while the person is
//! still deciding.
//!
//! The budget is measured against work ALREADY HELD, never the size of one
//! batch. Two nodes settling at once each trigger an advance on a real queue,
//! and a per-batch cap would let each dispatch its own quota.
//!
//! Pinned by `flow/durable-dispatch` in fancy-conformance, which PHP,
//! TypeScript, Python and this crate run against their own frontier and
//! selection.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::state::RunState;
use crate::error::FlowError;

/// Dispatch the whole ready frontier. Named so a host never writes a bare `0`.
pub const UNLIMITED_CONCURRENCY: i64 = 0;

/// One node of a run held at a time: what an unset `max_concurrent` means.
pub const DEFAULT_MAX_CONCURRENT: i64 = 1;

/// A validated dispatch limit: `0` is unlimited, anything else is the cap.
///
/// # Errors
///
/// [`FlowError::Contract`], naming `max_concurrent`, for a negative value.
pub fn check_max_concurrent(value: i64) -> Result<usize, FlowError> {
    if value < 0 {
        return Err(FlowError::Contract(alloc::format!(
            "max_concurrent must be a positive cap, or UNLIMITED_CONCURRENCY (0) for the whole \
             ready frontier; got {value}. A negative limit is refused rather than read as \
             unlimited."
        )));
    }
    // A cap wider than this target's `usize` admits every node the target can
    // address, which is exactly what the cap says; it is not a typo to refuse.
    Ok(usize::try_from(value).unwrap_or(usize::MAX))
}

/// The ready nodes that may be dispatched now, in the order given.
///
/// `ready` is [`FrontierResult::ready`](super::FrontierResult::ready), and this
/// never reorders it. `state` is the run's rows as they stand AFTER the
/// frontier's skips were settled; a skipped node is never held.
///
/// `max_concurrent` is a limit [`check_max_concurrent`] accepted: `0`
/// ([`UNLIMITED_CONCURRENCY`]) returns all of `ready`; otherwise the first
/// `max_concurrent - held` ids, where `held` counts CLAIMED and PAUSED rows,
/// and never fewer than none.
#[must_use]
pub fn select_dispatch(ready: &[String], state: &RunState, max_concurrent: usize) -> Vec<String> {
    if max_concurrent == 0 {
        return ready.to_vec();
    }

    let held = state
        .values()
        .filter(|entry| entry.status.is_held())
        .count();
    let room = max_concurrent.saturating_sub(held);
    ready.iter().take(room).map(ToString::to_string).collect()
}
