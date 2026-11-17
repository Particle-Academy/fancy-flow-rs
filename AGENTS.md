# Using fancy-flow (Rust) — guide for agents

How an agent installs and uses this crate in a consumer app. What the crate is,
is in [README.md](./README.md). Notes for working ON this repo's code are in
[CONTRIBUTING.md](./CONTRIBUTING.md).

## Install

**Not on crates.io yet** (0.1.0, built and unpublished). It publishes after its
one dependency, `fancy-json`, is live there. Once published:

```toml
fancy-flow = "0.1"
# or, for no_std + alloc:
fancy-flow = { version = "0.1", default-features = false }
```

- The repo is `fancy-flow-rs`; the crate is `fancy-flow`, imported as
  `fancy_flow`. Rust 1.85+.
- One dependency: first-party `fancy-json`, used to parse the document
  (`fancy_json::parse`).
- The `std` feature (default) adds `SystemClock` and `std::error::Error`.

## Find it on the Fancy MCP server

The catalog server is `https://ui.particle.academy/mcp` (streamable HTTP, no API
key; MCP id `fancy-ui`). This crate is not listed there yet; it appears once it
is published. Look for it later with `search-backend-packages`. Its twins
(`fancy-flow`, `fancy-flow-php`, `fancy-flow-py`) are listed.

## API essentials

```rust
use fancy_flow::nodes::support::ExecutorDeps;
use fancy_flow::registry::builtin;
use fancy_flow::{FlowRunner, NodeKindRegistry, RunOptions};

let document = fancy_json::parse(SCHEMA)?;

let mut kinds = NodeKindRegistry::new();
builtin::register(&mut kinds, true);

let imported = fancy_flow::import_workflow(&document, true, &kinds);
let deps = ExecutorDeps::default();               // offline, deterministic
let executors = builtin::executors(&deps);

let result = FlowRunner::new().run(&imported.graph, &executors, &RunOptions::new())?;
```

- The input is WorkflowSchema v1 JSON, as `<FlowEditor>` exports it.
- **Durable runs:** `fancy_flow::durable::Coordinator::new(&graph, &executors, RunIdentity::new(run_key, 0), &clock).with_store(&store)`,
  then `run_to_completion(None)`, or `advance()` + `run_node(node_id, owner)`
  from a queue. `InMemoryClaimStore` is the in-memory store; a database
  implements `NodeClaimStore`. Human gates: `DurableUserInput`,
  `DurableApproval`, answers recorded through `Submissions`.
- `with_max_concurrent(n)` raises the per-run cap; `UNLIMITED_CONCURRENCY`
  (`0`) hands out the whole ready frontier.
- Host capabilities: `LlmClient` and `WorkflowResolver` traits.
- `GraphPolicy` vets a graph that arrived over the wire.

## Traps

- **Inject the clock.** Nothing reads a wall clock. Pass `FixedClock` (e.g. the
  block timestamp) for determinism; `SystemClock` (feature `std`) is never a
  default. `RunIdentity::first_attempt_at` is required, not defaulted.
- **Owner tokens come from you.** `run_node(node_id, owner)` takes the worker's
  token; it must be unique across workers.
- **Sync executors only.** There is no async driver.
- **Events are buffered per node**, delivered when the node returns, not
  streamed.
- **Serial by default**: `advance()` hands out one node at a time, and a paused
  gate keeps its slot.
- **Human gates fail closed.** Only an answer recorded for that node resumes the
  run.
- `unsafe-to-replay` nodes get one attempt whatever the `RetryPolicy` says;
  backoff is reported, never slept.
- `GraphPolicy::untrusted()` starts with an empty allowlist.
- `embed_search` reads `topK`, not `limit`.
- `fancy_json::Value` implements `Drop`, so it cannot be destructured by move;
  use `as_array` / `into_array` / `take`.
- Assert that a pause decodes; never assert on an abort's text.
- The `NodeClaimStore` trait is infallible; a store cannot report an I/O failure
  through it yet.
