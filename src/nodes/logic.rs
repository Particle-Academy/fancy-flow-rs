//! Logic executors — the nodes that decide a graph's SHAPE.
//!
//! Worth precision, because everything downstream depends on which port lights
//! up. See `.ai/knowledge/flow-engine-spec.md` section 4.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use fancy_json::{Map, Value};

use crate::engine::FlowRunner;
use crate::error::RunAborted;
use crate::nodes::support::{expr, routing_diagnostics};
use crate::runtime::{ExecutionContext, LogLevel, Pause, Port, RunEvent, RunOptions};
use crate::schema::{FlowEdge, FlowGraph};

/// `branch` — two ports, exactly one taken.
///
/// The condition resolves through [`expr`] against the node's inputs and
/// [`expr::truthy`] decides. The incoming value passes through unchanged down
/// whichever side is taken, and the other edge stays dead for the rest of the
/// run.
///
/// # Errors
///
/// Never.
pub fn branch(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    let resolved = expr::evaluate_in(ctx.option("condition"), ctx.inputs());
    let port = if expr::truthy(&resolved) {
        "true"
    } else {
        "false"
    };

    // A condition that did not RESOLVE is falsy, so the run takes `false`
    // silently and for the wrong reason. Routing is unchanged; the reason is
    // now visible.
    routing_diagnostics::warn_if_unresolved(ctx, "condition", port);

    Ok(Port::branch(port, ctx.input_or_all()))
}

/// `switch_case` — N ports, one taken.
///
/// Routes on a key: `value` is resolved and looked up in the `cases` map
/// (value -> port id), falling back to `default`.
///
/// # Errors
///
/// Never.
pub fn switch_case(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    let resolved = expr::evaluate_in(ctx.option("value"), ctx.inputs());
    let key = expr::text(Some(&resolved));

    let port = ctx
        .option("cases")
        .and_then(Value::as_object)
        .and_then(|cases| cases.get(&key))
        .and_then(Value::as_str)
        .unwrap_or("default")
        .to_string();

    // The same silent mis-route as `branch`, one step over: a `value` that does
    // not resolve becomes "", matches no case, and falls to `default` --
    // indistinguishable from a value that genuinely matched nothing.
    routing_diagnostics::warn_if_unresolved(ctx, "value", &port);

    Ok(Port::only(&port, ctx.input_or_all()))
}

/// Default cap on how many items a `for_each` lane iterates.
pub const FOR_EACH_DEFAULT_MAX_ITEMS: u32 = 1000;

/// The ceiling `maxItems` may be raised to.
pub const FOR_EACH_HARD_MAX_ITEMS: u32 = 10_000;

/// Every node reachable from `starts`, the starts included. Iterative: a graph
/// is untrusted structure, and in this crate a stack overflow is an abort.
fn reachable<'g>(
    adjacency: &BTreeMap<&'g str, Vec<&'g str>>,
    starts: impl IntoIterator<Item = &'g str>,
) -> BTreeSet<&'g str> {
    let mut seen = BTreeSet::new();
    let mut queue: Vec<&'g str> = starts.into_iter().collect();
    while let Some(id) = queue.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(targets) = adjacency.get(id) {
            queue.extend(targets.iter().copied());
        }
    }
    seen
}

/// The edges leaving `node_id` on `port`. An edge with no source handle left
/// the default port, `out`.
fn edges_from<'g>(graph: &'g FlowGraph, node_id: &str, port: &str) -> Vec<&'g FlowEdge> {
    graph
        .edges
        .iter()
        .filter(|e| e.source == node_id && e.source_handle.as_deref().unwrap_or("out") == port)
        .collect()
}

/// The loop BODY: nodes reachable from this node's `item` port, stopping at
/// anything also reachable from `done`.
///
/// Derived from the graph rather than declared, so a graph says what the body
/// is by being drawn. The `done` subtraction is what lets a node sit after the
/// loop and still be reachable from inside it: it belongs to whichever port
/// leads to it first.
///
/// `None` means "no `item` edge" — the data-only case, not an error.
fn for_each_lane<'g>(
    graph: &'g FlowGraph,
    node_id: &str,
) -> Option<(FlowGraph, Vec<&'g FlowEdge>)> {
    let item_edges = edges_from(graph, node_id, "item");
    if item_edges.is_empty() {
        return None;
    }

    let mut adjacency: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in &graph.edges {
        adjacency
            .entry(edge.source.as_str())
            .or_default()
            .push(edge.target.as_str());
    }

    let done = reachable(
        &adjacency,
        edges_from(graph, node_id, "done")
            .into_iter()
            .map(|e| e.target.as_str()),
    );
    let body: BTreeSet<&str> = reachable(&adjacency, item_edges.iter().map(|e| e.target.as_str()))
        .into_iter()
        .filter(|id| !done.contains(id) && *id != node_id)
        .collect();

    let lane = FlowGraph {
        nodes: graph
            .nodes
            .iter()
            .filter(|n| body.contains(n.id.as_str()))
            .cloned()
            .collect(),
        edges: graph
            .edges
            .iter()
            .filter(|e| body.contains(e.source.as_str()) && body.contains(e.target.as_str()))
            .cloned()
            .collect(),
    };
    let entries = item_edges
        .into_iter()
        .filter(|e| body.contains(e.target.as_str()))
        .collect();

    Some((lane, entries))
}

/// `maxItems` as authored: a number, or a string holding one. Anything else is
/// not a cap, and is refused by the range check rather than read as zero.
fn max_items_option(ctx: &ExecutionContext<'_>) -> Option<f64> {
    match ctx.option("maxItems") {
        None | Some(Value::Null) => Some(f64::from(FOR_EACH_DEFAULT_MAX_ITEMS)),
        Some(value) => value.as_f64().or_else(|| {
            value
                .as_str()
                .and_then(|text| text.trim().parse::<f64>().ok())
        }),
    }
}

/// `for_each` — the collection as DATA, or the lane run once per item.
///
/// WITHOUT an `item` edge (or with `mode: "collect"`) this publishes the
/// resolved collection and its size and stops. That half is deliberate rather
/// than unfinished: on a durable run a `for_each` over 10,000 rows is one node,
/// one claim, one checkpoint — not 10,000.
///
/// WITH an `item` edge it runs the derived lane once per item and aggregates
/// `{items, results, failures, count}` on `done`. `results` is index-aligned
/// with `items` (`null` where an item's lane failed); `failures` holds
/// `{index, item, error}` for each one that did. That half was missing until
/// the context carried the graph: the schema accepted the edge and the engine
/// ignored it, so every downstream node ran ONCE against the whole collection.
/// fancy-labs' `batch-scoring` reference graph produced five per-item scores on
/// the PHP twin and one aggregate here.
///
/// The lane runs on the registry minus its node-id bindings. Under the durable
/// coordinator the context's registry is the replay's fork, with every node but
/// this one fenced by id — and a lane node is a node of the same graph. The
/// Python twin shipped that leak in 0.27.0.
///
/// `concurrency` is carried rather than acted on: items run in order, as they
/// do on every peer, and parity outranks throughput here.
///
/// # Errors
///
/// When the derived lane is empty, `maxItems` is outside 1..=10000, the list is
/// longer than `maxItems`, or an item's lane pauses — a pause is not a failure,
/// and recording it as one would strand whoever the run waits on. A lane that
/// merely FAILS is recorded in `failures` and the loop carries on.
pub fn for_each(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    let source = expr::evaluate_in(ctx.option("source"), ctx.inputs());

    let items: Vec<Value> = match &source {
        // An object fans out over its VALUES, matching PHP's `array_values`
        // and Python's `.values()`. Its keys are not the collection.
        Value::Object(map) => map.values().cloned().collect(),
        Value::Array(items) => items.clone(),
        Value::Null => Vec::new(),
        other => alloc::vec![other.clone()],
    };

    let lane = match (ctx.graph(), ctx.executors()) {
        (Some(graph), Some(executors)) if ctx.option_str("mode", "") != "collect" => {
            for_each_lane(graph, &ctx.node().id).map(|lane| (lane, executors))
        }
        _ => None,
    };

    let Some(((lane_graph, entries), executors)) = lane else {
        let mut out = Map::new();
        out.insert("count", Value::from(items.len() as u64));
        out.insert("items", Value::Array(items));
        // `items` before `count` on the peers; key ORDER is not part of equality
        // in any conformance loader, so this is presentation only.
        return Ok(Value::Object(out));
    };

    let node_id = ctx.node().id.clone();

    if lane_graph.nodes.is_empty() {
        return Err(ctx.abort(&alloc::format!(
            "for_each \"{node_id}\" has an item edge but its derived lane is empty"
        )));
    }

    let max_items = match max_items_option(ctx) {
        Some(max)
            if max.is_finite() && (1.0..=f64::from(FOR_EACH_HARD_MAX_ITEMS)).contains(&max) =>
        {
            max
        }
        _ => {
            return Err(ctx.abort(&alloc::format!(
                "for_each \"{node_id}\" maxItems must be between 1 and {FOR_EACH_HARD_MAX_ITEMS}"
            )))
        }
    };
    // Compared through u32 so the conversion is lossless; a list longer than
    // u32::MAX is over any cap the range check allows.
    if !matches!(u32::try_from(items.len()), Ok(n) if f64::from(n) <= max_items) {
        return Err(ctx.abort(&alloc::format!(
            "for_each \"{node_id}\" resolved {} items exceeds its maxItems cap of {max_items}",
            items.len()
        )));
    }

    let lane_executors = executors.without_node_bindings();
    let runner = ctx
        .kinds()
        .map_or_else(FlowRunner::new, FlowRunner::with_kinds);

    let mut results: Vec<Value> = Vec::with_capacity(items.len());
    let mut failures: Vec<Value> = Vec::new();

    for (index, item) in items.iter().enumerate() {
        let mut initial_inputs: BTreeMap<String, Map> = BTreeMap::new();
        for edge in &entries {
            initial_inputs
                .entry(edge.target.clone())
                .or_default()
                .insert(edge.target_handle.as_deref().unwrap_or("in"), item.clone());
        }

        let options = RunOptions {
            initial_inputs,
            depth: ctx.depth() + 1,
            // The index rides on the identity, so a node in iteration 3 cannot
            // share an idempotency key with the same node in iteration 4.
            run: ctx
                .run()
                .map(|run| run.descend(&node_id, Some(index as u64))),
            ..RunOptions::new()
        };

        let nested = runner.run(&lane_graph, &lane_executors, &options)?;

        if !nested.ok {
            let reason = nested.error.unwrap_or_else(|| "unknown error".to_string());

            // A PAUSE IS NOT A FAILURE. It travels the error channel verbatim.
            if Pause::is_pause(Some(&reason)) {
                return Err(ctx.abort(&reason));
            }

            results.push(Value::Null);
            let mut failure = Map::new();
            failure.insert("index", Value::from(index as u64));
            failure.insert("item", item.clone());
            failure.insert("error", Value::from(reason));
            failures.push(Value::Object(failure));
            continue;
        }

        let mut outputs = Map::new();
        for (id, value) in nested.outputs {
            outputs.insert(id, value);
        }
        results.push(Value::Object(outputs));
    }

    let count = items.len() as u64;
    let mut out = Map::new();
    out.insert("items", Value::Array(items));
    out.insert("results", Value::Array(results));
    out.insert("failures", Value::Array(failures));
    out.insert("count", Value::from(count));
    Ok(Port::only("done", Value::Object(out)))
}

/// `merge` — several inputs, one value.
///
/// `merge` (default) combines inputs into one object: a mapping is merged in by
/// key, anything else is keyed by its PORT id. `concat` flattens everything
/// into one list.
///
/// Null inputs are skipped, and because dead edges never reach `collect_inputs`
/// at all, a merge downstream of a branch receives only the side that actually
/// ran.
///
/// # Errors
///
/// Never.
pub fn merge(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    let mode = ctx.option_string("mode", "merge");

    if mode == "concat" {
        let mut out: Vec<Value> = Vec::new();
        for (_, value) in ctx.inputs().iter() {
            match value {
                Value::Null => {}
                Value::Array(items) => out.extend(items.iter().cloned()),
                other => out.push(other.clone()),
            }
        }
        return Ok(Value::Array(out));
    }

    let mut merged = Map::new();
    for (port, value) in ctx.inputs().iter() {
        match value {
            Value::Null => {}
            Value::Object(map) => {
                for (key, inner) in map.iter() {
                    merged.insert(key, inner.clone());
                }
            }
            other => {
                merged.insert(port, other.clone());
            }
        }
    }
    Ok(Value::Object(merged))
}

/// `wait` — a pause point.
///
/// The framework-free default does NOT sleep: it records the requested wait and
/// passes the input through, so tests stay fast and deterministic. A durable
/// adapter overrides this to schedule the run's continuation rather than block
/// a worker for an hour.
///
/// # Errors
///
/// Never.
pub fn wait(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    let mode = ctx.option_string("mode", "duration");
    let duration = ctx.option("duration").cloned().unwrap_or(Value::Null);

    let node_id = ctx.node().id.clone();
    ctx.emit(RunEvent::log(
        LogLevel::Info,
        &alloc::format!("wait ({mode}) - not sleeping in framework-free mode"),
        Some(&node_id),
    ));

    let mut out = Map::new();
    out.insert("waited", Value::from(mode.as_str()));
    out.insert("duration", duration);
    out.insert("input", ctx.input_or_all());
    Ok(Value::Object(out))
}

/// `transform` — reshape in place.
///
/// With no expression the input passes through untouched. One `out` port,
/// always active.
///
/// # Errors
///
/// Never.
pub fn transform(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    let Some(expression) = ctx.option("expression") else {
        return Ok(ctx.input_or_all());
    };
    if expression.as_str() == Some("") {
        return Ok(ctx.input_or_all());
    }
    let expression = expression.clone();
    Ok(expr::evaluate_in(Some(&expression), ctx.inputs()))
}
