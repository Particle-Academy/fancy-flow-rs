//! Parse, validate, import and export `WorkflowSchema` v1 documents.
//!
//! A graph an agent or human authors in `<FlowEditor>` round-trips through here
//! unchanged. This answers "is this graph COHERENT?" — unknown kinds, dangling
//! edges, missing required config. It does **not** answer "is it safe to
//! accept?"; that is [`crate::security`], and conflating the two is how a
//! payload gets treated as a document.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use fancy_json::{Map, Value};

use crate::analysis::check_graph_connectivity;
use crate::registry::NodeKindRegistry;
use crate::schema::{
    string_at, FlowEdge, FlowGraph, FlowNode, ImportIssue, ImportResult, PortDescriptor, Severity,
    WorkflowMetadata,
};

/// The schema version this crate reads and writes.
pub const SCHEMA_VERSION: i64 = 1;

/// The `$schema` URL every exported document carries.
pub const SCHEMA_URL: &str = "https://particle.academy/schemas/workflow/v1.json";

/// Hydrate a `WorkflowSchema` into a [`FlowGraph`].
///
/// Validates kinds and configs against the registry, reporting unknown kinds,
/// missing required config, and dangling edges. In `lenient` mode, schema-level
/// errors become warnings.
///
/// # The kind lands in exactly one place
///
/// The document's `kind` becomes [`FlowNode::kind`], and there is no second
/// field for it to hide in. That is what makes the TypeScript 0.48.1 bug — an
/// executor registry keyed by kind that never fired, silently — unrepresentable
/// here. `fancy-flow-py` collapses the same way.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "one pass over one document. Splitting the node loop from the edge \
              loop would hide that edges are validated against the node ids THIS \
              pass collected, which is the whole reason they are in one function."
)]
pub fn import_workflow(
    document: &Value,
    lenient: bool,
    registry: &NodeKindRegistry,
) -> ImportResult {
    let mut issues: Vec<ImportIssue> = Vec::new();

    let Some(root) = document.as_object() else {
        return ImportResult {
            ok: false,
            graph: FlowGraph::new(),
            issues: alloc::vec![ImportIssue::error("Schema is not an object.")],
        };
    };

    let version = root.get("version").and_then(Value::as_i64);
    if version != Some(SCHEMA_VERSION) {
        let message = alloc::format!(
            "Unsupported workflow schema version: {version:?} (expected {SCHEMA_VERSION})"
        );
        issues.push(if lenient {
            ImportIssue::warning(message)
        } else {
            ImportIssue::error(message)
        });
        if !lenient {
            return ImportResult {
                ok: false,
                graph: FlowGraph::new(),
                issues,
            };
        }
    }

    let empty = Map::new();
    let graph_raw = root
        .get("graph")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let no_nodes: Vec<Value> = Vec::new();
    let raw_nodes = graph_raw
        .get("nodes")
        .and_then(Value::as_array)
        .unwrap_or(&no_nodes);
    let raw_edges = graph_raw
        .get("edges")
        .and_then(Value::as_array)
        .unwrap_or(&no_nodes);

    let mut nodes: Vec<FlowNode> = Vec::new();
    let mut node_ids: alloc::collections::BTreeSet<String> = alloc::collections::BTreeSet::new();

    for raw in raw_nodes {
        let Some(id) = string_at(raw, "id") else {
            issues.push(ImportIssue::error("A node has no id."));
            continue;
        };
        let kind_name = string_at(raw, "kind").unwrap_or_default();
        let kind = registry.get(&kind_name);

        if kind.is_none() {
            let message =
                alloc::format!("Unknown kind \"{kind_name}\" - register it before importing.");
            issues.push(
                if lenient {
                    ImportIssue::warning(message)
                } else {
                    ImportIssue::error(message)
                }
                .at_node(&id),
            );
        }

        let config = match raw.get("config").and_then(Value::as_object) {
            Some(config) => config.clone(),
            None => kind
                .map(crate::registry::NodeKind::resolved_default_config)
                .unwrap_or_default(),
        };

        if kind.is_some() {
            for issue in registry.validate_config(&kind_name, &config) {
                issues.push(
                    ImportIssue::warning(alloc::format!("{}: {}", issue.key, issue.message))
                        .at_node(&id),
                );
            }
        }

        let position = raw.get("position");
        let node = FlowNode {
            id: id.clone(),
            kind: Some(kind_name.clone()),
            x: position
                .and_then(|p| p.get("x"))
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
            y: position
                .and_then(|p| p.get("y"))
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
            label: string_at(raw, "label")
                .or_else(|| kind.map(|k| k.label.clone()))
                .or_else(|| Some(kind_name.clone())),
            description: string_at(raw, "description"),
            config,
            // Deliberately left undeclared on import — the engine then falls
            // back to the kind's ports, or a single `out`, matching every peer.
            inputs: None,
            outputs: None,
        };

        node_ids.insert(node.id.clone());
        nodes.push(node);
    }

    let mut edges: Vec<FlowEdge> = Vec::new();
    for raw in raw_edges {
        let edge_id = string_at(raw, "id").unwrap_or_default();
        let source = string_at(raw, "source").unwrap_or_default();
        let target = string_at(raw, "target").unwrap_or_default();

        if !node_ids.contains(&source) {
            issues.push(
                ImportIssue::warning(alloc::format!("Edge source \"{source}\" not found."))
                    .at_edge(&edge_id),
            );
            continue;
        }
        if !node_ids.contains(&target) {
            issues.push(
                ImportIssue::warning(alloc::format!("Edge target \"{target}\" not found."))
                    .at_edge(&edge_id),
            );
            continue;
        }

        edges.push(FlowEdge {
            id: edge_id,
            source,
            target,
            source_handle: optional(raw, "sourceHandle"),
            target_handle: optional(raw, "targetHandle"),
            label: string_at(raw, "label"),
        });
    }

    // WIRING, not merely dataflow: a node no edge reaches and that reaches no
    // edge, and an edge reading from a node that publishes nothing.
    //
    // Deliberately AFTER the edge loop, so it sees the same edges the engine
    // will -- a dangling edge is dropped with a warning above, and running this
    // first would let a dropped edge count as a connection.
    //
    // Deliberately NOT gated on `lenient`. That flag is about unknown
    // VOCABULARY (a kind this host has not registered), never about wiring.
    issues.extend(check_graph_connectivity(&nodes, &edges, registry));

    let ok = !issues.iter().any(ImportIssue::is_error);
    ImportResult {
        ok,
        graph: FlowGraph { nodes, edges },
        issues,
    }
}

/// Parse a JSON document and import it.
///
/// # Errors
///
/// [`FlowError::Import`](crate::error::FlowError::Import) when the text is not
/// JSON at all. A document that IS JSON but is not a coherent workflow comes
/// back as an [`ImportResult`] with issues, because that is a graph the caller
/// can act on.
pub fn import_json(
    text: &str,
    lenient: bool,
    registry: &NodeKindRegistry,
) -> Result<ImportResult, crate::error::FlowError> {
    let document = fancy_json::parse(text)
        .map_err(|error| crate::error::FlowError::Import(error.to_string()))?;
    Ok(import_workflow(&document, lenient, registry))
}

/// Write a graph back out as a `WorkflowSchema` v1 document.
#[must_use]
pub fn export_workflow(graph: &FlowGraph, metadata: Option<&WorkflowMetadata>) -> Value {
    let mut nodes: Vec<Value> = Vec::new();
    for node in &graph.nodes {
        let mut out = Map::new();
        out.insert("id", Value::from(node.id.as_str()));
        out.insert(
            "kind",
            Value::from(node.kind.as_deref().unwrap_or("custom")),
        );

        let mut position = Map::new();
        position.insert("x", Value::from(node.x));
        position.insert("y", Value::from(node.y));
        out.insert("position", Value::Object(position));

        if let Some(label) = &node.label {
            out.insert("label", Value::from(label.as_str()));
        }
        if let Some(description) = &node.description {
            out.insert("description", Value::from(description.as_str()));
        }
        if !node.config.is_empty() {
            out.insert("config", Value::Object(node.config.clone()));
        }
        for (key, ports) in [("inputs", &node.inputs), ("outputs", &node.outputs)] {
            if let Some(ports) = ports {
                out.insert(
                    key,
                    Value::Array(ports.iter().map(PortDescriptor::to_value).collect()),
                );
            }
        }
        nodes.push(Value::Object(out));
    }

    let mut edges: Vec<Value> = Vec::new();
    for edge in &graph.edges {
        let mut out = Map::new();
        out.insert("id", Value::from(edge.id.as_str()));
        out.insert("source", Value::from(edge.source.as_str()));
        out.insert("target", Value::from(edge.target.as_str()));
        for (key, handle) in [
            ("sourceHandle", &edge.source_handle),
            ("targetHandle", &edge.target_handle),
        ] {
            if let Some(handle) = handle {
                out.insert(key, Value::from(handle.as_str()));
            }
        }
        if let Some(label) = &edge.label {
            out.insert("label", Value::from(label.as_str()));
        }
        edges.push(Value::Object(out));
    }

    let mut inner = Map::new();
    inner.insert("nodes", Value::Array(nodes));
    inner.insert("edges", Value::Array(edges));

    let mut document = Map::new();
    document.insert("$schema", Value::from(SCHEMA_URL));
    document.insert("version", Value::from(SCHEMA_VERSION));
    if let Some(metadata) = metadata {
        document.insert("metadata", metadata.to_value());
    }
    document.insert("graph", Value::Object(inner));
    Value::Object(document)
}

/// Export a graph as compact JSON text.
#[must_use]
pub fn to_json(graph: &FlowGraph, metadata: Option<&WorkflowMetadata>) -> String {
    fancy_json::to_string(&export_workflow(graph, metadata))
}

fn optional(raw: &Value, key: &str) -> Option<String> {
    match raw.get(key) {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}

/// Whether an import result carries anything at all worth reporting.
#[must_use]
pub fn has_warnings(result: &ImportResult) -> bool {
    result
        .issues
        .iter()
        .any(|issue| issue.severity == Severity::Warning)
}
