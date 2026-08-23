//! Third-party node packages — the `list` / `search` / `get` side of what
//! `npx fancy-cli add node <kind>` installs.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use fancy_json::Value;

use crate::registry::kind_id;
use crate::schema::{ImportIssue, Severity};

/// Does `version` satisfy the range `spec`?
///
/// A deliberately small semver subset — `^ ~ >= > <= < =`, unions with `||`,
/// and `*`. Asserted against `suites/shared/satisfies-range` in
/// `fancy-conformance`, so this and its three twins cannot drift.
///
/// Note the **pre-1.0 caret rule**: below `1.0.0` a minor bump is breaking, so
/// `^0.5` means `0.5.x`. That is the range every pre-1.0 package in this suite
/// actually needs, and it is one of two rows in the shared table that
/// deliberately disagree with standard semver.
#[must_use]
pub fn satisfies_range(version: &str, spec: &str) -> bool {
    let spec = spec.trim();
    if spec == "*" || spec.is_empty() {
        return true;
    }

    let Some(actual) = parse_version(version.trim()) else {
        return false;
    };

    spec.split("||")
        .any(|clause| satisfies_clause(actual, clause.trim()))
}

type Semver = (u64, u64, u64);

/// `v?MAJOR.MINOR(.PATCH)?`, scanned by hand.
///
/// A pattern would be four lines shorter and a whole dependency heavier, and
/// this crate's tree is one crate on purpose.
fn parse_version(text: &str) -> Option<Semver> {
    let text = text.strip_prefix('v').unwrap_or(text);
    let mut parts = text.split('.');

    let major = leading_number(parts.next()?)?;
    // A version needs at least MAJOR.MINOR to be one; `1` alone is a clause,
    // not a version, and the shared table distinguishes them.
    let minor = leading_number(parts.next()?)?;
    let patch = parts.next().and_then(leading_number).unwrap_or(0);
    Some((major, minor, patch))
}

/// The leading run of digits, and `None` when there is not one.
fn leading_number(text: &str) -> Option<u64> {
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

fn satisfies_clause(actual: Semver, clause: &str) -> bool {
    let clause = clause.trim();

    let (operator, rest) = if let Some(rest) = clause.strip_prefix(">=") {
        (">=", rest)
    } else if let Some(rest) = clause.strip_prefix("<=") {
        ("<=", rest)
    } else if let Some(rest) = clause.strip_prefix('>') {
        (">", rest)
    } else if let Some(rest) = clause.strip_prefix('<') {
        ("<", rest)
    } else if let Some(rest) = clause.strip_prefix('^') {
        ("^", rest)
    } else if let Some(rest) = clause.strip_prefix('~') {
        ("~", rest)
    } else if let Some(rest) = clause.strip_prefix('=') {
        ("=", rest)
    } else {
        ("=", clause)
    };

    let rest = rest.trim().strip_prefix('v').unwrap_or_else(|| rest.trim());
    let mut parts = rest.split('.');
    let Some(major) = parts.next().and_then(leading_number) else {
        return false;
    };
    // A CLAUSE may omit minor and patch — `^1` is a range. A VERSION may not.
    let minor = parts.next().and_then(leading_number).unwrap_or(0);
    let patch = parts.next().and_then(leading_number).unwrap_or(0);
    let target: Semver = (major, minor, patch);

    match operator {
        ">=" => actual >= target,
        ">" => actual > target,
        "<=" => actual <= target,
        "<" => actual < target,
        "=" => actual == target,
        // Same major AND minor; patch may rise.
        "~" => actual >= target && actual.0 == target.0 && actual.1 == target.1,
        "^" => {
            if target.0 == 0 {
                // Pre-1.0: a minor bump is breaking, so `^0.5` is `0.5.x`.
                actual >= target && actual.0 == 0 && actual.1 == target.1
            } else {
                actual >= target && actual.0 == target.0
            }
        }
        _ => false,
    }
}

/// Validate a third-party node package's manifest.
///
/// Answers "is this manifest coherent and safely named?", nothing more.
#[must_use]
pub fn validate_manifest(manifest: &Value) -> Vec<ImportIssue> {
    let mut issues = Vec::new();

    let Some(map) = manifest.as_object() else {
        return alloc::vec![ImportIssue::error("Manifest is not an object.")];
    };

    match map.get("kind").and_then(Value::as_str) {
        None => issues.push(ImportIssue::error(
            "kind: Required - the canonical kind id this package provides.",
        )),
        Some(kind) if kind.trim().is_empty() => issues.push(ImportIssue::error(
            "kind: Required - the canonical kind id this package provides.",
        )),
        Some(kind) => {
            if !is_namespaced_kind(kind) {
                // The one mistake that cannot be repaired: the ambiguous string
                // is already written into saved documents.
                issues.push(ImportIssue::error(alloc::format!(
                    "kind: \"{kind}\" must be namespaced as @scope/name - a bare id makes stored \
                     graphs ambiguous, and that is unfixable once documents carry it."
                )));
            } else if kind.starts_with(kind_id::NAMESPACE) {
                issues.push(ImportIssue::warning(alloc::format!(
                    "kind: {}* is reserved for first-party nodes; the registry will reject this \
                     unless the package is first-party.",
                    kind_id::NAMESPACE
                )));
            }
        }
    }

    if map
        .get("version")
        .and_then(Value::as_str)
        .and_then(parse_version)
        .is_none()
    {
        issues.push(ImportIssue::error(
            "version: Required - a MAJOR.MINOR.PATCH version.",
        ));
    }

    if let Some(aliases) = map.get("aliases") {
        if aliases.as_array().is_none() {
            issues.push(ImportIssue::error("aliases: Must be a list of ids."));
        }
    }

    issues
}

/// `@scope/name`, both segments lowercase-ish and non-empty.
fn is_namespaced_kind(kind: &str) -> bool {
    let Some(rest) = kind.strip_prefix('@') else {
        return false;
    };
    let Some((scope, name)) = rest.split_once('/') else {
        return false;
    };
    is_segment(scope) && is_segment(name)
}

fn is_segment(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    text.chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

/// Whether any issue is an error rather than a warning.
#[must_use]
pub fn manifest_is_usable(issues: &[ImportIssue]) -> bool {
    !issues.iter().any(|issue| issue.severity == Severity::Error)
}

/// The canonical spelling of a manifest's kind id.
#[must_use]
pub fn canonical_kind(manifest: &Value) -> Option<String> {
    manifest
        .get("kind")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}
