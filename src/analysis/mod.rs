//! Static analyses over a workflow graph — things decidable without running it.

pub mod graph_connectivity;

pub use graph_connectivity::{check_graph_connectivity, may_float};
