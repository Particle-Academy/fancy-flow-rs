//! IO executors — outbound HTTP.
//!
//! Both nodes take an injected [`HttpClient`]; core never imports an HTTP
//! library.

use alloc::rc::Rc;

use fancy_json::{Map, Value};

use crate::error::RunAborted;
use crate::nodes::support::clients::HttpClient;
use crate::nodes::support::expr;
use crate::runtime::{ExecutionContext, LogLevel, RunEvent};

fn headers_of(ctx: &ExecutionContext<'_>) -> Map {
    ctx.option("headers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// `api_request` — an HTTP request to any URL.
///
/// Returns the client's `{status, headers, body}` response **verbatim**: a node
/// that reshaped it would make every host's error handling guess.
pub struct ApiRequest {
    http: Rc<dyn HttpClient>,
}

impl ApiRequest {
    /// Bind the executor to a client.
    #[must_use]
    pub fn new(http: Rc<dyn HttpClient>) -> Self {
        Self { http }
    }
}

impl crate::executors::Executor for ApiRequest {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
        let method = ctx.option_string("method", "GET").to_ascii_uppercase();
        let url_template = ctx
            .option("url")
            .cloned()
            .unwrap_or_else(|| Value::from(""));
        let url = expr::text(Some(&expr::evaluate_in(Some(&url_template), ctx.inputs())));
        let headers = headers_of(ctx);
        let body = expr::evaluate_in(ctx.option("body"), ctx.inputs());

        let node_id = ctx.node().id.clone();
        ctx.emit(RunEvent::log(
            LogLevel::Info,
            &alloc::format!("api_request {method} {url}"),
            Some(&node_id),
        ));

        Ok(self.http.send(&method, &url, &headers, &body))
    }
}

/// `webhook_out` — POST a payload to a configured URL.
pub struct WebhookOut {
    http: Rc<dyn HttpClient>,
}

impl WebhookOut {
    /// Bind the executor to a client.
    #[must_use]
    pub fn new(http: Rc<dyn HttpClient>) -> Self {
        Self { http }
    }
}

impl crate::executors::Executor for WebhookOut {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
        let url_template = ctx
            .option("url")
            .cloned()
            .unwrap_or_else(|| Value::from(""));
        let url = expr::text(Some(&expr::evaluate_in(Some(&url_template), ctx.inputs())));
        let headers = headers_of(ctx);
        let payload = expr::evaluate_in(ctx.option("payload"), ctx.inputs());

        let node_id = ctx.node().id.clone();
        ctx.emit(RunEvent::log(
            LogLevel::Info,
            &alloc::format!("webhook_out -> {url}"),
            Some(&node_id),
        ));

        let response = self.http.send("POST", &url, &headers, &payload);

        let mut out = Map::new();
        out.insert("sent", Value::Bool(true));
        out.insert(
            "status",
            response.get("status").cloned().unwrap_or(Value::Null),
        );
        out.insert(
            "response",
            response.get("body").cloned().unwrap_or(Value::Null),
        );
        Ok(Value::Object(out))
    }
}
