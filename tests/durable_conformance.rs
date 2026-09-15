//! The shared tables, run through the durable layer.
//!
//! Three questions, and each is a table this crate did not write:
//!
//! 1. **`flow/durable-dispatch`** -- which nodes a queued run hands out, and
//!    when. The manifest's simulation, step for step, over this crate's OWN
//!    [`Frontier::compute`] and [`select_dispatch`] -- the same two functions
//!    `Coordinator::advance` calls. No engine runs; what is under test is the
//!    decision, not the transport.
//! 2. **`flow/run-diagnostics` through the [`Coordinator`]** -- the durable
//!    driver splits one walk into a replay per node and forwards only each job's
//!    own events, so a warning can be lost (its target never gets a job) or
//!    delivered twice. Both show up as a row failing.
//! 3. **`flow/graph-runs`, durable against single-process** -- a durable driver
//!    asks "what is unblocked?" rather than "what is next?", and the two
//!    derivations must not be able to disagree. Running every golden graph
//!    through BOTH is what makes that a test result instead of an argument.
//!
//! **Loaded through the shared runner. Rows are never transcribed here.**

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};

use fancy_conformance::{cases, format_summary, run_table, run_table_with, Language, Summary};
use fancy_json::{Map, Value};

use fancy_flow::durable::{
    check_max_concurrent, select_dispatch, Coordinator, Frontier, NodeClaimStore, NodeRunStatus,
    NodeState, RunState,
};
use fancy_flow::nodes::support::ExecutorDeps;
use fancy_flow::registry::builtin;
use fancy_flow::runtime::LogLevel;
use fancy_flow::{
    FixedClock, FlowGraph, FlowRunner, NodeKindRegistry, RunEvent, RunIdentity, RunOptions,
};

/// A LOCAL registry with the structural kinds, as every table specifies.
fn local_kinds() -> NodeKindRegistry {
    let mut kinds = NodeKindRegistry::new();
    builtin::register(&mut kinds, true);
    kinds
}

fn import(input: &Value, kinds: &NodeKindRegistry) -> Result<FlowGraph, String> {
    let schema = input.get("schema").ok_or("case has no schema")?;
    Ok(fancy_flow::import_workflow(schema, true, kinds).graph)
}

fn initial_inputs(input: &Value) -> BTreeMap<String, Map> {
    input
        .get("initialInputs")
        .and_then(Value::as_object)
        .map(|seeds| {
            seeds
                .iter()
                .map(|(node_id, seeded)| {
                    (
                        node_id.to_string(),
                        seeded.as_object().cloned().unwrap_or_default(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn strings(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn string_array(items: Vec<String>) -> Value {
    Value::Array(items.into_iter().map(Value::from).collect())
}

/// Print the summary unconditionally -- rule 3 -- then assert.
fn expect_green(summary: &Summary, expected_cases: usize) {
    println!("{}", format_summary(summary));
    assert!(
        summary.ok,
        "{} diverges from the shared table",
        summary.suite
    );
    assert_eq!(
        summary.passed, expected_cases,
        "every case must actually run; a table that shrank is a table that stopped covering"
    );
    assert_eq!(summary.skipped, 0, "no case is skipped for Rust");
}

// -- 1. flow/durable-dispatch ------------------------------------------------

/// The manifest's `dispatchTrace`, over this crate's frontier and selection.
///
/// `maxConcurrent` is handed to [`check_max_concurrent`] as the table gives it:
/// `0` is `UNLIMITED_CONCURRENCY` in this runtime too, so there is no `None`
/// translation to get wrong. Workers are FIFO and settle one at a time, as the
/// manifest specifies.
fn dispatch_trace(input: &Value) -> Result<Value, String> {
    let kinds = local_kinds();
    let graph = import(input, &kinds)?;
    let limit = input
        .get("maxConcurrent")
        .and_then(Value::as_i64)
        .ok_or("case has no integer maxConcurrent")?;
    let limit = check_max_concurrent(limit).map_err(|refused| refused.to_string())?;
    let pauses = strings(input.get("pauses"));
    let publishes = input.get("publishes").and_then(Value::as_object);

    let mut state = RunState::new();
    let mut in_flight: VecDeque<String> = VecDeque::new();
    let mut trace: Vec<String> = Vec::new();

    for _ in 0..1000 {
        let frontier = Frontier::compute(&graph, &state);
        for node_id in &frontier.skipped {
            state.insert(node_id.clone(), NodeState::new(NodeRunStatus::Skipped));
            trace.push(format!("skip {node_id}"));
        }

        for node_id in select_dispatch(&frontier.ready, &state, limit) {
            state.insert(node_id.clone(), NodeState::new(NodeRunStatus::Claimed));
            trace.push(format!("dispatch {node_id}"));
            in_flight.push_back(node_id);
        }

        let Some(node_id) = in_flight.pop_front() else {
            break;
        };

        if pauses.contains(&node_id) {
            state.insert(node_id.clone(), NodeState::new(NodeRunStatus::Paused));
            trace.push(format!("pause {node_id}"));
        } else {
            let ports = match publishes.and_then(|map| map.get(&node_id)) {
                Some(listed) => strings(Some(listed)),
                None => vec!["out".to_string()],
            };
            let mut row = NodeState::new(NodeRunStatus::Completed);
            row.ports = ports;
            state.insert(node_id.clone(), row);
            trace.push(format!("complete {node_id}"));
        }
    }

    let never: Vec<String> = graph
        .nodes
        .iter()
        .filter(|node| !state.contains_key(&node.id))
        .map(|node| node.id.clone())
        .collect();

    let mut out = Map::new();
    out.insert("trace", string_array(trace));
    out.insert("neverDispatched", string_array(never));
    Ok(Value::Object(out))
}

#[test]
fn the_frontier_and_selection_match_the_durable_dispatch_table() {
    // The rows that carry the weight: 0001 (the serial default), 0007
    // (declaration order among what is ready NOW -- breadth-first fails it),
    // 0008 / 0010 (a paused gate holds its slot, alone and under a cap), 0014 (a
    // cap is measured against held work, not one batch).
    let summary = run_table("flow/durable-dispatch", Language::Rust, None, |case| {
        dispatch_trace(case.input())
    })
    .expect("the shared suite must load; a missing checkout is a FAILURE, not a skip");

    // The vacuity floor: all fourteen rows, none skipped. A table that loaded no
    // rows has no failures either, and would read as green.
    expect_green(&summary, 14);
}

// -- 2. flow/run-diagnostics, through the Coordinator ------------------------

/// Import leniently, drive the graph node by node, report every `warn` log.
///
/// Set up exactly as `tests/run_diagnostics.rs` sets up the single-process run,
/// and the registry is handed to the coordinator for the same reason it is
/// handed to the runner there: a replay without it publishes `for_each` on
/// `out` rather than `item` / `done`, so 0012 would fail for a reason unrelated
/// to what it tests.
fn run_case_durably(id: &str, input: &Value) -> Result<Value, String> {
    let kinds = local_kinds();
    let graph = import(input, &kinds)?;
    let deps = ExecutorDeps::default();
    let executors = builtin::executors(&deps);
    let clock = FixedClock::new(0);

    let warnings: RefCell<Vec<RunEvent>> = RefCell::new(Vec::new());
    let collect = |event: &RunEvent| {
        if event.kind == RunEvent::LOG && event.level == Some(LogLevel::Warn) {
            warnings.borrow_mut().push(event.clone());
        }
    };

    let _ = Coordinator::new(&graph, &executors, RunIdentity::new(id, 0), &clock)
        .with_kinds(&kinds)
        .with_initial_inputs(initial_inputs(input))
        .on_event(&collect)
        .run_to_completion(None);

    let mut warnings = warnings.into_inner();
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
fn the_coordinator_reports_every_run_diagnostic_the_table_pins() {
    let summary = run_table("flow/run-diagnostics", Language::Rust, None, |case| {
        run_case_durably(case.id(), case.input())
    })
    .expect("the shared suite must load; a missing checkout is a FAILURE, not a skip");

    print!("[durable] ");
    expect_green(&summary, 14);
}

// -- 3. flow/graph-runs, durable against single-process ----------------------

/// Run one golden graph through BOTH drivers and say whether they agree.
///
/// Set up as `tests/graph_runs.rs` sets up the single-process run -- and, as
/// there, the registry is NOT handed to either driver: `flow/graph-runs` is
/// specified without a catalogue at the runner, and a coordinator given one
/// would route `for_each` differently from the run it is compared with.
///
/// Failure MESSAGES legitimately differ: a single run reports the first error it
/// hit walking a total order, while the durable driver reports what its frontier
/// could not resolve. Only the verdict is contractual, as in Python's parity
/// suite. On success the outputs must be equal, and equal to the golden too.
fn parity(id: &str, input: &Value, expected: &Value) -> Result<Value, String> {
    let kinds = local_kinds();
    let graph = import(input, &kinds)?;
    let deps = ExecutorDeps::default();
    let executors = builtin::executors(&deps);
    let seeds = initial_inputs(input);

    let mut options = RunOptions::new();
    options.initial_inputs = seeds.clone();
    let single = FlowRunner::new()
        .run(&graph, &executors, &options)
        .map_err(|cancelled| format!("single-process run cancelled: {}", cancelled.reason))?;

    let clock = FixedClock::new(0);
    let durable = Coordinator::new(&graph, &executors, RunIdentity::new(id, 0), &clock)
        .with_initial_inputs(seeds)
        .run_to_completion(None);

    if durable.ok != single.ok {
        return Err(format!(
            "the drivers disagree about whether the run succeeded (single ok={} error={:?}, \
             durable ok={} error={:?})",
            single.ok, single.error, durable.ok, durable.error
        ));
    }
    if expected.get("ok").and_then(Value::as_bool) != Some(single.ok) {
        return Err(format!(
            "both drivers say ok={}, the golden disagrees",
            single.ok
        ));
    }
    if !single.ok {
        return Ok(Value::Bool(true));
    }

    let single_outputs: Map = single
        .outputs
        .iter()
        .map(|(node_id, value)| (node_id.as_str(), value.clone()))
        .collect();
    let durable_outputs = Value::Object(durable.outputs);
    let single_outputs = Value::Object(single_outputs);
    if !fancy_conformance::equals(&durable_outputs, &single_outputs) {
        return Err(format!(
            "outputs differ: single={} durable={}",
            fancy_json::to_string(&single_outputs),
            fancy_json::to_string(&durable_outputs)
        ));
    }
    let golden = expected.get("outputs").cloned().unwrap_or(Value::Null);
    if !fancy_conformance::equals(&durable_outputs, &golden) {
        return Err(format!(
            "both drivers agree, and disagree with the golden: {}",
            fancy_json::to_string(&durable_outputs)
        ));
    }
    Ok(Value::Bool(true))
}

#[test]
fn the_durable_driver_agrees_with_the_single_process_run_on_every_golden_graph() {
    // The comparison is the drivers against each other (and the golden), so the
    // closure answers `true` for agreement and explains any disagreement as the
    // row's failure; the golden VALUE is consulted inside `parity`.
    let summary = run_table_with(
        "flow/graph-runs",
        Language::Rust,
        None,
        |case| parity(case.id(), case.input(), case.expected()),
        |actual, _golden| actual == &Value::Bool(true),
    )
    .expect("the shared suite must load; a missing checkout is a FAILURE, not a skip");

    print!("[durable parity] ");
    expect_green(&summary, 23);

    // Not all verdicts may be failures: a table of graphs that all fail compares
    // no outputs at all.
    let succeeding = cases("flow/graph-runs", None)
        .expect("suite loads")
        .iter()
        .filter(|case| case.expected().get("ok").and_then(Value::as_bool) == Some(true))
        .count();
    println!("[durable parity] {succeeding} of 23 rows compared outputs");
    assert!(succeeding >= 20, "only {succeeding} rows compared outputs");
}

#[test]
fn a_dead_branch_settles_instead_of_stalling() {
    // The cascade the frontier exists for. A branch routes one way; the other
    // side's node can never be unblocked, so it must be SKIPPED -- which settles
    // it, which is what lets the merge point run.
    let rows = cases("flow/graph-runs", None).expect("suite loads");
    let case = rows
        .iter()
        .find(|case| case.id() == "0005-merge-after-decision")
        .expect("the merge-after-decision golden (#1)");

    let kinds = local_kinds();
    let graph = import(case.input(), &kinds).expect("imports");
    let deps = ExecutorDeps::default();
    let executors = builtin::executors(&deps);
    let clock = FixedClock::new(0);
    let coordinator = Coordinator::new(
        &graph,
        &executors,
        RunIdentity::new("dead-branch", 0),
        &clock,
    )
    .with_initial_inputs(initial_inputs(case.input()));

    let result = coordinator.run_to_completion(None);

    assert!(result.ok, "{:?}", result.error);
    let state = coordinator.store().state("dead-branch");
    assert_eq!(
        state["b"].status,
        NodeRunStatus::Skipped,
        "the untaken branch must settle, not hang"
    );
    assert_eq!(
        state["m"].status,
        NodeRunStatus::Completed,
        "the merge point must still run"
    );
    assert_eq!(
        result.outputs.get("m"),
        case.expected()
            .get("outputs")
            .and_then(|outputs| outputs.get("m"))
    );
}

#[test]
fn the_suites_are_the_ones_the_other_runtimes_assert() {
    // A vacuity guard: pointed at an empty or renamed suite, the tests above
    // would pass by running nothing.
    let dispatch = cases("flow/durable-dispatch", None).expect("suite loads");
    assert_eq!(dispatch.len(), 14);
    let ids: Vec<&str> = dispatch.iter().map(fancy_conformance::Case::id).collect();
    for row in [
        "0001-a-fan-out-dispatches-one-node-at-a-time-by-default",
        "0007-a-successor-declared-before-a-waiting-sibling-goes-first",
        "0008-a-paused-gate-keeps-its-slot",
        "0014-a-cap-is-measured-against-held-work-not-the-batch",
    ] {
        assert!(ids.contains(&row), "{row}");
    }

    assert_eq!(
        cases("flow/run-diagnostics", None).expect("loads").len(),
        14
    );
    assert_eq!(cases("flow/graph-runs", None).expect("loads").len(), 23);
}
