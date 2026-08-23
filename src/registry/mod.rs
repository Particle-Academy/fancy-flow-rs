//! The catalogue of authorable node kinds.

pub mod builtin;
pub mod kind_id;
mod node_kind;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use fancy_json::{Map, Value};

pub use node_kind::{ConfigField, NodeKind};

/// A problem with one config key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssue {
    /// The config key.
    pub key: String,
    /// What is wrong with it.
    pub message: String,
}

/// Kind id -> kind, with alias-aware lookup.
///
/// # Every id a kind answers to is a key
///
/// Canonical ids are namespaced (`@particle-academy/user_input`) while a host
/// may have registered or referenced the bare name. Resolving only the literal
/// string turns a rename into a breaking change in disguise, so registration
/// indexes **all** of a kind's ids and lookup tries every spelling.
#[derive(Debug, Clone, Default)]
pub struct NodeKindRegistry {
    kinds: Vec<NodeKind>,
    /// Every id (canonical + aliases + convention variants) -> index into `kinds`.
    index: BTreeMap<String, usize>,
}

impl NodeKindRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a kind under every id it answers to.
    ///
    /// Re-registering the same canonical name replaces the kind, so a host can
    /// override a builtin.
    pub fn register(&mut self, kind: NodeKind) -> &mut Self {
        let at = if let Some(&existing) = self.index.get(&kind.name) {
            self.kinds[existing] = kind;
            existing
        } else {
            self.kinds.push(kind);
            self.kinds.len() - 1
        };

        for id in self.id_spellings(at) {
            self.index.insert(id, at);
        }
        self
    }

    fn id_spellings(&self, at: usize) -> Vec<String> {
        let kind = &self.kinds[at];
        let mut out: Vec<String> = Vec::new();
        for id in kind.ids() {
            for variant in kind_id::variants(&id) {
                if !out.contains(&variant) {
                    out.push(variant);
                }
            }
        }
        out
    }

    /// Look a kind up under any of its spellings.
    #[must_use]
    pub fn get(&self, kind_id: &str) -> Option<&NodeKind> {
        if let Some(&at) = self.index.get(kind_id) {
            return Some(&self.kinds[at]);
        }
        kind_id::variants(kind_id)
            .iter()
            .find_map(|variant| self.index.get(variant.as_str()))
            .map(|&at| &self.kinds[at])
    }

    /// Whether a kind is registered under any of its spellings.
    #[must_use]
    pub fn has(&self, kind_id: &str) -> bool {
        self.get(kind_id).is_some()
    }

    /// Every id the kind named by `kind_id` answers to; empty when unknown.
    #[must_use]
    pub fn ids_for(&self, kind_id: &str) -> Vec<String> {
        self.get(kind_id).map(NodeKind::ids).unwrap_or_default()
    }

    /// Every registered kind, in registration order.
    #[must_use]
    pub fn all(&self) -> &[NodeKind] {
        &self.kinds
    }

    /// How many kinds are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    /// Whether nothing is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// The default config a new node of `kind_id` starts with.
    #[must_use]
    pub fn default_config_for(&self, kind_id: &str) -> Map {
        self.get(kind_id)
            .map(NodeKind::resolved_default_config)
            .unwrap_or_default()
    }

    /// Check a config against a kind's schema.
    ///
    /// Reports a missing required field and a `select` whose value is not one
    /// of its options. Deliberately shallow: this answers "is this coherent?",
    /// not "is this safe?" — the latter is [`GraphPolicy`], and conflating them
    /// is how a payload gets treated as a document.
    ///
    /// [`GraphPolicy`]: crate::security::GraphPolicy
    #[must_use]
    pub fn validate_config(&self, kind_id: &str, config: &Map) -> Vec<ConfigIssue> {
        let Some(kind) = self.get(kind_id) else {
            return Vec::new();
        };

        let mut issues = Vec::new();
        for field in &kind.config_schema {
            // PHP `??` semantics: an explicit null is absent.
            let present = !matches!(config.get(&field.key), None | Some(Value::Null));

            if field.required && !present {
                issues.push(ConfigIssue {
                    key: field.key.clone(),
                    message: alloc::format!("{} is required.", field.label),
                });
                continue;
            }

            if !present || field.options.is_empty() {
                continue;
            }

            if let Some(chosen) = config.get(&field.key).and_then(Value::as_str) {
                if !field.options.iter().any(|(value, _)| value == chosen) {
                    issues.push(ConfigIssue {
                        key: field.key.clone(),
                        message: alloc::format!(
                            "\"{chosen}\" is not one of the options for {}.",
                            field.label
                        ),
                    });
                }
            }
        }
        issues
    }

    /// The registry as the editor's catalogue document.
    #[must_use]
    pub fn to_value(&self) -> Value {
        Value::Array(self.kinds.iter().map(NodeKind::to_value).collect())
    }
}

impl FromIterator<NodeKind> for NodeKindRegistry {
    fn from_iter<I: IntoIterator<Item = NodeKind>>(iter: I) -> Self {
        let mut registry = Self::new();
        for kind in iter {
            registry.register(kind);
        }
        registry
    }
}

/// Categories the engine treats specially.
pub mod category {
    /// Never executed; the engine walks straight past it.
    pub const ANNOTATION: &str = "annotation";
    /// A swimlane or pool. Visual grouping only; edges cross lanes freely.
    pub const LAYOUT: &str = "layout";
    /// A terminal node.
    pub const OUTPUT: &str = "output";
}

impl NodeKind {
    /// Whether the engine must walk past this kind without executing it.
    ///
    /// Notes and layout nodes are visual only. Their config — a note's text, a
    /// lane's title — stays in the document for editors and MCP tools, but a
    /// runner never sees them, and grouping never affects topology.
    #[must_use]
    pub fn is_visual_only(&self) -> bool {
        self.category == category::ANNOTATION || self.category == category::LAYOUT
    }
}

pub(crate) fn dedup_push(into: &mut Vec<String>, value: String) {
    if !into.contains(&value) {
        into.push(value);
    }
}
