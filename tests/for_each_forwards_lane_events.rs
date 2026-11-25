//! `for_each` forwards what its item lane EMITS, as fancy-flow-php does (#25).
//!
//! Each iteration's nested run collected its events and `for_each` dropped
//! them, so a status or log a lane node emitted never reached the run's
//! `events`. A host reading them (a lab snapshot, a progress feed) saw nothing
//! from inside the loop: the PHP twin recorded five per-item events for five
//! items, and this crate recorded none.
//!
//! The rule is PHP's `RunEvent::forParent`: node-scoped events reach the parent
//! with their node id re-addressed as `<for_each id>/<index>/<node id>`, and the
//! lane's own run-start / run-end / run-error are NOT forwarded.

use std::collections::BTreeMap;

use fancy_json::{Map, Value};

use fancy_flow::executors::executor;
use fancy_flow::nodes::support::ExecutorDeps;
use fancy_flow::registry::builtin;
use fancy_flow::runtime::{LogLevel, NodeStatus, RunEvent};
use fancy_flow::{FlowRunner, NodeKindRegistry, RunOptions};

fn json(text: &str) -> Value {
    fancy_json::parse(text).expect("test JSON parses")
}

#[test]
fn events_emitted_inside_each_iteration_reach_the_run_re_addressed_per_item() {
    let mut kinds = NodeKindRegistry::new();
    builtin::register(&mut kinds, true);

    let schema = json(
        r#"{
            "version": 1,
            "metadata": { "name": "lane events" },
            "graph": {
                "nodes": [
                    { "id": "start", "kind": "manual_trigger" },
                    { "id": "each", "kind": "for_each", "config": { "source": "{{ in.in.rows }}" } },
                    { "id": "rec", "kind": "recorder" }
                ],
                "edges": [
                    { "id": "e1", "source": "start", "target": "each" },
                    { "id": "e2", "source": "each", "sourceHandle": "item", "target": "rec" }
                ]
            }
        }"#,
    );
    let graph = fancy_flow::import_workflow(&schema, true, &kinds).graph;

    let mut executors = builtin::executors(&ExecutorDeps::default());
    executors.bind(
        "recorder",
        executor(|ctx| {
            let node_id = ctx.node().id.clone();
            let value = ctx.input("in").cloned().unwrap_or(Value::Null);
            ctx.emit(RunEvent::log_with_detail(
                LogLevel::Info,
                "recorded",
                Some(&node_id),
                value,
            ));
            Ok(Value::Null)
        }),
    );

    let mut start = Map::new();
    start.insert("in", json(r#"{ "rows": ["a", "b", "c"] }"#));
    let mut seeds = BTreeMap::new();
    seeds.insert("start".to_string(), start);
    let mut options = RunOptions::new();
    options.initial_inputs = seeds;

    let result = FlowRunner::with_kinds(&kinds)
        .run(&graph, &executors, &options)
        .expect("no host signal was set, so the run cannot be cancelled");

    assert!(result.ok, "{:?}", result.error);

    let logs: Vec<&RunEvent> = result
        .events
        .iter()
        .filter(|e| e.kind == RunEvent::LOG && e.message.as_deref() == Some("recorded"))
        .collect();
    let ids: Vec<Option<&str>> = logs.iter().map(|e| e.node_id.as_deref()).collect();
    assert_eq!(
        ids,
        vec![Some("each/0/rec"), Some("each/1/rec"), Some("each/2/rec")]
    );
    let values: Vec<Option<&Value>> = logs.iter().map(|e| e.detail.as_ref()).collect();
    assert_eq!(
        values,
        vec![
            Some(&json(r#""a""#)),
            Some(&json(r#""b""#)),
            Some(&json(r#""c""#))
        ]
    );

    assert!(result.events.iter().any(|e| e.kind == RunEvent::NODE_STATUS
        && e.node_id.as_deref() == Some("each/1/rec")
        && e.status == Some(NodeStatus::Done)));

    // The lanes' own run boundaries are not the parent's.
    assert_eq!(
        result
            .events
            .iter()
            .filter(|e| e.kind == RunEvent::RUN_START)
            .count(),
        1
    );
    assert_eq!(
        result
            .events
            .iter()
            .filter(|e| e.kind == RunEvent::RUN_END)
            .count(),
        1
    );
}
