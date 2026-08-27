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
  driver and nothing else.
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

`FlowRunner::run` is the only driver today. An async driver and the per-node
durable driver must drive the **same** `Walk` — they never re-derive topology,
branching, skipping or port activation. Add behaviour to `Walk`, never to a
driver.

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

`tests/conformance.rs` and `tests/graph_runs.rs` run **four** shared tables from
`particle-academy/fancy-conformance`, through its Rust loader. **Never
transcribe rows into this repo** — `satisfiesRange` was asserted against a
hand-copied duplicate until someone added a row to one copy and nothing
reported it.

`flow/graph-runs` specifies the run precisely and it must be reproduced exactly:
**lenient import, a LOCAL kind registry with the structural kinds registered,
and the built-in offline executors — and the registry is NOT handed to
`FlowRunner`.** Handing it over gives `for_each` its `item`/`done` ports from
the kind fallback and disagrees on a case nobody changed.

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

**0.1.0 — core parity, built and green, unpublished.** 74 tests, **none
ignored**: 104 shared conformance cases across FIVE tables — `shared/expr` (20),
`shared/satisfies-range` (17), `shared/flow-run-identity` (25),
`flow/kind-declaration-surface` (19, 1 skipped) and `flow/graph-runs` (23) —
plus the invariant, policy, schema and graph-connectivity suites and three
doctests.

Counted from the run, not carried forward. The previous line said "85 across
four tables" while the fifth was not being counted at all.

**A stale local `Cargo.lock` makes the conformance suite look broken.**
`fancy-conformance` is `branch = "main"` with no version and the lock is
gitignored, so a fresh clone resolves to the latest commit — but a working copy
whose lock predates a new shared table fails with `the shared suite must load
... NotFound`. That is not a repo defect and `cargo update -p fancy-conformance`
fixes it. The trade is real though: two machines can be asserting against
different revisions of the tables that exist to make the runtimes agree.

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

**Not built, and deliberately after 0.1:** the durable layer (claims, frontier,
per-node replay through the same `Walk`, retry policy, human gates,
coordinator), and an async driver. The durable layer's shape should be informed
by the consumer's real storage model rather than guessed — its four rules in the
PHP and Python twins are all about *silent* failure modes, which are worth
porting carefully rather than quickly.
