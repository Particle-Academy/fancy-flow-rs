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

// -- a whitespace-only key is blank, in admission as in the executor ---------
//
// `{"key": "   ", "value": "Configured synthetic value"}` was admitted, then
// ran as a passthrough: the executor trims the key, finds it blank and skips
// the row, so the value vanished. Reported by Maipa against the published TS
// 0.82.0 and PHP 0.65.0; here `validate_config` did not walk rows at all, so
// even an EMPTY key with a value was admitted. It now checks a repeater row's
// required cells, judging a `skip_blank_rows` row's text cells by the
// executor's own `js_trim`.

/// A fields-mode config with one row; `key` and `value` are spliced in as JSON
/// string CONTENT (raw non-ASCII is legal JSON; ASCII controls go in escaped).
fn one_row_text(key: &str, value: &str) -> String {
    format!(r#"{{"mode":"fields","fields":[{{"key":"{key}","value":"{value}"}}]}}"#)
}

fn one_row(key: &str, value: &str) -> Map {
    object(&one_row_text(key, value))
}

fn issue_messages(config: &Map) -> Vec<(String, String)> {
    kinds()
        .validate_config("transform", config)
        .into_iter()
        .map(|i| (i.key, i.message))
        .collect()
}

fn key_required() -> Vec<(String, String)> {
    vec![(
        String::from("fields"),
        String::from("Output fields item 1: Key is required."),
    )]
}

/// Whitespace the executor trims. The non-ASCII ones are built from code
/// points so no editor or shell can collapse an escape on the way in.
fn blank_keys() -> Vec<String> {
    let mut keys: Vec<String> = [r"\t", r"\n", r" \r\n "]
        .iter()
        .map(|k| String::from(*k))
        .collect();
    for code in [0x00a0_u32, 0x2003, 0xfeff, 0x3000] {
        keys.push(char::from_u32(code).unwrap().to_string());
    }
    keys
}

#[test]
fn a_whitespace_only_key_with_a_value_is_refused() {
    assert_eq!(
        issue_messages(&one_row("   ", "Configured synthetic value")),
        key_required()
    );
}

#[test]
fn an_empty_key_with_a_value_is_refused() {
    assert_eq!(
        issue_messages(&one_row("", "{{ $json.a }}")),
        key_required()
    );
}

#[test]
fn every_whitespace_the_executor_trims_is_refused() {
    for key in blank_keys() {
        assert_eq!(
            issue_messages(&one_row(&key, "x")),
            key_required(),
            "key {key:?}"
        );
        // ...and the executor really does drop that row: same helper, same verdict.
        assert_eq!(
            run(Some(&one_row_text(&key, "x")), r#"{"a":1}"#),
            json(r#"{"a":1}"#),
            "key {key:?}"
        );
    }
}

#[test]
fn a_whitespace_only_key_with_an_empty_value_is_a_blank_row() {
    assert!(issue_messages(&one_row("   ", "")).is_empty());
    let no_value = object(r#"{"mode":"fields","fields":[{"key":"\t"}]}"#);
    assert!(issue_messages(&no_value).is_empty());
}

#[test]
fn a_padded_key_is_admitted_and_runs_under_the_trimmed_key() {
    assert!(issue_messages(&one_row("  name ", "{{ $json.first }}")).is_empty());
    assert_eq!(
        run(
            Some(&one_row_text("  name ", "{{ $json.first }}")),
            r#"{"first":"Ada"}"#
        ),
        json(r#"{"name":"Ada"}"#)
    );
}

#[test]
fn the_value_is_judged_untrimmed() {
    assert!(issue_messages(&one_row("pad", "  ")).is_empty());
    assert_eq!(issue_messages(&one_row("", "  ")), key_required());
}

#[test]
fn a_repeater_that_did_not_opt_in_does_not_trim() {
    let mut kinds = NodeKindRegistry::new();
    kinds.register(
        fancy_flow::NodeKind::new("acme/rows", "logic", "Rows").config(vec![
            fancy_flow::ConfigField::new("repeater", "rows", "Rows").rows_of(vec![
                fancy_flow::ConfigField::new("text", "key", "Key").required(),
            ]),
        ]),
    );
    let padded = object(r#"{"rows":[{"key":"  "}]}"#);
    assert!(kinds.validate_config("acme/rows", &padded).is_empty());
    // An EMPTY required cell is refused there: walking rows is general.
    let empty = object(r#"{"rows":[{"key":""}]}"#);
    let keys: Vec<String> = kinds
        .validate_config("acme/rows", &empty)
        .into_iter()
        .map(|i| i.key)
        .collect();
    assert_eq!(keys, ["rows"]);
}
