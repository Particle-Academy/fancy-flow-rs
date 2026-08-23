//! The host seam for the built-in executors, plus deterministic offline fakes.
//!
//! Core never imports an HTTP client, an LLM SDK or a database. Every node that
//! would need one takes an injected trait object, and the default is a fake
//! that is **deterministic by construction** — so a graph runs end to end in a
//! test, on a laptop, and inside a blockchain node, with no network and no
//! clock.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use fancy_json::{Map, Value};

/// A keyed store — `memory_store` and `data_store` write through this.
pub trait KeyValueStore {
    /// Read a key.
    fn get(&self, key: &str) -> Option<Value>;
    /// Write a key.
    fn set(&self, key: &str, value: Value);
    /// Remove a key.
    fn delete(&self, key: &str);
    /// Every key currently held, for a table scan.
    fn all(&self) -> BTreeMap<String, Value>;
}

/// An outbound HTTP client — `api_request` and `webhook_out` send through this.
pub trait HttpClient {
    /// Send a request and return `{status, headers, body}` verbatim.
    ///
    /// Returning the response unreshaped is deliberate: a node that normalised
    /// it would make every host's error handling guess.
    fn send(&self, method: &str, url: &str, headers: &Map, body: &Value) -> Value;
}

/// A free-form completion backend — `llm_call` uses this.
pub trait CompletionClient {
    /// Complete a prompt.
    fn complete(&self, prompt: &str, options: &Map) -> Value;
}

/// A named-tool backend — `tool_use` calls through this.
pub trait ToolClient {
    /// Invoke a tool with resolved arguments.
    fn invoke(&self, tool: &str, args: &Value) -> Value;
}

/// A vector store — `embed_search` queries this.
pub trait VectorStore {
    /// Search for `query`, returning at most `limit` matches.
    fn search(&self, query: &str, limit: usize) -> Vec<Value>;
}

/// An outbound message channel — `notify` sends through this.
pub trait Notifier {
    /// Deliver a message.
    fn notify(&self, channel: &str, to: &str, message: &str);
}

// -- offline defaults ----------------------------------------------------

/// An in-memory [`KeyValueStore`].
#[derive(Debug, Default)]
pub struct MemoryKeyValueStore {
    entries: RefCell<BTreeMap<String, Value>>,
}

impl MemoryKeyValueStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeyValueStore for MemoryKeyValueStore {
    fn get(&self, key: &str) -> Option<Value> {
        self.entries.borrow().get(key).cloned()
    }

    fn set(&self, key: &str, value: Value) {
        self.entries.borrow_mut().insert(key.to_string(), value);
    }

    fn delete(&self, key: &str) {
        self.entries.borrow_mut().remove(key);
    }

    fn all(&self) -> BTreeMap<String, Value> {
        self.entries.borrow().clone()
    }
}

/// Records requests and echoes them back. Never touches a socket.
#[derive(Debug, Default)]
pub struct EchoHttpClient {
    /// Every request sent, in order.
    pub requests: RefCell<Vec<Value>>,
}

impl EchoHttpClient {
    /// A client with no history.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl HttpClient for EchoHttpClient {
    fn send(&self, method: &str, url: &str, headers: &Map, body: &Value) -> Value {
        let mut record = Map::new();
        record.insert("method", Value::from(method));
        record.insert("url", Value::from(url));
        record.insert("headers", Value::Object(headers.clone()));
        record.insert("body", body.clone());
        self.requests.borrow_mut().push(Value::Object(record));

        let mut echoed = Map::new();
        echoed.insert("method", Value::from(method));
        echoed.insert("url", Value::from(url));
        echoed.insert("body", body.clone());

        let mut inner = Map::new();
        inner.insert("echoed", Value::Object(echoed));

        let mut response = Map::new();
        response.insert("status", Value::from(200));
        response.insert("headers", Value::Object(headers.clone()));
        response.insert("body", Value::Object(inner));
        Value::Object(response)
    }
}

/// Prefixes the prompt with the model name. Deterministic by construction.
///
/// The `usage` counts are PHP's `str_word_count` of the PROMPT — runs of
/// letters, apostrophes and hyphens. Reproduced rather than approximated with a
/// whitespace split, because the numbers are baked into the shared
/// `flow/graph-runs` goldens that all four runtimes assert.
#[derive(Debug, Default)]
pub struct EchoCompletionClient;

impl CompletionClient for EchoCompletionClient {
    fn complete(&self, prompt: &str, options: &Map) -> Value {
        let model = options
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("echo");
        let words = word_count(prompt) as u64;

        let mut usage = Map::new();
        usage.insert("input_tokens", Value::from(words));
        usage.insert("output_tokens", Value::from(words));

        let mut out = Map::new();
        out.insert("text", Value::from(alloc::format!("[{model}] {prompt}")));
        out.insert("usage", Value::Object(usage));
        Value::Object(out)
    }
}

/// PHP's `str_word_count` default: runs of letters, apostrophes and hyphens.
///
/// Written as a scan rather than a pattern for the same reason `expr` is — and
/// because a `regex` dependency would double this crate's tree for four lines.
#[must_use]
pub fn word_count(text: &str) -> usize {
    let mut count = 0;
    let mut inside = false;
    for ch in text.chars() {
        let is_word = ch.is_ascii_alphabetic() || ch == '\'' || ch == '-';
        if is_word && !inside {
            count += 1;
        }
        inside = is_word;
    }
    count
}

/// Echoes the tool name and its arguments.
#[derive(Debug, Default)]
pub struct EchoToolClient;

impl ToolClient for EchoToolClient {
    fn invoke(&self, tool: &str, args: &Value) -> Value {
        // `{tool, args}` and nothing else. An invented `result` key is exactly
        // what made this fake disagree with three peers on its first run.
        let mut out = Map::new();
        out.insert("tool", Value::from(tool));
        out.insert("args", args.clone());
        Value::Object(out)
    }
}

/// A vector store with nothing in it.
#[derive(Debug, Default)]
pub struct EmptyVectorStore;

impl VectorStore for EmptyVectorStore {
    fn search(&self, _query: &str, _limit: usize) -> Vec<Value> {
        Vec::new()
    }
}

/// Records notifications instead of sending them.
#[derive(Debug, Default)]
pub struct RecordingNotifier {
    /// Every message, in order.
    pub sent: RefCell<Vec<Value>>,
}

impl Notifier for RecordingNotifier {
    fn notify(&self, channel: &str, to: &str, message: &str) {
        let mut record = Map::new();
        record.insert("channel", Value::from(channel));
        record.insert("to", Value::from(to));
        record.insert("message", Value::from(message));
        self.sent.borrow_mut().push(Value::Object(record));
    }
}

/// Everything the built-in executors need from the host.
///
/// Every field has an offline default, so `ExecutorDeps::default()` runs a
/// whole graph with no network, no database and no clock — which is what makes
/// the shared graph fixtures reproducible on four runtimes.
pub struct ExecutorDeps {
    /// Backs `memory_store`.
    pub memory: Rc<dyn KeyValueStore>,
    /// Backs `data_store`.
    pub data: Rc<dyn KeyValueStore>,
    /// Backs `api_request` and `webhook_out`.
    pub http: Rc<dyn HttpClient>,
    /// Backs `llm_call`.
    pub completions: Rc<dyn CompletionClient>,
    /// Backs `tool_use`.
    pub tools: Rc<dyn ToolClient>,
    /// Backs `embed_search`.
    pub vectors: Rc<dyn VectorStore>,
    /// Backs `notify`.
    pub notifier: Rc<dyn Notifier>,
}

impl Default for ExecutorDeps {
    fn default() -> Self {
        Self {
            memory: Rc::new(MemoryKeyValueStore::new()),
            data: Rc::new(MemoryKeyValueStore::new()),
            http: Rc::new(EchoHttpClient::new()),
            completions: Rc::new(EchoCompletionClient),
            tools: Rc::new(EchoToolClient),
            vectors: Rc::new(EmptyVectorStore),
            notifier: Rc::new(RecordingNotifier::default()),
        }
    }
}

impl core::fmt::Debug for ExecutorDeps {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ExecutorDeps { .. }")
    }
}
