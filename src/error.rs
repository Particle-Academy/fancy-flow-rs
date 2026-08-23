//! Every error this crate produces.
//!
//! The hierarchy is deliberately shallow, and one distinction in it is
//! load-bearing: **an executor's abort is not decorated, ever.**

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::schema::ImportIssue;

/// An executor stopped the run.
///
/// # This is not necessarily a failure
///
/// A human gate pauses by aborting with an **encoded** reason (see
/// [`Pause`](crate::runtime::Pause)), which the durable layer decodes back out
/// of the message. The runner therefore records `reason` **verbatim** and
/// decides nothing about what it meant.
///
/// Nothing may wrap, prefix, suffix or reformat it. Decorating every error
/// including the control-flow ones broke 72 tests in the PHP twin, because a
/// pause stopped decoding — and a run parked on an approval that no longer
/// decodes is a run nobody can resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunAborted {
    /// The reason, exactly as the executor gave it.
    pub reason: String,
}

impl RunAborted {
    /// Abort with a reason.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

impl fmt::Display for RunAborted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The reason and nothing else. See the type docs.
        f.write_str(&self.reason)
    }
}

/// A graph was refused by [`GraphPolicy`](crate::security::GraphPolicy).
///
/// Carries **every** issue, not the first: a caller fixing a rejected graph
/// wants the whole list, and a validator that reveals one problem per attempt
/// turns a five-minute fix into five round trips.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsafeGraph {
    /// Everything wrong with it.
    pub issues: Vec<ImportIssue>,
}

impl fmt::Display for UnsafeGraph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("The graph was refused: ")?;
        for (index, issue) in self.issues.iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            f.write_str(&issue.message)?;
        }
        Ok(())
    }
}

/// Anything this crate says no to, outside a run.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FlowError {
    /// A document could not be read as a `WorkflowSchema`.
    Import(String),
    /// A graph failed its policy.
    Unsafe(UnsafeGraph),
    /// A host contract was used wrongly.
    Contract(String),
}

impl fmt::Display for FlowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Import(message) | Self::Contract(message) => f.write_str(message),
            Self::Unsafe(inner) => inner.fmt(f),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for RunAborted {}
#[cfg(feature = "std")]
impl std::error::Error for UnsafeGraph {}
#[cfg(feature = "std")]
impl std::error::Error for FlowError {}
