//! Rust runtime for `fancy-flow` workflow graphs.
//!
//! The framework-free twin of `@particle-academy/fancy-flow`'s TypeScript
//! engine, of `particle-academy/fancy-flow-php`, and of the `fancy-flow` distribution for Python.
//!
//! > A graph an agent or human authors in `<FlowEditor>` runs **unchanged**
//! > here. Same `WorkflowSchema` JSON in, same `RunResult.outputs` out.
//!
//! # What is different about the Rust twin
//!
//! The other three runtimes are servers. This one has a named consumer that is
//! not: a blockchain node that needs the engine **in-process** — no sidecar, no
//! HTTP hop. Three consequences run through the whole crate.
//!
//! 1. **Determinism is a correctness requirement.** Nothing reads a wall clock:
//!    a [`Clock`](runtime::Clock) is injected, and
//!    [`RunIdentity::first_attempt_at`](runtime::RunIdentity::first_attempt_at)
//!    is required rather than defaulted. Nothing iterates a randomly-seeded
//!    hash map.
//! 2. **The dependency tree is one crate**, first-party
//!    `fancy_json`, which has none of its own.
//! 3. **Money is integer minor units**, exactly as the other three do. No float
//!    touches a value.
//!
//! # Parity is a test result
//!
//! `tests/` runs the shared fixture tables from
//! `particle-academy/fancy-conformance` — including `flow/graph-runs`, the 23
//! golden whole-graph cases every runtime asserts. A divergence is a red build
//! in whichever runtime drifted, not a support ticket months later.

#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

/// The README's examples, compiled as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct Readme;

pub mod analysis;
pub mod capabilities;
pub mod engine;
pub mod error;
pub mod executors;
pub mod marketplace;
pub mod nodes;
pub mod registry;
pub mod runtime;
pub mod schema;
pub mod security;
pub mod workflow;

pub use engine::FlowRunner;
pub use error::{FlowError, RunAborted, UnsafeGraph};
pub use executors::{executor, Executor, ExecutorRegistry};
pub use registry::{ConfigField, NodeKind, NodeKindRegistry};
pub use runtime::{
    Clock, ExecutionContext, FixedClock, Pause, PauseSignal, Port, RunEvent, RunIdentity,
    RunOptions, RunResult,
};
pub use schema::{FlowEdge, FlowGraph, FlowNode, ImportResult, PortDescriptor};
pub use workflow::{export_workflow, import_workflow, SCHEMA_VERSION};
