//! What the builtin kinds declare they emit, and what they deliberately do not.
//!
//! Every declaration was read from THIS crate's executors and cited beside it.
//! None copied from the PHP, TypeScript or Python declarations: two
//! declarations agreeing is not evidence, and that is precisely how a
//! consumer's hand-maintained table drifted into refusing a legitimate field
//! while accepting one that did not exist.

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
        "schedule_trigger",
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
