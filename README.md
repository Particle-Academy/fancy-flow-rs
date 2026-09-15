# fancy-flow

[![Fancified](art/fancified.svg)](https://particle.academy)

Rust runtime for [`fancy-flow`](https://ui.particle.academy) workflow graphs —
the framework-free twin of `@particle-academy/fancy-flow`'s TypeScript engine,
of `particle-academy/fancy-flow-php`, and of `fancy-flow` on PyPI.

> A graph an agent or human authors in `<FlowEditor>` runs **unchanged** here.
> Same `WorkflowSchema` JSON in, same `RunResult.outputs` out.

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
assert!(result.ok);
# const SCHEMA: &str = r#"{"version":1,"graph":{"nodes":[],"edges":[]}}"#;
# Ok::<(), Box<dyn std::error::Error>>(())
```

## What is different about the Rust twin

The other three runtimes are servers. This one has a named consumer that is
not: a blockchain node that needs the engine **in-process** — no sidecar, no
HTTP hop. Three consequences run through the whole crate.

### Determinism is a correctness requirement

Nothing reads a wall clock. A `Clock` is **injected**, `RunIdentity`'s
`first_attempt_at` is required rather than defaulted, and nothing iterates a
randomly-seeded hash map. A workflow executing inside a node has to produce the
same result on every validator, and a node that reads the host's clock does not.

`FixedClock` is what a deterministic host passes — hand it the block timestamp
and every node in the run agrees on the time. `SystemClock` exists behind the
`std` feature and is never a default anywhere, so reading the clock is always a
visible decision.

### One dependency, first-party, with an empty tree of its own

[`fancy-json`](https://github.com/Particle-Academy/fancy-json-rs). Rust has no
JSON in its standard library — the one thing the PHP and Python twins get free —
and every third-party crate here would be audit surface inside a node.

### Money is integer minor units

Exactly as the other three do. No float touches a value.

## Parity is a test result, not a README claim

Seven shared fixture tables from
[`particle-academy/fancy-conformance`](https://github.com/Particle-Academy/fancy-conformance),
loaded through its runner — **never transcribed into this repo**. Two of them are
also driven through the durable coordinator:

| suite | cases | what it pins |
|---|---|---|
| `flow/graph-runs` | 23 | whole-graph execution: the same document in, the same outputs out. Also run through `Coordinator::run_to_completion`, which must agree with the single-process run on every row |
| `flow/run-diagnostics` | 14 | the warnings for a graph that delivers nothing. Also run through the coordinator, where a skipped node's warning is delivered at the skip |
| `flow/durable-dispatch` | 14 | a queued run hands out one node at a time by default; a paused gate keeps its slot |
| `flow/kind-declaration-surface` | 19 (+1 skipped, with its reason) | what a node kind declares |
| `shared/flow-run-identity` | 25 | the idempotency key a retrying connector sends, and when a retry may reuse it |
| `shared/expr` | 26 | `{{ }}` dot-path resolution and branch truthiness |
| `shared/satisfies-range` | 17 | minimal semver range matching |

A divergence is a red build in whichever runtime drifted, not a support ticket
months later. Running this suite found three real bugs on its first pass,
including one that had been hiding in a peer for two releases.

## The rules the engine holds

- **A node runs when at least ONE incoming edge is active**, never when all are.
  Requiring all wrongly skips a merge point after a decision — the untaken
  branch's edge stays dead forever.
- **A dead edge never clobbers a live one** on the same handle.
- **`inputs` / `outputs` are three-state.** `None` is "no ports declared" and
  falls back; `Some(vec![])` is "explicitly no ports". Collapsing them is how a
  terminal node starts publishing.
- **Control flow is not failure.** An abort's reason travels **verbatim** — a
  human gate pauses through the same channel, and the durable layer decodes an
  encoded payload back out of it. Nothing decorates it.
- **An unregistered kind fails closed**, loudly.

## Durable runs, one node at a time

`fancy_flow::durable` checkpoints a run **per node, keyed by node id**, so it
survives a crash, a deploy or a person taking a week to approve something. It is
the port of the durable layers in the Python and TypeScript twins and of
fancy-flow-php's `per_node` driver, and it runs the graph through the **same
walk** as `FlowRunner` — completed nodes are republished, every other node is
fenced off, and the engine decides the inputs and the ports. A queue adapter
supplies transport and nothing else.

```rust
use std::rc::Rc;

use fancy_flow::durable::{Coordinator, DurableApproval, InMemoryClaimStore, Submissions};
use fancy_flow::executors::executor;
use fancy_flow::{ExecutorRegistry, FixedClock, FlowEdge, FlowGraph, FlowNode, RunIdentity};
use fancy_json::Value;

let graph = FlowGraph {
    nodes: vec![
        FlowNode::new("draft", "step"),
        FlowNode::new("approve", "human_approval"),
        FlowNode::new("send", "step"),
    ],
    edges: vec![
        FlowEdge::new("e1", "draft", "approve"),
        FlowEdge::new("e2", "approve", "send").from_port("approved"),
    ],
};

let submissions = Submissions::shared();
let mut executors = ExecutorRegistry::new();
executors
    .bind("step", executor(|ctx| Ok(Value::from(ctx.node().id.as_str()))))
    .bind("human_approval", Rc::new(DurableApproval::new(Rc::clone(&submissions))));

let store = InMemoryClaimStore::new(); // a database implements `NodeClaimStore`
let clock = FixedClock::new(1_767_225_600_000); // block time, never the wall clock
let coordinator = || {
    Coordinator::new(&graph, &executors, RunIdentity::new("invoice-7", 0), &clock)
        .with_store(&store)
};

// Serial by default, and the gate PAUSES: it is never walked past.
let parked = coordinator().run_to_completion(None);
assert_eq!(parked.pause.map(|pause| pause.node_id), Some("approve".to_string()));

// A person answers; release the parked row and drive the run on.
submissions.borrow_mut().record("approve", Value::Bool(true))?;
store.release("invoice-7", "approve");

let finished = coordinator().run_to_completion(None);
assert!(finished.ok);
assert_eq!(finished.outputs.get("send"), Some(&Value::from("send")));
# Ok::<(), Box<dyn std::error::Error>>(())
```

- **Serial is the default.** `advance()` hands out one node, and the next only
  once it has settled, in declaration order. `with_max_concurrent(n)` raises the
  cap; `UNLIMITED_CONCURRENCY` (`0`) hands out the whole ready frontier; a
  negative limit is refused where it is set. A paused gate keeps its slot.
- **A claim is a unique constraint.** Two workers racing for one node produce one
  execution and one no-op. A retry re-enters its own claim with the same owner
  token, so the idempotency key its connector sends does not change.
- **`unsafe-to-replay` gets one attempt**, whatever the `RetryPolicy` says.
  Backoff is reported, never slept.
- **Human gates fail closed.** `DurableUserInput` and `DurableApproval` pause
  because they ARE human nodes; a pre-filled input never satisfies one, only an
  answer recorded for that node does.
- **Determinism.** `first_attempt_at` is stamped from the injected `Clock` on a
  node's first claim and never moved. Owner tokens are never random:
  `run_node(node_id, owner)` takes the worker's token, and `run_to_completion`
  counts its own.

Three more shared tables pin it: `flow/durable-dispatch` (14) over this crate's
own frontier and dispatch selection, `flow/run-diagnostics` (14) driven through
the coordinator, and every `flow/graph-runs` golden (23) run durably and compared
with the single-process run.

## The kind field

`FlowNode` has exactly one: `kind`. The TypeScript side stores a kind in two
places — the xyflow `type` and `data.kind` — and its executor lookup consulted
only the first, so a registry keyed by kind silently never fired (fixed in
`fancy-flow` 0.48.1). Here the importer maps the document's `kind` onto the one
field, so there is no second place for it to hide.

## `no_std`

```toml
fancy-flow = { version = "0.1", default-features = false }
```

The `std` feature adds `SystemClock` and `std::error::Error`. The engine, the
registries, the built-in kinds and every executor work on `no_std` + `alloc`.

## Status

**0.1.0 — core parity, built and green, unpublished.** The engine, both
registries, the built-in kinds and their deterministic executors, `{{ }}`,
`Pause`, `RunIdentity`, the injected `Clock`, the capability traits,
`GraphPolicy` and `satisfies_range`.

**The durable layer is built** (claims, frontier, per-node replay, retries,
human gates, a serial-by-default coordinator), unreleased. **Not built:** an
async driver. `Walk` is an explicit state machine precisely so every driver
drives the same walk rather than a second copy of the routing rules.

## License

MIT
