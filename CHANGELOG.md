# Changelog

All notable changes to this crate are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

**Pre-1.0, breaking changes land in MINOR releases.** The version number is not
promising otherwise until 1.0.

## [Unreleased]

### Added

- **`for_each`'s `item` port now runs a lane once per item.** Wire `item` to a
  node and the lane reachable from it — stopping at anything reachable from
  `done` — runs once per resolved item, aggregating `{items, results, failures,
  count}` on `done`. `results` is index-aligned with `items` (`null` where an
  item's lane failed); `failures` holds `{index, item, error}` for each one
  that did.

  Until now this crate accepted the edge and IGNORED it: every downstream node
  ran ONCE against the whole collection, silently. fancy-labs' `batch-scoring`
  reference graph produced five per-item scores on the PHP twin and one
  aggregate here, and its assertion failed with "the path names nothing"
  because `results` was never produced. The TypeScript (0.78.0) and Python
  (0.27.1) runtimes implement the same lane.

  **What a consumer must DO: nothing, unless you wired `item`.** An unwired
  `item`, or `mode: "collect"`, publishes the list and its size exactly as
  before — one node, one claim, one checkpoint. If you DID wire `item` and
  relied on downstream nodes receiving the whole collection, they now receive
  one item at a time; `mode: "collect"` restores the old behaviour explicitly.

  A lane is capped at `maxItems` (default 1000, ceiling 10000); a longer list
  FAILS the run rather than iterating past the cap or truncating. A pause
  inside a lane pauses the run instead of being recorded as one item's failure.

- **`ExecutionContext::graph()` and `ExecutionContext::executors()`.** The
  graph a node runs in and the registry the run uses. A structural executor
  cannot derive a nested lane from its node and inputs alone, which is why the
  `item` port had no implementation. Additive.

- **`ExecutorRegistry::without_node_bindings()`.** What a lane runs with. Under
  the durable coordinator the context's registry is the replay's fork, with
  every node but the one running fenced BY ID — and a lane node is a node of
  the same graph, so a lane run on that registry "succeeds" with a fence marker
  for every result. The Python twin shipped exactly that in 0.27.0;
  `tests/for_each_lane.rs` runs the lane durably and fails if it recurs.

- **A node can activate a CHOSEN SUBSET of its output ports** (fancy-flow-php#18,
  reported by MOIC). The engine knew two answers — `__port` / `branch` lit
  exactly one port, anything else lit EVERY declared port — so a router matching
  two of five lanes had to drop the rest of the work or wake lanes nobody asked
  for.

  `Port::many(&["a", "c"], value)` lights those two, each carrying `value`.
  `Port::many_with(map)` gives each lit port its OWN payload. The wire shape is
  `{"__ports": ["a","c"], "value": …}` or `{"__ports": {"a": …, "c": …}}`, and
  the engine reads it directly, so a host in another language can emit it
  without the sugar. **An empty list lights nothing, deliberately** — the same
  answer an explicitly empty `outputs` already gives, and the honest one for a
  router that matched no rule.

  Per-port payloads are read by KEY PRESENCE, not by truthiness: a payload that
  is present and `null` is a payload, which is the distinction `branch` already
  had to learn. Nothing about the one-port and every-port rules changed, and a
  node that never emits `__ports` behaves exactly as before.

  Same shape, same tests, in all four runtimes — `@particle-academy/fancy-flow`,
  `fancy-flow-php`, `fancy-flow` (PyPI) and this crate. `tests/multi_port_activation.rs`
  asserts all seven rows here.

- **`flow/port-activation` (12 rows) is asserted here** (`tests/port_activation.rs`),
  the EIGHTH shared table this crate runs. It pins the `__ports` subset rule, the
  two single-port rules and the declared-port fallbacks across all four runtimes.
  Row 0303 is skipped for **node**, not for Rust: an explicitly empty `outputs`
  publishes nothing here — the three-state invariant — and
  `@particle-academy/fancy-flow` collapses it to `out`. The summary prints that
  skip on every run.

- **A durable, per-node coordinator: `fancy_flow::durable`, serial by default**
  (fancy-flow-php#17). The port of fancy-flow-py's `fancy_flow.durable`, of
  `@particle-academy/fancy-flow`'s `src/durable/`, and of fancy-flow-php's
  `per_node` driver; Rust was the one runtime with no durable layer.

  - `NodeClaimStore` (six `&self` operations) and `InMemoryClaimStore`. A claim
    is won exactly once and a lost race is a no-op; an owner re-enters its own
    claim, paused ones included; `skip` settles once and reports whether THIS
    call settled it; `release` drops a paused row.
  - `Frontier` (ready nodes and the skip cascade, restated from the engine's
    rule), `select_dispatch` with `UNLIMITED_CONCURRENCY` / `check_max_concurrent`,
    `replay_up_to` with `FENCE_PORT`, `RetryPolicy`, and the pausing human gates
    `DurableUserInput` / `DurableApproval` over shared `Submissions`
    (`NotAwaitingHuman` when an answer names a node the run is not parked on).
  - `Coordinator`: `advance()` settles the frontier's skips, delivers each skipped
    node's undelivered-edge warnings exactly once, then selects against the
    post-skip state; `run_node(node_id, owner)` claims, replays through
    `FlowRunner::run` with completed outputs resumed and every other node fenced
    by `bind_node` on a forked registry, and checkpoints the output with the
    ENGINE's ports, or pauses, skips or fails. A retryable failure leaves the
    claim CLAIMED for the same owner. `run_to_completion` defaults to
    `max(10_000, nodes + 1)` passes, returns on a pause, and reports outputs in
    graph node order. Each attempt runs under a `RunIdentity` off the claim row,
    so the step key is the same across a retry.

  **What it pins.** `max_concurrent` defaults to `1`; `0` is the whole ready
  frontier; a negative limit is refused by name where it is set. Held means
  CLAIMED or PAUSED, so a paused gate keeps its slot. Human gates fail closed:
  only an answer recorded for that node satisfies one, never a pre-filled input,
  unless the node opts in with `autoAnswerFromInput`. `unsafe-to-replay` is
  pinned to one attempt, retries are counted per kind id under every id the kind
  answers to, and backoff is reported, never slept. The replay walks past fences
  instead of aborting, so a sibling dispatched out of topological order runs
  rather than being recorded skipped.

  **The determinism choices, which the peers do not make.** `first_attempt_at` is
  stamped from an INJECTED `Clock` (i64 milliseconds) on a node's first claim and
  never refreshed on a re-claim: the retry clock is the first attempt, not the
  latest claim. A row no attempt started carries `None`, not an invented
  timestamp. Owner tokens are never random: `run_node` takes the caller's token,
  because only the caller can make one unique across workers, and
  `run_to_completion` mints `"{run_key}:{node_id}:{n}"` from a per-coordinator
  counter. Backoff is integer milliseconds (`backoff_ms`) where the peers carry
  float seconds.

  Pinned by `flow/durable-dispatch` (14/14, over this crate's own frontier and
  selection), `flow/run-diagnostics` driven through the coordinator (14/14), and a
  parity test running every `flow/graph-runs` golden durably and single-process
  (23/23 agree, 21 of them comparing outputs).

  **What you must do:** nothing. It is a new module; the engine is unchanged.

- **`import_workflow` refuses a graph containing a node that cannot take part in
  it.** The fourth and last runtime to get this rule — PHP 0.48, TypeScript
  0.64, Python 0.16 — so all four now agree on what a valid graph is.

  Two shapes, both measured against the engine first, and neither of which
  fails: a **floating node** (no inbound and no outbound edge — not skipped, it
  is a root, so it runs disconnected), and an **edge whose source is a terminal
  node** (`output`, `log` — the downstream node runs anyway with an empty
  input).

  What may float: a `note` across every id it answers to, any kind categorised
  `annotation` or `layout` (a swimlane is never wired — that is what a lane IS),
  and any kind the registry does not know. The last is not a loophole: an
  unknown kind already has its own issue, and we cannot know whether it is a
  step, an annotation or a lane.

  New: `fancy_flow::analysis::{check_graph_connectivity, may_float}`.

- **A graph that runs and delivers nothing now says so: two run-time `warn`
  log events** (fancy-flow#17). fancy-flow-php already emitted both; this crate,
  Node and Python ran the identical graph, reported success and said nothing.
  Pinned on every runtime by fancy-conformance's `flow/run-diagnostics` table
  (14 rows, 8 of them deliberately silent), whose messages and `detail` objects
  this crate now reproduces exactly.

  1. **Undelivered edge.** An edge whose source COMPLETED, whose port was not
     published, and whose `sourceHandle` (default `out`) is not among the ports
     that source could POSSIBLY publish. Reported against the TARGET node, with
     detail `{ edge, source, sourceHandle }`; the message lists what the source
     did publish, names a handle that is really one of the source's output
     FIELDS, and suggests leaving `sourceHandle` off when there is one. An
     untaken branch port is possible, so ordinary branching never warns.
  2. **Route taken on an unresolved path.** `branch` (`condition`) or
     `switch_case` (`value`) holding a single whole `{{ path }}` that does not
     resolve against the node's inputs. Reported against that node, with detail
     `{ node, configKey, path, tookPort }`. A path that resolves to null is
     resolved, and is silent.

  "Possible" follows PHP's precedence: the node's declared `outputs`, then ports
  derived from config (`switch_case` cases plus `default`, `llm_router` routes
  plus `fallback`, `subflow` + `stream` in the streaming modes), then the kind's
  ports, then `out`. A runner holding no catalogue asks the built-in one, so
  `FlowRunner::new()` does not call an untaken `false` impossible.

  New: `RunEvent::log_with_detail`, `registry::possible_ports`, and
  `nodes::support::routing_diagnostics::warn_if_unresolved`.

  **What you must do:** nothing. Routing is unchanged, and the warnings are
  extra `log` events that appear only on graphs that have one of these defects.

### Changed

- **BREAKING:** Bare or unclosed routing strings in `branch.condition` and
  `switch_case.value` now abort with an actionable error. Wrap bare values in
  `{{ }}` (for example, `{{ in.data.fits }}`) and close every opening `{{`.
  Bare branch strings previously
  selected a constant route (normally `true`), regardless of the input data;
  bare switch strings selected a literal case key. Blank strings, non-string
  values, and properly closed mixed interpolation retain their existing behavior.
- The two routing fields now describe the required expression wrapping.

- **`for_each` declares `results` and `failures`**, and its description says
  what it now does. The pinned fixture set moves to `fancy-conformance`
  v0.31.0, Cargo tag and `PINNED_SUITE_VERSION` together: 0.30.0 added
  `results` and 0.31.0 `failures` to `flow/kind-declaration-surface` row 0106,
  both BREAKING, precisely so that a runtime which had not implemented
  iteration would fail there instead of reporting parity.

- **An empty output declaration means NO PORTS, from the node OR the kind — and
  a chain it cuts says so.** `activated_ports` refused an empty list coming from
  a kind and published `out` instead, so a terminal kind could never actually
  terminate and a chain ran straight through it. That refusal was defensible
  while the alternative was a SILENT cut; the owner's ruling was
  strict-but-loud, so the cut now happens and announces itself.

  `possible_ports` honours it too, and that is what makes the above safe. It had
  the SAME empty-to-`out` collapse, in the other of the two gates that shape a
  port set. Fixing only the walk would have been half a fix and the dangerous
  half: the node publishes nothing while the lookup still reports `out` as
  deliverable, leaving the undelivered-edge warning silent for exactly the edge
  that had just stopped delivering. A terminal node with nothing downstream
  stays silent — the warning is keyed on the EDGE, because a diagnostic that
  fires on correct graphs is how a real one stops being read.

- **The pinned fixture set moves to `fancy-conformance` v0.29.0**, Cargo tag and
  `PINNED_SUITE_VERSION` together. `flow/graph-runs` row 0003's golden changed
  with the ruling, and 0.29.0 declares that row's ports on the NODE — without
  which the row turned on whether the runner holds a kind CATALOGUE, which this
  crate's graph-runs harness deliberately does not and PHP's always does. The
  same document would have given two defensible answers; it now gives one.

- **The undelivered-edge rule moved out of `Walk` into
  `engine::undelivered_edges`**, one crate-private function that both the walk
  and the durable coordinator call, as TypeScript's `undeliveredEdgeWarnings`,
  PHP's `UndeliveredEdges::warnings` and Python's `undelivered_edge_warnings` do.
  The walk answers it from its own bookkeeping and the coordinator from the ports
  stored on its claim rows. **No behaviour change:** every `warn` event of every
  `flow/run-diagnostics`, `flow/graph-runs` and `flow/durable-dispatch` graph --
  with and without a catalogue at the runner -- plus hand graphs covering a
  resumed source, a duplicated declared port, an executor-emitted `node-output`
  and the field note, serialised byte-identically before and after, and
  `tests/run_diagnostics.rs` passes unchanged.
- **The conformance tests pin fancy-conformance `v0.26.0`** (was `v0.24.0`).
  0.25.0 added `flow/durable-dispatch`, run by `tests/durable_conformance.rs`, and
  0.26.0 lists this crate among its implementations. Every existing table printed
  the same counts as at `v0.24.0`: `shared/satisfies-range`
  17, `shared/expr` 26, `shared/flow-run-identity` 25,
  `flow/kind-declaration-surface` 19 (+1 documented skip), `flow/graph-runs` 23,
  `flow/run-diagnostics` 14.
- **The conformance tests pin fancy-conformance `v0.23.0`** (was `v0.22.1`),
  whose `shared/expr` 0021-0026 pin the fix above; that table is now 26 rows.
  Every other table printed the same counts as before.
- **The conformance tests pin fancy-conformance `v0.24.0`** (was `v0.23.0`),
  which adds `flow/run-diagnostics` for the warnings above, run by
  `tests/run_diagnostics.rs`. Before the warnings this crate failed its six
  warning rows. Every existing table printed the same counts as at `v0.23.0`.

### Fixed

- Pin fancy-conformance 0.32.0 and assert shared `flow/graph-runs` refusal
  rows 0024–0031, including exact errors, trimmed suggestions and unclosed
  templates (#24).

- **`import_workflow` now READS a node's declared `inputs` / `outputs`; the
  exporter already wrote them** (fancy-flow-php#20). They were dropped, so the
  one field the TypeScript exporter writes FOR the other runtimes was the one
  field this one threw away, and the fallback quietly substituted the kind's
  placeholder ports for the node's real ones. `fancy-flow-php` and `fancy-flow`
  (PyPI) had the identical gap and move in the same release. All three states
  survive a round trip, and a bare-string port is read as well as an object.

- **A template that starts with `{{` and ends with `}}` but holds more than one
  reference evaluated to null** (fancy-flow-php#16). `expr::evaluate` read
  `{{ in.text }} --- {{ user.transcript }}` as ONE path,
  `in.text }} --- {{ user.transcript`, because `whole_expression` asked only
  whether the trimmed template starts with `{{` and ends with `}}`. That path
  never resolves, so a prompt, message or document template shaped like that
  produced nothing, with every reference valid. `whole_expression` now also
  requires the inner text to contain neither `}}` nor `{{`; anything else
  interpolates each reference. `{{ a }}{{ b }}` is `"12"`. Its doc comment called
  the corner deliberate and reproducing it the point, and all four runtimes did,
  which is why no parity table saw it. fancy-flow-php 0.52.2, fancy-flow 0.70.4
  and fancy-flow (Python) 0.20.2 carry the same fix.

  **What to do:** nothing, unless something relied on such a template evaluating
  to null. It now evaluates to the interpolated string. A single expression,
  whitespace-padded or not, still returns its typed value.

### Note (no code change)

- **A stale local `Cargo.lock` can make the conformance suite look broken.**
  `fancy-conformance` is declared as `branch = "main"` with no version, and
  `Cargo.lock` is gitignored — so a fresh clone resolves to the latest commit
  and is green, but a working copy whose lock predates a NEW shared table fails
  with `the shared suite must load ... NotFound` for that table.

  That happened here with `flow/kind-declaration-surface`. It reads exactly like
  a repo defect and is not one; `cargo update -p fancy-conformance` fixes it.

  Worth writing down rather than fixing, because the alternative — committing
  the lock — would pin a library crate's dependency resolution for its
  consumers, and the floating branch is deliberate while the table set is still
  growing. The cost is that two machines can silently be asserting against
  different revisions of the shared tables, which is the same class of problem
  the shared tables exist to remove. Revisit when the suite settles.
