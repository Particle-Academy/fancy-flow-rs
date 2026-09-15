//! Shared machinery for the built-in executors.

pub mod clients;
pub mod expr;
pub mod routing_diagnostics;

pub use clients::{
    CompletionClient, EchoCompletionClient, EchoHttpClient, EchoToolClient, EmptyVectorStore,
    ExecutorDeps, HttpClient, KeyValueStore, MemoryKeyValueStore, Notifier, RecordingNotifier,
    ToolClient, VectorStore,
};
