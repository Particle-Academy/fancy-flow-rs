//! AI executors.
//!
//! On AI this engine is a **shuttle, not an engine**: core declares client
//! contracts and never imports a provider SDK. An adapter takes its dependency
//! behind a feature, never here.

use alloc::rc::Rc;
use alloc::string::ToString;
use alloc::vec::Vec;

use fancy_json::{Map, Value};

use crate::error::RunAborted;
use crate::nodes::support::clients::{CompletionClient, ToolClient, VectorStore};
use crate::nodes::support::expr;
use crate::runtime::{ExecutionContext, Port};

/// `llm_call` — a free-form completion.
pub struct LlmCall {
    client: Rc<dyn CompletionClient>,
}

impl LlmCall {
    /// Bind the executor to a client.
    #[must_use]
    pub fn new(client: Rc<dyn CompletionClient>) -> Self {
        Self { client }
    }
}

impl crate::executors::Executor for LlmCall {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
        let template = ctx
            .option("prompt")
            .cloned()
            .unwrap_or_else(|| Value::from(""));
        let prompt = expr::text(Some(&expr::evaluate_in(Some(&template), ctx.inputs())));

        // Every option the peers forward, and ONLY the ones actually set —
        // `??` semantics, so an absent key never reaches the adapter as null.
        let mut options = Map::new();
        for key in [
            "provider",
            "model",
            "system",
            "temperature",
            "max_tokens",
            "tools",
            "response_schema",
        ] {
            if let Some(value) = ctx.option(key) {
                options.insert(key, value.clone());
            }
        }

        let model = ctx.option_string("model", "model");
        let node_id = ctx.node().id.clone();
        ctx.emit(crate::runtime::RunEvent::log(
            crate::runtime::LogLevel::Info,
            &alloc::format!("llm_call -> {model}"),
            Some(&node_id),
        ));

        Ok(self.client.complete(&prompt, &options))
    }
}

/// `llm_router` — route to one of several named branches.
///
/// Core, not marketplace, and it introduces no third-party dependency: the
/// decision is a `choose_route` contract the host implements. The offline
/// default takes the FIRST declared route, which is deterministic and is what
/// makes a graph containing one runnable in a test.
///
/// Renamed from `llm_branch`. That rename is survivable only because every
/// previous spelling stays registered as an alias.
#[derive(Debug, Default)]
pub struct LlmRouter;

impl crate::executors::Executor for LlmRouter {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
        let routes: Vec<String> = ctx
            .option("routes")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(ToString::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let port = routes
            .first()
            .cloned()
            .unwrap_or_else(|| "default".to_string());
        Ok(Port::branch(&port, ctx.input_or_all()))
    }
}

use alloc::string::String;

/// `tool_use` — invoke a named tool with resolved args.
pub struct ToolUse {
    tools: Rc<dyn ToolClient>,
}

impl ToolUse {
    /// Bind the executor to a tool backend.
    #[must_use]
    pub fn new(tools: Rc<dyn ToolClient>) -> Self {
        Self { tools }
    }
}

impl crate::executors::Executor for ToolUse {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
        let tool = ctx.option_string("tool", "");
        let resolved = expr::evaluate_in(ctx.option("args"), ctx.inputs());

        // A non-object argument is wrapped under `value` rather than passed
        // through, so a tool always receives a map and never has to type-check
        // what the expression happened to resolve to.
        let args = if resolved.as_object().is_some() {
            resolved
        } else {
            let mut wrapped = Map::new();
            wrapped.insert("value", resolved);
            Value::Object(wrapped)
        };

        Ok(self.tools.invoke(&tool, &args))
    }
}

/// `embed_search` — embed a query and search a vector store.
pub struct EmbedSearch {
    vectors: Rc<dyn VectorStore>,
}

impl EmbedSearch {
    /// Bind the executor to a vector store.
    #[must_use]
    pub fn new(vectors: Rc<dyn VectorStore>) -> Self {
        Self { vectors }
    }
}

impl crate::executors::Executor for EmbedSearch {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
        let template = ctx
            .option("query")
            .cloned()
            .unwrap_or_else(|| Value::from(""));
        let query = expr::text(Some(&expr::evaluate_in(Some(&template), ctx.inputs())));
        // `topK`, not `limit`. The config key is part of the authored document.
        let top_k =
            usize::try_from(ctx.option("topK").and_then(Value::as_i64).unwrap_or(5)).unwrap_or(5);

        let matches = self.vectors.search(&query, top_k);

        // `{query, matches}` and nothing else — no count. A shared golden pins
        // the shape, and an extra key fails it.
        let mut out = Map::new();
        out.insert("query", Value::from(query.as_str()));
        out.insert("matches", Value::Array(matches));
        Ok(Value::Object(out))
    }
}
