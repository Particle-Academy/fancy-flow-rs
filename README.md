# fancy-flow

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

Four shared fixture tables from
[`particle-academy/fancy-conformance`](https://github.com/Particle-Academy/fancy-conformance),
loaded through its runner — **never transcribed into this repo**:

| suite | cases | what it pins |
|---|---|---|
| `flow/graph-runs` | 23 | whole-graph execution: the same document in, the same outputs out |
| `shared/flow-run-identity` | 25 | the idempotency key a retrying connector sends, and when a retry may reuse it |
| `shared/expr` | 20 | `{{ }}` dot-path resolution and branch truthiness |
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

**Not built:** the durable layer (claims, frontier, per-node replay, retries,
human gates, coordinator) and an async driver. `Walk` is an explicit state
machine precisely so both can drive the same walk rather than a second copy of
the routing rules.

## License

MIT
