//! `flow/graph-runs` — the 23 golden whole-graph cases, from the shared table.
//!
//! **Loaded through a runner. Rows are never transcribed into this repo.**
//! `satisfiesRange` was asserted against a hand-copied duplicate until someone
//! added a row to one copy and nothing reported it; these 23 goldens spent
//! their whole life as a private fixture directory copied between two runtimes
//! for the same reason. They live in `particle-academy/fancy-conformance` now,
//! and this file reads that one file.
//!
//! The run is fully specified by the suite manifest and must be reproduced
//! exactly: **lenient import, a LOCAL kind registry with the structural kinds
//! registered, and the built-in offline executors.** A runner that hands the
//! registry to `FlowRunner` instead gets `for_each`'s `item`/`done` ports from
//! the kind fallback and disagrees on a case nobody changed.

use fancy_conformance::{cases, format_summary, run_table, Language};
use fancy_json::{Map, Value};

use fancy_flow::nodes::support::ExecutorDeps;
use fancy_flow::registry::builtin;
use fancy_flow::{FlowRunner, NodeKindRegistry, RunOptions};

/// Run one case's graph exactly as the manifest specifies.
fn run_case(input: &Value) -> Result<Value, String> {
    let schema = input.get("schema").ok_or("case has no schema")?;

    // A LOCAL registry, exactly as the PHP and Python harnesses do — and it is
    // NOT handed to the runner. See the module docs.
    let mut kinds = NodeKindRegistry::new();
    builtin::register(&mut kinds, true);

    let imported = fancy_flow::import_workflow(schema, true, &kinds);

    let mut options = RunOptions::new();
    if let Some(seeds) = input.get("initialInputs").and_then(Value::as_object) {
        for (node_id, seeded) in seeds.iter() {
            let seeded = seeded.as_object().cloned().unwrap_or_default();
            options.initial_inputs.insert(node_id.into(), seeded);
        }
    }

    let deps = ExecutorDeps::default();
    let executors = builtin::executors(&deps);

    let result = FlowRunner::new()
        .run(&imported.graph, &executors, &options)
        .map_err(|cancelled| alloc_string(&cancelled.reason))?;

    // The shape the golden compares against: outputs on success, the exact
    // error on failure. Never both, never neither — which is what tells the
    // comparison which question it is asking.
    let mut out = Map::new();
    out.insert("ok", Value::Bool(result.ok));
    if result.ok {
        let mut outputs = Map::new();
        for (node_id, value) in &result.outputs {
            outputs.insert(node_id.as_str(), value.clone());
        }
        out.insert("outputs", Value::Object(outputs));
    } else {
        out.insert(
            "error",
            Value::from(result.error.unwrap_or_default().as_str()),
        );
    }
    Ok(Value::Object(out))
}

fn alloc_string(text: &str) -> String {
    String::from(text)
}

#[test]
fn the_rust_engine_reproduces_every_golden_graph_run() {
    let summary = run_table("flow/graph-runs", Language::Rust, None, |case| {
        run_case(case.input())
    })
    .expect("the shared suite must load; a missing checkout is a FAILURE, not a skip");

    // Rule 3: print it unconditionally, skips and all. A bare "3 skipped" reads
    // identically to full coverage at a glance.
    println!("{}", format_summary(&summary));

    assert!(
        summary.ok,
        "the Rust engine diverges from the shared graph goldens"
    );
    assert_eq!(summary.passed, 23, "every case must actually run");
    assert_eq!(summary.skipped, 0, "no case is skipped for Rust");
}

#[test]
fn the_suite_is_the_one_the_other_runtimes_assert() {
    // A vacuity guard. If this file ever pointed at an empty or renamed suite,
    // the test above would pass by running nothing.
    let rows = cases("flow/graph-runs", None).expect("suite loads");
    assert_eq!(rows.len(), 23);

    let ids: Vec<&str> = rows.iter().map(fancy_conformance::Case::id).collect();
    assert!(
        ids.contains(&"0005-merge-after-decision"),
        "the merge-after-decision case (#1)"
    );
    assert!(
        ids.contains(&"0023-merge-same-handle"),
        "the dead-edge clobber case"
    );
    assert!(ids.contains(&"0021-cycle"));
    assert!(ids.contains(&"0022-unknown-kind"));
}
