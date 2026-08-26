//! What the builtin kinds declare they emit, and what they deliberately do not.
//!
//! Every declaration was read from THIS crate's executors and cited beside it.
//! None copied from the PHP, TypeScript or Python declarations: two
//! declarations agreeing is not evidence, and that is precisely how a
//! consumer's hand-maintained table drifted into refusing a legitimate field
//! while accepting one that did not exist.

use fancy_flow::registry::EmitsRelation;
use fancy_flow::registry::{builtin, NodeKindRegistry};

fn registry() -> NodeKindRegistry {
    let mut r = NodeKindRegistry::new();
    builtin::register(&mut r, true);
    r
}

fn paths(name: &str) -> Option<Vec<String>> {
    let r = registry();
    let kind = r
        .get(name)
        .unwrap_or_else(|| panic!("builtin `{name}` is not registered"));
    kind.output_fields()
        .map(|fields| fields.iter().map(|f| f.path.clone()).collect())
}

#[test]
fn declares_the_fields_of_kinds_whose_output_is_enumerable() {
    let cases: &[(&str, &[&str])] = &[
        ("notify", &["sent", "channel", "to", "message"]),
        ("webhook_out", &["sent", "status", "response"]),
        ("for_each", &["items", "count"]),
        ("wait", &["waited", "duration", "input"]),
        ("log", &["logged", "level"]),
        ("embed_search", &["query", "matches"]),
    ];

    for (name, expected) in cases {
        let got = paths(name).unwrap_or_else(|| panic!("`{name}` should declare a shape"));
        assert_eq!(got, *expected, "`{name}` fields");
    }
}

#[test]
fn config_dependent_kinds_say_so_rather_than_staying_silent() {
    // `Dynamic` is not "emits nothing" and not "nobody declared". A reader must
    // tell all three apart, because the right response differs: ask the host,
    // fall back to your own knowledge, or refuse.
    let r = registry();

    for name in ["llm_call", "user_input"] {
        let kind = r
            .get(name)
            .unwrap_or_else(|| panic!("`{name}` is not registered"));
        assert!(
            kind.has_dynamic_output_shape(),
            "`{name}` should report as dynamic"
        );
        // Dynamic yields no field list HERE -- that is the point, not a bug.
        assert!(
            kind.output_fields().is_none(),
            "`{name}` must not hand out a fixed list"
        );
    }

    let notify = r.get("notify").expect("notify");
    assert!(!notify.has_dynamic_output_shape());
}

#[test]
fn pass_through_kinds_stay_undeclared_rather_than_guessing() {
    // They emit whatever arrived, so their shape is not knowable from the kind
    // alone. Undeclared is the honest answer, and a reader must treat it as
    // "unknown, do not refuse" -- never as "emits nothing".
    //
    // `schedule_trigger` is the sharp one: the reference executors merge their
    // inputs into the TOP level, so a partial list of ["cron", "timezone"]
    // would make a validator refuse every merged-in key. A partial static list
    // on a merging kind is a false-rejection generator, and a false rejection
    // is one an author cannot comply with.
    for name in [
        "branch",
        "switch_case",
        "output",
        "transform",
        "merge",
        "manual_trigger",
        "webhook_trigger",
        "human_approval",
        "variable",
        // schedule_trigger LEFT this list when `emits` arrived: a partial
        // ["cron", "timezone"] list was unsafe only while nothing could say the
        // inputs also merge. With EmitsRelation::InputsMerged beside it, the two
        // are complete together.
    ] {
        let r = registry();
        let kind = r
            .get(name)
            .unwrap_or_else(|| panic!("`{name}` is not registered"));
        assert!(
            kind.output_shape.is_none(),
            "`{name}` passes input through; declaring a shape would cause false refusals"
        );
    }
}

#[test]
fn no_declared_field_has_an_empty_path() {
    // An empty path is unaddressable, so it can only ever be noise a reader has
    // to special-case.
    let r = registry();
    for kind in r.all() {
        if let Some(fields) = kind.output_fields() {
            for field in fields {
                assert!(
                    !field.path.is_empty(),
                    "`{}` declared a field with no path",
                    kind.name
                );
            }
        }
    }
}

#[test]
fn declares_the_relation_where_a_field_list_cannot() {
    // Each read from this crate's executor and checked for MERGE vs NEST before
    // being assigned -- a relation with no destination can only describe a
    // top-level merge.
    let cases: &[(&str, EmitsRelation)] = &[
        ("branch", EmitsRelation::Input),
        ("switch_case", EmitsRelation::Input),
        ("output", EmitsRelation::Input),
        ("human_approval", EmitsRelation::Input),
        ("manual_trigger", EmitsRelation::Input),
        ("variable", EmitsRelation::Expression("value".to_string())),
        ("schedule_trigger", EmitsRelation::InputsMerged),
        // Config-dependent, so a marker here rather than a closure -- the same
        // shape the peers decay to across a JSON manifest.
        ("transform", EmitsRelation::Dynamic),
        ("merge", EmitsRelation::Dynamic),
    ];

    for (name, expected) in cases {
        let r = registry();
        let kind = r
            .get(name)
            .unwrap_or_else(|| panic!("`{name}` is not registered"));
        assert_eq!(kind.emits.as_ref(), Some(expected), "`{name}` relation");
    }
}

#[test]
fn an_expression_relation_names_its_own_config_key() {
    // `transform` reads `expression`; `variable` reads `value`. A consumer
    // hardcoding "the field called expression" has copied our knowledge one
    // level down -- the thing this removes.
    let r = registry();
    assert_eq!(
        r.get("variable").unwrap().expression_config_key(),
        Some("value")
    );
    assert_eq!(r.get("branch").unwrap().expression_config_key(), None);
}

#[test]
fn wait_and_webhook_trigger_declare_no_relation() {
    // `wait` NESTS its input under a key, so a relation would make a reader
    // accept {{ in.<any inbound field> }} at top level and resolve to nothing
    // at run time. `webhook_trigger`'s choice is DATA-dependent, not
    // config-dependent, so no relation is honest for it either.
    let r = registry();
    assert_eq!(r.get("wait").unwrap().emits, None);
    assert_eq!(r.get("webhook_trigger").unwrap().emits, None);

    // wait still declares its FIELDS -- that is the alternative, not a gap.
    assert_eq!(
        paths("wait"),
        Some(vec!["waited".into(), "duration".into(), "input".into()])
    );
}
