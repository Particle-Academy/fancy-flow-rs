//! `flow/run-diagnostics` — a graph that runs and delivers nothing must say so.
//!
//! Two warnings, and both are the same silent failure: a graph that runs,
//! reports success, and delivers nothing down one path.
//!
//! 1. **An undelivered edge.** Its source completed, and its `sourceHandle`
//!    names a port that source could never publish. The target is skipped (its
//!    only edge was the bad one) or runs with that input missing, and a correct
//!    downstream template renders empty.
//! 2. **A route taken on an unresolved path.** `branch` / `switch_case` routed
//!    on a whole `{{ path }}` that did not resolve, so the run took `false` or
//!    `default` for a reason that has nothing to do with the data.
//!
//! fancy-flow-php emitted both; this engine, Node and Python ran the identical
//! graph and said nothing (fancy-flow#17). Half the rows are silent on purpose:
//! a warning that fires on ordinary branching is noise, and noise is how a real
//! warning stops being read.
//!
//! **Loaded through the shared runner. Rows are never transcribed here.**

use fancy_conformance::{cases, format_summary, run_table, Language};
use fancy_json::{Map, Value};

use fancy_flow::nodes::support::ExecutorDeps;
use fancy_flow::registry::{builtin, possible_ports};
use fancy_flow::runtime::LogLevel;
use fancy_flow::{
    FlowEdge, FlowGraph, FlowNode, FlowRunner, NodeKindRegistry, PortDescriptor, RunEvent,
    RunOptions,
};

/// Run one case and report every `warn` log, sorted by message.
///
/// Sorted rather than in emission order because undelivered-edge warnings come
/// out as each target is reached, and runtimes may break ties between
/// same-depth nodes differently; `flow/graph-runs` owns ordering.
///
/// Lenient import, a LOCAL registry with the structural kinds, and the built-in
/// offline executors -- as `flow/graph-runs`. One difference, and it is
/// required rather than chosen: **the runner is handed the registry.** PHP's
/// runner always consults a catalogue (its executor registry falls back to the
/// default one, which is exactly this registry's contents), so its `for_each`
/// publishes `item` / `done`. Without one this crate's `for_each` publishes
/// `out`, and 0012's "Available: item, done" fails for a reason unrelated to
/// what the row tests. `graph-runs` is indifferent because it compares outputs,
/// never ports.
fn run_case(input: &Value) -> Result<Value, String> {
    let schema = input.get("schema").ok_or("case has no schema")?;

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

    let result = FlowRunner::with_kinds(&kinds)
        .run(&imported.graph, &executors, &options)
        .map_err(|cancelled| String::from(cancelled.reason.as_str()))?;

    let mut warnings: Vec<&RunEvent> = result
        .events
        .iter()
        .filter(|event| event.kind == RunEvent::LOG && event.level == Some(LogLevel::Warn))
        .collect();

    // Byte order of UTF-8 IS code-point order, which is what the table sorts by.
    warnings.sort_by(|a, b| a.message.cmp(&b.message));

    let rows = warnings
        .into_iter()
        .map(|event| {
            let mut row = Map::new();
            row.insert(
                "nodeId",
                event.node_id.as_deref().map_or(Value::Null, Value::from),
            );
            row.insert(
                "message",
                Value::from(event.message.as_deref().unwrap_or("")),
            );
            row.insert("detail", event.detail.clone().unwrap_or(Value::Null));
            Value::Object(row)
        })
        .collect();

    Ok(Value::Array(rows))
}

#[test]
fn the_rust_engine_reports_every_run_diagnostic_the_table_pins() {
    let summary = run_table("flow/run-diagnostics", Language::Rust, None, |case| {
        run_case(case.input())
    })
    .expect("the shared suite must load; a missing checkout is a FAILURE, not a skip");

    // Rule 3: print it unconditionally, skips and all.
    println!("{}", format_summary(&summary));

    assert!(
        summary.ok,
        "the Rust engine disagrees with the shared run-diagnostics table"
    );
    assert_eq!(summary.passed, 14, "every case must actually run");
    assert_eq!(summary.skipped, 0, "no case is skipped for Rust");
}

#[test]
fn the_suite_is_the_one_the_other_runtimes_assert() {
    // A vacuity guard. Half these rows expect NO warnings, so an engine that
    // never warns passes them; pointed at an empty or renamed suite, the test
    // above would pass by running nothing at all.
    let rows = cases("flow/run-diagnostics", None).expect("suite loads");
    assert_eq!(rows.len(), 14);

    let warning_rows = rows
        .iter()
        .filter(|row| {
            row.expected()
                .as_array()
                .is_some_and(|expected| !expected.is_empty())
        })
        .count();
    assert_eq!(
        warning_rows, 6,
        "six rows warn, eight are silent on purpose"
    );

    let ids: Vec<&str> = rows.iter().map(fancy_conformance::Case::id).collect();
    assert!(
        ids.contains(&"0010-inverted-switch-cases-map"),
        "the flabs graph (fancy-flow#17)"
    );
    assert!(ids.contains(&"0003-branch-condition-resolves-to-null"));
    assert!(ids.contains(&"0011-configured-third-case-is-a-real-port"));
}

// -- beyond the table -----------------------------------------------------
//
// The table runs with a catalogue handed to the runner, so it cannot see the
// answers this crate had to decide for itself.

fn warnings_of(result: &fancy_flow::RunResult) -> Vec<String> {
    result
        .events
        .iter()
        .filter(|event| event.kind == RunEvent::LOG && event.level == Some(LogLevel::Warn))
        .map(|event| event.message.clone().unwrap_or_default())
        .collect()
}

fn json(text: &str) -> Value {
    fancy_json::parse(text).expect("test JSON parses")
}

#[test]
fn a_runner_holding_no_catalogue_still_knows_an_untaken_branch_is_ordinary() {
    // `FlowRunner::new()` has no kinds to ask, and with none a `branch`
    // resolves to a lone `out` -- which would call `false` impossible and warn
    // on every branching graph run that way. The built-in catalogue is the
    // lookup of last resort.
    let graph = FlowGraph {
        nodes: vec![
            FlowNode::new("b", "branch").with_config("condition", Value::from("{{ go }}")),
            FlowNode::new("yes", "output"),
            FlowNode::new("no", "output"),
            FlowNode::new("typo", "output"),
        ],
        edges: vec![
            FlowEdge::new("e1", "b", "yes").from_port("true"),
            FlowEdge::new("e2", "b", "no").from_port("false"),
            FlowEdge::new("e3", "b", "typo").from_port("ture"),
        ],
    };

    let mut seed = Map::new();
    seed.insert("go", Value::Bool(true));
    let mut options = RunOptions::new();
    options.initial_inputs.insert("b".into(), seed);

    let deps = ExecutorDeps::default();
    let result = FlowRunner::new()
        .run(&graph, &builtin::executors(&deps), &options)
        .expect("not cancelled");

    // Exactly one, and it is the impossible handle. Asserting the bad edge as
    // well is what stops "the untaken port is silent" passing by never warning.
    let warnings = warnings_of(&result);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].starts_with("Edge e3 reads port \"ture\" from node b"),
        "{warnings:?}"
    );
}

#[test]
fn possible_ports_reads_config_for_the_kinds_that_derive_ports_from_it() {
    // The table pins `switch_case`. `llm_router` and `subflow` derive ports from
    // config too, and a wrong answer for either is a warning on a graph that
    // works.
    let mut kinds = NodeKindRegistry::new();
    builtin::register(&mut kinds, true);
    let ports = |node: &FlowNode| {
        possible_ports(node, node.kind.as_deref().and_then(|kind| kinds.get(kind)))
    };

    // A blank port is a half-typed route, not a real one.
    let declared_routes = json(r#"[{"port":"billing"},{"port":"support"},{"port":""}]"#);
    let router = FlowNode::new("r", "llm_router").with_config("routes", declared_routes);
    assert_eq!(ports(&router), ["billing", "support", "fallback"]);
    assert_eq!(
        ports(&router.clone().with_config("fallback", Value::Bool(false))),
        ["billing", "support"]
    );
    // Bare port names: the shape this crate's own offline executor reads.
    let bare = FlowNode::new("r", "llm_router").with_config("routes", json(r#"["a","b"]"#));
    assert_eq!(ports(&bare), ["a", "b", "fallback"]);
    // Nothing configured yet falls back to the kind, not to nothing.
    assert_eq!(ports(&FlowNode::new("r", "llm_router")), ["default"]);

    let streaming = FlowNode::new("s", "subflow").with_config("mode", Value::from("both"));
    assert_eq!(ports(&streaming), ["out", "stream"]);
    let plain = FlowNode::new("s", "subflow").with_config("mode", Value::from("output"));
    assert_eq!(ports(&plain), ["out"]);

    // `switch_case` routes through an OBJECT only; a list's entries can never
    // be published.
    let listed = FlowNode::new("s", "switch_case").with_config("cases", json(r#"["case_a"]"#));
    assert_eq!(ports(&listed), ["default"]);

    // The node's own declaration outranks the kind and its config...
    let declared = router.with_outputs(vec![PortDescriptor::new("only")]);
    assert_eq!(ports(&declared), ["only"]);
    // ...a terminal kind's EMPTY declaration is `out`, which is what it
    // publishes...
    assert_eq!(ports(&FlowNode::new("o", "output")), ["out"]);
    // ...and a kind nobody registered publishes exactly `out`.
    assert_eq!(
        possible_ports(&FlowNode::new("x", "@acme/widget"), None),
        ["out"]
    );
}
