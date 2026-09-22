//! `for_each`'s `item` port runs a lane once per item.
//!
//! Until the context carried the graph, this crate accepted an `item` edge and
//! ignored it: every downstream node ran ONCE against the whole collection, and
//! nothing said so. fancy-labs' `batch-scoring` reference graph produced five
//! per-item scores on the PHP twin and one aggregate here. These pin the lane
//! against what the TypeScript, Python and PHP runtimes do with the same graph.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use fancy_json::{Map, Value};

use fancy_flow::durable::Coordinator;
use fancy_flow::executors::executor;
use fancy_flow::nodes::support::ExecutorDeps;
use fancy_flow::registry::builtin;
use fancy_flow::{
    ExecutorRegistry, FixedClock, FlowGraph, FlowRunner, NodeKindRegistry, Pause, RunIdentity,
    RunOptions, RunResult,
};

fn json(text: &str) -> Value {
    fancy_json::parse(text).expect("test JSON parses")
}

fn kinds() -> NodeKindRegistry {
    let mut kinds = NodeKindRegistry::new();
    builtin::register(&mut kinds, true);
    kinds
}

/// `start -> each`, `each.item -> body`, `each.done -> after`.
fn lane_graph(each_config: &str, body_kind: &str, body_config: &str) -> FlowGraph {
    let schema = json(&format!(
        r#"{{
            "version": 1,
            "metadata": {{ "name": "lane" }},
            "graph": {{
                "nodes": [
                    {{ "id": "start", "kind": "manual_trigger" }},
                    {{ "id": "each", "kind": "for_each", "config": {each_config} }},
                    {{ "id": "body", "kind": "{body_kind}", "config": {body_config} }},
                    {{ "id": "after", "kind": "transform" }}
                ],
                "edges": [
                    {{ "id": "e1", "source": "start", "target": "each" }},
                    {{ "id": "e2", "source": "each", "sourceHandle": "item", "target": "body" }},
                    {{ "id": "e3", "source": "each", "sourceHandle": "done", "target": "after" }}
                ]
            }}
        }}"#
    ));
    fancy_flow::import_workflow(&schema, true, &kinds()).graph
}

fn seeds(payload: &str) -> BTreeMap<String, Map> {
    let mut start = Map::new();
    start.insert("in", json(payload));
    let mut seeds = BTreeMap::new();
    seeds.insert("start".to_string(), start);
    seeds
}

fn run(graph: &FlowGraph, executors: &ExecutorRegistry, payload: &str) -> RunResult {
    let kinds = kinds();
    let mut options = RunOptions::new();
    options.initial_inputs = seeds(payload);
    FlowRunner::with_kinds(&kinds)
        .run(graph, executors, &options)
        .expect("no host signal was set, so the run cannot be cancelled")
}

fn builtins() -> ExecutorRegistry {
    builtin::executors(&ExecutorDeps::default())
}

fn done_value(result: &RunResult) -> &Value {
    let each = result
        .outputs
        .get("each")
        .expect("for_each produced an output");
    assert_eq!(
        each.get("__port").and_then(Value::as_str),
        Some("done"),
        "a lane publishes on `done` only"
    );
    each.get("value").expect("the port carries a value")
}

#[test]
fn an_item_edge_runs_the_lane_once_per_item_and_aggregates_on_done() {
    let graph = lane_graph(
        r#"{ "source": "{{ in.in.rows }}" }"#,
        "transform",
        r#"{ "expression": "{{ in.n }}" }"#,
    );
    let result = run(
        &graph,
        &builtins(),
        r#"{ "rows": [{ "n": 1 }, { "n": 2 }, { "n": 3 }] }"#,
    );

    assert!(result.ok, "{:?}", result.error);
    assert_eq!(
        done_value(&result),
        &json(
            r#"{
                "items": [{ "n": 1 }, { "n": 2 }, { "n": 3 }],
                "results": [{ "body": 1 }, { "body": 2 }, { "body": 3 }],
                "failures": [],
                "count": 3
            }"#
        )
    );
}

#[test]
fn the_body_runs_per_item_not_once_on_the_collection() {
    // The defect as it was: one call, handed the whole list.
    let seen: Rc<RefCell<Vec<Value>>> = Rc::new(RefCell::new(Vec::new()));
    let recorder = Rc::clone(&seen);

    let mut executors = builtins();
    executors.bind(
        "record",
        executor(move |ctx| {
            let value = ctx.input_or_all();
            recorder.borrow_mut().push(value.clone());
            Ok(value)
        }),
    );

    let graph = lane_graph(r#"{ "source": "{{ in.in.rows }}" }"#, "record", "{}");
    let result = run(&graph, &executors, r#"{ "rows": ["a", "b"] }"#);

    assert!(result.ok, "{:?}", result.error);
    assert_eq!(*seen.borrow(), vec![json(r#""a""#), json(r#""b""#)]);
    // A lane node belongs to the lane: it is not ALSO run by the outer walk.
    assert!(!result.outputs.contains_key("body"));
}

#[test]
fn a_host_kind_resolves_inside_the_lane() {
    // The lab's lane holds its own `lab_score` kind. Running the lane on the
    // builtins alone would drop it, which is what `subflow` once did here.
    let mut executors = builtins();
    executors.bind(
        "score",
        executor(|ctx| {
            let mut scored = Map::new();
            scored.insert("scored", ctx.input_or_all());
            Ok(Value::Object(scored))
        }),
    );

    let graph = lane_graph(r#"{ "source": "{{ in.in.rows }}" }"#, "score", "{}");
    let result = run(
        &graph,
        &executors,
        r#"{ "rows": [{ "n": 1 }, { "n": 2 }] }"#,
    );

    assert!(result.ok, "{:?}", result.error);
    assert_eq!(
        done_value(&result).get("results"),
        Some(&json(
            r#"[{ "body": { "scored": { "n": 1 } } }, { "body": { "scored": { "n": 2 } } }]"#
        ))
    );
}

#[test]
fn collect_mode_keeps_the_data_only_shape() {
    let graph = lane_graph(
        r#"{ "source": "{{ in.in.rows }}", "mode": "collect" }"#,
        "transform",
        "{}",
    );
    let result = run(&graph, &builtins(), r#"{ "rows": ["a", "b"] }"#);

    assert!(result.ok, "{:?}", result.error);
    let each = result.outputs.get("each").expect("for_each ran");
    assert_eq!(each, &json(r#"{ "count": 2, "items": ["a", "b"] }"#));
}

#[test]
fn an_unwired_item_port_keeps_the_data_only_shape() {
    let schema = json(
        r#"{
            "version": 1,
            "metadata": { "name": "no-lane" },
            "graph": {
                "nodes": [
                    { "id": "start", "kind": "manual_trigger" },
                    { "id": "each", "kind": "for_each", "config": { "source": "{{ in.in.rows }}" } },
                    { "id": "after", "kind": "transform" }
                ],
                "edges": [
                    { "id": "e1", "source": "start", "target": "each" },
                    { "id": "e3", "source": "each", "sourceHandle": "done", "target": "after" }
                ]
            }
        }"#,
    );
    let graph = fancy_flow::import_workflow(&schema, true, &kinds()).graph;
    let result = run(&graph, &builtins(), r#"{ "rows": ["a", "b"] }"#);

    assert!(result.ok, "{:?}", result.error);
    assert_eq!(
        result.outputs.get("each"),
        Some(&json(r#"{ "count": 2, "items": ["a", "b"] }"#))
    );
}

#[test]
fn a_max_items_cap_fails_the_run_rather_than_iterating_past_it() {
    let graph = lane_graph(
        r#"{ "source": "{{ in.in.rows }}", "maxItems": 2 }"#,
        "transform",
        "{}",
    );
    let result = run(&graph, &builtins(), r#"{ "rows": [1, 2, 3] }"#);

    assert!(!result.ok);
    let error = result.error.unwrap_or_default();
    assert!(error.contains("exceeds its maxItems cap of 2"), "{error}");
}

#[test]
fn a_max_items_outside_the_ceiling_is_refused() {
    let graph = lane_graph(
        r#"{ "source": "{{ in.in.rows }}", "maxItems": 10001 }"#,
        "transform",
        "{}",
    );
    let result = run(&graph, &builtins(), r#"{ "rows": [1] }"#);

    assert!(!result.ok);
    let error = result.error.unwrap_or_default();
    assert!(
        error.contains("maxItems must be between 1 and 10000"),
        "{error}"
    );
}

#[test]
fn a_failing_item_is_recorded_and_the_loop_carries_on() {
    let mut executors = builtins();
    executors.bind(
        "picky",
        executor(|ctx| {
            if ctx.input_or_all().as_str() == Some("bad") {
                return Err(ctx.abort("refused bad"));
            }
            Ok(ctx.input_or_all())
        }),
    );

    let graph = lane_graph(r#"{ "source": "{{ in.in.rows }}" }"#, "picky", "{}");
    let result = run(&graph, &executors, r#"{ "rows": ["ok", "bad", "fine"] }"#);

    assert!(
        result.ok,
        "one item failing does not fail the run: {:?}",
        result.error
    );
    let done = done_value(&result);
    // Index-aligned: the failed item leaves null in its own slot.
    assert_eq!(
        done.get("results"),
        Some(&json(r#"[{ "body": "ok" }, null, { "body": "fine" }]"#))
    );
    assert_eq!(
        done.get("failures"),
        Some(&json(
            r#"[{ "index": 1, "item": "bad", "error": "refused bad" }]"#
        ))
    );
}

#[test]
fn a_pause_inside_the_lane_pauses_the_run_rather_than_failing_one_item() {
    let mut executors = builtins();
    executors.bind(
        "gate",
        executor(|ctx| Err(ctx.pause_for_human("input", None))),
    );

    let graph = lane_graph(r#"{ "source": "{{ in.in.rows }}" }"#, "gate", "{}");
    let result = run(&graph, &executors, r#"{ "rows": ["a", "b"] }"#);

    assert!(!result.ok);
    // Assert that it DECODES, never on its text.
    assert!(
        Pause::is_pause(result.error.as_deref()),
        "a pause inside a lane must reach the top as a pause: {:?}",
        result.error
    );
}

#[test]
fn the_done_tail_receives_the_aggregate() {
    let graph = lane_graph(
        r#"{ "source": "{{ in.in.rows }}" }"#,
        "transform",
        r#"{ "expression": "{{ in }}" }"#,
    );
    let result = run(&graph, &builtins(), r#"{ "rows": ["a"] }"#);

    assert!(result.ok, "{:?}", result.error);
    assert_eq!(
        result.outputs.get("after"),
        Some(&json(
            r#"{ "items": ["a"], "results": [{ "body": "a" }], "failures": [], "count": 1 }"#
        ))
    );
}

#[test]
fn a_durable_lane_runs_real_executors_not_the_replays_fences() {
    // The coordinator runs one node by replaying the graph with every OTHER node
    // bound, by id, to a fence that succeeds with a marker port. A lane node is
    // a node of the same graph, so a lane run on that registry "succeeds" with a
    // fence marker for every result. The Python twin shipped exactly that in
    // 0.27.0; the single-process run of the same graph was correct.
    let graph = lane_graph(
        r#"{ "source": "{{ in.in.rows }}" }"#,
        "transform",
        r#"{ "expression": "{{ in.n }}" }"#,
    );
    let executors = builtins();
    let payload = r#"{ "rows": [{ "n": 1 }, { "n": 2 }] }"#;

    let single = run(&graph, &executors, payload);
    assert!(single.ok, "{:?}", single.error);
    // Stated outright, so a fenced or empty lane cannot pass by agreeing with an
    // equally wrong single-process run.
    assert_eq!(
        done_value(&single).get("results"),
        Some(&json(r#"[{ "body": 1 }, { "body": 2 }]"#))
    );

    let kinds = kinds();
    let clock = FixedClock::new(0);
    let durable = Coordinator::new(&graph, &executors, RunIdentity::new("lane", 0), &clock)
        .with_kinds(&kinds)
        .with_initial_inputs(seeds(payload))
        .run_to_completion(None);

    assert!(durable.ok, "{:?}", durable.error);
    assert_eq!(
        durable.outputs.get("each"),
        single.outputs.get("each"),
        "the durable driver must reach the single-process answer"
    );
}
