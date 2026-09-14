//! A template that starts with `{{` and ends with `}}` but holds several
//! references (fancy-flow-php#16).
//!
//! `whole_expression` used to ask only whether the trimmed template starts with
//! `{{` and ends with `}}`, so `{{ in.text }} --- {{ user.transcript }}` was ONE
//! path, `in.text }} --- {{ user.transcript`, which resolves to nothing: the
//! template evaluated to null and a document node wrote nothing, with every
//! reference valid.
//!
//! It was documented as a deliberate corner and mirrored in all four runtimes,
//! which is why no parity table could catch it. The whole-string branch now
//! applies only to exactly ONE expression; anything else interpolates.
//!
//! Mirrors fancy-flow-php 0.52.2's `TemplateWithSeveralReferencesTest` for the
//! one policy this crate has: an unresolved path is empty (interpolated) or
//! null (whole expression). There is no Keep or Throw here to port.

use fancy_json::Value;

use fancy_flow::nodes::support::expr;

fn context() -> Value {
    fancy_json::parse(
        r#"{"in":{"text":"SUMMARY"},"user":{"transcript":"TRANSCRIPT","title":"Call"},"a":1,"b":2}"#,
    )
    .unwrap()
}

fn evaluate(template: &str) -> Value {
    expr::evaluate(&Value::from(template), &context())
}

#[test]
fn interpolates_every_reference_of_a_template_that_starts_and_ends_with_one() {
    assert_eq!(
        evaluate("{{ in.text }}\n\n---\n\n## Original\n\n{{ user.transcript }}"),
        Value::from("SUMMARY\n\n---\n\n## Original\n\nTRANSCRIPT")
    );
    assert_eq!(
        evaluate("{{ user.title }} - {{ in.text }}"),
        Value::from("Call - SUMMARY")
    );
    assert_eq!(
        evaluate("Summary: {{ in.text }} --- {{ user.transcript }}"),
        Value::from("Summary: SUMMARY --- TRANSCRIPT")
    );
}

#[test]
fn adjacent_references_are_two_references_not_one_path_spanning_them() {
    assert_eq!(evaluate("{{ a }}{{ b }}"), Value::from("12"));
}

#[test]
fn still_returns_the_typed_value_for_exactly_one_expression() {
    assert_eq!(evaluate("{{ in.text }}"), Value::from("SUMMARY"));
    assert_eq!(evaluate(" {{ a }} "), fancy_json::parse("1").unwrap());
}

#[test]
fn an_inner_opening_brace_is_not_one_expression_either() {
    // The rule's second condition. `{{ a {{ b }}` is malformed; the scan pairs
    // the first `{{` with the first `}}`, and that path does not resolve, so it
    // interpolates to empty -- never the whole-string branch's null.
    assert_eq!(evaluate("{{ a {{ b }}"), Value::from(""));
}

#[test]
fn one_unresolved_reference_of_several_empties_only_itself() {
    assert_eq!(
        evaluate("{{ in.text }} / {{ in.nope }}"),
        Value::from("SUMMARY / ")
    );
}
