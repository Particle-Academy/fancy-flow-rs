//! The run event stream.
//!
//! One struct with a `kind` tag and the union of every arm's payload, matching
//! the PHP and Python twins rather than Rust's usual "an enum with data per
//! variant" — because the **serialized** shape is the contract four runtimes
//! share, and a union that serializes differently per language is not a union.

use alloc::string::{String, ToString};

use fancy_json::{Map, Value};

/// The lifecycle status a node reports through a `node-status` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStatus {
    /// Not run — skipped, or visual only.
    Idle,
    /// Waiting to run in a durable frontier.
    Queued,
    /// Executing now.
    Running,
    /// Finished, output published.
    Done,
    /// Failed, or aborted.
    Error,
}

impl NodeStatus {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Error => "error",
        }
    }
}

/// A log line's severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// Ordinary progress.
    Info,
    /// Something worth noticing.
    Warn,
    /// Something went wrong.
    Error,
    /// Detail for a developer.
    Debug,
}

impl LogLevel {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Debug => "debug",
        }
    }

    /// Read one from a config value, defaulting to `info`.
    #[must_use]
    pub fn from_str_or_info(text: &str) -> Self {
        match text {
            "warn" => Self::Warn,
            "error" => Self::Error,
            "debug" => Self::Debug,
            _ => Self::Info,
        }
    }
}

/// A single event in a run's stream.
///
/// Kinds: `run-start`, `node-status`, `node-output`, `log`, `run-end`,
/// `run-error`. Build them with the constructors.
#[derive(Debug, Clone, PartialEq)]
pub struct RunEvent {
    /// Which kind of event this is.
    pub kind: &'static str,
    /// The node it concerns.
    pub node_id: Option<String>,
    /// For `node-status`.
    pub status: Option<NodeStatus>,
    /// Free text on a status.
    pub text: Option<String>,
    /// For `node-output`.
    pub port_id: Option<String>,
    /// For `node-output`.
    pub value: Option<Value>,
    /// For `log`.
    pub level: Option<LogLevel>,
    /// For `log`.
    pub message: Option<String>,
    /// For `log`.
    pub detail: Option<Value>,
    /// For `run-end`.
    pub ok: Option<bool>,
    /// For `run-error`.
    pub error: Option<String>,
}

impl RunEvent {
    /// The `run-start` kind tag.
    pub const RUN_START: &'static str = "run-start";
    /// The `node-status` kind tag.
    pub const NODE_STATUS: &'static str = "node-status";
    /// The `node-output` kind tag.
    pub const NODE_OUTPUT: &'static str = "node-output";
    /// The `log` kind tag.
    pub const LOG: &'static str = "log";
    /// The `run-end` kind tag.
    pub const RUN_END: &'static str = "run-end";
    /// The `run-error` kind tag.
    pub const RUN_ERROR: &'static str = "run-error";

    fn bare(kind: &'static str) -> Self {
        Self {
            kind,
            node_id: None,
            status: None,
            text: None,
            port_id: None,
            value: None,
            level: None,
            message: None,
            detail: None,
            ok: None,
            error: None,
        }
    }

    /// The run began.
    #[must_use]
    pub fn run_start() -> Self {
        Self::bare(Self::RUN_START)
    }

    /// A node changed state.
    #[must_use]
    pub fn node_status(node_id: &str, status: NodeStatus, text: Option<&str>) -> Self {
        let mut event = Self::bare(Self::NODE_STATUS);
        event.node_id = Some(node_id.to_string());
        event.status = Some(status);
        event.text = text.map(ToString::to_string);
        event
    }

    /// A node published a value on a port.
    ///
    /// **The activated ports come from these events**, and a durable driver
    /// must read them back off the stream rather than re-deriving them. A
    /// second copy of the routing table agrees for a year and then disagrees on
    /// one branch.
    #[must_use]
    pub fn node_output(node_id: &str, port_id: &str, value: Value) -> Self {
        let mut event = Self::bare(Self::NODE_OUTPUT);
        event.node_id = Some(node_id.to_string());
        event.port_id = Some(port_id.to_string());
        event.value = Some(value);
        event
    }

    /// Something to say on the feed.
    #[must_use]
    pub fn log(level: LogLevel, message: &str, node_id: Option<&str>) -> Self {
        let mut event = Self::bare(Self::LOG);
        event.node_id = node_id.map(ToString::to_string);
        event.level = Some(level);
        event.message = Some(message.to_string());
        event
    }

    /// The run finished.
    #[must_use]
    pub fn run_end(ok: bool) -> Self {
        let mut event = Self::bare(Self::RUN_END);
        event.ok = Some(ok);
        event
    }

    /// The run could not start.
    #[must_use]
    pub fn run_error(error: &str) -> Self {
        let mut event = Self::bare(Self::RUN_ERROR);
        event.error = Some(error.to_string());
        event
    }

    /// Serialize only the active arm, in the peer runtimes' key casing.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("type", Value::from(self.kind));

        match self.kind {
            Self::NODE_STATUS => {
                insert_str(&mut map, "nodeId", self.node_id.as_deref());
                if let Some(status) = self.status {
                    map.insert("status", Value::from(status.as_str()));
                }
                insert_str(&mut map, "text", self.text.as_deref());
            }
            Self::NODE_OUTPUT => {
                insert_str(&mut map, "nodeId", self.node_id.as_deref());
                insert_str(&mut map, "portId", self.port_id.as_deref());
                map.insert("value", self.value.clone().unwrap_or(Value::Null));
            }
            Self::LOG => {
                insert_str(&mut map, "nodeId", self.node_id.as_deref());
                if let Some(level) = self.level {
                    map.insert("level", Value::from(level.as_str()));
                }
                insert_str(&mut map, "message", self.message.as_deref());
                if let Some(detail) = &self.detail {
                    map.insert("detail", detail.clone());
                }
            }
            Self::RUN_END => {
                if let Some(ok) = self.ok {
                    map.insert("ok", Value::Bool(ok));
                }
            }
            Self::RUN_ERROR => insert_str(&mut map, "error", self.error.as_deref()),
            _ => {}
        }

        Value::Object(map)
    }
}

fn insert_str(map: &mut Map, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        map.insert(key, Value::from(value));
    }
}
