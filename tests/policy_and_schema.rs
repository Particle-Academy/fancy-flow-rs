//! `GraphPolicy`, the importer, and the round trip.

use fancy_json::Value;

use fancy_flow::registry::builtin;
use fancy_flow::security::GraphPolicy;
use fancy_flow::workflow::{export_workflow, import_json, to_json};
use fancy_flow::{FlowEdge, FlowGraph, FlowNode, NodeKindRegistry};

fn kinds() -> NodeKindRegistry {
    let mut kinds = NodeKindRegistry::new();
    builtin::register(&mut kinds, true);
    kinds
}

// -- GraphPolicy ---------------------------------------------------------

#[test]
fn untrusted_starts_with_an_empty_allowlist_and_refuses_everything() {
    // The deliberate divergence from the PHP twin. There, `untrusted()` returns
    // a policy whose allowlist is ABSENT — and an absent allowlist permits every
    // kind, so a caller who forgets `allowKinds()` gets size caps and byte
    // hygiene with NO kind restriction, from a method named `untrusted`.
    //
    // Here an empty allowlist permits nothing until something is named. That
    // changes no verdict for a correctly configured policy and turns a silent
    // fail-open into a loud rejection.
    let graph = FlowGraph {
        nodes: vec![FlowNode::new("n", "api_request")],
        edges: vec![],
    };

    assert!(
        !GraphPolicy::untrusted().accepts(&graph),
        "untrusted() must not fail open"
    );
    assert!(GraphPolicy::untrusted()
        .allow_kinds(&["api_request"])
        .accepts(&graph));
    // `trusted()` has no kind policy at all, which is the correct posture for a
    // graph your own code produced.
    assert!(GraphPolicy::trusted().accepts(&graph));
}

#[test]
fn the_allowlist_is_keyed_on_the_bare_name_so_every_spelling_is_one_decision() {
    // A policy matching literal strings would permit a kind under one spelling
    // and refuse it under another — an allowlist with a hole in it.
    let policy = GraphPolicy::untrusted().allow_kinds(&["transform"]);
    for spelling in [
        "transform",
        "@particle-academy/transform",
        "@fancy/transform",
    ] {
        assert!(policy.permits(spelling), "{spelling} should be permitted");
    }
    assert!(!policy.permits("api_request"));
    // And a third party's `@acme/transform` is NOT the builtin.
    assert!(
        policy.permits("@acme/transform"),
        "bare-name keying is documented behaviour"
    );
}

#[test]
fn a_denial_beats_an_allowance() {
    let policy = GraphPolicy::untrusted()
        .allow_kinds(&["transform", "notify"])
        .deny_kinds(&["notify"]);
    assert!(policy.permits("transform"));
    assert!(!policy.permits("@particle-academy/notify"));
}

#[test]
fn every_problem_is_reported_not_just_the_first() {
    // A validator that reveals one problem per attempt turns a five-minute fix
    // into five round trips.
    let graph = FlowGraph {
        nodes: vec![
            FlowNode::new("a", "api_request"),
            FlowNode::new("b", "notify"),
        ],
        edges: vec![],
    };
    let policy = GraphPolicy::untrusted().allow_kinds(&["transform"]);
    let issues = policy.inspect(&graph);
    assert_eq!(issues.len(), 2, "both refused kinds must be named");

    let error = policy.assert_safe(&graph).unwrap_err();
    assert_eq!(error.issues.len(), 2);
}

#[test]
fn size_caps_are_enforced() {
    let nodes: Vec<FlowNode> = (0..70)
        .map(|i| FlowNode::new(format!("n{i}"), "transform"))
        .collect();
    let graph = FlowGraph {
        nodes,
        edges: vec![],
    };

    let policy = GraphPolicy::untrusted().allow_kinds(&["transform"]);
    let issues = policy.inspect(&graph);
    assert!(
        issues
            .iter()
            .any(|issue| issue.message.contains("70 nodes")),
        "{issues:?}"
    );
    assert!(
        GraphPolicy::trusted().accepts(&graph),
        "the trusted cap is far higher"
    );
}

#[test]
fn deep_config_is_refused_without_overflowing_the_stack() {
    // The check walks ITERATIVELY: the thing being inspected is untrusted
    // nesting, so a recursive walk would blow the stack on exactly the input
    // the depth cap exists to refuse, before the cap could report it.
    // 200 is far past `GraphPolicy`'s cap of 12 and needs only a modest raise
    // of the reader's own cap. An earlier draft used 20,000 and overflowed the
    // stack inside the PARSER — which is exactly the hazard `fancy-json`'s
    // default cap exists to prevent, reintroduced by a test raising it.
    let deep = "[".repeat(200) + &"]".repeat(200);
    let nested = fancy_json::parse_with(&deep, fancy_json::ParseOptions::new().with_max_depth(500))
        .expect("the fixture itself parses with a modestly raised cap");

    let node = FlowNode::new("n", "transform").with_config("payload", nested);
    let graph = FlowGraph {
        nodes: vec![node],
        edges: vec![],
    };

    let issues = GraphPolicy::untrusted()
        .allow_kinds(&["transform"])
        .inspect(&graph);
    assert!(
        issues
            .iter()
            .any(|issue| issue.message.contains("nests deeper")),
        "{issues:?}"
    );
}

#[test]
fn an_over_long_string_is_refused() {
    let node = FlowNode::new("n", "transform")
        .with_config("blob", Value::from("x".repeat(20_001).as_str()));
    let graph = FlowGraph {
        nodes: vec![node],
        edges: vec![],
    };

    let issues = GraphPolicy::untrusted()
        .allow_kinds(&["transform"])
        .inspect(&graph);
    assert!(
        issues
            .iter()
            .any(|issue| issue.message.contains("characters")),
        "{issues:?}"
    );
}

// -- the importer --------------------------------------------------------

#[test]
fn a_strict_import_refuses_an_unknown_kind_and_a_lenient_one_warns() {
    let document =
        r#"{"version":1,"graph":{"nodes":[{"id":"n","kind":"no_such_kind"}],"edges":[]}}"#;
    let kinds = kinds();

    let strict = import_json(document, false, &kinds).unwrap();
    assert!(!strict.ok);
    assert_eq!(strict.errors().len(), 1);

    let lenient = import_json(document, true, &kinds).unwrap();
    assert!(lenient.ok, "lenient mode downgrades it to a warning");
    assert_eq!(lenient.graph.nodes.len(), 1);
}

#[test]
fn a_dangling_edge_is_dropped_with_a_warning_rather_than_carried() {
    // An edge to a node that is not there is not a graph the engine can walk,
    // and carrying it would put the failure somewhere unrelated.
    let document = r#"{"version":1,"graph":{
        "nodes":[{"id":"a","kind":"transform"}],
        "edges":[{"id":"e1","source":"a","target":"ghost"}]}}"#;

    let result = import_json(document, true, &kinds()).unwrap();
    assert!(result.graph.edges.is_empty());
    assert!(result
        .issues
        .iter()
        .any(|issue| issue.message.contains("not found")));
}

#[test]
fn an_unsupported_version_is_refused_outright() {
    let document = r#"{"version":99,"graph":{"nodes":[],"edges":[]}}"#;
    let result = import_json(document, false, &kinds()).unwrap();
    assert!(!result.ok);
    assert!(result.issues[0]
        .message
        .contains("Unsupported workflow schema version"));
}

#[test]
fn a_node_with_no_config_gets_its_kinds_defaults() {
    let document =
        r#"{"version":1,"graph":{"nodes":[{"id":"n","kind":"schedule_trigger"}],"edges":[]}}"#;
    let result = import_json(document, true, &kinds()).unwrap();
    let node = &result.graph.nodes[0];
    assert_eq!(
        node.config.get("timezone").and_then(Value::as_str),
        Some("UTC")
    );
}

#[test]
fn the_documents_kind_lands_in_the_one_kind_field() {
    let document =
        r#"{"version":1,"graph":{"nodes":[{"id":"n","kind":"@fancy/branch"}],"edges":[]}}"#;
    let result = import_json(document, true, &kinds()).unwrap();
    // The alias is preserved VERBATIM, not canonicalised: the document said
    // what it said, and lookup is alias-aware so nothing needs rewriting.
    assert_eq!(result.graph.nodes[0].kind.as_deref(), Some("@fancy/branch"));
}

// -- the round trip ------------------------------------------------------

#[test]
fn a_graph_survives_export_and_re_import() {
    let graph = FlowGraph {
        nodes: vec![
            FlowNode::new("t", "manual_trigger"),
            FlowNode::new("x", "transform").with_config("expression", Value::from("{{ $json.a }}")),
        ],
        edges: vec![FlowEdge::new("e1", "t", "x").to_port("in")],
    };

    let text = to_json(&graph, None);
    let back = import_json(&text, true, &kinds()).unwrap();

    assert!(back.ok, "{:?}", back.issues);
    assert_eq!(back.graph.nodes.len(), 2);
    assert_eq!(back.graph.edges.len(), 1);
    assert_eq!(
        back.graph
            .node("x")
            .unwrap()
            .config
            .get("expression")
            .and_then(Value::as_str),
        Some("{{ $json.a }}")
    );
}

#[test]
fn an_exported_document_declares_the_schema_it_conforms_to() {
    let exported = export_workflow(&FlowGraph::new(), None);
    assert_eq!(exported.get("version").and_then(Value::as_i64), Some(1));
    assert!(exported
        .get("$schema")
        .and_then(Value::as_str)
        .is_some_and(|url| url.ends_with("/workflow/v1.json")));
}

// -- the marketplace manifest -------------------------------------------

#[test]
fn a_bare_kind_id_is_refused_because_it_cannot_be_repaired_later() {
    // The one mistake that is unfixable: the ambiguous string is already
    // written into saved documents by the time anyone notices.
    let manifest = fancy_json::parse(r#"{"kind":"my_node","version":"1.0.0"}"#).unwrap();
    let issues = fancy_flow::marketplace::validate_manifest(&manifest);
    assert!(
        issues
            .iter()
            .any(|issue| issue.message.contains("must be namespaced")),
        "{issues:?}"
    );

    let good = fancy_json::parse(r#"{"kind":"@acme/my_node","version":"1.0.0"}"#).unwrap();
    assert!(fancy_flow::marketplace::validate_manifest(&good).is_empty());
}

#[test]
fn the_first_party_namespace_is_reserved() {
    let manifest =
        fancy_json::parse(r#"{"kind":"@particle-academy/my_node","version":"1.0.0"}"#).unwrap();
    let issues = fancy_flow::marketplace::validate_manifest(&manifest);
    assert!(
        issues
            .iter()
            .any(|issue| issue.message.contains("reserved")),
        "{issues:?}"
    );
    // A warning, not an error: the registry decides, and a first-party package
    // legitimately uses the namespace.
    assert!(fancy_flow::marketplace::manifest_is_usable(&issues));
}
