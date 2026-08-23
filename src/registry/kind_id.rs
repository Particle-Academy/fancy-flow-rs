//! The naming convention for node-kind ids, and the only place it is spelled out.
//!
//! A kind's name is its CANONICAL id and is what gets written into saved
//! documents — so a bare name two packages could both claim is unfixable after
//! the fact: the ambiguous string is already in the document. Canonical ids are
//! therefore namespaced (`@particle-academy/llm_router`), and every previous
//! spelling stays registered as an ALIAS so graphs saved before a rename keep
//! opening.
//!
//! [`variants`] is the structural fallback for lookups with no registry to
//! consult; explicit aliases always take precedence over convention.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

/// The namespace every first-party kind is canonically written under.
pub const NAMESPACE: &str = "@particle-academy/";

/// The namespace shipped before the package name was settled.
pub const LEGACY_NAMESPACE: &str = "@fancy/";

/// Whether an id carries a namespace at all.
#[must_use]
pub fn is_namespaced(kind_id: &str) -> bool {
    kind_id.starts_with('@')
}

/// `manual_trigger` -> `@particle-academy/manual_trigger`. Idempotent.
#[must_use]
pub fn canonical(name: &str) -> String {
    if is_namespaced(name) {
        name.to_string()
    } else {
        alloc::format!("{NAMESPACE}{name}")
    }
}

/// `@particle-academy/manual_trigger` -> `manual_trigger`.
#[must_use]
pub fn bare(kind_id: &str) -> &str {
    if !is_namespaced(kind_id) {
        return kind_id;
    }
    match kind_id.rfind('/') {
        Some(slash) => &kind_id[slash + 1..],
        None => kind_id,
    }
}

/// The aliases a built-in kind keeps: its bare name and the legacy namespace.
#[must_use]
pub fn builtin_aliases(name: &str) -> Vec<String> {
    let bare = bare(name);
    vec![bare.to_string(), alloc::format!("{LEGACY_NAMESPACE}{bare}")]
}

/// Does `kind_id` name the built-in `bare_name` under any of its spellings?
///
/// Deliberately narrow: only the bare name and fancy-flow's own namespaces
/// match, so a third party's `@acme/note` is NOT mistaken for the builtin.
#[must_use]
pub fn matches(kind_id: &str, bare_name: &str) -> bool {
    kind_id == bare_name
        || kind_id.strip_prefix(NAMESPACE) == Some(bare_name)
        || kind_id.strip_prefix(LEGACY_NAMESPACE) == Some(bare_name)
}

/// Every id this one could also be written as, `kind_id` first.
///
/// Order is preference order: an exact match wins, then the canonical form,
/// then the older spellings.
#[must_use]
pub fn variants(kind_id: &str) -> Vec<String> {
    let bare = bare(kind_id);
    let ordered = [
        kind_id.to_string(),
        alloc::format!("{NAMESPACE}{bare}"),
        alloc::format!("{LEGACY_NAMESPACE}{bare}"),
        bare.to_string(),
    ];

    let mut seen: Vec<String> = Vec::with_capacity(4);
    for item in ordered {
        if !seen.contains(&item) {
            seen.push(item);
        }
    }
    seen
}
