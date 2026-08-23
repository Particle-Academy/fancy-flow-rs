//! The invariants the peers paid for, pinned here so this port cannot re-earn them.
//!
//! Each test names the defect it exists to catch. A regression test whose title
//! is "it works" is a test nobody can decide to delete.

use fancy_json::Value;

use fancy_flow::executors::executor;
use fancy_flow::nodes::support::ExecutorDeps;
use fancy_flow::registry::builtin;
use fancy_flow::runtime::{AbortSignal, FixedClock, LogLevel, NodeStatus, RunEvent};
use fancy_flow::schema::PortDescriptor;
use fancy_flow::{
    ExecutorRegistry, FlowEdge, FlowGraph, FlowNode, FlowRunner, NodeKindRegistry, Pause,
    PauseSignal, Port, RunIdentity, RunOptions,
};

fn constant(value: Value) -> fancy_flow::executors::SharedExecutor {
    executor(move |_ctx| Ok(value.clone()))
}

fn run(graph: &FlowGraph, executors: &ExecutorRegistry) -> fancy_flow::RunResult {
    FlowRunner::new()
        .run(graph, executors, &RunOptions::new())
        .expect("not cancelled")
}

// -- the merge-after-decision rule (#1) ----------------------------------

#[test]
fn a_node_runs_when_at_least_one_incoming_edge_is_active() {
    // Requiring ALL incoming edges to be active wrongly skips a merge point:
    // when a decision routes down one branch, the other branch's edge stays
    // dead FOREVER, so an `every` check skips the shared continuation and the
    // run halts after the first branch — reporting success.
    let graph = FlowGraph {
        nodes: alloc_nodes(&[
            ("br", "branch"),
            ("a", "transform"),
            ("b", "transform"),
            ("m", "merge"),
        ]),
        edges: vec![
            FlowEdge::new("e1", "br", "a").from_port("true"),
            FlowEdge::new("e2", "br", "b").from_port("false"),
            FlowEdge::new("e3", "a", "m").to_port("a"),
            FlowEdge::new("e4", "b", "m").to_port("b"),
        ],
    };

    let mut executors = ExecutorRegistry::new();
    executors
        .bind(
            "branch",
            executor(|_| Ok(Port::branch("true", Value::from("payload")))),
        )
        .bind("transform", executor(|ctx| Ok(ctx.input_or_all())))
        .bind(
            "merge",
            executor(|ctx| Ok(Value::Object(ctx.inputs().clone()))),
        );

    let result = run(&graph, &executors);
    assert!(result.ok);

    let merged = result.output("m").expect("the merge point must have RUN");
    assert_eq!(merged.get("a").and_then(Value::as_str), Some("payload"));
    // And the dead branch contributed nothing at all — not a null.
    assert!(
        merged.get("b").is_none(),
        "a dead edge must not appear in the inputs"
    );
}

#[test]
fn a_dead_edge_never_clobbers_a_live_one_on_the_same_handle() {
    // The other half of the merge-point bug. TypeScript assigned inputs
    // unconditionally, so a trailing DEAD edge overwrote a live one with
    // `undefined` whenever two branches rejoined on the same handle — emptying
    // every merge point downstream of a decision, silently, with the run still
    // reporting success. Edge order matters: the dead one is LAST.
    let graph = FlowGraph {
        nodes: alloc_nodes(&[
            ("br", "branch"),
            ("a", "transform"),
            ("b", "transform"),
            ("m", "merge"),
        ]),
        edges: vec![
            FlowEdge::new("e1", "br", "a").from_port("true"),
            FlowEdge::new("e2", "br", "b").from_port("false"),
            // BOTH arrive on the default `in` handle, live one first.
            FlowEdge::new("e3", "a", "m"),
            FlowEdge::new("e4", "b", "m"),
        ],
    };

    let mut executors = ExecutorRegistry::new();
    executors
        .bind(
            "branch",
            executor(|_| Ok(Port::branch("true", Value::from("kept")))),
        )
        .bind("transform", executor(|ctx| Ok(ctx.input_or_all())))
        .bind(
            "merge",
            executor(|ctx| Ok(ctx.input("in").cloned().unwrap_or(Value::Null))),
        );

    let result = run(&graph, &executors);
    assert_eq!(result.output("m").and_then(Value::as_str), Some("kept"));
}

// -- three-state ports ---------------------------------------------------

#[test]
fn declared_empty_outputs_publish_on_nothing_and_undeclared_fall_back_to_out() {
    // `None` means "no ports declared" and the engine falls back; `Some(vec![])`
    // means "explicitly no ports". Collapsing the two is how a terminal node
    // starts publishing on `out` — or a branch node stops branching.
    let mut terminal = FlowNode::new("t", "sink");
    terminal.outputs = Some(vec![]);

    let graph = FlowGraph {
        nodes: vec![terminal, FlowNode::new("after", "sink")],
        edges: vec![FlowEdge::new("e1", "t", "after")],
    };

    let mut executors = ExecutorRegistry::new();
    executors.bind("sink", constant(Value::from("value")));

    let result = run(&graph, &executors);
    assert!(result.ok);
    assert!(
        result.activated_ports("t").is_empty(),
        "an empty declaration is not a fallback"
    );
    assert!(
        result.output("after").is_none(),
        "nothing downstream of a terminal node may run"
    );

    // And the same graph with the declaration ABSENT does publish on `out`.
    let graph = FlowGraph {
        nodes: vec![FlowNode::new("t", "sink"), FlowNode::new("after", "sink")],
        edges: vec![FlowEdge::new("e1", "t", "after")],
    };
    let result = run(&graph, &executors);
    assert_eq!(result.activated_ports("t"), vec!["out"]);
    assert!(result.output("after").is_some());
}

#[test]
fn a_declared_port_set_is_used_verbatim() {
    let node = FlowNode::new("n", "fan").with_outputs(vec![
        PortDescriptor::new("left"),
        PortDescriptor::new("right"),
    ]);
    let graph = FlowGraph {
        nodes: vec![node],
        edges: vec![],
    };

    let mut executors = ExecutorRegistry::new();
    executors.bind("fan", constant(Value::from(1)));

    let result = run(&graph, &executors);
    assert_eq!(result.activated_ports("n"), vec!["left", "right"]);
}

// -- executor lookup -----------------------------------------------------

#[test]
fn an_executor_bound_by_kind_fires_for_a_node_imported_from_a_document() {
    // The TypeScript 0.48.1 bug: the lookup consulted only the xyflow `type`
    // while every other reader consulted `data.kind`, so a registry keyed by
    // kind NEVER fired — silently, because an unregistered kind fails closed
    // with no outputs.
    //
    // This port has exactly one kind field and the importer maps the document's
    // `kind` onto it, so the miss is structurally impossible. This pins it.
    let document = fancy_json::parse(
        r#"{"version":1,"graph":{"nodes":[{"id":"n","kind":"transform","position":{"x":0,"y":0}}],"edges":[]}}"#,
    )
    .unwrap();

    let mut kinds = NodeKindRegistry::new();
    builtin::register(&mut kinds, true);
    let imported = fancy_flow::import_workflow(&document, true, &kinds);

    let mut executors = ExecutorRegistry::new();
    executors.bind("transform", constant(Value::from("fired")));

    let result = run(&imported.graph, &executors);
    assert_eq!(result.output("n").and_then(Value::as_str), Some("fired"));
}

#[test]
fn binding_one_spelling_of_a_builtin_binds_every_id_it_answers_to() {
    // A durable override bound under the bare name only never matched a node
    // saved as `@particle-academy/user_input`. Nothing errored; the run went
    // straight past the person it was meant to stop for.
    let mut executors = ExecutorRegistry::new();
    executors.bind("user_input", constant(Value::from("overridden")));

    for spelling in [
        "user_input",
        "@particle-academy/user_input",
        "@fancy/user_input",
    ] {
        let graph = FlowGraph {
            nodes: vec![FlowNode::new("n", spelling)],
            edges: vec![],
        };
        let result = run(&graph, &executors);
        assert_eq!(
            result.output("n").and_then(Value::as_str),
            Some("overridden"),
            "binding did not reach the node saved as {spelling}"
        );
    }
}

#[test]
fn the_llm_branch_rename_still_resolves() {
    // Convention alone cannot get you from `llm_branch` to `llm_router` — only
    // the kind's declared alias list does. A rename without it is a breaking
    // change wearing the costume of a rename.
    let deps = ExecutorDeps::default();
    let executors = builtin::executors(&deps);

    for spelling in ["llm_router", "llm_branch", "@particle-academy/llm_branch"] {
        let graph = FlowGraph {
            nodes: vec![FlowNode::new("n", spelling)
                .with_config("routes", fancy_json::parse(r#"["a","b"]"#).unwrap())],
            edges: vec![],
        };
        let result = run(&graph, &executors);
        assert!(result.ok, "{spelling} did not resolve: {:?}", result.error);
    }
}

#[test]
fn an_unregistered_kind_fails_closed_and_says_so() {
    let graph = FlowGraph {
        nodes: vec![FlowNode::new("n", "no_such_kind")],
        edges: vec![],
    };
    let result = run(&graph, &ExecutorRegistry::new());
    assert!(!result.ok);
    assert_eq!(
        result.error.as_deref(),
        Some("No executor registered for kind=no_such_kind")
    );
}

// -- control flow is not failure ----------------------------------------

#[test]
fn a_pause_reason_survives_the_engine_verbatim_and_still_decodes() {
    // The rule that broke 72 tests in the PHP twin when errors were decorated.
    // Assert that it DECODES — never assert on the text, which is exactly the
    // mistake a decoration would hide.
    let detail = fancy_json::parse(r#"{"fields":["name"]}"#).unwrap();
    let graph = FlowGraph {
        nodes: vec![FlowNode::new("gate", "user_input")],
        edges: vec![],
    };

    let mut executors = ExecutorRegistry::new();
    let expected_detail = detail.clone();
    executors.bind(
        "user_input",
        executor(move |ctx| Err(ctx.pause_for_human("input", Some(expected_detail.clone())))),
    );

    let result = run(&graph, &executors);
    assert!(!result.ok, "a pause ends the run");

    let signal = Pause::decode(result.error.as_deref()).expect("the pause must still decode");
    assert_eq!(signal.node_id, "gate");
    assert_eq!(signal.awaiting, "input");
    assert_eq!(signal.detail, Some(detail));
}

#[test]
fn a_legacy_pause_prefix_still_decodes() {
    // Those strings are sitting in the `error` column of every run that paused
    // under an older version. A resume path that only works for new runs is not
    // a resume path — it strands everything already in flight.
    let signal = Pause::decode(Some("awaiting-approval:node-7")).expect("legacy prefix");
    assert_eq!(signal.node_id, "node-7");
    assert!(signal.is_approval());
}

#[test]
fn a_node_id_containing_a_colon_round_trips() {
    // Why the payload is JSON rather than delimited fields: a positional
    // encoding that breaks on user data only ever shows up in someone else's
    // graph.
    let encoded = Pause::encode(&PauseSignal::new("ns:node:7", "approval", None));
    let signal = Pause::decode(Some(&encoded)).unwrap();
    assert_eq!(signal.node_id, "ns:node:7");
}

#[test]
fn an_ordinary_failure_is_not_a_pause() {
    assert!(Pause::decode(Some("the API returned 500")).is_none());
    assert!(Pause::decode(None).is_none());
    // A malformed payload is a CORRUPT pause, not something to invent an id for.
    assert!(Pause::decode(Some("fancy-flow:pause:{\"nodeId\":7}")).is_none());
}

// -- resume, cancellation, timeout, cycles -------------------------------

#[test]
fn a_checkpointed_node_is_republished_never_re_executed() {
    // The primitive every durable driver is built on: the stored value goes
    // back onto the same ports, so downstream routing reproduces exactly what
    // it did the first time.
    let graph = FlowGraph {
        nodes: alloc_nodes(&[("a", "boom"), ("b", "echo")]),
        edges: vec![FlowEdge::new("e1", "a", "b")],
    };

    let mut executors = ExecutorRegistry::new();
    executors
        .bind(
            "boom",
            executor(|ctx| Err(ctx.abort("this must never run"))),
        )
        .bind("echo", executor(|ctx| Ok(ctx.input_or_all())));

    let options = RunOptions::new().resume("a", Value::from("checkpointed"));
    let result = FlowRunner::new().run(&graph, &executors, &options).unwrap();

    assert!(result.ok, "the checkpointed node must not have executed");
    assert_eq!(
        result.output("b").and_then(Value::as_str),
        Some("checkpointed")
    );

    let resumed = result.events.iter().any(|event| {
        event.kind == RunEvent::NODE_STATUS
            && event.node_id.as_deref() == Some("a")
            && event.text.as_deref() == Some("resumed")
    });
    assert!(resumed, "a resumed node reports itself as resumed");
}

#[test]
fn a_host_signal_cancels_the_run_rather_than_failing_it() {
    // A cancelled run has no result to report; a FAILED one does. Collapsing
    // the two would make a cancel indistinguishable from a node that threw.
    let signal = AbortSignal::new();
    signal.abort(Some("host said stop"));

    let graph = FlowGraph {
        nodes: vec![FlowNode::new("n", "any")],
        edges: vec![],
    };
    let mut executors = ExecutorRegistry::new();
    executors.bind("any", constant(Value::Null));

    let options = RunOptions {
        signal: Some(&signal),
        ..RunOptions::new()
    };
    let cancelled = FlowRunner::new()
        .run(&graph, &executors, &options)
        .unwrap_err();
    assert_eq!(cancelled.reason, "host said stop");
}

#[test]
fn a_cycle_aborts_with_the_exact_message_every_peer_emits() {
    // An EM DASH. The Python twin emitted an ASCII hyphen for two releases and
    // nothing reported it, because the shared fixture asserted a SUBSTRING that
    // stopped before the character they disagreed on.
    let graph = FlowGraph {
        nodes: alloc_nodes(&[("a", "x"), ("b", "x")]),
        edges: vec![FlowEdge::new("e1", "a", "b"), FlowEdge::new("e2", "b", "a")],
    };
    let result = run(&graph, &ExecutorRegistry::new());
    assert!(!result.ok);
    assert_eq!(
        result.error.as_deref(),
        Some("Cycle detected in flow graph \u{2014} aborting.")
    );
}

#[test]
fn a_timeout_is_measured_against_the_injected_clock_and_never_the_host() {
    // The engine must never call `SystemTime::now()`: a workflow inside a
    // blockchain node has to produce the same result on every validator.
    let graph = FlowGraph {
        nodes: alloc_nodes(&[("a", "x"), ("b", "x"), ("c", "x")]),
        edges: vec![FlowEdge::new("e1", "a", "b"), FlowEdge::new("e2", "b", "c")],
    };
    let mut executors = ExecutorRegistry::new();
    executors.bind("x", constant(Value::from(1)));

    // A clock that never advances can never blow a budget, however long the
    // wall clock says the run took.
    let frozen = FixedClock::new(1_000);
    let options = RunOptions::new().with_timeout(0, &frozen);
    let result = FlowRunner::new().run(&graph, &executors, &options).unwrap();
    assert!(
        result.ok,
        "a frozen clock means no elapsed time, so no timeout"
    );
    assert_eq!(result.outputs.len(), 3);
}

// -- annotations ---------------------------------------------------------

#[test]
fn a_note_is_never_executed_under_any_of_its_spellings() {
    // A graph saved with the canonical `@particle-academy/note` must stay an
    // annotation, not become an unrunnable node.
    for spelling in ["note", "@particle-academy/note", "@fancy/note"] {
        let graph = FlowGraph {
            nodes: vec![FlowNode::new("n", spelling)],
            edges: vec![],
        };
        // No executors at all: if the engine tried to run it, this would fail
        // closed with "No executor registered".
        let result = run(&graph, &ExecutorRegistry::new());
        assert!(result.ok, "{spelling} was executed");
        assert!(result.output("n").is_none());

        let annotated = result.events.iter().any(|event| {
            event.status == Some(NodeStatus::Idle) && event.text.as_deref() == Some("annotation")
        });
        assert!(annotated, "{spelling} was not reported as an annotation");
    }
}

// -- the run identity ----------------------------------------------------

#[test]
fn a_subflow_descent_makes_the_same_node_a_different_step() {
    let top = RunIdentity::new("run_a", 0);
    assert_eq!(top.step_key("pay", None), "run_a:pay");

    let inside = top.descend("billing", None);
    assert_eq!(inside.step_key("pay", None), "run_a:billing/pay");

    // Two invocations of the same subflow are different steps.
    let first = top.descend("billing", Some(0));
    let second = top.descend("billing", Some(1));
    assert_ne!(first.step_key("pay", None), second.step_key("pay", None));
    // And occurrence ZERO is a real occurrence, not an absent one.
    assert_eq!(first.step_key("pay", None), "run_a:billing#0/pay");
}

#[test]
fn attempt_is_never_part_of_the_key() {
    // The bug the key exists to prevent, reintroduced by its own fix.
    let identity = RunIdentity::new("run_a", 0);
    assert_eq!(
        identity.clone().with_attempt(1).step_key("pay", None),
        identity.with_attempt(5).step_key("pay", None)
    );
}

// -- events --------------------------------------------------------------

#[test]
fn an_executors_log_lines_reach_the_stream_in_order() {
    let graph = FlowGraph {
        nodes: vec![FlowNode::new("n", "chatty")],
        edges: vec![],
    };
    let mut executors = ExecutorRegistry::new();
    executors.bind(
        "chatty",
        executor(|ctx| {
            ctx.emit(RunEvent::log(LogLevel::Info, "first", Some("n")));
            ctx.emit(RunEvent::log(LogLevel::Warn, "second", Some("n")));
            Ok(Value::Null)
        }),
    );

    let result = run(&graph, &executors);
    let messages: Vec<&str> = result
        .events
        .iter()
        .filter(|event| event.kind == RunEvent::LOG)
        .filter_map(|event| event.message.as_deref())
        .collect();
    assert_eq!(messages, vec!["first", "second"]);
}

#[test]
fn activated_ports_are_read_off_the_event_stream() {
    // A durable driver must read them, never recompute them: a second copy of
    // the routing table agrees for a year and then disagrees on one branch.
    let graph = FlowGraph {
        nodes: vec![FlowNode::new("d", "decide")],
        edges: vec![],
    };
    let mut executors = ExecutorRegistry::new();
    executors.bind(
        "decide",
        executor(|_| Ok(Port::only("chosen", Value::from(7)))),
    );

    let result = run(&graph, &executors);
    assert_eq!(result.activated_ports("d"), vec!["chosen"]);
}

// -- helpers -------------------------------------------------------------

fn alloc_nodes(pairs: &[(&str, &str)]) -> Vec<FlowNode> {
    pairs
        .iter()
        .map(|(id, kind)| FlowNode::new(*id, *kind))
        .collect()
}
