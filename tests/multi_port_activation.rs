//! A node can activate a CHOSEN SUBSET of its ports (fancy-flow-php#18, MOIC).
//!
//! The engine knew two answers: `__port` / `branch` lit exactly one port, and
//! anything else lit EVERY declared port. There was no way to say "these two of
//! five", so a router that matched two lanes had to drop the rest of the work or
//! wake lanes nobody asked for.
//!
//! `Port::many(&["a", "c"], value)` lights both with one payload;
//! `Port::many_with(...)` gives each lit port its own. An empty list lights
//! nothing, which is the rule an explicitly empty `outputs` already follows.

use std::cell::RefCell;
use std::rc::Rc;

use fancy_json::{Map, Value};

use fancy_flow::executors::executor;
use fancy_flow::registry::NodeKind;
use fancy_flow::schema::PortDescriptor;
use fancy_flow::{
    ExecutorRegistry, FlowEdge, FlowGraph, FlowNode, FlowRunner, NodeKindRegistry, Port, RunOptions,
};

/// What each sink received, by node id. A port that never fired is absent.
type Seen = Rc<RefCell<Vec<(String, Value)>>>;

fn router_kinds() -> NodeKindRegistry {
    let mut kinds = NodeKindRegistry::new();
    kinds.register(
        NodeKind::new("router", "logic", "Router").outputs(
            ["a", "b", "c", "d", "e"]
                .iter()
                .map(|id| PortDescriptor::new(*id))
                .collect(),
        ),
    );
    kinds
}

fn run_router_with(result: Value) -> Vec<(String, Value)> {
    let graph = FlowGraph {
        nodes: vec![
            FlowNode::new("r", "router"),
            FlowNode::new("A", "sink"),
            FlowNode::new("B", "sink"),
            FlowNode::new("C", "sink"),
        ],
        edges: vec![
            FlowEdge::new("e1", "r", "A").from_port("a"),
            FlowEdge::new("e2", "r", "B").from_port("b"),
            FlowEdge::new("e3", "r", "C").from_port("c"),
        ],
    };

    let seen: Seen = Rc::new(RefCell::new(Vec::new()));
    let recorder = seen.clone();

    let kinds = Rc::new(router_kinds());
    let mut executors = ExecutorRegistry::new().with_kinds(kinds.clone());
    executors.bind("router", executor(move |_ctx| Ok(result.clone())));
    executors.bind(
        "sink",
        executor(move |ctx| {
            // Presence, never "or the whole map": a payload that IS null is a
            // payload, which is half of what these rows asserts.
            let value = ctx
                .inputs()
                .get("in")
                .cloned()
                .unwrap_or(Value::from("__absent__"));
            recorder.borrow_mut().push((ctx.node().id.clone(), value));
            Ok(Value::Null)
        }),
    );

    FlowRunner::with_kinds(&kinds)
        .run(&graph, &executors, &RunOptions::new())
        .expect("not cancelled");

    let mut out = seen.borrow().clone();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn ports(list: &[&str], value: Value) -> Value {
    Port::many(list, value)
}

#[test]
fn lights_the_listed_ports_and_leaves_the_rest_dark() {
    let seen = run_router_with(ports(&["a", "c"], Value::from("matched")));

    assert_eq!(
        seen,
        vec![
            ("A".to_string(), Value::from("matched")),
            ("C".to_string(), Value::from("matched")),
        ]
    );
}

#[test]
fn gives_each_lit_port_its_own_payload_when_handed_a_map() {
    let mut per_port = Map::new();
    per_port.insert("a", Value::from("billing"));
    per_port.insert("c", Value::from("abuse"));

    let seen = run_router_with(Port::many_with(per_port));

    assert_eq!(
        seen,
        vec![
            ("A".to_string(), Value::from("billing")),
            ("C".to_string(), Value::from("abuse")),
        ]
    );
}

#[test]
fn carries_a_per_port_payload_of_null_rather_than_the_result() {
    // The distinction `branch` had to learn, per port: present-and-null is a
    // payload, not an absent one.
    let mut per_port = Map::new();
    per_port.insert("a", Value::Null);

    let seen = run_router_with(Port::many_with(per_port));

    assert_eq!(seen, vec![("A".to_string(), Value::Null)]);
}

#[test]
fn lights_nothing_for_an_explicitly_empty_list() {
    assert_eq!(run_router_with(Port::many(&[], Value::Null)), vec![]);
}

#[test]
fn reads_the_raw_wire_shape_not_only_the_sugar() {
    // A host in another language emits the shape directly; the engine is what
    // has to agree.
    let document = fancy_json::parse(r#"{"__ports":["a","c"],"value":"v"}"#).unwrap();
    let seen = run_router_with(document);

    assert_eq!(
        seen,
        vec![
            ("A".to_string(), Value::from("v")),
            ("C".to_string(), Value::from("v")),
        ]
    );
}

#[test]
fn leaves_the_one_port_and_every_port_rules_exactly_as_they_were() {
    assert_eq!(
        run_router_with(Port::only("b", Value::from("only-b"))),
        vec![("B".to_string(), Value::from("only-b"))]
    );

    let plain = fancy_json::parse(r#"{"plain":true}"#).unwrap();
    let every: Vec<String> = run_router_with(plain)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(
        every,
        vec!["A".to_string(), "B".to_string(), "C".to_string()]
    );
}

#[test]
fn emits_one_node_output_event_per_lit_port() {
    let graph = FlowGraph {
        nodes: vec![FlowNode::new("r", "router")],
        edges: vec![],
    };
    let kinds = Rc::new(router_kinds());
    let mut executors = ExecutorRegistry::new().with_kinds(kinds.clone());
    let mut per_port = Map::new();
    per_port.insert("a", Value::from(1));
    per_port.insert("c", Value::from(2));
    let result = Port::many_with(per_port);
    executors.bind("router", executor(move |_ctx| Ok(result.clone())));

    let run = FlowRunner::with_kinds(&kinds)
        .run(&graph, &executors, &RunOptions::new())
        .expect("not cancelled");

    let lit: Vec<(String, Value)> = run
        .events
        .iter()
        .filter(|event| event.kind == "node-output" && event.node_id.as_deref() == Some("r"))
        .filter_map(|event| Some((event.port_id.clone()?, event.value.clone()?)))
        .collect();

    assert_eq!(
        lit,
        vec![
            ("a".to_string(), Value::from(1)),
            ("c".to_string(), Value::from(2)),
        ]
    );
}
