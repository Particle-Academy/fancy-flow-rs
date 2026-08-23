//! `{{ }}` resolution — the Rust twin of `FancyFlow\Nodes\Support\Expr` and of
//! `@particle-academy/fancy-flow`'s `evaluateExpression`.
//!
//! Deliberately NOT a general expression language, and it must not grow into
//! one: it resolves a dot-path against a context and nothing else — no
//! arithmetic, no comparisons, no calls. Hosts that want real expressions
//! override the executor.
//!
//! Divergence here is a correctness bug rather than a style difference: the
//! same graph is authored once and may run on any of the four runtimes.
//! `suites/shared/expr` in `particle-academy/fancy-conformance` is the fixture
//! table all four run, so parity is a test result instead of a claim.
//!
//! Two decisions are load-bearing:
//!
//! * **Scanning, not a pattern.** The TypeScript twin was rewritten to
//!   `indexOf` after two `CodeQL` `js/polynomial-redos` alerts, and the 2nd
//!   survived the obvious pattern fix. Scanning is also the only way to
//!   reproduce the peers' one odd corner exactly — see `whole_expression`.
//! * **Truthiness is PHP's, not the host language's.** `"0"`, `"false"` and
//!   `[]` are all truthy in JavaScript and falsy here; a branch node reading a
//!   form value or a JSON body hits every one of them.

use alloc::string::{String, ToString};

use fancy_json::{Map, Number, Value};

/// Strings the peer runtimes treat as false.
const FALSY_STRINGS: [&str; 6] = ["", "0", "false", "no", "off", "null"];

/// The inner text of a template that is exactly one expression, else `None`.
///
/// Note the deliberate corner: `{{a}}{{b}}` is a WHOLE expression whose path is
/// `a}}{{b` (which resolves to nothing), because the PHP pattern is end-anchored
/// and its lazy capture has to grow to reach the end. Every peer runtime does
/// this; reproducing it is the point.
fn whole_expression(trimmed: &str) -> Option<&str> {
    if trimmed.len() < 4 || !trimmed.starts_with("{{") || !trimmed.ends_with("}}") {
        return None;
    }
    Some(&trimmed[2..trimmed.len() - 2])
}

/// Replace every `{{ ... }}` run, left to right, in a single pass.
///
/// An unterminated `{{` is literal text — what the pattern did by simply not
/// matching, and the case an author hits constantly while typing.
fn interpolate(template: &str, context: &Value) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;

    loop {
        let Some(open_at) = rest.find("{{") else {
            out.push_str(rest);
            return out;
        };
        let Some(close_offset) = rest[open_at + 2..].find("}}") else {
            out.push_str(rest);
            return out;
        };
        let close_at = open_at + 2 + close_offset;

        out.push_str(&rest[..open_at]);
        out.push_str(&text(
            resolve_path(&rest[open_at + 2..close_at], context).as_ref(),
        ));
        rest = &rest[close_at + 2..];
    }
}

/// Resolve a dot-path against the context, honouring the `$json` alias.
///
/// `$json` and `$input` both point at the `in` port value when the context has
/// one, and at the whole context otherwise — the fallback that makes
/// `{{ $json.x }}` work on a trigger node with no upstream input.
///
/// A path that does not resolve returns `None`. Arrays are addressed by numeric
/// segment, matching PHP list access; nothing else is special-cased, which is
/// on purpose. JavaScript resolves `list.length` because arrays carry a
/// `length` property, and PHP does not — so neither does this.
#[must_use]
pub fn resolve_path(path: &str, context: &Value) -> Option<Value> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut segments = trimmed.split('.');
    let first = segments.next()?;

    let mut cursor: Value = if first == "$json" || first == "$input" {
        match context.get("in") {
            Some(value) => value.clone(),
            None => context.clone(),
        }
    } else {
        // Not an alias: the first segment is a real key, so walk it below.
        let mut cursor = context.clone();
        cursor = step(&cursor, first)?;
        cursor
    };

    for segment in segments {
        cursor = step(&cursor, segment)?;
    }
    Some(cursor)
}

fn step(cursor: &Value, segment: &str) -> Option<Value> {
    match cursor {
        Value::Object(map) => map.get(segment).cloned(),
        Value::Array(items) => {
            // Numeric segments only, and never negative: PHP list access has no
            // Python-style wrap-around, and a graph that behaved differently on
            // one runtime because of it would be very hard to spot.
            let index: usize = segment.parse().ok()?;
            items.get(index).cloned()
        }
        _ => None,
    }
}

/// Evaluate a template against a context.
///
/// A string that is EXACTLY one expression returns the resolved value with its
/// type intact — `{{ $json.count }}` gives a number, not `"3"`. Anything else
/// interpolates each run as text. That distinction is load-bearing: it is what
/// lets one config field carry either a value or a sentence.
///
/// Non-string templates pass through untouched, so this is safe to map over a
/// whole config object.
#[must_use]
pub fn evaluate(template: &Value, context: &Value) -> Value {
    let Some(text_template) = template.as_str() else {
        return template.clone();
    };

    let trimmed = text_template.trim();
    if let Some(inner) = whole_expression(trimmed) {
        return resolve_path(inner, context).unwrap_or(Value::Null);
    }

    Value::from(interpolate(text_template, context))
}

/// Evaluate against a map of inputs — the shape an executor holds.
#[must_use]
pub fn evaluate_in(template: Option<&Value>, inputs: &Map) -> Value {
    let context = Value::Object(inputs.clone());
    match template {
        Some(template) => evaluate(template, &context),
        None => Value::Null,
    }
}

/// Truthiness for branch / switch decisions — PHP's rules, not Rust's.
#[must_use]
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::String(text) => {
            let normalised = text.trim().to_ascii_lowercase();
            !FALSY_STRINGS.contains(&normalised.as_str())
        }
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
        Value::Number(number) => match number {
            Number::PosInt(value) => *value != 0,
            Number::NegInt(value) => *value != 0,
            Number::Float(value) => *value != 0.0,
        },
    }
}

/// Coerce a value to text the way interpolation does.
#[must_use]
pub fn text(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    match value {
        Value::Null => String::new(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::String(text) => text.clone(),
        Value::Number(number) => number_text(*number),
        // Separator-free, matching `JSON.stringify` and `json_encode`: an
        // interpolated object must read the same in a Slack message whichever
        // runtime sent it.
        other => fancy_json::to_string(other),
    }
}

fn number_text(number: Number) -> String {
    match number {
        Number::PosInt(value) => alloc::format!("{value}"),
        Number::NegInt(value) => alloc::format!("{value}"),
        Number::Float(value) => {
            // An integral float prints as an integer on every peer: `3.0`
            // interpolates as "3", not "3.0". Rust's `Display` already drops
            // the fraction, so this is only guarding the sign of -0.0.
            if value == 0.0 {
                return "0".to_string();
            }
            alloc::format!("{value}")
        }
    }
}
