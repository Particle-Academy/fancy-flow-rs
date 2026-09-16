//! The shared fixture tables, run against this runtime.
//!
//! The TypeScript, PHP and Python runtimes of fancy-flow run
//! the identical rows from the identical files. That is the whole mechanism:
//! four runtimes read one table, so a divergence is a red build in whichever
//! one drifted rather than a support ticket months later.
//!
//! **Loaded through the shared runner. Rows are never transcribed here.**
//! `satisfiesRange` was asserted against a hand-copied 17-row duplicate until
//! someone added a row to one copy and nothing anywhere reported it.

use fancy_conformance::{format_summary, run_table, Language, Summary};
use fancy_json::{Map, Value};

use fancy_flow::nodes::support::expr;
use fancy_flow::registry::{builtin, EmitsRelation, NodeKindRegistry, OutputShape};
use fancy_flow::RunIdentity;

/// The fixture set every table in this crate runs against -- rule 4 of
/// fancy-conformance's `runners/README.md`: print AND assert it.
///
/// Pinned at 0.22.1 on 2026-09-13, after re-running all five tables against the
/// `v0.22.1` tag: `shared/satisfies-range` 17, `shared/expr` 20,
/// `shared/flow-run-identity` 25, `flow/kind-declaration-surface` 19 (+1
/// documented skip), `flow/graph-runs` 23 -- nothing failed, the same counts as
/// 0.22.0 and 0.20.0. 0.22.1 exists so this crate can pin fancy-json by tag too:
/// the loader in 0.22.0 took fancy-json from `main`, and two refs of one crate
/// do not unify.
///
/// Moved 0.22.1 -> 0.23.0 on 2026-09-14, in the same change as the
/// fancy-flow-php#16 fix. 0.23.0 adds `shared/expr` 0021-0026 (a whole-string
/// expression is exactly one `{{ }}`), so that table is now 26; before the fix
/// in `whole_expression` this crate failed 0021, 0022, 0023, 0025 and 0026.
/// No other row changed: every table was re-run against the `v0.23.0` tag and
/// printed the other counts above, the one documented skip included.
///
/// Moved 0.23.0 -> 0.24.0 on 2026-09-14, in the same change as the fancy-flow#17
/// warnings. 0.24.0 adds `flow/run-diagnostics` (14 rows, run by
/// `tests/run_diagnostics.rs`); before the warnings existed this crate failed
/// its six warning rows and passed the eight silent ones. No existing row
/// changed.
///
/// Moved 0.24.0 -> 0.25.0 on 2026-09-14, in the same change as the durable
/// coordinator. 0.25.0 adds `flow/durable-dispatch` (14 rows, run by
/// `tests/durable_conformance.rs` over this crate's own `Frontier::compute` and
/// `select_dispatch`, all passing). Every existing table was re-run against the
/// `v0.25.0` tag and printed the counts above (expr 26, run-diagnostics 14), the
/// one documented skip included. That manifest's notes still say Rust is not
/// listed -- it was written before this crate had a coordinator -- which is a
/// fixture-repo edit, not something this crate can change.
///
/// `Cargo.toml` pulls `tag = "v<this>"`. Move the two together, and only after
/// re-running the tables; `cargo_pulls_the_fixture_tag_this_suite_pins` fails
/// otherwise. A pin that follows disk asserts nothing.
const PINNED_SUITE_VERSION: &str = "0.27.0";

#[test]
fn the_pinned_fixture_version_is_the_one_on_disk() {
    let on_disk = fancy_conformance::version()
        .expect("the fixtures must load; a missing checkout is a FAILURE, not a skip");

    // Printed unconditionally: "we are on an old fixture set" belongs in the
    // log, not in someone's inference months later.
    println!("fancy-conformance on disk: {on_disk}, pinned: {PINNED_SUITE_VERSION}");

    assert_eq!(
        on_disk, PINNED_SUITE_VERSION,
        "fancy-conformance is at {on_disk}, this crate pins {PINNED_SUITE_VERSION}. \
         Re-run the tables and move the pin deliberately."
    );
}

/// The `git` source keys of the `fancy-conformance` dependency line in a
/// `Cargo.toml`: `(tag, branch, rev)`. `None` when no such line exists.
///
/// Plain text on purpose -- a TOML parser would be a third-party crate for one
/// assertion, and this crate's tree is audit surface. The dependency is an
/// inline table on one line, which is how this manifest writes it.
fn conformance_git_source(
    manifest: &str,
) -> Option<(Option<String>, Option<String>, Option<String>)> {
    let line = manifest.lines().find(|line| {
        let line = line.trim_start();
        line.strip_prefix("fancy-conformance")
            .is_some_and(|rest| rest.trim_start().starts_with('='))
    })?;

    let key = |name: &str| {
        let (_, after) = line.split_once(&format!("{name} = \""))?;
        after.split_once('"').map(|(value, _)| String::from(value))
    };
    Some((key("tag"), key("branch"), key("rev")))
}

#[test]
fn the_manifest_parser_sees_a_branch_and_a_tag() {
    // The parser is test-only code, and test-only code that is wrong makes the
    // assertion below pass for the wrong reason.
    let branch = "# fancy-conformance = { tag = \"v9\" }\n\
                  fancy-conformance = { git = \"https://x\", branch = \"main\" }\n";
    assert_eq!(
        conformance_git_source(branch),
        Some((None, Some(String::from("main")), None))
    );

    let tag = "fancy-json = { tag = \"v0.9.9\" }\n\
               fancy-conformance = { git = \"https://x\", tag = \"v1.2.3\" }\n";
    assert_eq!(
        conformance_git_source(tag),
        Some((Some(String::from("v1.2.3")), None, None))
    );

    assert_eq!(conformance_git_source("fancy-json = \"0.1\"\n"), None);
}

#[test]
fn cargo_pulls_the_fixture_tag_this_suite_pins() {
    // `Cargo.lock` is not tracked (this is a library), so the dependency line
    // IS the pin. With `branch = "main"` every fresh clone and every CI run
    // resolved whatever `main` was that day, and a fixture release could turn
    // this build red for a reason no commit here caused.
    let source = conformance_git_source(include_str!("../Cargo.toml"))
        .expect("Cargo.toml declares no fancy-conformance dependency");

    let expected = format!("v{PINNED_SUITE_VERSION}");
    assert_eq!(
        source,
        (Some(expected.clone()), None, None),
        "Cargo.toml must pull fancy-conformance at `tag = \"{expected}\"` and nothing else \
         (found tag, branch, rev = {source:?}). Move the tag and PINNED_SUITE_VERSION together."
    );
}

/// Print the summary unconditionally — rule 3 — then assert.
fn expect_green(summary: &Summary, expected_cases: usize) {
    println!("{}", format_summary(summary));
    assert!(
        summary.ok,
        "{} diverges from the shared table",
        summary.suite
    );
    assert_eq!(
        summary.passed, expected_cases,
        "every case must actually run; a table that shrank is a table that stopped covering"
    );
    assert_eq!(summary.skipped, 0, "no case is skipped for Rust");
}

#[test]
fn expression_resolution_matches_every_peer() {
    // The rows that carry the weight are the truthiness ones. `"0"`, `"false"`
    // and `[]` are all truthy in JavaScript and falsy here; a branch node
    // reading a form value or a JSON body hits every one of them. A port that
    // forwards to native truthiness fails exactly those and nothing else, which
    // is the signal this table exists to produce.
    let summary = run_table("shared/expr", Language::Rust, None, |case| {
        let input = case.input();
        match case.function() {
            Some("evaluateExpression") => {
                let template = input.get("template").cloned().unwrap_or(Value::Null);
                let context = input.get("context").cloned().unwrap_or(Value::Null);
                Ok(expr::evaluate(&template, &context))
            }
            Some("truthy") => {
                let value = input.get("value").cloned().unwrap_or(Value::Null);
                Ok(Value::Bool(expr::truthy(&value)))
            }
            Some(other) => Err(alloc_string(other)),
            None => Err("case declares no fn".into()),
        }
    })
    .expect("the shared suite must load; a missing checkout is a FAILURE, not a skip");

    // 26 since fancy-conformance 0.23.0: 0021-0026 pin fancy-flow-php#16.
    expect_green(&summary, 26);
}

#[test]
fn run_identity_matches_every_peer() {
    // Two rows are a PAIR and only mean something read together: the same step
    // on attempt 1 and attempt 5 produces the SAME key. An implementation that
    // folds `attempt` into the key passes every other case in the table and
    // creates a second charge on the first timeout in production.
    let summary = run_table("shared/flow-run-identity", Language::Rust, None, |case| {
        let input = case.input();
        match case.function() {
            Some("stepKey") => {
                let run_key = input.get("runKey").and_then(Value::as_str).unwrap_or("");
                let node_id = input.get("nodeId").and_then(Value::as_str).unwrap_or("");
                let occurrence = input
                    .get("occurrence")
                    .filter(|value| !value.is_null())
                    .and_then(Value::as_u64);

                // `first_attempt_at` is required here and irrelevant to the
                // key — which is the point: `attempt` is carried for logging
                // and replay-safety, and is NOT part of the key.
                // The table's `path` holds ALREADY-RENDERED segments — its own
                // notes say so — so they are set verbatim. Feeding them through
                // `descend` would escape the `#` a repeated invocation already
                // rendered.
                let path: Vec<String> = input
                    .get("path")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .map(|s| String::from(s.as_str().unwrap_or("")))
                            .collect()
                    })
                    .unwrap_or_default();

                let mut identity = RunIdentity::new(run_key, 0).with_rendered_path(path);
                if let Some(attempt) = input.get("attempt").and_then(Value::as_u64) {
                    // A table attempt number that did not fit in a u32 would be
                    // a broken fixture, not a value to silently truncate.
                    let attempt = u32::try_from(attempt).map_err(|_| "attempt out of range")?;
                    identity = identity.with_attempt(attempt);
                }

                Ok(Value::from(identity.step_key(node_id, occurrence).as_str()))
            }
            Some("isReplaySafe") => {
                let attempt = input.get("attempt").and_then(Value::as_u64).unwrap_or(1);
                let first = millis_of(input.get("firstAttemptAt"))?;
                let now = millis_of(input.get("now"))?;
                let window = input
                    .get("windowSeconds")
                    .filter(|value| !value.is_null())
                    .and_then(Value::as_i64);

                let attempt = u32::try_from(attempt).map_err(|_| "attempt out of range")?;
                let identity = RunIdentity::new("run", first).with_attempt(attempt);
                Ok(Value::Bool(identity.is_replay_safe(now, window)))
            }
            Some(other) => Err(alloc_string(other)),
            None => Err("case declares no fn".into()),
        }
    })
    .expect("the shared suite must load");

    expect_green(&summary, 25);
}

/// Parse an ISO-8601 UTC instant into epoch milliseconds.
///
/// A tiny parser rather than a date crate: the table's instants are all
/// `YYYY-MM-DDTHH:MM:SS(.mmm)?Z`, and adding a dependency to read four fields
/// would double this crate's tree.
fn millis_of(value: Option<&Value>) -> Result<i64, String> {
    let text = value.and_then(Value::as_str).ok_or("not a timestamp")?;
    parse_iso8601_millis(text).ok_or_else(|| alloc_string(text))
}

fn parse_iso8601_millis(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> { text[from..to].parse().ok() };

    let year = num(0, 4)?;
    let month = num(5, 7)?;
    let day = num(8, 10)?;
    let hour = num(11, 13)?;
    let minute = num(14, 16)?;
    let second = num(17, 19)?;

    // Fractional seconds, to millisecond precision. `.5` is 500ms, not 5.
    let mut rest = &text[19..];
    let mut millis = 0_i64;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits: String = fraction.chars().take_while(char::is_ascii_digit).collect();
        rest = &fraction[digits.len()..];
        let mut scaled = digits.clone();
        scaled.truncate(3);
        while scaled.len() < 3 {
            scaled.push('0');
        }
        millis = scaled.parse().ok()?;
    }

    // The UTC offset. Dropping it made case 0024 (`+02:00`) pass for the WRONG
    // reason: the instant came out two hours late, `now` landed before
    // `firstAttemptAt`, and the clock-skew clamp rescued the verdict.
    let offset_seconds = match rest.as_bytes().first() {
        None | Some(b'Z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let body = &rest[1..];
            let (hours, minutes) = match body.split_once(':') {
                Some((h, m)) => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?),
                None if body.len() == 4 => (
                    body[..2].parse::<i64>().ok()?,
                    body[2..].parse::<i64>().ok()?,
                ),
                _ => return None,
            };
            let magnitude = hours * 3600 + minutes * 60;
            if *sign == b'-' {
                -magnitude
            } else {
                magnitude
            }
        }
        _ => return None,
    };

    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
        - offset_seconds;
    Some(seconds * 1000 + millis)
}

/// Howard Hinnant's `days_from_civil`, which is exact and has no table.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn alloc_string(text: &str) -> String {
    String::from(text)
}

#[test]
fn the_iso_parser_agrees_with_known_instants() {
    // The parser above is test-only code, and test-only code that is wrong
    // makes a conformance suite pass for the wrong reason. Three fixed points.
    // Derived by running a reference implementation, not by what the number
    // obviously is — the first draft of this test asserted a value 4 days out.
    assert_eq!(parse_iso8601_millis("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(
        parse_iso8601_millis("2026-08-23T00:00:00Z"),
        Some(1_787_443_200_000)
    );
    assert_eq!(
        parse_iso8601_millis("2026-08-23T00:00:00.250Z"),
        Some(1_787_443_200_250)
    );
    // `.5` is 500ms, not 5.
    assert_eq!(
        parse_iso8601_millis("2026-08-23T00:00:00.5Z"),
        Some(1_787_443_200_500)
    );
    // A leap day, because that is where a hand-rolled civil-date conversion goes wrong.
    assert_eq!(
        parse_iso8601_millis("2024-02-29T12:00:00Z"),
        Some(1_709_208_000_000)
    );
    // Offsets, in both directions. `02:00+02:00` IS midnight UTC — the row this
    // parser used to get wrong while still passing.
    assert_eq!(
        parse_iso8601_millis("2026-08-19T02:00:00+02:00"),
        parse_iso8601_millis("2026-08-19T00:00:00Z")
    );
    assert_eq!(
        parse_iso8601_millis("2026-08-18T22:00:00-02:00"),
        parse_iso8601_millis("2026-08-19T00:00:00Z")
    );
}

#[test]
fn semver_range_matching_matches_every_peer() {
    // A three-way duplicate that has NOT drifted, and the reason is this table:
    // `fancy-ui-cli`, `fancy-flow` and `fancy-flow-php` each carry the identical
    // case table in their own CI. It was asserted against a hand-copied 17-row
    // duplicate until someone added a row to one copy and nothing reported it.
    //
    // Two rows deliberately disagree with standard semver: below 1.0.0 a minor
    // bump is breaking, so `^0.5` means `0.5.x`.
    let summary = run_table("shared/satisfies-range", Language::Rust, None, |case| {
        let input = case.input();
        let version = input
            .get("version")
            .and_then(Value::as_str)
            .ok_or("no version")?;
        let range = input
            .get("range")
            .and_then(Value::as_str)
            .ok_or("no range")?;
        Ok(Value::Bool(fancy_flow::marketplace::satisfies_range(
            version, range,
        )))
    })
    .expect("the shared suite must load");

    expect_green(&summary, 17);
}

/// Parity of SURFACE -- what a kind DECLARES, not what the engine does.
///
/// Every other table here pins behaviour. Nothing pinned surface, and four
/// capabilities were found present in one runtime and absent in the others as a
/// result -- including `output_shape`, which this crate did not have at all.
/// In each of them ABSENT reads as a legitimate answer, so nothing reported it.
///
/// TypeScript is the SPECIFICATION for this table, not a peer: it ships no
/// executors, so its declarations cannot be checked against code. This crate
/// ships its own, so a disagreement here means THIS implementation is wrong --
/// read the executor before touching the fixture.
#[test]
fn kind_declaration_surface_matches_every_peer() {
    let mut registry = NodeKindRegistry::new();
    builtin::register(&mut registry, true);

    let summary = run_table(
        "flow/kind-declaration-surface",
        Language::Rust,
        None,
        |case| {
            let input = case.input();
            let kind_id = input
                .get("kind")
                .and_then(Value::as_str)
                .ok_or_else(|| alloc_string("case declares no kind"))?;

            let kind = registry
                .get(kind_id)
                .ok_or_else(|| format!("builtin `{kind_id}` is not registered"))?;

            // A config-dependent shape reports the MARKER, never a resolved
            // list: the table asks what the kind DECLARES, and "depends on
            // config" IS the declaration.
            let output_shape = match &kind.output_shape {
                None => Value::Null,
                Some(OutputShape::Dynamic) => Value::from("dynamic"),
                Some(OutputShape::Fields(fields)) => {
                    // A SET, not an ordered list. These come out of maps, and
                    // THIS crate inserts `count` before `items` where the peers
                    // do the reverse -- so asserting order would report a
                    // divergence that is not one.
                    let mut paths: Vec<String> = fields.iter().map(|f| f.path.clone()).collect();
                    paths.sort();
                    Value::Array(paths.into_iter().map(Value::from).collect())
                }
            };

            let emits = match &kind.emits {
                None => Value::Null,
                Some(EmitsRelation::Input) => Value::from("input"),
                Some(EmitsRelation::InputsMerged) => Value::from("inputs-merged"),
                Some(EmitsRelation::InputMapMerged) => Value::from("input-map-merged"),
                Some(EmitsRelation::Expression(key)) => {
                    Value::from(format!("expression:{key}").as_str())
                }
                // The peers resolve this from config; this crate carries a
                // MARKER, because NodeKind derives Clone + PartialEq and a boxed
                // closure is neither. So it cannot answer a per-config row, and
                // reports "dynamic" rather than null -- null would claim nobody
                // declared, about a kind that has. The shared table marks those
                // rows `dynamicIsConforming`, so this is agreement, not drift.
                Some(EmitsRelation::Dynamic) => Value::from("dynamic"),
            };

            let mut out = Map::new();
            out.insert("outputShape", output_shape);
            out.insert("emits", emits);
            Ok(Value::Object(out))
        },
    )
    .expect("the shared suite must load; a missing checkout is a FAILURE, not a skip");

    // 19, not 20: 0202 is skipped for Rust with a recorded reason -- this crate
    // cannot resolve a config-dependent relation in-process. expect_green
    // asserts skipped == 0, so this row is counted deliberately rather than
    // hidden by relaxing that check for the whole table.
    println!("{}", format_summary(&summary));
    assert!(
        summary.ok,
        "{} diverges from the shared table",
        summary.suite
    );
    assert_eq!(summary.passed, 19, "every runnable case must actually run");
    assert_eq!(
        summary.skipped, 1,
        "exactly one row is skipped, and it states why"
    );
}
