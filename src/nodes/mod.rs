//! The default executors, grouped by domain.
//!
//! On the TypeScript side the built-in kinds ship *without* executors — each
//! host wires where memory, HTTP and AI actually go. Every server twin ships
//! defaults so a flow runs out of the box, while each stays overridable through
//! the same kind + executor path a custom node uses. Inject real clients via
//! [`ExecutorDeps`](support::ExecutorDeps).

pub mod ai;
pub mod data;
pub mod human;
pub mod io;
pub mod logic;
pub mod output;
pub mod structural;
pub mod support;
pub mod trigger;
