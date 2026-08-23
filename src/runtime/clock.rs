//! Time, injected — never read.
//!
//! # Why this is a trait and not a call to the system clock
//!
//! The Python twin defaults `first_attempt_at` from the wall clock and the
//! TypeScript one calls `setTimeout`. Both are correct for a web backend and
//! both are **nondeterminism** for this port's headline consumer: a workflow
//! executing inside a blockchain node must produce the same result on every
//! validator, and a node that reads the host's clock does not.
//!
//! So the engine never calls a clock. It is handed one, and a deterministic
//! host hands it block time, a counter, or [`FixedClock`].
//!
//! This is deliberate divergence **D1** in `.ai/plans/fancy-flow-rs.md`.

/// A source of "now", in milliseconds since the Unix epoch.
///
/// Milliseconds because that is what `WorkflowMetadata.createdAt` and every
/// peer runtime's timestamps already use, and an integer because a float
/// timestamp is a rounding bug waiting for a long-running workflow.
pub trait Clock {
    /// Milliseconds since the Unix epoch.
    fn now_millis(&self) -> i64;
}

/// A clock that always reports the same instant.
///
/// What a deterministic host uses: hand it the block timestamp and every node
/// in the run agrees on the time, on every validator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedClock {
    millis: i64,
}

impl FixedClock {
    /// A clock pinned to `millis`.
    #[must_use]
    pub const fn new(millis: i64) -> Self {
        Self { millis }
    }
}

impl Clock for FixedClock {
    fn now_millis(&self) -> i64 {
        self.millis
    }
}

/// The host's wall clock.
///
/// Available only with the `std` feature, and deliberately **not** a default
/// anywhere in the engine: a host that wants wall-clock time passes this
/// explicitly, so reading the clock is always a visible decision.
#[cfg(feature = "std")]
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

#[cfg(feature = "std")]
impl Clock for SystemClock {
    fn now_millis(&self) -> i64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
            })
    }
}
