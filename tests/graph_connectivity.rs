//! A graph must not contain a node that cannot take part in it.
//!
//! The Rust twin of `FancyFlow\Analysis\GraphConnectivity` (PHP 0.48),
//! `checkGraphConnectivity` (TypeScript 0.64) and
//! `fancy_flow.analysis.check_graph_connectivity` (Python 0.16).
//!
//! Both refused shapes were MEASURED against the engine before any runtime
//! implemented the rule, and NEITHER of them fails today:
//!
//! - a floating `log` in a three-node graph ran (`t,lonely,o`), disconnected;
//! - `t -> output -> log` imported clean and the `log` ran, with `{{ input }}`
//!   resolving to `""`.
//!
//! So these are not "does the validator notice", they are "does the validator
//! notice something the runtime never will".

use fancy_json::{Map, Value};

use fancy_flow::registry::builtin;
use fancy_flow::registry::NodeKind;
use fancy_flow::workflow::import_workflow;
use fancy_flow::NodeKindRegistry;

fn kinds() -> NodeKindRegistry {
    let mut kinds = NodeKindRegistry::new();
    builtin::register(&mut kinds, true);
    kinds
}

fn node(id: &str, kind: &str) -> Value {
    let mut position = Map::new();
    position.insert("x".to_string(), Value::from(0.0));
    position.insert("y".to_string(), Value::from(0.0));

    let mut n = Map::new();
    n.insert("id".to_string(), Value::from(id));
    n.insert("kind".to_string(), Value::from(kind));
    n.insert("position".to_string(), Value::Object(position));
    Value::Object(n)
}

fn edge(id: &str, source: &str, target: &str) -> Value {
    let mut e = Map::new();
    e.insert("id".to_string(), Value::from(id));
    e.insert("source".to_string(), Value::from(source));
    e.insert("target".to_string(), Value::from(target));
    Value::Object(e)
}

fn document(nodes: Vec<Value>, edges: Vec<Value>) -> Value {
    let mut graph = Map::new();
    graph.insert("nodes".to_string(), Value::Array(nodes));
    graph.insert("edges".to_string(), Value::Array(edges));

    let mut doc = Map::new();
    doc.insert("version".to_string(), Value::from(1_i64));
    doc.insert("graph".to_string(), Value::Object(graph));
    Value::Object(doc)
}

/// Every error message from importing this document, joined.
fn errors(nodes: Vec<Value>, edges: Vec<Value>) -> Vec<String> {
    with_registry(&kinds(), nodes, edges)
}

fn with_registry(registry: &NodeKindRegistry, nodes: Vec<Value>, edges: Vec<Value>) -> Vec<String> {
    import_workflow(&document(nodes, edges), false, registry)
        .issues
        .into_iter()
        .filter(fancy_flow::schema::ImportIssue::is_error)
        .map(|i| i.message)
        .collect()
}

fn joined(nodes: Vec<Value>, edges: Vec<Value>) -> String {
    errors(nodes, edges).join("\n")
}

// -- floating nodes ------------------------------------------------------

#[test]
fn a_node_with_no_inbound_and_no_outbound_edge_is_refused() {
    let result = import_workflow(
        &document(
            vec![
                node("t", "manual_trigger"),
                node("o", "output"),
                node("lonely", "log"),
            ],
            vec![edge("e1", "t", "o")],
        ),
        false,
        &kinds(),
    );

    assert!(
        !result.ok,
        "a graph with a floating node must not import ok"
    );
    assert!(joined(
        vec![
            node("t", "manual_trigger"),
            node("o", "output"),
            node("lonely", "log")
        ],
        vec![edge("e1", "t", "o")]
    )
    .contains("\"lonely\" is connected to nothing"));
}

#[test]
fn the_floating_node_is_named_so_an_editor_can_highlight_it() {
    let result = import_workflow(
        &document(
            vec![
                node("t", "manual_trigger"),
                node("o", "output"),
                node("lonely", "log"),
            ],
            vec![edge("e1", "t", "o")],
        ),
        false,
        &kinds(),
    );

    let found: Vec<_> = result.issues.iter().filter(|i| i.is_error()).collect();

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].node_id.as_deref(), Some("lonely"));
}

#[test]
fn a_trigger_that_reaches_nobody_is_refused() {
    // Outbound-only is the direction people forget: the node fires and the
    // graph never hears it.
    assert!(joined(
        vec![
            node("t1", "manual_trigger"),
            node("o", "output"),
            node("orphan", "webhook_trigger")
        ],
        vec![edge("e1", "t1", "o")]
    )
    .contains("orphan"));
}

#[test]
fn every_disconnected_node_is_reported_not_just_the_first() {
    // Stopping at the first would make fixing a graph an N-round trip, and an
    // agent authoring one would burn a call per stray node.
    assert_eq!(
        errors(
            vec![
                node("t", "manual_trigger"),
                node("o", "output"),
                node("a", "log"),
                node("b", "log")
            ],
            vec![edge("e1", "t", "o")]
        )
        .len(),
        2
    );
}

#[test]
fn a_disconnected_island_is_allowed_being_two_workflows_in_one_document() {
    // Each node has an edge, so none floats by the letter of the rule. Recorded
    // deliberately: an island is a defensible thing to author, unlike a node
    // nobody wired up.
    assert!(errors(
        vec![
            node("t1", "manual_trigger"),
            node("o1", "output"),
            node("t2", "manual_trigger"),
            node("o2", "output")
        ],
        vec![edge("e1", "t1", "o1"), edge("e2", "t2", "o2")]
    )
    .is_empty());
}

// -- what may float ------------------------------------------------------

#[test]
fn a_note_may_float_because_a_note_is_an_annotation_not_a_step() {
    assert!(errors(
        vec![
            node("t", "manual_trigger"),
            node("o", "output"),
            node("sticky", "note")
        ],
        vec![edge("e1", "t", "o")]
    )
    .is_empty());
}

#[test]
fn a_note_may_float_under_its_canonical_namespaced_id_too() {
    // A graph saved by a newer editor carries `@particle-academy/note`. Keying
    // the exemption on the bare spelling alone would turn every sticky note
    // into an error the moment it round-tripped.
    assert!(errors(
        vec![
            node("t", "manual_trigger"),
            node("o", "output"),
            node("sticky", "@particle-academy/note")
        ],
        vec![edge("e1", "t", "o")]
    )
    .is_empty());
}

#[test]
fn an_annotation_or_layout_host_kind_may_float_and_an_ordinary_one_may_not() {
    // PAIRED WITH ITS CONTROL, and the control is the point.
    //
    // Alone, the first two assertions CANNOT FAIL: if registration silently did
    // nothing, `design_note` and `lane` would be UNKNOWN kinds — and unknown
    // kinds float too. They would pass whether the category rule worked or not.
    //
    // The third registers an ordinary kind and asserts it IS refused, which is
    // what makes registration observable.
    let mut registry = kinds();
    registry.register(NodeKind::new("design_note", "annotation", "Design Note"));
    registry.register(NodeKind::new("lane", "layout", "Lane"));
    registry.register(NodeKind::new("design_step", "data", "Design Step"));

    let wired = || vec![edge("e1", "t", "o")];
    let base = || vec![node("t", "manual_trigger"), node("o", "output")];

    let mut with_note = base();
    with_note.push(node("d", "design_note"));
    assert!(with_registry(&registry, with_note, wired()).is_empty());

    let mut with_lane = base();
    with_lane.push(node("l", "lane"));
    assert!(with_registry(&registry, with_lane, wired()).is_empty());

    let mut with_step = base();
    with_step.push(node("s", "design_step"));
    assert!(with_registry(&registry, with_step, wired())
        .join("\n")
        .contains("connected to nothing"));
}

#[test]
fn the_exemption_does_not_extend_to_an_ordinary_kind() {
    assert!(joined(
        vec![
            node("t", "manual_trigger"),
            node("o", "output"),
            node("x", "transform")
        ],
        vec![edge("e1", "t", "o")]
    )
    .contains("connected to nothing"));
}

#[test]
fn an_unknown_kind_is_not_also_called_floating_on_top_of_its_own_error() {
    // We cannot know whether an unknown kind is a step, an annotation or a
    // lane, so claiming it must be wired asserts something unverifiable — and
    // it lands hardest on the graphs that deserve it least. A laned graph
    // authored in the TS editor carries `lane` nodes a registry without them
    // does not have; before this exemption every swimlane collected a second,
    // misleading error underneath the real one.
    let text = joined(
        vec![
            node("t", "manual_trigger"),
            node("o", "output"),
            node("c", "no_such_kind"),
        ],
        vec![edge("e1", "t", "o")],
    );

    assert!(text.contains("Unknown kind"));
    assert!(!text.contains("connected to nothing"));
}

// -- edges out of a terminator -------------------------------------------

#[test]
fn an_edge_whose_source_is_a_terminal_node_is_refused() {
    assert!(joined(
        vec![
            node("t", "manual_trigger"),
            node("out", "output"),
            node("after", "log")
        ],
        vec![edge("e1", "t", "out"), edge("e2", "out", "after")]
    )
    .contains("is a TERMINAL node"));
}

#[test]
fn the_offending_edge_is_named_not_the_node() {
    let result = import_workflow(
        &document(
            vec![
                node("t", "manual_trigger"),
                node("out", "output"),
                node("after", "log"),
            ],
            vec![edge("e1", "t", "out"), edge("e2", "out", "after")],
        ),
        false,
        &kinds(),
    );

    let found: Vec<_> = result.issues.iter().filter(|i| i.is_error()).collect();

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].edge_id.as_deref(), Some("e2"));
    assert_eq!(found[0].node_id, None);
}

#[test]
fn an_edge_out_of_log_is_refused_because_log_is_terminal_too() {
    assert!(joined(
        vec![
            node("t", "manual_trigger"),
            node("l", "log"),
            node("after", "output")
        ],
        vec![edge("e1", "t", "l"), edge("e2", "l", "after")]
    )
    .contains("TERMINAL"));
}

#[test]
fn an_edge_out_of_a_node_declaring_no_outputs_at_all_is_allowed() {
    // THE DISTINCTION THIS TURNS ON. `Some(vec![])` is an explicit "there is
    // nothing to connect from"; `None` is "nobody declared it", which resolves
    // to `out` and describes most nodes in most graphs. Reading them alike
    // would refuse nearly every workflow ever written.
    assert!(errors(
        vec![
            node("t", "manual_trigger"),
            node("w", "wait"),
            node("o", "output")
        ],
        vec![edge("e1", "t", "w"), edge("e2", "w", "o")]
    )
    .is_empty());
}

#[test]
fn an_edge_from_an_unknown_kind_is_not_refused() {
    // An unregistered kind falls back to `out` in the engine, so it is not a
    // terminator. Using "I do not know" as evidence is the failure this suite
    // keeps finding elsewhere.
    assert!(!joined(
        vec![
            node("t", "manual_trigger"),
            node("x", "some_host_kind"),
            node("o", "output")
        ],
        vec![edge("e1", "t", "x"), edge("e2", "x", "o")]
    )
    .contains("TERMINAL"));
}

// -- the graphs people actually write still pass -------------------------

#[test]
fn an_ordinary_linear_workflow_passes() {
    assert!(errors(
        vec![
            node("t", "manual_trigger"),
            node("h", "api_request"),
            node("x", "transform"),
            node("o", "output")
        ],
        vec![
            edge("e1", "t", "h"),
            edge("e2", "h", "x"),
            edge("e3", "x", "o")
        ]
    )
    .is_empty());
}

#[test]
fn a_single_node_graph_passes_being_a_small_workflow_not_a_floating_node() {
    // Refusing this would make an editor unusable from the first node placed,
    // and the node genuinely runs — there is no second node it fails to reach.
    assert!(errors(vec![node("t", "manual_trigger")], vec![]).is_empty());
}

#[test]
fn an_empty_graph_passes() {
    let e = errors(vec![], vec![]);
    assert!(e.is_empty(), "unexpected: {e:?}");
}

#[test]
fn a_dangling_edge_still_only_warns_and_does_not_strand_its_source() {
    // A dangling edge is DROPPED with a warning by the importer. Running
    // connectivity on the surviving edges alone would strand its source and
    // turn one warning into an error — changing an existing, documented
    // behaviour as a side effect.
    let result = import_workflow(
        &document(
            vec![node("t", "manual_trigger"), node("o", "output")],
            vec![edge("e1", "t", "o"), edge("e2", "t", "ghost")],
        ),
        false,
        &kinds(),
    );

    assert!(result.ok);
    assert_eq!(result.issues.iter().filter(|i| !i.is_error()).count(), 1);
    assert_eq!(result.issues.iter().filter(|i| i.is_error()).count(), 0);
}

#[test]
fn the_rule_is_lenient_mode_independent() {
    // `lenient` exists so a host can load a graph containing a kind IT has not
    // registered yet. It is about unknown vocabulary, never about wiring: a
    // floating node floats in every registry.
    let result = import_workflow(
        &document(
            vec![
                node("t", "manual_trigger"),
                node("o", "output"),
                node("lonely", "log"),
            ],
            vec![edge("e1", "t", "o")],
        ),
        true,
        &kinds(),
    );

    assert!(!result.ok);
}
