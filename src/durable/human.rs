//! Human gates that cannot be walked past.
//!
//! The framework-free executors in [`nodes::human`](crate::nodes::human) are
//! pass-throughs, so a graph can be exercised offline. These are their durable
//! replacements: they PAUSE the run, and a recorded answer -- not an input
//! value -- is what resumes it. Nothing here sleeps, polls or waits on a
//! person: a gate aborts with an encoded pause, which is a *return*.
//!
//! # Fail closed, and why
//!
//! A gate pauses because it **is** a human node, not because its input port
//! happens to be empty. This is not a preference; it is a fix. Both peer
//! runtimes once decided whether to pause by reading their own input, so a
//! pre-filled `values` or `approved` value ran the flow straight past the person
//! it was waiting for -- silently, with the run reporting success.
//!
//! Pre-filled inputs -- initial inputs, an upstream edge -- never satisfy a
//! gate. Only an answer recorded for THAT node does.
//!
//! Restoring the old behaviour is possible, explicit, and per node:
//! `autoAnswerFromInput`. Turn it on for a step that is a form when a human is
//! present and a pass-through when an upstream node already produced the
//! answer. On an approval node, weigh it harder: it means the graph, not a
//! person, can approve.
//!
//! # The other half of the fix
//!
//! Recording an answer for a node the run is not parked on is REFUSED rather
//! than queueing a write nobody reads. See [`Submissions::record`].
//!
//! # Binding them
//!
//! Bind under the bare kind name; [`ExecutorRegistry::bind`] binds every id the
//! kind answers to, so a node saved as `@particle-academy/user_input` pauses
//! too.
//!
//! [`ExecutorRegistry::bind`]: crate::executors::ExecutorRegistry::bind

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use core::cell::RefCell;
use core::fmt;

use fancy_json::{Map, Value};

use crate::error::{FlowError, RunAborted};
use crate::executors::Executor;
use crate::nodes::support::expr;
use crate::runtime::{ExecutionContext, Port};

/// An answer was recorded for a node the run is not waiting on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAwaitingHuman {
    /// What was wrong, naming both nodes.
    pub message: String,
}

impl fmt::Display for NotAwaitingHuman {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for NotAwaitingHuman {}

impl From<NotAwaitingHuman> for FlowError {
    fn from(error: NotAwaitingHuman) -> Self {
        Self::Contract(error.message)
    }
}

/// Answers recorded for human gates, keyed by node id.
///
/// Deliberately separate from the run's inputs. Keeping them in one bag is
/// precisely what let a pre-filled input satisfy a gate.
///
/// Shared between the host and the gates as `Rc<RefCell<Submissions>>` -- see
/// [`Submissions::shared`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Submissions {
    answers: BTreeMap<String, Value>,
    /// The node the run is currently parked on, if any.
    awaiting: Option<String>,
}

impl Submissions {
    /// No answers, and nothing waiting.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A fresh set, shared: one handle for the host, clones for the gates.
    #[must_use]
    pub fn shared() -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self::new()))
    }

    /// Record an answer for the node the run is parked on.
    ///
    /// # Errors
    ///
    /// [`NotAwaitingHuman`] when the run is not waiting on that node. A queued
    /// answer for a node that never paused is a write nobody reads -- and it
    /// looks, from the outside, exactly like a submission that worked.
    pub fn record(&mut self, node_id: &str, value: Value) -> Result<(), NotAwaitingHuman> {
        match self.awaiting.as_deref() {
            Some(awaiting) if awaiting != node_id => Err(NotAwaitingHuman {
                message: alloc::format!(
                    "This run is waiting on '{awaiting}', not '{node_id}'. Recording an answer \
                     for a node the run is not parked on would be stored and never read."
                ),
            }),
            None => Err(NotAwaitingHuman {
                message: alloc::format!(
                    "This run is not waiting for anyone, so an answer for '{node_id}' has \
                     nothing to resume."
                ),
            }),
            Some(_) => {
                self.answers.insert(node_id.to_string(), value);
                self.awaiting = None;
                Ok(())
            }
        }
    }

    /// Whether an answer is recorded for `node_id`.
    ///
    /// Presence, not truthiness: an empty submission is a real answer, and a
    /// truthiness test pauses forever on an empty form.
    #[must_use]
    pub fn answered(&self, node_id: &str) -> bool {
        self.answers.contains_key(node_id)
    }

    /// The answer recorded for `node_id`.
    #[must_use]
    pub fn answer(&self, node_id: &str) -> Option<&Value> {
        self.answers.get(node_id)
    }

    /// Mark the run as parked on `node_id`.
    pub fn park(&mut self, node_id: &str) {
        self.awaiting = Some(node_id.to_string());
    }

    /// The node the run is parked on, if any.
    #[must_use]
    pub fn awaiting(&self) -> Option<&str> {
        self.awaiting.as_deref()
    }
}

/// `user_input` -- pauses until a submission for THIS node is recorded.
#[derive(Debug, Clone)]
pub struct DurableUserInput {
    submissions: Rc<RefCell<Submissions>>,
}

impl DurableUserInput {
    /// A gate reading and parking on `submissions`.
    #[must_use]
    pub fn new(submissions: Rc<RefCell<Submissions>>) -> Self {
        Self { submissions }
    }
}

impl Executor for DurableUserInput {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
        let node_id = ctx.node().id.clone();

        if let Some(answer) = recorded(&self.submissions, ctx, &node_id)? {
            return Ok(answer);
        }

        if auto_answers(ctx) {
            if let Some(values) = present(ctx.input("values")) {
                return Ok(values.clone());
            }
        }

        park(&self.submissions, ctx, &node_id)?;

        let mut detail = Map::new();
        detail.insert(
            "title",
            ctx.option("title")
                .cloned()
                .unwrap_or_else(|| Value::from("Need your input")),
        );
        detail.insert(
            "fields",
            ctx.option("fields")
                .cloned()
                .unwrap_or_else(|| Value::Array(alloc::vec::Vec::new())),
        );
        Err(ctx.pause_for_human("input", Some(Value::Object(detail))))
    }
}

/// `human_approval` -- pauses until a decision for THIS node is recorded.
#[derive(Debug, Clone)]
pub struct DurableApproval {
    submissions: Rc<RefCell<Submissions>>,
}

impl DurableApproval {
    /// A gate reading and parking on `submissions`.
    #[must_use]
    pub fn new(submissions: Rc<RefCell<Submissions>>) -> Self {
        Self { submissions }
    }
}

impl Executor for DurableApproval {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
        let node_id = ctx.node().id.clone();

        if let Some(answer) = recorded(&self.submissions, ctx, &node_id)? {
            return Ok(decide(ctx, &answer));
        }

        if auto_answers(ctx) {
            if let Some(decision) = present(ctx.input("approved")) {
                let decision = decision.clone();
                return Ok(decide(ctx, &decision));
            }
        }

        park(&self.submissions, ctx, &node_id)?;

        let mut detail = Map::new();
        detail.insert(
            "title",
            ctx.option("title")
                .cloned()
                .unwrap_or_else(|| Value::from("Approve action")),
        );
        detail.insert(
            "description",
            ctx.option("description").cloned().unwrap_or(Value::Null),
        );
        Err(ctx.pause_for_human("approval", Some(Value::Object(detail))))
    }
}

/// `approved` or `denied`, carrying the gate's input on.
fn decide(ctx: &ExecutionContext<'_>, decision: &Value) -> Value {
    let port = if expr::truthy(decision) {
        "approved"
    } else {
        "denied"
    };
    Port::branch(port, ctx.input_or_all())
}

/// `autoAnswerFromInput` is on -- and only an explicit `true` turns it on.
fn auto_answers(ctx: &ExecutionContext<'_>) -> bool {
    ctx.option("autoAnswerFromInput") == Some(&Value::Bool(true))
}

/// An input that holds a value. A port bound to null holds no answer, as in
/// the Python twin, so a null upstream value still pauses: the fail-closed
/// direction.
fn present(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !value.is_null())
}

/// The answer recorded for this gate, if any.
///
/// A host holding a mutable borrow of the submissions across a run would make a
/// plain `borrow()` panic, and a panic here is an abort of the whole node. So
/// the gate fails the node instead, naming the cause.
fn recorded(
    submissions: &RefCell<Submissions>,
    ctx: &ExecutionContext<'_>,
    node_id: &str,
) -> Result<Option<Value>, RunAborted> {
    let submissions = submissions.try_borrow().map_err(|_| ctx.abort(BORROWED))?;
    Ok(submissions.answer(node_id).cloned())
}

fn park(
    submissions: &RefCell<Submissions>,
    ctx: &ExecutionContext<'_>,
    node_id: &str,
) -> Result<(), RunAborted> {
    submissions
        .try_borrow_mut()
        .map_err(|_| ctx.abort(BORROWED))?
        .park(node_id);
    Ok(())
}

const BORROWED: &str =
    "the human gate's Submissions are borrowed elsewhere while the run executes; release the \
     borrow before running the graph";
