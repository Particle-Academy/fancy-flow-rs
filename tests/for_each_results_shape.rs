//! `for_each` says what a `results` entry looks like.
//!
//! Each entry is that iteration's node outputs KEYED BY NODE ID, and a failed
//! item's entry is null. An authoring agent reads this text, and a builder in
//! lab run 1406 wrote `{{ in.results.0.total }}` where the value is at
//! `results.0.score-1.total`. Kept identical in meaning to the TypeScript, PHP
//! and Python declarations.

use fancy_flow::registry::{builtin, NodeKind};

fn for_each() -> NodeKind {
    builtin::kinds()
        .into_iter()
        .find(|k| k.name.ends_with("for_each"))
        .expect("for_each is a builtin")
}

#[test]
fn the_results_field_names_the_keyed_shape() {
    let kind = for_each();
    let field = kind
        .output_fields()
        .expect("for_each declares its fields")
        .iter()
        .find(|f| f.path == "results")
        .expect("results is declared");
    let text = field.description.clone().unwrap_or_default();
    assert!(text.contains("keyed by node id"), "{text}");
    assert!(text.contains("results.0.<node-id>.<field>"), "{text}");
    assert!(text.contains("null"), "{text}");
}

#[test]
fn the_kind_description_says_the_same() {
    let text = for_each().description.unwrap_or_default();
    assert!(text.contains("keyed by node id"), "{text}");
    assert!(text.contains("results.0.<node-id>.<field>"), "{text}");
}
