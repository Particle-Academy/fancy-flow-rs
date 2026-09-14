# Changelog

All notable changes to this crate are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this crate adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

**Pre-1.0, breaking changes land in MINOR releases.** The version number is not
promising otherwise until 1.0.

## [Unreleased]

### Added

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

### Fixed

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

### Changed

- **The conformance tests pin fancy-conformance `v0.23.0`** (was `v0.22.1`),
  whose `shared/expr` 0021-0026 pin the fix above; that table is now 26 rows.
  Every other table printed the same counts as before.

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
