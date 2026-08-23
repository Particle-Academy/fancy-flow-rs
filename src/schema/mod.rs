//! `WorkflowSchema` v1 shapes — the Rust twins of `FancyFlow\Schema\*`.
//!
//! Plain data. The graph is the wire format four runtimes share, so nothing
//! here may carry behaviour a peer runtime does not also have: if a method
//! decides anything, it belongs in the engine.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use fancy_json::{Map, Value};

/// A connection point on a node.
///
/// `id` is what an edge references through `source_handle` / `target_handle`.
/// The default input port is `in` and the default output port is `out`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortDescriptor {
    /// The port id an edge names.
    pub id: String,
    /// A human label for the canvas.
    pub label: Option<String>,
    /// An optional type hint.
    pub kind: Option<String>,
}

impl PortDescriptor {
    /// A port with just an id.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: None,
            kind: None,
        }
    }

    /// A port with an id and a label.
    #[must_use]
    pub fn labelled(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: Some(label.into()),
            kind: None,
        }
    }

    /// Read one from a `WorkflowSchema` fragment.
    #[must_use]
    pub fn from_value(raw: &Value) -> Self {
        Self {
            id: string_at(raw, "id").unwrap_or_else(|| "out".to_string()),
            label: string_at(raw, "label"),
            kind: string_at(raw, "type"),
        }
    }

    /// Write one back out.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("id", Value::from(self.id.as_str()));
        if let Some(label) = &self.label {
            map.insert("label", Value::from(label.as_str()));
        }
        if let Some(kind) = &self.kind {
            map.insert("type", Value::from(kind.as_str()));
        }
        Value::Object(map)
    }
}

/// A runtime node.
///
/// # There is exactly one kind field
///
/// `kind` is the registry kind id (`@particle-academy/branch`, or a bare
/// alias). The TypeScript side stores that value in **two** places — the xyflow
/// node `type` and `data.kind` — and its executor lookup consulted only the
/// first, so a registry keyed by kind simply never fired. Nothing said so,
/// because an unregistered kind fails closed with no outputs. It was fixed in
/// `fancy-flow` 0.48.1.
///
/// This port has no second place for a kind to hide: the importer maps the
/// document's `kind` onto this one field, exactly as `fancy-flow-py` does. Rust
/// forces the better name too — `type` is a keyword.
///
/// # `inputs` / `outputs` are three-state
///
/// `None` means "no ports declared" and the engine falls back; `Some(vec![])`
/// means "explicitly no ports" (a terminal node). Collapsing those two is how a
/// terminal node starts publishing on `out` — or a branch node stops branching.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowNode {
    /// Unique within the graph.
    pub id: String,
    /// The registry kind id.
    pub kind: Option<String>,
    /// Canvas position. Never affects execution.
    pub x: f64,
    /// Canvas position. Never affects execution.
    pub y: f64,
    /// Display label.
    pub label: Option<String>,
    /// Longer description.
    pub description: Option<String>,
    /// The node's resolved config.
    pub config: Map,
    /// Declared input ports. See the type docs for why this is three-state.
    pub inputs: Option<Vec<PortDescriptor>>,
    /// Declared output ports. See the type docs for why this is three-state.
    pub outputs: Option<Vec<PortDescriptor>>,
}

impl FlowNode {
    /// A node with an id and a kind.
    #[must_use]
    pub fn new(id: impl Into<String>, kind: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: Some(kind.into()),
            x: 0.0,
            y: 0.0,
            label: None,
            description: None,
            config: Map::new(),
            inputs: None,
            outputs: None,
        }
    }

    /// Set one config key, builder-style.
    #[must_use]
    pub fn with_config(mut self, key: impl Into<String>, value: Value) -> Self {
        self.config.insert(key, value);
        self
    }

    /// Declare the output ports, builder-style.
    #[must_use]
    pub fn with_outputs(mut self, outputs: Vec<PortDescriptor>) -> Self {
        self.outputs = Some(outputs);
        self
    }

    /// Read one config key, with the peers' `??` semantics — **null means absent**.
    ///
    /// PHP's `??` and Python's `is None` check both treat an explicit null as
    /// "not set", and a graph authored on one runtime must behave the same on
    /// this one.
    #[must_use]
    pub fn option<'a>(&'a self, key: &str, default: &'a Value) -> &'a Value {
        match self.config.get(key) {
            Some(Value::Null) | None => default,
            Some(value) => value,
        }
    }

    /// Read one config key as a string, honouring the `??` rule above.
    #[must_use]
    pub fn option_str(&self, key: &str) -> Option<&str> {
        match self.config.get(key) {
            Some(Value::String(text)) => Some(text.as_str()),
            _ => None,
        }
    }
}

/// A directed connection between two nodes' ports.
///
/// With `source_handle` / `target_handle` omitted the engine reads `out` on the
/// source and writes `in` on the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowEdge {
    /// Unique within the graph.
    pub id: String,
    /// Source node id.
    pub source: String,
    /// Target node id.
    pub target: String,
    /// Source port; `out` when absent.
    pub source_handle: Option<String>,
    /// Target port; `in` when absent.
    pub target_handle: Option<String>,
    /// Optional edge label.
    pub label: Option<String>,
}

impl FlowEdge {
    /// An edge from one node's default output to another's default input.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        source: impl Into<String>,
        target: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            source: source.into(),
            target: target.into(),
            source_handle: None,
            target_handle: None,
            label: None,
        }
    }

    /// Leave a named source port instead of `out`.
    #[must_use]
    pub fn from_port(mut self, handle: impl Into<String>) -> Self {
        self.source_handle = Some(handle.into());
        self
    }

    /// Arrive on a named target port instead of `in`.
    #[must_use]
    pub fn to_port(mut self, handle: impl Into<String>) -> Self {
        self.target_handle = Some(handle.into());
        self
    }

    /// The source port this edge actually reads.
    #[must_use]
    pub fn source_port(&self) -> &str {
        self.source_handle.as_deref().unwrap_or("out")
    }

    /// The target port this edge actually writes.
    #[must_use]
    pub fn target_port(&self) -> &str {
        self.target_handle.as_deref().unwrap_or("in")
    }
}

/// Nodes plus edges — the unit a host persists and the engine executes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FlowGraph {
    /// The nodes, in document order.
    pub nodes: Vec<FlowNode>,
    /// The edges, in document order.
    pub edges: Vec<FlowEdge>,
}

impl FlowGraph {
    /// An empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// One node by id.
    #[must_use]
    pub fn node(&self, id: &str) -> Option<&FlowNode> {
        self.nodes.iter().find(|node| node.id == id)
    }
}

/// The portable `metadata` block of a `WorkflowSchema` v1 document.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkflowMetadata {
    /// Stable workflow id.
    pub id: Option<String>,
    /// Display name.
    pub name: Option<String>,
    /// Longer description.
    pub description: Option<String>,
    /// Unix milliseconds.
    pub created_at: Option<i64>,
    /// Unix milliseconds.
    pub updated_at: Option<i64>,
    /// Who authored it.
    pub author: Option<String>,
    /// Free-form tags.
    pub tags: Option<Vec<String>>,
}

impl WorkflowMetadata {
    /// Read the metadata block.
    #[must_use]
    pub fn from_value(raw: &Value) -> Self {
        Self {
            id: string_at(raw, "id"),
            name: string_at(raw, "name"),
            description: string_at(raw, "description"),
            created_at: raw.get("createdAt").and_then(Value::as_i64),
            updated_at: raw.get("updatedAt").and_then(Value::as_i64),
            author: string_at(raw, "author"),
            tags: raw.get("tags").and_then(Value::as_array).map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(ToString::to_string))
                    .collect()
            }),
        }
    }

    /// Write it back out, omitting anything absent.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        let pairs: [(&str, Option<Value>); 6] = [
            ("id", self.id.as_deref().map(Value::from)),
            ("name", self.name.as_deref().map(Value::from)),
            ("description", self.description.as_deref().map(Value::from)),
            ("createdAt", self.created_at.map(Value::from)),
            ("updatedAt", self.updated_at.map(Value::from)),
            ("author", self.author.as_deref().map(Value::from)),
        ];
        for (key, value) in pairs {
            if let Some(value) = value {
                map.insert(key, value);
            }
        }
        if let Some(tags) = &self.tags {
            map.insert(
                "tags",
                Value::Array(tags.iter().map(|tag| Value::from(tag.as_str())).collect()),
            );
        }
        Value::Object(map)
    }
}

/// How serious an import problem is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// The document cannot be used as written.
    Error,
    /// The document is usable but something is off.
    Warning,
}

/// One problem found while importing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportIssue {
    /// Error or warning.
    pub severity: Severity,
    /// What is wrong, in a sentence.
    pub message: String,
    /// The node it concerns, when it concerns one.
    pub node_id: Option<String>,
    /// The edge it concerns, when it concerns one.
    pub edge_id: Option<String>,
}

impl ImportIssue {
    /// An error.
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            node_id: None,
            edge_id: None,
        }
    }

    /// A warning.
    #[must_use]
    pub fn warning(message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            message: message.into(),
            node_id: None,
            edge_id: None,
        }
    }

    /// Attach the node this concerns.
    #[must_use]
    pub fn at_node(mut self, node_id: impl Into<String>) -> Self {
        self.node_id = Some(node_id.into());
        self
    }

    /// Attach the edge this concerns.
    #[must_use]
    pub fn at_edge(mut self, edge_id: impl Into<String>) -> Self {
        self.edge_id = Some(edge_id.into());
        self
    }

    /// Whether this is an error rather than a warning.
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// The outcome of importing a document.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportResult {
    /// Whether nothing was an error.
    pub ok: bool,
    /// The graph — possibly partial when `ok` is false.
    pub graph: FlowGraph,
    /// Everything noticed on the way.
    pub issues: Vec<ImportIssue>,
}

impl ImportResult {
    /// Only the errors.
    #[must_use]
    pub fn errors(&self) -> Vec<&ImportIssue> {
        self.issues
            .iter()
            .filter(|issue| issue.is_error())
            .collect()
    }
}

pub(crate) fn string_at(raw: &Value, key: &str) -> Option<String> {
    raw.get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}
