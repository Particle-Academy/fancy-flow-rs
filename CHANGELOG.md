# Changelog

All notable changes to this crate are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

**Pre-1.0, breaking changes land in MINOR releases.** The version number is not
promising otherwise until 1.0.

## [Unreleased]

### Added

- **`output_shape` — the FIELDS a kind emits, not its ports.** `OutputField`,
  the `OutputShape` enum, `NodeKind::output_shape` plus `output_fields()` and
  `has_dynamic_output_shape()`; six builtins declare (`notify`, `webhook_out`,
  `for_each`, `wait`, `log`, `embed_search`) and two declare themselves
  config-dependent (`llm_call`, `user_input`).

  This crate was the FOURTH runtime found missing the surface. TypeScript had
  it, PHP and Python had just gained it, and here it did not exist — so a host
  had nothing to check `{{ in.field }}` against and would have to hand-maintain
  a table read off these executors, which is exactly what a design partner had
  been forced to do and exactly how their table drifted into refusing a
  legitimate field.

  **The config-dependent case is a MARKER here, not a closure.** `NodeKind`
  derives `Clone` and `PartialEq`, and a boxed closure is neither — so
  `OutputShape::Dynamic` says *"depends on config, ask the host"*, which is the
  same shape the other runtimes decay to after crossing a JSON manifest. Rust's
  in-memory and serialised forms therefore agree, which is one fewer thing to
  get wrong.

  Every declaration was read from THIS crate's executors and cited
  (`nodes/human.rs:81-84`, `nodes/io.rs:93-97`, `nodes/logic.rs:80-82`,
  `nodes/logic.rs:155-157`, `nodes/output.rs:39-40`, `nodes/ai.rs:165-166`).
  None was copied from the other three: two declarations agreeing is not
  evidence, which is the mechanism behind every instance of this bug.

### Deliberately still undeclared

- `branch`, `switch_case`, `output`, `transform`, `merge`, `manual_trigger`,
  `webhook_trigger`, `human_approval`, `variable`, `schedule_trigger`. They emit
  what arrived, so their shape is not knowable from the kind alone, and `None`
  is the honest answer — read as *unknown, do not refuse*, never *emits
  nothing*. `tests/output_shape.rs` asserts they stay that way.

### Changed

- The two `ignore`d doc examples now compile and run. Neither was a parity gap
  and neither was slow — they were fragments that would not compile, held to a
  weaker standard than the README in the same crate. `pause_for_human`'s example
  now runs a graph and asserts the pause DECODES, so it proves the contract its
  prose describes. **No doctest is ignored; 46 tests, none ignored.**

## [0.1.0] - 2026-08-23

### Added

- First release: the **fourth** runtime for `fancy-flow` workflow graphs, after
  TypeScript, PHP and Python. Same `WorkflowSchema` JSON in, same
  `RunResult.outputs` out.

- **The engine.** `Walk` is an explicit state machine — `next_step()` yields the
  node, `resume()` takes the outcome, `finish()` produces the result — so a
  future async driver and the per-node durable driver drive the SAME walk rather
  than a second copy of the routing rules. Kahn topological order, the three
  port-activation conventions, branch routing, dead-edge handling at merge
  points, cycle detection, resume-from-checkpoint, host cancellation, and a
  budget measured against an injected clock.

- **Both registries**, the built-in kinds and their deterministic offline
  executors, `{{ }}` resolution, `Pause`, `RunIdentity`, the capability traits,
  `GraphPolicy`, and `satisfies_range`.

- **Parity asserted, not claimed.** Four shared tables from
  `particle-academy/fancy-conformance`, loaded through its Rust loader and never
  transcribed: `flow/graph-runs` (23), `shared/flow-run-identity` (25),
  `shared/expr` (20), `shared/satisfies-range` (17).

- **One dependency**, first-party `fancy-json`, which has none of its own. A CI
  job fails the build if the tree grows.

### Notes for a consumer coming from another runtime

- **Time is injected, never read.** `Clock` is a parameter; `SystemClock` exists
  behind the `std` feature and is a default nowhere.
  `RunIdentity::first_attempt_at` is **required** rather than defaulted, unlike
  the Python twin — a silently-minted timestamp is a nondeterminism, and this
  port's consumer runs inside a blockchain node.

- **`FlowNode` has exactly one kind field.** The TypeScript side keeps a kind in
  both the xyflow `type` and `data.kind`; its lookup consulted one, so a
  kind-keyed registry silently never fired (`fancy-flow` 0.48.1). The importer
  here maps the document's `kind` onto the single field.

- **`GraphPolicy::untrusted()` fails closed** — an empty allowlist, matching
  `fancy-flow-py` and diverging from PHP, where an absent allowlist permits
  every kind.

- **Executors are synchronous** and are not required to be `Send + Sync`.

- **Events are buffered per node**, not streamed: `ctx.emit` appends to a buffer
  the engine drains when the node returns. Ordering is preserved.

### Found while building this

Running the shared tables for the first time surfaced three real defects, two of
them in peers:

- The **Python twin's cycle message** used an ASCII hyphen where PHP and
  TypeScript use an em dash. It had been that way for two releases and nothing
  reported it, because the shared fixture asserted a SUBSTRING that stopped
  before the character they disagreed on.
- `flow/graph-runs` case `0014` recorded PHP's *encoding* of an empty header map
  (`[]`) rather than its value.
- A `+02:00` timestamp row in `shared/flow-run-identity` passed this port's
  first test harness **for the wrong reason** — the harness dropped the UTC
  offset and a clock-skew clamp rescued the verdict.
