//! `transform`: the editor's defaults are admitted, and fields mode runs.
//!
//! The TypeScript editor writes `{mode: "fields", fields: [{key: "", value: ""}]}`
//! for a new transform node. This port declared only `expression`, and its
//! executor read `expression` alone — so fields mode, the editor's DEFAULT, did
//! not exist here and a graph built field by field passed its input straight
//! through. Reported by Maipa against the Python port, which had the same gap.
//!
//! Every expected output was read off `@particle-academy/fancy-flow` 0.82.0
//! (`transformPath` / `transformExecutor`), the reference for this kind.
//!
//! This port's `validate_config` is deliberately shallow (required fields and
//! select options only), so the admission rows assert what it CAN say: the
//! editor's defaults are admitted and an unknown `mode` is refused.

use fancy_json::{Map, Value};

use fancy_flow::nodes::support::ExecutorDeps;
use fancy_flow::registry::builtin;
use fancy_flow::{FlowRunner, NodeKindRegistry, RunOptions};

const EDITOR_DEFAULTS: &str = r#"{"mode":"fields","fields":[{"key":"","value":""}]}"#;

fn json(text: &str) -> Value {
    fancy_json::parse(text).expect("test JSON parses")
}

fn object(text: &str) -> Map {
    json(text).as_object().cloned().expect("an object")
}

fn kinds() -> NodeKindRegistry {
    let mut kinds = NodeKindRegistry::new();
    builtin::register(&mut kinds, true);
    kinds
}

/// Run `manual_trigger -> transform` with `config` (or none) on `payload`.
fn run(config: Option<&str>, payload: &str) -> Value {
    let node = match config {
        Some(config) => alloc_format(&format!(
            r#"{{"id":"tf","kind":"transform","position":{{"x":1,"y":0}},"config":{config}}}"#
        )),
        None => alloc_format(r#"{"id":"tf","kind":"transform","position":{"x":1,"y":0}}"#),
    };
    let document = json(&format!(
        r#"{{"version":1,"graph":{{"nodes":[{{"id":"t","kind":"manual_trigger","position":{{"x":0,"y":0}}}},{node}],"edges":[{{"id":"e1","source":"t","target":"tf"}}]}}}}"#
    ));
    let kinds = kinds();
    let imported = fancy_flow::import_workflow(&document, true, &kinds);

    let mut options = RunOptions::new();
    options.initial_inputs.insert("t".into(), object(payload));

    let deps = ExecutorDeps::default();
    let result = FlowRunner::new()
        .run(&imported.graph, &builtin::executors(&deps), &options)
        .expect("the run is not cancelled");
    assert!(result.ok, "{:?}", result.error);
    result.outputs.get("tf").cloned().expect("tf ran")
}

fn alloc_format(text: &str) -> String {
    String::from(text)
}

#[test]
fn the_defaults_are_exactly_what_the_editor_writes() {
    assert_eq!(
        Value::Object(kinds().default_config_for("transform")),
        json(EDITOR_DEFAULTS)
    );
}

#[test]
fn the_declared_keys_match_the_reference() {
    let kinds = kinds();
    let kind = kinds.get("transform").expect("registered");
    let schema: Vec<Value> = kind
        .config_schema
        .iter()
        .map(fancy_flow::ConfigField::to_value)
        .collect();
    let keys: Vec<&str> = schema
        .iter()
        .map(|f| f.get("key").and_then(Value::as_str).unwrap())
        .collect();
    assert_eq!(keys, ["mode", "fields", "expression"]);
    assert_eq!(schema[1].get("type"), Some(&Value::from("repeater")));
    assert_eq!(schema[1].get("skipBlankRows"), Some(&Value::Bool(true)));
    let rows = schema[1].get("fields").and_then(Value::as_array).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get("required"), Some(&Value::Bool(true)));
    assert_eq!(schema[2].get("required"), None);
}

#[test]
fn the_editor_defaults_are_admitted() {
    assert!(kinds()
        .validate_config("transform", &object(EDITOR_DEFAULTS))
        .is_empty());
}

#[test]
fn expression_mode_with_the_stale_default_row_is_admitted() {
    let config = object(
        r#"{"mode":"expression","fields":[{"key":"","value":""}],"expression":"{{ $json.a }}"}"#,
    );
    assert!(kinds().validate_config("transform", &config).is_empty());
}

#[test]
fn an_unknown_mode_is_refused() {
    let issues = kinds().validate_config("transform", &object(r#"{"mode":"rows","fields":[]}"#));
    let keys: Vec<&str> = issues.iter().map(|i| i.key.as_str()).collect();
    assert_eq!(keys, ["mode"]);
}

#[test]
fn the_untouched_default_passes_the_input_through() {
    assert_eq!(run(Some(EDITOR_DEFAULTS), r#"{"a":1}"#), json(r#"{"a":1}"#));
}

#[test]
fn a_node_saved_with_no_config_runs_the_defaults() {
    assert_eq!(run(None, r#"{"a":1}"#), json(r#"{"a":1}"#));
}

#[test]
fn fields_mode_builds_the_object_and_skips_blank_rows() {
    let config = r#"{"mode":"fields","fields":[{"key":"","value":""},{"key":" name ","value":"{{ $json.first }}"},{"key":"greeting","value":"Hi {{ $json.first }}"},{"key":"n","value":3},null,"x"]}"#;
    assert_eq!(
        run(Some(config), r#"{"first":"Ada"}"#),
        json(r#"{"name":"Ada","greeting":"Hi Ada","n":3}"#)
    );
}

#[test]
fn a_later_row_overwrites_an_earlier_one_with_the_same_key() {
    let config = r#"{"mode":"fields","fields":[{"key":"a","value":"1"},{"key":"a","value":"2"}]}"#;
    assert_eq!(run(Some(config), "{}"), json(r#"{"a":"2"}"#));
}

#[test]
fn expression_mode_ignores_stale_fields() {
    let config = r#"{"mode":"expression","fields":[{"key":"who","value":"{{ $json.user.name }}"}],"expression":"{{ $json.user }}"}"#;
    assert_eq!(
        run(Some(config), r#"{"user":{"name":"Ada"}}"#),
        json(r#"{"name":"Ada"}"#)
    );
}

#[test]
fn fields_mode_ignores_a_stale_expression() {
    let config = r#"{"mode":"fields","fields":[{"key":"who","value":"{{ $json.user.name }}"}],"expression":"{{ $json.user }}"}"#;
    assert_eq!(
        run(Some(config), r#"{"user":{"name":"Ada"}}"#),
        json(r#"{"who":"Ada"}"#)
    );
}

#[test]
fn expression_mode_with_a_blank_expression_passes_through() {
    assert_eq!(
        run(
            Some(r#"{"mode":"expression","expression":"   "}"#),
            r#"{"a":1}"#
        ),
        json(r#"{"a":1}"#)
    );
}

#[test]
fn no_mode_and_no_expression_falls_through_to_fields() {
    assert_eq!(
        run(
            Some(r#"{"fields":[{"key":"b","value":"{{ $json.a }}"}]}"#),
            r#"{"a":1}"#
        ),
        json(r#"{"b":1}"#)
    );
}

#[test]
fn an_unknown_mode_takes_the_fields_path_as_the_reference_does() {
    let config = r#"{"mode":"rows","fields":[{"key":"b","value":"{{ $json.a }}"}],"expression":"{{ $json }}"}"#;
    assert_eq!(run(Some(config), r#"{"a":1}"#), json(r#"{"b":1}"#));
}
