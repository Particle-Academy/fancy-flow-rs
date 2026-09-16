//! `flow/port-activation` — which output ports a node lights, and what each carries.
//!
//! Until fancy-flow-php#18 the engine knew two answers: `__port` / `branch` lit
//! exactly one port, and anything else lit EVERY declared port. There was no
//! way to say "these two of five", so a router that matched two lanes had to
//! drop the rest of the work or wake lanes nobody asked for. `__ports` is the
//! third answer, and this table is what keeps all four runtimes giving it
//! identically.
//!
//! The rows assert the `node-output` EVENTS, not `Walk::activated_ports`' return
//! value. That function is private in every runtime, and the events are what a
//! consumer — and the durable layer, which reads activated ports straight off
//! them — actually observes. Asserting the private function would also let this
//! file pass while the events it feeds were wrong.
//!
//! Row 0303 is skipped for **node**, not here: an explicitly empty `outputs`
//! publishes nothing in this crate, in PHP and in Python, and
//! `@particle-academy/fancy-flow` collapses it to `out`.
//!
//! **Loaded through the shared runner. Rows are never transcribed here.**

use fancy_conformance::{cases, format_summary, run_table, Language};
use fancy_json::{Map, Value};

use fancy_flow::executors::executor;
use fancy_flow::{ExecutorRegistry, FlowGraph, FlowNode, FlowRunner, PortDescriptor, RunOptions};

/// Run a one-node graph and report what the node published, in order.
///
/// The kind is `hostRouter`, which no runtime ships, and **no registry is handed
/// to the runner**. Both are deliberate: the declared-port fallback reaches for
/// the KIND's ports before falling back to `out`, so naming a builtin here would
/// quietly assert the builtin's ports instead of the rule under test.
fn run_case(input: &Value) -> Result<Value, String> {
    let declared = input.get("declaredOutputs").ok_or("case has no outputs")?;
    let result = input.get("result").cloned().unwrap_or(Value::Null);

    let mut node = FlowNode::new("r", "hostRouter");
    node.outputs = match declared {
        Value::Null => None,
        Value::Array(ids) => Some(
            ids.iter()
                .filter_map(|id| id.as_str().map(PortDescriptor::new))
                .collect(),
        ),
        _ => return Err("declaredOutputs must be a list or null".to_string()),
    };

    let graph = FlowGraph {
        nodes: vec![node],
        edges: Vec::new(),
    };

    let mut executors = ExecutorRegistry::new();
    executors.bind("hostRouter", executor(move |_ctx| Ok(result.clone())));

    let run = FlowRunner::new()
        .run(&graph, &executors, &RunOptions::new())
        .map_err(|cancelled| String::from(cancelled.reason.as_str()))?;

    // Emission order, never sorted: a map-shaped `__ports` lights its ports in
    // the order the map declares them, and row 0105 is only an assertion at all
    // because this list stays in the order the engine produced it.
    let rows = run
        .events
        .iter()
        .filter(|event| event.kind == "node-output" && event.node_id.as_deref() == Some("r"))
        .map(|event| {
            let mut row = Map::new();
            row.insert(
                "port",
                event.port_id.as_deref().map_or(Value::Null, Value::from),
            );
            row.insert("value", event.value.clone().unwrap_or(Value::Null));
            Value::Object(row)
        })
        .collect();

    Ok(Value::Array(rows))
}

#[test]
fn the_rust_engine_lights_the_ports_the_table_pins() {
    let summary = run_table("flow/port-activation", Language::Rust, None, |case| {
        run_case(case.input())
    })
    .expect("the shared suite must load; a missing checkout is a FAILURE, not a skip");

    // Rule 3: print it unconditionally, skips and all.
    println!("{}", format_summary(&summary));

    assert!(
        summary.ok,
        "the Rust engine disagrees with the shared port-activation table"
    );
    assert_eq!(summary.passed, 12, "every case must actually run");
    assert_eq!(
        summary.skipped, 0,
        "no case is skipped for Rust — 0303 is skipped for NODE only"
    );
}

#[test]
fn the_suite_is_the_one_the_other_runtimes_assert() {
    // A vacuity guard. One row expects NO ports at all, so an engine that
    // published nothing would pass it; pointed at an empty or renamed suite,
    // the test above would pass by running nothing at all.
    let rows = cases("flow/port-activation", None).expect("suite loads");
    assert_eq!(rows.len(), 12);

    let dark = rows
        .iter()
        .filter(|row| {
            row.expected()
                .as_array()
                .is_some_and(std::vec::Vec::is_empty)
        })
        .count();
    assert_eq!(
        dark, 2,
        "two rows light nothing: the empty `__ports`, and the empty `outputs` that node skips"
    );

    let ids: Vec<&str> = rows.iter().map(fancy_conformance::Case::id).collect();
    assert!(
        ids.contains(&"0101-ports-list-lights-the-named-subset"),
        "the row #18 exists for must be present"
    );
    assert!(
        ids.contains(&"0303-an-explicitly-empty-outputs-lights-nothing"),
        "the divergence row must be present, not quietly dropped"
    );
}
