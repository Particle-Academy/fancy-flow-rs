//! The human-pause contract.
//!
//! A workflow waiting for a person is not an error, but it travels the same
//! channel as one: the executor aborts, the runner records a reason string, and
//! something downstream decides whether that string meant "failed" or
//! "waiting".
//!
//! **The wire format is byte-identical to the TypeScript, PHP and Python twins
//! on purpose.** The same string is produced by a node running on any runtime
//! and decoded by a runner on any runtime — which is what lets a consumer
//! author in TypeScript, execute on Rust, and resume from a PHP admin screen
//! without the pause semantics quietly diverging.

use alloc::string::{String, ToString};

use fancy_json::{Map, Value};

/// A run halted, waiting for a person.
#[derive(Debug, Clone, PartialEq)]
pub struct PauseSignal {
    /// The node that paused — where a submission is injected on resume.
    pub node_id: String,
    /// What is being waited for.
    ///
    /// `approval` and `input` are what the builtins emit, but the value is
    /// open: a marketplace node may define its own (`signature`, `payment`),
    /// and a runner that does not recognise one should surface it rather than
    /// guess.
    pub awaiting: String,
    /// Context for whoever renders the wait — a form schema, the question, a
    /// diff to approve. Crosses a queue boundary and a database column.
    pub detail: Option<Value>,
}

impl PauseSignal {
    /// A pause on a node, waiting for something.
    #[must_use]
    pub fn new(node_id: &str, awaiting: &str, detail: Option<Value>) -> Self {
        Self {
            node_id: node_id.to_string(),
            awaiting: awaiting.to_string(),
            detail,
        }
    }

    /// Whether this is waiting for an approval decision.
    #[must_use]
    pub fn is_approval(&self) -> bool {
        self.awaiting == "approval"
    }

    /// Whether this is waiting for submitted values.
    #[must_use]
    pub fn is_input(&self) -> bool {
        self.awaiting == "input"
    }
}

/// Encode and decode the reason string that marks a pause.
pub struct Pause;

impl Pause {
    /// The prefix every current pause reason carries.
    pub const PREFIX: &'static str = "fancy-flow:pause:";

    /// Prefixes shipped before the contract existed, kept decodable forever.
    ///
    /// They are sitting in the `error` column of every run that paused under an
    /// older version, and a resume path that only works for new runs is not a
    /// resume path — it strands everything already in flight.
    pub const LEGACY_PREFIXES: [(&'static str, &'static str); 2] = [
        ("awaiting-approval:", "approval"),
        ("awaiting-input:", "input"),
    ];

    /// The reason string an executor aborts with.
    ///
    /// The payload is JSON rather than delimited fields because a node id may
    /// contain a colon, and a positional encoding that breaks on user data is
    /// the kind of bug that only ever shows up in someone else's graph.
    ///
    /// A `None` detail is omitted, matching PHP and Python. TypeScript
    /// distinguishes an absent detail from an explicitly-null one; the round
    /// trip is lossy in exactly that one direction.
    #[must_use]
    pub fn encode(signal: &PauseSignal) -> String {
        let mut payload = Map::new();
        payload.insert("nodeId", Value::from(signal.node_id.as_str()));
        payload.insert("awaiting", Value::from(signal.awaiting.as_str()));
        if let Some(detail) = &signal.detail {
            if !detail.is_null() {
                payload.insert("detail", detail.clone());
            }
        }
        alloc::format!(
            "{}{}",
            Self::PREFIX,
            fancy_json::to_string(&Value::Object(payload))
        )
    }

    /// Decode a run's error reason into a pause, or `None` for a real failure.
    ///
    /// This is the whole contract from a runner's side: call it on
    /// `RunResult.error`, and if it returns `Some`, persist the run as waiting
    /// on `signal.node_id` instead of failing it.
    #[must_use]
    pub fn decode(reason: Option<&str>) -> Option<PauseSignal> {
        let reason = reason?;

        if let Some(body) = reason.strip_prefix(Self::PREFIX) {
            let parsed = fancy_json::parse(body).ok()?;
            // A malformed payload is a CORRUPT pause, not something to invent a
            // node id for.
            let node_id = parsed.get("nodeId")?.as_str()?;
            let awaiting = parsed.get("awaiting")?.as_str()?;
            return Some(PauseSignal::new(
                node_id,
                awaiting,
                parsed.get("detail").cloned(),
            ));
        }

        for (prefix, awaiting) in Self::LEGACY_PREFIXES {
            if let Some(node_id) = reason.strip_prefix(prefix) {
                return Some(PauseSignal::new(node_id, awaiting, None));
            }
        }

        None
    }

    /// Whether a reason marks a pause rather than a failure.
    #[must_use]
    pub fn is_pause(reason: Option<&str>) -> bool {
        Self::decode(reason).is_some()
    }
}
