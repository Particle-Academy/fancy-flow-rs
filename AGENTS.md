# AGENTS.md — fancy-flow-rs

Rust runtime for `fancy-flow` workflow graphs. The framework-free twin of
`@particle-academy/fancy-flow`'s TypeScript engine, of
`particle-academy/fancy-flow-php`, and of `fancy-flow` on PyPI. `CLAUDE.md`
symlinks here.

This file describes **this crate's code**. Process rules — publishing, kit
versioning, backports, the third-party approval bar — live in the envelope's
`AGENTS.md` and are deliberately not repeated.

## What this crate is

A faithful **port**, not a redesign. Behaviour questions are settled against the
peers, in this order: `@particle-academy/fancy-flow`'s `src/runtime/run-flow.ts`
and `src/registry/*` for the contract, `fancy-flow-py` for how the most recent
port realised it, `fancy-flow-php` for how a *server* twin does.

The guarantee: **same `WorkflowSchema` JSON in, same `RunResult.outputs` out**
on Node, PHP, Python and Rust. Don't break it.

## Why this port is not just a fourth transliteration

Its consumer is not a server. The Impactium blockchain agent compiles this
engine **into a node**, and three things follow that the other three runtimes
never had to think about:

1. **Determinism is correctness.** Nothing reads a wall clock, nothing iterates
   a randomly-seeded map. The `Clock` is injected and
   `RunIdentity::first_attempt_at` is required rather than defaulted.
2. **The dependency tree is one crate** — first-party `fancy-json`, which has
   none of its own. Every crate here is audit surface in a node.
3. **A panic is an abort**, not something the caller catches. Anything walking
   untrusted structure does it iteratively.

## Architecture

- `schema/` — `FlowGraph`, `FlowNode`, `FlowEdge`, `PortDescriptor`,
  `WorkflowMetadata`, `ImportIssue`.
- `workflow.rs` — import / export / validate `WorkflowSchema` v1.
- `engine/walk.rs` — **the** graph walk (below). `engine/mod.rs` is the sync
  driver and nothing else. `engine/undelivered_edges.rs` is the one
  implementation of the undelivered-edge warning, called by the walk for every
  node it reaches and by the durable coordinator for every node its frontier
  skips.
- `durable/` — per-node checkpointed runs, serial by default: `state`
  (`NodeClaimStore`, `InMemoryClaimStore`), `frontier`, `dispatch`
  (`select_dispatch`), `replay` (`replay_up_to`, `FENCE_PORT`), `retry`,
  `human` (`DurableUserInput`, `DurableApproval`, `Submissions`) and
  `coordinator`. The port of fancy-flow-py's `fancy_flow.durable`, file for file.
- `registry/` — `NodeKindRegistry`, `NodeKind`, `ConfigField`, `kind_id`, and
  `builtin` (the authorable kinds, the structural `note` / `subgraph`, the
  declared-but-executorless `agent`, and a default executor for each one that
  executes).
- `executors.rs` — `ExecutorRegistry`; resolves node id -> kind -> `*`.
- `runtime/` — `RunEvent`, `RunOptions`, `RunResult`, `ExecutionContext`,
  `Port`, `Pause`, `AbortSignal`, `RunIdentity`, `Clock`.
- `nodes/` — the default executors by domain, plus `nodes/support/` (injectable
  client traits, offline fakes, the `{{ }}` resolver).
- `capabilities.rs` — the HOST seam: `LlmClient` and `WorkflowResolver`.
- `analysis/` — static analyses over a graph: `graph_connectivity`
  (floating nodes, edges out of a terminator), decidable without running it.
- `security.rs` — `GraphPolicy`, for a graph that arrived over the wire.
- `marketplace.rs` — node-manifest validation and `satisfies_range`.

### The engine is one walk, driven by whoever

TypeScript executors may be `async`; PHP's are synchronous; Python drives both
with a **generator**. Rust has no stable generators, so `Walk` is an explicit
state machine: `next_step()` yields the node, `resume()` is handed the outcome,
`finish()` produces the result.

Two drivers exist, and both drive the **same** `Walk`. `FlowRunner::run` walks
a whole graph. The durable `Coordinator` runs ONE node per job by calling
`FlowRunner::run` itself (`durable::replay_up_to`): completed nodes are handed
over as `resume_outputs` and republished, every other node is fenced by
`bind_node` on a forked registry and publishes only `FENCE_PORT`, and the ports
a node lit are read back off the walk's `node-output` events and stored on its
claim row. The frontier restates the walk's readiness rule to ask "what is
unblocked?", and every `flow/graph-runs` golden is run through both drivers to
check they agree. An async driver, when one exists, drives it the same way.
Nothing re-derives topology, branching, skipping or port activation. Add
behaviour to `Walk`, never to a driver.

**The fence publishes; it must never abort.** An aborting fence stops the walk
at an unrelated node that precedes the target in topological order — a sibling
still running, or merely declared in a different order from its edge — and the
coordinator reads "never ran" as "the engine skipped it". The sibling is
recorded skipped and the run reports success without it. The Python and
TypeScript twins both shipped exactly that. `tests/durable.rs`' two sibling tests
fail if the fence aborts; `flow/graph-runs` does NOT catch it, because none of
its graphs orders siblings that way.

**The durable layer reads no clock and mints nothing at random**, where every
peer does both. `first_attempt_at` comes from the coordinator's injected `Clock`,
stamped on a node's FIRST claim and never refreshed (the retry clock is the first
attempt, as fancy-flow-php's rule 4 says). `run_node` takes the owner token from
the caller — only a caller can make one unique across workers, and two workers
sharing a token would both win a claim — and `run_to_completion` mints
`"{run_key}:{node_id}:{n}"` from a per-coordinator counter.

## The invariants, and the defect each one exists to stop

Every one has a test in `tests/engine_invariants.rs` that names it.

**A node runs when ≥1 incoming edge is active, never when all are.** Requiring
all wrongly skips a merge point: a decision leaves the untaken branch's edge
dead forever, so an `every` check skips the shared continuation and halts the
run after the first branch — reporting success.

**`collect_inputs` reads only ACTIVE edges.** The other half of the same bug: a
trailing dead edge assigning unconditionally overwrites a live value on the same
handle, emptying every merge point downstream of a decision.

**`inputs`/`outputs` are three-state.** `None` = undeclared (fall back);
`Some(vec![])` = explicitly none. Rust's `Option<Vec<_>>` makes the distinction
hard to collapse; keep it that way.

**There is exactly ONE kind field.** `FlowNode.kind`, and the importer maps the
document's `kind` onto it. The TypeScript engine kept a kind in two places and
its lookup consulted one, so a kind-keyed registry never fired and nothing said
so — an unregistered kind fails closed with no outputs, which is the right
default and exactly what made the miss silent (`fancy-flow` 0.48.1). Do not add
a second place.

**Anything keyed by kind name keys on EVERY id the kind answers to.** Binding
`user_input` binds `@particle-academy/user_input` and `@fancy/user_input` too. A
durable override bound under the bare name only once walked a run straight past
the person it was meant to stop for. And convention alone cannot get you from
`llm_branch` to `llm_router` — only the kind's declared alias list does.

**An abort's reason is VERBATIM.** `RunAborted` carries one string and nothing
wraps, prefixes or reformats it, because a human gate pauses through the same
channel and the durable layer decodes an encoded payload back out. Decorating
every error including the control-flow ones broke 72 tests in the PHP twin.
Assert that a pause **decodes**; never assert on its text.

**The cycle message is `Cycle detected in flow graph — aborting.` with an EM
DASH.** The Python twin emitted an ASCII hyphen for two releases and nothing
reported it, because the shared fixture asserted a substring that stopped before
the character they disagreed on.

## Deliberate divergences

Each has a doc comment at the point of divergence.

- **D1 — `RunIdentity::first_attempt_at` is required, not defaulted.** Python
  defaults it from the wall clock. A silently-minted timestamp is a
  nondeterminism this port's consumer cannot tolerate.
- **D2 — object keys serialise sorted only via `to_string_canonical`.** The
  value tree preserves insertion order like every peer; canonical output is a
  separate writer, so a consumer that hashes a graph gets a stable form without
  the document losing its authored order.
- **D3 — one kind field, no `type` / `data.kind` split.** Above. This *removes*
  an ambiguity rather than adding one.
- **D4 — sync executors only.** No async runtime, and no `Send + Sync` bounds on
  every executor for a caller that will never await one.
- **D5 — events are buffered per node, not streamed.** `ctx.emit` appends to a
  buffer the engine drains when the node returns, which is what lets an executor
  be a plain `&self` method instead of borrowing the engine's sink. Ordering is
  preserved; only a live progress UI would notice, and that is not this
  consumer.
- **D6 — `GraphPolicy::untrusted()` fails closed**, matching `fancy-flow-py` and
  diverging from PHP, where an absent allowlist permits every kind.

## Parity is a test result, not a claim

**Seven** shared tables from `particle-academy/fancy-conformance` run through
its Rust loader: four in `tests/conformance.rs`, `flow/graph-runs` in
`tests/graph_runs.rs`, `flow/run-diagnostics` in `tests/run_diagnostics.rs`,
and `flow/durable-dispatch` in `tests/durable_conformance.rs` — which also runs
`flow/run-diagnostics` THROUGH the durable coordinator and every
`flow/graph-runs` golden durably, comparing each with the single-process run. **Never
transcribe rows into this repo** — `satisfiesRange` was asserted against a
hand-copied duplicate until someone added a row to one copy and nothing
reported it.

`flow/graph-runs` specifies the run precisely and it must be reproduced exactly:
**lenient import, a LOCAL kind registry with the structural kinds registered,
and the built-in offline executors — and the registry is NOT handed to
`FlowRunner`.** Handing it over gives `for_each` its `item`/`done` ports from
the kind fallback and disagrees on a case nobody changed.

`flow/run-diagnostics` (`tests/run_diagnostics.rs`) is the one table that DOES
hand the registry to the runner, and must: its warnings name the ports a source
published, and PHP's runner always has a catalogue, so its `for_each` publishes
`item`/`done`. Run without one, row 0012 fails on "Available: out" and nothing
else changes. The possible-ports lookup behind the undelivered-edge warning
falls back to the built-in catalogue when the runner has none; port activation
does not, and must not start to.

The durable copies follow the same two rules. The `flow/graph-runs` parity run
hands the catalogue to NEITHER driver, and `flow/run-diagnostics` through the
coordinator hands it over with `with_kinds`, because the replay's runner resolves
ports against whatever the coordinator holds. `flow/durable-dispatch` never runs
the engine: it is the manifest's simulation over `Frontier::compute` and
`select_dispatch`, the same two calls `Coordinator::advance` makes.

A missing conformance checkout is a **failure**, never a skip.

## Traps

**The offline fakes are part of the contract.** `EchoCompletionClient`'s `usage`
counts are PHP's `str_word_count` of the prompt, `EchoToolClient` returns
`{tool, args}` and nothing else, and `embed_search` returns `{query, matches}`
with no count. Three graph goldens failed on the first run because those shapes
were invented rather than ported. Goldens come from running the reference
implementation, never from what the value obviously is.

**`embed_search` reads `topK`, not `limit`.** The config key is part of the
authored document.

**`Value` implements `Drop`** (its tree is dismantled iteratively), so it cannot
be destructured by move. Use `as_array` / `into_array` / `take`.

**`fancy-json`'s parse depth cap is the guarantee, and raising it removes it.**
The reader is recursive descent. A test here raised it to 50,000 to build a deep
fixture and overflowed the stack inside the parser.

## Testing

```bash
cargo test --all-features
cargo test --release --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
cargo build --no-default-features --target thumbv7em-none-eabi
```

`FANCY_CONFORMANCE_ROOT` overrides the sibling-repo discovery when the
conformance checkout is somewhere unusual.

## Status

**0.1.0 — core parity and the durable layer, built and green, unpublished.**
150 tests, **none ignored** (145 `#[test]`s plus five doctests), at
fancy-conformance 0.25.0: 175 shared conformance rows asserted across SEVEN
tables. Single-process: `shared/expr` (26), `shared/satisfies-range` (17),
`shared/flow-run-identity` (25), `flow/kind-declaration-surface` (19, 1
skipped), `flow/graph-runs` (23) and `flow/run-diagnostics` (14). Durable:
`flow/durable-dispatch` (14), `flow/run-diagnostics` through the coordinator
(14) and `flow/graph-runs` durable-vs-single-process (23, 21 comparing
outputs). Plus 56 durable unit tests, the invariant, policy, schema and
graph-connectivity suites, one in-crate unit test and five doctests.

Counted from the run, not carried forward. The previous line said "85 across
four tables" while the fifth was not being counted at all.

**The fixture set is pinned by a git TAG, because `Cargo.lock` is not
tracked.** This is a library, so the lock stays gitignored and the dependency
line is the only pin there is: `fancy-conformance` is `tag = "v0.25.0"`, and
`tests/conformance.rs` holds the same version as `PINNED_SUITE_VERSION`. It was
`branch = "main"` until 2026-09-13, which meant a fresh clone and every CI run
resolved whatever `main` was that day — two machines could assert against
different revisions of the tables that exist to make the runtimes agree, and a
working copy whose lock predated a new table failed with `the shared suite must
load ... NotFound`.

Three tests hold it: `the_pinned_fixture_version_is_the_one_on_disk` prints
and asserts the `VERSION` the loader actually read (rule 4 of the runners
README, which honours `FANCY_CONFORMANCE_ROOT` too), and
`cargo_pulls_the_fixture_tag_this_suite_pins` fails when `Cargo.toml` names a
branch, a rev or a different tag. Its parser is plain text, since a TOML crate
would be third-party audit surface for one assertion, so
`the_manifest_parser_sees_a_branch_and_a_tag` tests the parser itself. **Moving the
pin is a deliberate commit:** change the tag and the constant together, only
after re-running every table against the new tag.

**No doc example is `ignore`d, and none may be.** The README is compiled because
a README that does not compile is one that stopped being true and nothing else
in the build would notice — and a doc comment is held to the same bar. Two
started as `ignore` fragments and were made real; `pause_for_human`'s now runs a
graph and asserts the pause DECODES, so the example proves the contract its
prose describes instead of illustrating it.

**Publish order is enforced by cargo, and verified.** `fancy-flow` declares
`fancy-json` with both a `version` and a `git` source; cargo strips the git
source on publish and keeps the version requirement, so `cargo publish` refuses
with `no matching package named fancy-json found` until `fancy-json 0.1` is live
on crates.io. The version-less git dev-dependency on `fancy-conformance` is
dropped at publish and does not gate. **fancy-json first, then fancy-flow.**

**Not built:** an async driver.

**The durable layer is built but its store contract is still a guess.** The
shape of `NodeClaimStore` is the peers' — infallible, six operations — and the
consumer's real storage model has not been seen. In particular a store cannot
report an I/O failure through the trait; decide that against the consumer's
storage rather than by adding `Result` everywhere speculatively. fancy-flow-php's
four durable rules are all about *silent* failure modes, and each has a test in
`tests/durable.rs` or `tests/durable_conformance.rs`: the engine is not
reimplemented (the sibling tests, the graph-runs parity run), ports come from
the engine's events (`the_replay_resolves_ports_against_the_coordinators_registry`),
the claim is a unique constraint (`a_claim_is_exclusive`,
`a_lost_claim_race_is_a_no_op`), and the retry clock is the first attempt
(`first_attempt_at_comes_from_the_injected_clock_and_never_moves`).
