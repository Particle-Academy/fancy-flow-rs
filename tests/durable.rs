//! The durable layer: frontier, claims, retries, dispatch and human gates that
//! fail closed.
//!
//! A port of fancy-flow-py's `tests/unit/test_durable.py`, case for case, plus
//! the behavioural cases of its `tests/parity/test_durable_driver_parity.py`
//! and the rules only this port has (the injected clock, deterministic owners).
//! Where Rust makes a Python case inapplicable, the comment at that spot says
//! why. The shared tables run in `tests/durable_conformance.rs`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use fancy_json::Value;

use fancy_flow::durable::{
    check_max_concurrent, replay_up_to, select_dispatch, Coordinator, DurableApproval,
    DurableUserInput, Frontier, InMemoryClaimStore, NodeClaimStore, NodeOutcomeStatus,
    NodeRunStatus, NodeState, RetryPolicy, RunState, Submissions, FENCE_PORT,
    UNLIMITED_CONCURRENCY,
};
use fancy_flow::executors::{executor, SharedExecutor};
use fancy_flow::runtime::{Clock, LogLevel, NodeStatus};
use fancy_flow::{
    ExecutorRegistry, FixedClock, FlowEdge, FlowError, FlowGraph, FlowNode, NodeKind,
    NodeKindRegistry, Pause, PauseSignal, PortDescriptor, RunEvent, RunIdentity, RunOptions,
};

// -- helpers ---------------------------------------------------------------

const CLOCK: FixedClock = FixedClock::new(1_700_000_000_000);

fn identity(run_key: &str) -> RunIdentity {
    RunIdentity::new(run_key, 0)
}

fn node(id: &str, kind: &str) -> FlowNode {
    FlowNode::new(id, kind)
}

fn edge(id: &str, source: &str, target: &str) -> FlowEdge {
    FlowEdge::new(id, source, target)
}

fn graph(nodes: Vec<FlowNode>, edges: Vec<FlowEdge>) -> FlowGraph {
    FlowGraph { nodes, edges }
}

fn json(text: &str) -> Value {
    fancy_json::parse(text).expect("test JSON parses")
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|id| (*id).to_string()).collect()
}

fn state(rows: Vec<(&str, NodeState)>) -> RunState {
    rows.into_iter()
        .map(|(id, row)| (id.to_string(), row))
        .collect()
}

/// An executor that records each node it runs and returns the node's id.
fn recording(ran: &Rc<RefCell<Vec<String>>>) -> SharedExecutor {
    let ran = Rc::clone(ran);
    executor(move |ctx| {
        ran.borrow_mut().push(ctx.node().id.clone());
        Ok(Value::from(ctx.node().id.as_str()))
    })
}

fn constant(value: Value) -> SharedExecutor {
    executor(move |_ctx| Ok(value.clone()))
}

fn registry(bindings: Vec<(&str, SharedExecutor)>) -> ExecutorRegistry {
    let mut executors = ExecutorRegistry::new();
    for (kind, bound) in bindings {
        executors.bind(kind, bound);
    }
    executors
}

fn warn_logs(events: &[RunEvent]) -> Vec<RunEvent> {
    events
        .iter()
        .filter(|event| event.kind == RunEvent::LOG && event.level == Some(LogLevel::Warn))
        .cloned()
        .collect()
}

/// A node id and what arrived on its `in` port.
type Received = (String, Option<Value>);

/// A clock that moves a second every time it is read.
struct Stepping(Cell<i64>);

impl Clock for Stepping {
    fn now_millis(&self) -> i64 {
        let now = self.0.get();
        self.0.set(now + 1000);
        now
    }
}

// -- the frontier ----------------------------------------------------------

fn chain() -> FlowGraph {
    graph(
        vec![node("a", "k"), node("b", "k"), node("c", "k")],
        vec![edge("e1", "a", "b"), edge("e2", "b", "c")],
    )
}

#[test]
fn only_entry_nodes_are_ready_at_the_start() {
    assert_eq!(
        Frontier::compute(&chain(), &RunState::new()).ready,
        ids(&["a"])
    );
}

#[test]
fn a_successor_unblocks_when_its_predecessor_publishes() {
    let rows = state(vec![("a", NodeState::completed(&["out"]))]);
    assert_eq!(Frontier::compute(&chain(), &rows).ready, ids(&["b"]));
}

#[test]
fn a_claimed_node_blocks_its_successors() {
    // Held, not settled. Dispatching past a node still being worked would run a
    // successor with inputs that do not exist yet.
    let rows = state(vec![("a", NodeState::new(NodeRunStatus::Claimed))]);
    assert!(Frontier::compute(&chain(), &rows).ready.is_empty());
}

#[test]
fn a_skip_cascades_through_the_whole_tail() {
    // Skipping SETTLES a node, which can skip its own successors. Without the
    // cascade a dead branch leaves the run stuck on a node no value will reach.
    let rows = state(vec![("a", NodeState::completed(&["other"]))]);
    let result = Frontier::compute(&chain(), &rows);

    assert!(result.ready.is_empty());
    assert_eq!(result.skipped, ids(&["b", "c"]));
}

#[test]
fn a_failed_node_settles_so_the_run_does_not_hang() {
    let rows = state(vec![(
        "a",
        NodeState::new(NodeRunStatus::Failed).with_error("boom"),
    )]);
    assert_eq!(Frontier::compute(&chain(), &rows).skipped, ids(&["b", "c"]));
}

#[test]
fn a_parallel_join_waits_for_both_sides() {
    // The "all settled" half of the rule: what distinguishes a genuine parallel
    // join from a merge after a decision. The join must not run on the first
    // arrival.
    let join = graph(
        vec![
            node("t", "k"),
            node("p1", "k"),
            node("p2", "k"),
            node("m", "k"),
        ],
        vec![
            edge("e1", "t", "p1"),
            edge("e2", "t", "p2"),
            edge("e3", "p1", "m"),
            edge("e4", "p2", "m"),
        ],
    );

    let mut half = state(vec![
        ("t", NodeState::completed(&["out"])),
        ("p1", NodeState::completed(&["out"])),
        ("p2", NodeState::new(NodeRunStatus::Claimed)),
    ]);
    assert!(!Frontier::compute(&join, &half)
        .ready
        .contains(&"m".to_string()));

    half.insert("p2".into(), NodeState::completed(&["out"]));
    assert_eq!(Frontier::compute(&join, &half).ready, ids(&["m"]));
}

#[test]
fn fan_out_returns_every_live_successor() {
    // Nothing says only one branch may be active.
    let fan = graph(
        vec![node("t", "k"), node("a", "k"), node("b", "k")],
        vec![edge("e1", "t", "a"), edge("e2", "t", "b")],
    );
    let rows = state(vec![("t", NodeState::completed(&["out"]))]);
    assert_eq!(Frontier::compute(&fan, &rows).ready, ids(&["a", "b"]));
}

#[test]
fn a_note_is_settled_by_the_frontier_not_dispatched() {
    // A graph can carry a lot of sticky notes, and each one would otherwise cost
    // a queue round trip.
    let notes = graph(vec![node("n", "@particle-academy/note")], vec![]);
    let result = Frontier::compute(&notes, &RunState::new());
    assert!(result.ready.is_empty());
    assert_eq!(result.skipped, ids(&["n"]));
}

#[test]
fn an_edge_reads_the_source_handle_it_names() {
    let branchy = graph(
        vec![node("a", "k"), node("b", "k")],
        vec![edge("e", "a", "b").from_port("true")],
    );
    let untaken = state(vec![("a", NodeState::completed(&["false"]))]);
    assert!(Frontier::compute(&branchy, &untaken).ready.is_empty());

    let taken = state(vec![("a", NodeState::completed(&["true"]))]);
    assert_eq!(Frontier::compute(&branchy, &taken).ready, ids(&["b"]));
}

#[test]
fn work_in_flight_distinguishes_waiting_from_stuck() {
    // An empty frontier means two different things, and only one is a bug.
    for (status, in_flight) in [
        (NodeRunStatus::Claimed, true),
        (NodeRunStatus::Paused, true),
        (NodeRunStatus::Completed, false),
    ] {
        let rows = state(vec![("a", NodeState::new(status))]);
        assert_eq!(Frontier::has_work_in_flight(&rows), in_flight, "{status:?}");
    }
}

// -- the claim store -------------------------------------------------------

#[test]
fn a_claim_is_exclusive() {
    let store = InMemoryClaimStore::new();
    assert!(store.claim("run", "n", "worker-a", 0));
    assert!(!store.claim("run", "n", "worker-b", 0));
}

#[test]
fn an_owner_can_re_enter_its_own_claim() {
    // What lets a job's retry resume instead of deadlocking against the row it
    // wrote itself.
    let store = InMemoryClaimStore::new();
    assert!(store.claim("run", "n", "worker-a", 0));
    assert!(store.claim("run", "n", "worker-a", 0));
    assert_eq!(store.state("run")["n"].attempts, 2);
}

#[test]
fn an_owner_re_enters_its_own_paused_claim_and_nobody_else_does() {
    // A paused gate is re-dispatched with the SAME token; the pause must not have
    // turned the row into something its own owner cannot get back into.
    let store = InMemoryClaimStore::new();
    assert!(store.claim("run", "gate", "worker-1", 0));
    store.pause("run", "gate", "fancy-flow:pause:{}");

    assert!(
        !store.claim("run", "gate", "worker-2", 0),
        "another owner loses"
    );
    assert_eq!(store.state("run")["gate"].status, NodeRunStatus::Paused);

    assert!(store.claim("run", "gate", "worker-1", 0));
    let row = &store.state("run")["gate"];
    assert_eq!((row.status, row.attempts), (NodeRunStatus::Claimed, 2));
}

#[test]
fn a_settled_node_cannot_be_reclaimed() {
    let store = InMemoryClaimStore::new();
    store.claim("run", "n", "worker-a", 0);
    store.complete("run", "n", Value::from("value"), ids(&["out"]));
    assert!(!store.claim("run", "n", "worker-a", 0));
}

#[test]
fn a_skip_reports_whether_it_settled_the_node() {
    // What lets the Coordinator deliver a skipped node's warning exactly once.
    // A settled row is never overwritten -- a stale skip landing on a COMPLETED
    // node would erase its output.
    let store = InMemoryClaimStore::new();
    assert!(store.skip("run", "n"));
    assert!(!store.skip("run", "n"));

    store.claim("run", "c", "worker-a", 0);
    assert!(store.skip("run", "c"), "a held claim is not settled");

    store.claim("run", "done", "worker-a", 0);
    store.complete("run", "done", Value::from("value"), ids(&["out"]));
    assert!(!store.skip("run", "done"));
    let row = &store.state("run")["done"];
    assert_eq!(row.status, NodeRunStatus::Completed);
    assert_eq!(row.output, Some(Value::from("value")));
}

#[test]
fn first_attempt_at_comes_from_the_injected_clock_and_never_moves() {
    // The retry clock is the FIRST attempt, not the latest claim. A store that
    // refreshed it on each reclaim would report a retry 25 hours late as seconds
    // old, and a connector would reuse a key the provider forgot yesterday.
    let store = InMemoryClaimStore::new();
    assert!(store.claim("run", "n", "worker-a", 1_000));
    assert!(store.claim("run", "n", "worker-a", 90_000_000));
    assert!(store.claim("run", "n", "worker-a", 180_000_000));

    let row = &store.state("run")["n"];
    assert_eq!(row.attempts, 3);
    assert_eq!(row.first_attempt_at, Some(1_000));

    // A row no attempt ever started carries no invented timestamp.
    store.skip("run", "never");
    assert_eq!(store.state("run")["never"].first_attempt_at, None);
}

#[test]
fn release_drops_a_paused_row() {
    let store = InMemoryClaimStore::new();
    store.claim("run", "gate", "worker-1", 0);
    store.pause("run", "gate", "fancy-flow:pause:{}");

    store.release("run", "gate");
    assert!(!store.state("run").contains_key("gate"));
    assert!(
        store.claim("run", "gate", "worker-2", 5),
        "a released gate is claimable by anyone"
    );
    assert_eq!(store.state("run")["gate"].first_attempt_at, Some(5));

    // Releasing a run or a node the store never saw is not an error.
    store.release("other", "x");
}

// -- retries ---------------------------------------------------------------

fn kinds_with(side_effects: Option<&str>) -> NodeKindRegistry {
    let mut kind = NodeKind::new("@particle-academy/git_pr_open", "io", "Open PR")
        .aliases(vec!["git_pr_open".to_string()]);
    if let Some(effects) = side_effects {
        kind = kind.side_effects(effects);
    }
    let mut kinds = NodeKindRegistry::new();
    kinds.register(kind);
    kinds
}

#[test]
fn an_unsafe_to_replay_node_gets_one_attempt_regardless() {
    // Retrying it repeats the effect rather than recovering from it --
    // `git_pr_open` opens a second pull request.
    let policy = RetryPolicy::new().with_tries(5).with_backoff_ms(30_000);
    let pr = node("n", "git_pr_open");
    let kinds = kinds_with(Some("unsafe-to-replay"));

    assert_eq!(policy.tries_for(&pr, &kinds), 1);
    assert_eq!(policy.backoff_ms_for(&pr, &kinds), 0);
    assert!(RetryPolicy::is_unsafe_to_replay(&pr, &kinds));
}

#[test]
fn undeclared_side_effects_take_the_configured_default() {
    // Not assumed safe, and not assumed unsafe. Inventing a safety claim on a
    // node author's behalf is how a retry loop posts the same webhook twice.
    let policy = RetryPolicy::new().with_tries(3).with_backoff_ms(250);
    let pr = node("n", "git_pr_open");
    assert_eq!(policy.tries_for(&pr, &kinds_with(None)), 3);
    assert_eq!(policy.backoff_ms_for(&pr, &kinds_with(None)), 250);
}

#[test]
fn a_per_kind_override_matches_any_spelling() {
    // Keying on the literal string makes the override silently stop applying
    // the day a kind is renamed.
    let policy = RetryPolicy::new()
        .with_tries(1)
        .with_kind_tries("git_pr_open", 4);
    let namespaced = node("n", "@particle-academy/git_pr_open");
    assert_eq!(policy.tries_for(&namespaced, &kinds_with(None)), 4);

    // And the other way round, for a kind nobody registered: convention alone.
    let bare = RetryPolicy::new().with_kind_tries("@particle-academy/webhook_x", 2);
    assert_eq!(
        bare.tries_for(&node("n", "webhook_x"), &NodeKindRegistry::new()),
        2
    );
}

#[test]
fn a_retryable_failure_leaves_the_claim_held_for_the_same_owner() {
    // Settling it FAILED mid-retry would skip everything downstream -- a run
    // reporting a tidy finish having done half its work.
    let calls = Rc::new(Cell::new(0));
    let counted = Rc::clone(&calls);
    let flaky = executor(move |ctx| {
        counted.set(counted.get() + 1);
        Err(ctx.abort("the API returned 500"))
    });
    let one = graph(vec![node("n", "flaky")], vec![]);
    let executors = registry(vec![("flaky", flaky)]);
    let coordinator = Coordinator::new(&one, &executors, identity("retry"), &CLOCK)
        .with_retry(RetryPolicy::new().with_tries(2));

    let first = coordinator.run_node("n", "worker-a");
    assert_eq!(
        (first.status, first.retryable, first.attempt),
        (NodeOutcomeStatus::Failed, true, 1)
    );
    let row = &coordinator.store().state("retry")["n"];
    assert_eq!(row.status, NodeRunStatus::Claimed);
    assert_eq!(row.owner.as_deref(), Some("worker-a"));

    assert!(!coordinator.run_node("n", "worker-b").claimed());
    assert_eq!(calls.get(), 1, "a lost race executed nothing");

    let last = coordinator.run_node("n", "worker-a");
    assert_eq!(
        (last.status, last.retryable, last.attempt),
        (NodeOutcomeStatus::Failed, false, 2)
    );
    assert_eq!(
        coordinator.store().state("retry")["n"].status,
        NodeRunStatus::Failed
    );
    assert_eq!(last.error.as_deref(), Some("the API returned 500"));
}

#[test]
fn run_to_completion_retries_up_to_the_policy_and_no_further() {
    let calls = Rc::new(Cell::new(0));
    let counted = Rc::clone(&calls);
    let recovers_on_third = executor(move |ctx| {
        counted.set(counted.get() + 1);
        if counted.get() < 3 {
            return Err(ctx.abort("transient"));
        }
        Ok(Value::from("done"))
    });
    let one = graph(vec![node("n", "flaky")], vec![]);
    let executors = registry(vec![("flaky", recovers_on_third)]);

    let short = Coordinator::new(&one, &executors, identity("two"), &CLOCK)
        .with_retry(RetryPolicy::new().with_tries(2))
        .run_to_completion(None);
    assert!(!short.ok);
    assert_eq!(short.error.as_deref(), Some("transient"));
    assert_eq!(calls.get(), 2);

    calls.set(0);
    let enough = Coordinator::new(&one, &executors, identity("three"), &CLOCK)
        .with_retry(RetryPolicy::new().with_tries(3))
        .run_to_completion(None);
    assert!(enough.ok, "{:?}", enough.error);
    assert_eq!(calls.get(), 3);
}

#[test]
fn an_unsafe_to_replay_node_is_attempted_once_by_the_coordinator() {
    let calls = Rc::new(Cell::new(0));
    let counted = Rc::clone(&calls);
    let failing = executor(move |ctx| {
        counted.set(counted.get() + 1);
        Err(ctx.abort("503"))
    });
    let kinds = kinds_with(Some("unsafe-to-replay"));
    let one = graph(vec![node("pr", "git_pr_open")], vec![]);
    let executors = registry(vec![("git_pr_open", failing)]);

    let result = Coordinator::new(&one, &executors, identity("pr"), &CLOCK)
        .with_kinds(&kinds)
        .with_retry(RetryPolicy::new().with_tries(5))
        .run_to_completion(None);

    assert!(!result.ok);
    assert_eq!(
        calls.get(),
        1,
        "a second attempt opens a second pull request"
    );
}

#[test]
fn every_attempt_runs_under_the_same_step_key_and_first_attempt_clock() {
    // The identity is per ATTEMPT, off the claim row: attempt counts up, while
    // the step key and `first_attempt_at` stay put -- which is what makes the
    // retry idempotent rather than duplicative.
    let seen: Rc<RefCell<Vec<(String, u32, i64)>>> = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&seen);
    let flaky = executor(move |ctx| {
        let run = ctx
            .run()
            .expect("the coordinator hands every attempt an identity");
        log.borrow_mut().push((
            run.step_key(&ctx.node().id, None),
            run.attempt(),
            run.first_attempt_at(),
        ));
        if log.borrow().len() == 1 {
            return Err(ctx.abort("transient"));
        }
        Ok(Value::from("charged"))
    });
    let one = graph(vec![node("charge", "flaky")], vec![]);
    let executors = registry(vec![("flaky", flaky)]);
    let clock = Stepping(Cell::new(5_000));

    let result = Coordinator::new(&one, &executors, RunIdentity::new("order-42", 1), &clock)
        .with_retry(RetryPolicy::new().with_tries(2))
        .run_to_completion(None);

    assert!(result.ok, "{:?}", result.error);
    assert_eq!(
        *seen.borrow(),
        vec![
            ("order-42:charge".to_string(), 1, 5_000),
            ("order-42:charge".to_string(), 2, 5_000),
        ],
        "the clock moved between claims; the retry clock did not"
    );
}

// -- run diagnostics under the durable driver -------------------------------

/// A claim store whose READS lag its writes.
///
/// Every `state()` returns the snapshot it was built with, which is what two
/// `advance()` calls see when they decide from the same frontier before either
/// has settled it.
///
/// Python's version can also make `skip` return `None`, standing in for a store
/// written before `skip` reported anything. That case is inapplicable here: the
/// trait is new, `skip` returns `bool`, and a store that reports nothing does
/// not compile.
struct Lagging<'s> {
    inner: &'s InMemoryClaimStore,
    snapshot: RunState,
}

impl NodeClaimStore for Lagging<'_> {
    fn claim(&self, run_key: &str, node_id: &str, owner: &str, now_millis: i64) -> bool {
        self.inner.claim(run_key, node_id, owner, now_millis)
    }

    fn state(&self, _run_key: &str) -> RunState {
        self.snapshot.clone()
    }

    fn complete(&self, run_key: &str, node_id: &str, output: Value, ports: Vec<String>) {
        self.inner.complete(run_key, node_id, output, ports);
    }

    fn skip(&self, run_key: &str, node_id: &str) -> bool {
        self.inner.skip(run_key, node_id)
    }

    fn fail(&self, run_key: &str, node_id: &str, error: &str) {
        self.inner.fail(run_key, node_id, error);
    }

    fn pause(&self, run_key: &str, node_id: &str, reason: &str) {
        self.inner.pause(run_key, node_id, reason);
    }
}

/// `s` publishes `out`; the only edge into `o` reads `result`.
fn undelivered_graph() -> FlowGraph {
    graph(
        vec![node("s", "src"), node("o", "sink")],
        vec![edge("e", "s", "o").from_port("result")],
    )
}

fn undelivered_executors() -> ExecutorRegistry {
    registry(vec![
        ("src", constant(Value::from("value"))),
        ("sink", constant(Value::from("sunk"))),
    ])
}

#[test]
fn a_skipped_targets_undelivered_edge_warning_reaches_the_host() {
    // The Python 0.21.0 known gap: the target never gets a job, so nothing
    // forwarded it.
    let events = RefCell::new(Vec::new());
    let sink = |event: &RunEvent| events.borrow_mut().push(event.clone());
    let gap = undelivered_graph();
    let executors = undelivered_executors();

    let result = Coordinator::new(&gap, &executors, identity("gap"), &CLOCK)
        .on_event(&sink)
        .run_to_completion(None);

    assert!(result.ok);
    let warnings = warn_logs(&events.borrow());
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].node_id.as_deref(), Some("o"));
    assert_eq!(
        warnings[0].detail,
        Some(json(r#"{"edge":"e","source":"s","sourceHandle":"result"}"#))
    );
    assert_eq!(
        warnings[0].message.as_deref(),
        Some(
            "Edge e reads port \"result\" from node s, which never publishes it \u{2014} nothing \
             would reach o at run time. Available: out. Leave sourceHandle off to read the \
             node's output."
        )
    );
}

#[test]
fn a_skip_another_caller_already_settled_does_not_warn_again() {
    // Exactly once per skipped node, however many callers reach the decision.
    let race = undelivered_graph();
    let executors = undelivered_executors();
    let inner = InMemoryClaimStore::new();
    let first = Coordinator::new(&race, &executors, identity("race"), &CLOCK).with_store(&inner);
    assert_eq!(
        first.run_node("s", "worker-a").status,
        NodeOutcomeStatus::Completed
    );

    let events = RefCell::new(Vec::new());
    let sink = |event: &RunEvent| events.borrow_mut().push(event.clone());
    let lagging = Coordinator::new(&race, &executors, identity("race"), &CLOCK)
        .with_store(Lagging {
            inner: &inner,
            snapshot: inner.state("race"),
        })
        .on_event(&sink);

    // Both read the same frontier, so both decide `o` is skipped.
    assert_eq!(
        Frontier::compute(&race, &lagging.store().state("race")).skipped,
        ids(&["o"])
    );
    let _ = lagging.advance();
    let _ = lagging.advance();

    assert_eq!(inner.state("race")["o"].status, NodeRunStatus::Skipped);
    let warned: Vec<Option<String>> = warn_logs(&events.borrow())
        .into_iter()
        .map(|event| event.node_id)
        .collect();
    assert_eq!(warned, vec![Some("o".to_string())]);
}

#[test]
fn the_replay_resolves_ports_against_the_coordinators_registry() {
    // A node with no declared outputs publishes on the RUN's catalogue's idea of
    // its kind. Replayed against no catalogue it publishes `out`, and the edge
    // reading the port its own catalogue declares never lights.
    let mut kinds = NodeKindRegistry::new();
    kinds.register(
        NodeKind::new("two_port_under_test", "logic", "Two ports")
            .outputs(vec![PortDescriptor::new("yes"), PortDescriptor::new("no")]),
    );
    let two = graph(
        vec![node("t", "two_port_under_test"), node("n", "k")],
        vec![edge("e", "t", "n").from_port("yes")],
    );
    let executors = registry(vec![
        ("two_port_under_test", constant(Value::from("v"))),
        (
            "k",
            executor(|ctx| Ok(ctx.input("in").cloned().unwrap_or(Value::Null))),
        ),
    ]);

    let coordinator =
        Coordinator::new(&two, &executors, identity("kinds"), &CLOCK).with_kinds(&kinds);
    let result = coordinator.run_to_completion(None);

    assert_eq!(
        coordinator.store().state("kinds")["t"].ports,
        ids(&["yes", "no"])
    );
    assert!(result.ok, "{:?}", result.error);
    assert_eq!(Value::Object(result.outputs), json(r#"{"t":"v","n":"v"}"#));

    // The CONTROL: the same graph without the catalogue routes differently.
    let without = Coordinator::new(&two, &executors, identity("no-kinds"), &CLOCK);
    let result = without.run_to_completion(None);
    assert_eq!(without.store().state("no-kinds")["t"].ports, ids(&["out"]));
    assert_eq!(Value::Object(result.outputs), json(r#"{"t":"v"}"#));
}

#[test]
fn a_job_forwards_only_its_own_nodes_events() {
    // A replay re-emits the whole completed prefix. Passing it through would
    // report a 20-node workflow as 200 status changes.
    let events = RefCell::new(Vec::new());
    let sink = |event: &RunEvent| events.borrow_mut().push(event.clone());
    let ran = Rc::new(RefCell::new(Vec::new()));
    let executors = registry(vec![("k", recording(&ran))]);
    let three = chain();

    let result = Coordinator::new(&three, &executors, identity("feed"), &CLOCK)
        .on_event(&sink)
        .run_to_completion(None);
    assert!(result.ok);

    for id in ["a", "b", "c"] {
        let done = events
            .borrow()
            .iter()
            .filter(|event| {
                event.kind == RunEvent::NODE_STATUS
                    && event.node_id.as_deref() == Some(id)
                    && event.status == Some(NodeStatus::Done)
            })
            .count();
        assert_eq!(
            done, 1,
            "node {id} reported done once, never as a republish"
        );
    }
    let resumed = events
        .borrow()
        .iter()
        .any(|event| event.text.as_deref() == Some("resumed"));
    assert!(!resumed, "no republish of the prefix reached the host");
}

// -- sibling jobs out of order ---------------------------------------------

#[test]
fn a_node_whose_earlier_sibling_has_not_finished_runs_instead_of_skipping() {
    // Siblings that become ready together are dispatched together, when a host
    // opts in. `b`'s replay walks the engine's topological order, and `a` --
    // unfinished, so not resumed -- comes first. An ABORTING fence stopped the
    // replay there, and `b` was recorded SKIPPED, never ran, and the run
    // completed as a success with half its work missing.
    //
    // This runs `b`'s job first, on purpose. The replay reads only completed
    // outputs, so an `a` that is claimed and still running looks exactly like
    // this unclaimed one.
    let ran: Rc<RefCell<Vec<Received>>> = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&ran);
    let record = executor(move |ctx| {
        log.borrow_mut()
            .push((ctx.node().id.clone(), ctx.input("in").cloned()));
        Ok(Value::from(ctx.node().id.as_str()))
    });
    let siblings = graph(
        vec![node("t", "rec"), node("a", "rec"), node("b", "rec")],
        vec![edge("e1", "t", "a"), edge("e2", "t", "b")],
    );
    let executors = registry(vec![("rec", record)]);
    // Opted in: the defect needs both siblings handed out at once, which the
    // serial default no longer does.
    let coordinator = Coordinator::new(&siblings, &executors, identity("siblings"), &CLOCK)
        .with_max_concurrent(UNLIMITED_CONCURRENCY)
        .expect("0 is a valid limit");

    assert_eq!(coordinator.advance(), ids(&["t"]));
    assert_eq!(
        coordinator.run_node("t", "w-t").status,
        NodeOutcomeStatus::Completed
    );
    assert_eq!(coordinator.advance(), ids(&["a", "b"]));

    let outcome = coordinator.run_node("b", "w-b");

    assert_eq!(
        (outcome.status, outcome.error.as_deref()),
        (NodeOutcomeStatus::Completed, None)
    );
    assert_eq!(
        coordinator.store().state("siblings")["b"].status,
        NodeRunStatus::Completed
    );
    // Its input is its own settled source's, never a fenced sibling's.
    assert_eq!(
        *ran.borrow(),
        vec![
            ("t".to_string(), None),
            ("b".to_string(), Some(Value::from("t")))
        ]
    );

    // The rest drains normally, and every node ran exactly once.
    let result = coordinator.run_to_completion(None);
    assert!(result.ok, "{:?}", result.error);
    assert_eq!(
        Value::Object(result.outputs),
        json(r#"{"t":"t","a":"a","b":"b"}"#)
    );
    assert_eq!(
        *ran.borrow(),
        vec![
            ("t".to_string(), None),
            ("b".to_string(), Some(Value::from("t"))),
            ("a".to_string(), Some(Value::from("t"))),
        ]
    );
}

#[test]
fn run_to_completion_runs_siblings_declared_out_of_topological_order() {
    // The same defect with no second worker anywhere: the frontier reports ready
    // nodes in the order the graph DECLARES them, the engine walks siblings in
    // the order their EDGES are listed. Here `b` is declared first and `a`'s edge
    // is listed first.
    let ran = Rc::new(RefCell::new(Vec::new()));
    let declared = graph(
        vec![node("t", "rec"), node("b", "rec"), node("a", "rec")],
        vec![edge("e1", "t", "a"), edge("e2", "t", "b")],
    );
    let executors = registry(vec![("rec", recording(&ran))]);

    let result = Coordinator::new(&declared, &executors, identity("declared"), &CLOCK)
        .run_to_completion(None);

    assert!(result.ok, "{:?}", result.error);
    assert_eq!(
        Value::Object(result.outputs),
        json(r#"{"t":"t","b":"b","a":"a"}"#)
    );
    let mut sorted = ran.borrow().clone();
    sorted.sort();
    assert_eq!(sorted, ids(&["a", "b", "t"]));
}

#[test]
fn a_target_the_engine_skips_is_recorded_skipped_not_failed() {
    // The engine's own verdict, whichever node happens to follow the target. An
    // aborting fence made a dead target with a later node read as a boundary and
    // skip, while a dead target that came LAST finished cleanly and was recorded
    // FAILED.
    for trailing in [true, false] {
        let mut nodes = vec![node("s", "rec"), node("dead", "rec")];
        let mut edges = vec![edge("e1", "s", "dead").from_port("never")];
        if trailing {
            nodes.push(node("later", "rec"));
            edges.push(edge("e2", "s", "later"));
        }
        let dead = graph(nodes, edges);
        let executors = registry(vec![("rec", constant(Value::from("v")))]);
        let run_key = format!("dead-{trailing}");
        let coordinator = Coordinator::new(&dead, &executors, identity(&run_key), &CLOCK);
        let _ = coordinator.run_node("s", "w-s");

        let outcome = coordinator.run_node("dead", "w-dead");

        assert_eq!(
            outcome.status,
            NodeOutcomeStatus::Skipped,
            "trailing={trailing}"
        );
        assert_eq!(
            coordinator.store().state(&run_key)["dead"].status,
            NodeRunStatus::Skipped,
            "trailing={trailing}"
        );
    }
}

// -- the replay ------------------------------------------------------------

#[test]
fn a_probe_with_no_target_executes_nothing() {
    let calls = Rc::new(Cell::new(0));
    let counted = Rc::clone(&calls);
    let counting = executor(move |_ctx| {
        counted.set(counted.get() + 1);
        Ok(Value::from("ran"))
    });
    let executors = registry(vec![("k", counting)]);
    let three = chain();

    let probe =
        replay_up_to(&three, None, &executors, None, &RunOptions::new()).expect("no signal");
    assert_eq!(calls.get(), 0);
    assert!(probe.result.ok);
    assert!(
        probe.result.outputs.is_empty(),
        "a fence's placeholder is not an output: {:?}",
        probe.result.outputs
    );

    // A resumed node is republished on its ports, and that is ALL a probe says.
    let resumed = RunOptions::new().resume("a", Value::from("stored"));
    let probe = replay_up_to(&three, None, &executors, None, &resumed).expect("no signal");
    assert_eq!(calls.get(), 0);
    assert_eq!(probe.output_of("a"), Some(&Value::from("stored")));
    assert_eq!(probe.ports_of("a"), ids(&["out"]).as_slice());
    assert!(probe.output_of("b").is_none());
    assert!(!probe
        .ports
        .values()
        .flatten()
        .any(|port| port == FENCE_PORT));
}

#[test]
fn a_completed_node_is_republished_not_re_executed() {
    // Resume must not repeat work, and must still route identically.
    let calls = Rc::new(RefCell::new(Vec::new()));
    let counted = Rc::clone(&calls);
    let upstream = executor(move |ctx| {
        counted.borrow_mut().push(ctx.node().id.clone());
        Ok(json(r#"{"n":1}"#))
    });
    let seen = Rc::new(RefCell::new(Vec::new()));
    let saw = Rc::clone(&seen);
    let downstream = executor(move |ctx| {
        saw.borrow_mut().push(ctx.input("in").cloned());
        Ok(Value::from("done"))
    });
    let two = graph(
        vec![node("a", "counted"), node("b", "downstream")],
        vec![edge("e", "a", "b")],
    );
    let executors = registry(vec![("counted", upstream), ("downstream", downstream)]);

    let coordinator = Coordinator::new(&two, &executors, identity("resume"), &CLOCK);
    let _ = coordinator.run_node("a", "w-a");
    let _ = coordinator.run_node("b", "w-b");

    assert_eq!(
        *calls.borrow(),
        ids(&["a"]),
        "the upstream node ran exactly once"
    );
    assert_eq!(
        *seen.borrow(),
        vec![Some(json(r#"{"n":1}"#))],
        "the downstream node saw the republished checkpoint"
    );
}

#[test]
fn a_lost_claim_race_is_a_no_op() {
    // Two workers, one node, one execution. The loser learns it lost from the
    // store, not from a duplicate side effect.
    let ran = Rc::new(RefCell::new(Vec::new()));
    let one = graph(vec![node("t", "rec")], vec![]);
    let executors = registry(vec![("rec", recording(&ran))]);
    let coordinator = Coordinator::new(&one, &executors, identity("race"), &CLOCK);

    assert_eq!(coordinator.advance(), ids(&["t"]));
    let first = coordinator.run_node("t", "worker-a");
    let second = coordinator.run_node("t", "worker-b");

    assert_eq!(first.status, NodeOutcomeStatus::Completed);
    assert!(!second.claimed());
    assert_eq!(second.status.as_str(), "not-claimed");
    assert_eq!(second.attempt, 0);
    assert_eq!(*ran.borrow(), ids(&["t"]));
}

// -- dispatch: one node at a time unless the host asks ---------------------

/// `t` fans out to `a`, `b` and `c`, declared in that order.
fn fan_out_graph() -> FlowGraph {
    graph(
        ["t", "a", "b", "c"]
            .iter()
            .map(|id| node(id, "rec"))
            .collect(),
        vec![
            edge("e1", "t", "a"),
            edge("e2", "t", "b"),
            edge("e3", "t", "c"),
        ],
    )
}

#[test]
fn a_queued_run_dispatches_one_node_at_a_time_by_default() {
    // fancy-flow-php#17, the owner's ruling: a node goes on the queue only after
    // the node before it finishes. Every `advance()` hands out exactly one id,
    // in declaration order.
    let ran = Rc::new(RefCell::new(Vec::new()));
    let fan = fan_out_graph();
    let executors = registry(vec![("rec", recording(&ran))]);
    let coordinator = Coordinator::new(&fan, &executors, identity("serial"), &CLOCK);

    let mut dispatched: Vec<Vec<String>> = Vec::new();
    loop {
        let ready = coordinator.advance();
        if ready.is_empty() {
            break;
        }
        for node_id in &ready {
            assert_eq!(
                coordinator
                    .run_node(node_id, &format!("w-{node_id}"))
                    .status,
                NodeOutcomeStatus::Completed
            );
        }
        dispatched.push(ready);
    }

    assert_eq!(
        dispatched,
        vec![ids(&["t"]), ids(&["a"]), ids(&["b"]), ids(&["c"])]
    );
    assert_eq!(*ran.borrow(), ids(&["t", "a", "b", "c"]));
}

#[test]
fn run_to_completion_takes_the_first_ready_node_in_declaration_order() {
    // `c` becomes ready once `a` settles and is declared before the waiting `b`,
    // so it goes next. A whole-frontier driver ran `b` first. Outputs unchanged.
    let ran = Rc::new(RefCell::new(Vec::new()));
    let order = graph(
        ["t", "c", "a", "b"]
            .iter()
            .map(|id| node(id, "rec"))
            .collect(),
        vec![
            edge("e1", "t", "a"),
            edge("e2", "t", "b"),
            edge("e3", "a", "c"),
        ],
    );
    let executors = registry(vec![("rec", recording(&ran))]);

    let result =
        Coordinator::new(&order, &executors, identity("order"), &CLOCK).run_to_completion(None);

    assert!(result.ok);
    assert_eq!(*ran.borrow(), ids(&["t", "a", "c", "b"]));
    // Graph node order, not completion order.
    let keys: Vec<&str> = result.outputs.keys().collect();
    assert_eq!(keys, ["t", "c", "a", "b"]);
}

#[test]
fn an_explicit_pass_limit_is_honoured_exactly() {
    // The DEFAULT allows a pass per node -- see the unit test beside
    // `default_passes` in `src/durable/coordinator.rs`, which is where Python's
    // monkeypatched floor is ported. An explicit limit is not raised.
    let ran = Rc::new(RefCell::new(Vec::new()));
    let four = graph(
        ["a", "b", "c", "d"]
            .iter()
            .map(|id| node(id, "rec"))
            .collect(),
        vec![
            edge("e1", "a", "b"),
            edge("e2", "b", "c"),
            edge("e3", "c", "d"),
        ],
    );
    let executors = registry(vec![("rec", recording(&ran))]);

    let short =
        Coordinator::new(&four, &executors, identity("short"), &CLOCK).run_to_completion(Some(2));
    assert!(!short.ok);
    assert_eq!(*ran.borrow(), ids(&["a", "b"]));

    ran.borrow_mut().clear();
    let full =
        Coordinator::new(&four, &executors, identity("passes"), &CLOCK).run_to_completion(None);
    assert!(full.ok, "{:?}", full.error);
    assert_eq!(*ran.borrow(), ids(&["a", "b", "c", "d"]));
}

#[test]
fn unlimited_dispatches_the_whole_frontier_and_a_cap_of_two_dispatches_two() {
    let fan = fan_out_graph();
    let ran = Rc::new(RefCell::new(Vec::new()));
    let executors = registry(vec![("rec", recording(&ran))]);

    for (limit, expected) in [
        (UNLIMITED_CONCURRENCY, ids(&["a", "b", "c"])),
        (2, ids(&["a", "b"])),
    ] {
        let coordinator = Coordinator::new(
            &fan,
            &executors,
            identity(&format!("limit-{limit}")),
            &CLOCK,
        )
        .with_max_concurrent(limit)
        .expect("a valid limit");
        assert_eq!(coordinator.advance(), ids(&["t"]));
        let _ = coordinator.run_node("t", "w-t");

        assert_eq!(coordinator.advance(), expected, "max_concurrent={limit}");
    }
}

#[test]
fn the_budget_counts_work_already_held_not_the_batch() {
    // A racing worker's claim takes the only slot. On a real queue two settles
    // each trigger an `advance()`, and a cap applied to one call's batch would
    // let each hand out its own quota.
    let three = graph(
        ["t", "a", "b"].iter().map(|id| node(id, "rec")).collect(),
        vec![edge("e1", "t", "a"), edge("e2", "t", "b")],
    );
    let ran = Rc::new(RefCell::new(Vec::new()));
    let executors = registry(vec![("rec", recording(&ran))]);

    let racing = |limit: i64| {
        let coordinator = Coordinator::new(&three, &executors, identity("race"), &CLOCK)
            .with_max_concurrent(limit)
            .expect("a valid limit");
        let _ = coordinator.run_node("t", "w-t");
        assert!(coordinator.store().claim("race", "a", "another-worker", 0));
        coordinator.advance()
    };

    assert!(racing(1).is_empty());
    // The CONTROL: the same held claim leaves a cap of two room for `b`.
    assert_eq!(racing(2), ids(&["b"]));
}

/// `t` fans out to an approval gate and to `b`; the gate is declared first.
fn paused_gate_graph() -> FlowGraph {
    graph(
        vec![
            node("t", "rec"),
            node("gate", "human_approval").with_outputs(vec![
                PortDescriptor::new("approved"),
                PortDescriptor::new("denied"),
            ]),
            node("b", "rec"),
        ],
        vec![edge("e1", "t", "gate"), edge("e2", "t", "b")],
    )
}

fn gated_executors(
    ran: &Rc<RefCell<Vec<String>>>,
    submissions: &Rc<RefCell<Submissions>>,
) -> ExecutorRegistry {
    let mut executors = registry(vec![("rec", recording(ran))]);
    executors.bind(
        "human_approval",
        Rc::new(DurableApproval::new(Rc::clone(submissions))),
    );
    executors
}

#[test]
fn a_paused_gate_keeps_its_slot() {
    // A pause does not park the RUN on this coordinator, so a queue adapter
    // calling `advance()` after the gate paused was handed `b` while the person
    // was still deciding. The serial case is built with the default on purpose.
    let ran = Rc::new(RefCell::new(Vec::new()));
    let gated = paused_gate_graph();
    let executors = gated_executors(&ran, &Submissions::shared());

    let serial = Coordinator::new(&gated, &executors, identity("serial"), &CLOCK);
    assert!(serial.run_to_completion(None).paused());
    assert_eq!(
        serial.store().state("serial")["gate"].status,
        NodeRunStatus::Paused
    );
    assert!(serial.advance().is_empty());

    // The CONTROL: under unlimited, the paused gate leaves `b` to be handed out.
    let parallel = Coordinator::new(&gated, &executors, identity("parallel"), &CLOCK)
        .with_max_concurrent(UNLIMITED_CONCURRENCY)
        .expect("a valid limit");
    assert!(parallel.run_to_completion(None).paused());
    assert_eq!(parallel.advance(), ids(&["b"]));

    assert_eq!(
        *ran.borrow(),
        ids(&["t", "t"]),
        "nothing but the trigger ran in either run"
    );
}

#[test]
fn a_released_gate_resumes_and_the_run_continues_under_serial() {
    // The slot a paused gate holds must not outlive the pause. Resuming is
    // recording the answer and releasing the paused row.
    let ran = Rc::new(RefCell::new(Vec::new()));
    let submissions = Submissions::shared();
    let resume = graph(
        vec![
            node("t", "rec"),
            node("gate", "human_approval").with_outputs(vec![
                PortDescriptor::new("approved"),
                PortDescriptor::new("denied"),
            ]),
            node("b", "rec"),
            node("d", "rec"),
        ],
        vec![
            edge("e1", "t", "gate"),
            edge("e2", "t", "b"),
            edge("e3", "gate", "d").from_port("approved"),
        ],
    );
    let executors = gated_executors(&ran, &submissions);
    let store = InMemoryClaimStore::new();
    let coordinator =
        Coordinator::new(&resume, &executors, identity("resume"), &CLOCK).with_store(&store);

    assert!(coordinator.run_to_completion(None).paused());
    assert!(coordinator.advance().is_empty());
    assert!(
        !store.state("resume").contains_key("b"),
        "nothing was handed out behind the gate"
    );

    submissions
        .borrow_mut()
        .record("gate", Value::Bool(true))
        .expect("the run is parked on the gate");
    store.release("resume", "gate");

    assert_eq!(coordinator.advance(), ids(&["gate"]));
    let resumed = coordinator.run_to_completion(None);

    assert!(resumed.ok, "{:?}", resumed.error);
    let mut keys: Vec<&str> = resumed.outputs.keys().collect();
    keys.sort_unstable();
    assert_eq!(keys, ["b", "d", "gate", "t"]);
    assert_eq!(*ran.borrow(), ids(&["t", "b", "d"]));
}

#[test]
fn a_gate_re_entered_by_its_own_owner_frees_the_slot() {
    // The other way back in: a queue adapter re-dispatches the paused job with
    // the SAME owner token, which re-enters the claim.
    let ran = Rc::new(RefCell::new(Vec::new()));
    let submissions = Submissions::shared();
    let gated = paused_gate_graph();
    let executors = gated_executors(&ran, &submissions);
    let coordinator = Coordinator::new(&gated, &executors, identity("owner"), &CLOCK);

    assert_eq!(coordinator.advance(), ids(&["t"]));
    let _ = coordinator.run_node("t", "w-t");
    assert_eq!(coordinator.advance(), ids(&["gate"]));
    assert_eq!(
        coordinator.run_node("gate", "worker-1").status,
        NodeOutcomeStatus::Paused
    );
    assert!(coordinator.advance().is_empty());

    submissions
        .borrow_mut()
        .record("gate", Value::Bool(true))
        .expect("parked on the gate");
    let resumed = coordinator.run_node("gate", "worker-1");
    assert_eq!(resumed.status, NodeOutcomeStatus::Completed);
    assert_eq!(resumed.ports, ids(&["approved"]));

    assert_eq!(coordinator.advance(), ids(&["b"]));
}

#[test]
fn an_invalid_max_concurrent_is_refused_by_name() {
    // Refused where it is set. Python also refuses `True`, `False`, `1.0`, `"2"`
    // and `None`; each of those is a type error here and cannot reach a running
    // program, so only the negative cases have anything to test.
    let one = chain();
    let executors = ExecutorRegistry::new();

    for value in [-1, i64::MIN] {
        let refused = Coordinator::new(&one, &executors, identity("bad"), &CLOCK)
            .with_max_concurrent(value)
            .err()
            .expect("a negative limit is refused");
        match refused {
            FlowError::Contract(message) => {
                assert!(message.contains("max_concurrent"), "{message}");
            }
            other => panic!("refused with the wrong error: {other:?}"),
        }
        assert!(check_max_concurrent(value).is_err());
    }

    assert_eq!(check_max_concurrent(0).ok(), Some(0));
    assert_eq!(check_max_concurrent(3).ok(), Some(3));
    assert_eq!(
        Coordinator::new(&one, &executors, identity("default"), &CLOCK).max_concurrent(),
        1,
        "serial is the default"
    );
}

#[test]
fn select_dispatch_counts_claimed_and_paused_and_never_goes_negative() {
    let rows = state(vec![
        ("x", NodeState::new(NodeRunStatus::Claimed)),
        ("y", NodeState::new(NodeRunStatus::Paused)),
        ("z", NodeState::completed(&["out"])),
        ("s", NodeState::new(NodeRunStatus::Skipped)),
        ("f", NodeState::new(NodeRunStatus::Failed)),
    ]);
    let ready = ids(&["c", "a", "b"]);
    assert!(select_dispatch(&ready, &rows, 1).is_empty());
    assert_eq!(select_dispatch(&ready, &rows, 3), ids(&["c"]));
    assert_eq!(select_dispatch(&ready, &rows, 0), ids(&["c", "a", "b"]));
}

// -- human gates -----------------------------------------------------------

fn gate_graph(kind: &str) -> FlowGraph {
    let outputs = if kind == "human_approval" {
        vec![
            PortDescriptor::new("approved"),
            PortDescriptor::new("denied"),
        ]
    } else {
        vec![PortDescriptor::new("out")]
    };
    graph(
        vec![node("t", "seed"), node("g", kind).with_outputs(outputs)],
        vec![edge("e", "t", "g")],
    )
}

fn gate_executors(seed: Value, submissions: &Rc<RefCell<Submissions>>) -> ExecutorRegistry {
    let mut executors = registry(vec![("seed", constant(seed))]);
    executors
        .bind(
            "user_input",
            Rc::new(DurableUserInput::new(Rc::clone(submissions))),
        )
        .bind(
            "human_approval",
            Rc::new(DurableApproval::new(Rc::clone(submissions))),
        );
    executors
}

#[test]
fn a_pre_filled_input_does_not_satisfy_a_user_input_gate() {
    // The fail-closed rule, and the bug it fixes: a gate pauses because it IS a
    // human node, not because its input port happens to be empty.
    let submissions = Submissions::shared();
    let gate = gate_graph("user_input");
    let executors = gate_executors(
        json(r#"{"values":{"answer":"already here"}}"#),
        &submissions,
    );

    let result =
        Coordinator::new(&gate, &executors, identity("gate"), &CLOCK).run_to_completion(None);

    assert!(result.paused());
    assert_eq!(
        result.pause,
        Some(PauseSignal::new(
            "g",
            "input",
            Some(json(r#"{"title":"Need your input","fields":[]}"#))
        ))
    );
    assert!(!result.outputs.contains_key("g"));

    // Stronger than the Python case: the answer arriving on the `values` port
    // itself -- what the offline pass-through reads -- does not satisfy it either.
    let direct = graph(
        vec![node("t", "seed"), node("g", "user_input")],
        vec![edge("e", "t", "g").to_port("values")],
    );
    let executors = gate_executors(json(r#"{"answer":"already here"}"#), &Submissions::shared());
    let result =
        Coordinator::new(&direct, &executors, identity("direct"), &CLOCK).run_to_completion(None);
    assert!(result.paused());
}

#[test]
fn a_gate_bound_under_its_bare_name_pauses_a_namespaced_node() {
    // A durable override bound under the bare name only once walked a run
    // straight past the person it was meant to stop for, in the PHP twin.
    let submissions = Submissions::shared();
    let namespaced = graph(
        vec![
            node("t", "seed"),
            node("g", "@particle-academy/human_approval"),
        ],
        vec![edge("e", "t", "g").to_port("approved")],
    );
    let executors = gate_executors(Value::Bool(true), &submissions);

    let result =
        Coordinator::new(&namespaced, &executors, identity("ns"), &CLOCK).run_to_completion(None);
    assert!(result.paused());
    assert_eq!(submissions.borrow().awaiting(), Some("g"));
}

#[test]
fn a_recorded_answer_resumes_the_gate() {
    let submissions = Submissions::shared();
    let gate = gate_graph("user_input");
    let executors = gate_executors(json("{}"), &submissions);
    let store = InMemoryClaimStore::new();

    let first = Coordinator::new(&gate, &executors, identity("gate"), &CLOCK).with_store(&store);
    assert!(first.run_to_completion(None).paused());

    submissions
        .borrow_mut()
        .record("g", json(r#"{"answer":"yes"}"#))
        .expect("parked on g");
    store.release("gate", "g");

    let second = Coordinator::new(&gate, &executors, identity("gate"), &CLOCK).with_store(&store);
    let resumed = second.run_to_completion(None);

    assert!(resumed.ok, "{:?}", resumed.error);
    assert_eq!(resumed.outputs.get("g"), Some(&json(r#"{"answer":"yes"}"#)));
}

#[test]
fn an_approval_decision_routes_the_branch_it_names() {
    for (answer, taken) in [
        (Value::Bool(true), "approved"),
        (Value::from("0"), "denied"),
    ] {
        let submissions = Submissions::shared();
        let gate = gate_graph("human_approval");
        let executors = gate_executors(Value::from("payload"), &submissions);
        let store = InMemoryClaimStore::new();
        let coordinator =
            Coordinator::new(&gate, &executors, identity("decide"), &CLOCK).with_store(&store);

        assert!(coordinator.run_to_completion(None).paused());
        submissions
            .borrow_mut()
            .record("g", answer)
            .expect("parked on g");
        store.release("decide", "g");

        assert!(coordinator.run_to_completion(None).ok);
        assert_eq!(store.state("decide")["g"].ports, ids(&[taken]));
    }
}

#[test]
fn auto_answer_from_input_is_opt_in_and_works_when_opted_into() {
    // The upstream value has to arrive on the `values` handle, which is what
    // "an upstream node already produced the answer" means.
    let opted = graph(
        vec![
            node("t", "seed"),
            node("g", "user_input").with_config("autoAnswerFromInput", Value::Bool(true)),
        ],
        vec![edge("e", "t", "g").to_port("values")],
    );
    let executors = gate_executors(
        json(r#"{"answer":"from upstream"}"#),
        &Submissions::shared(),
    );

    let result =
        Coordinator::new(&opted, &executors, identity("auto"), &CLOCK).run_to_completion(None);
    assert!(result.ok, "{:?}", result.error);
    assert_eq!(
        result.outputs.get("g"),
        Some(&json(r#"{"answer":"from upstream"}"#))
    );

    // Only an explicit `true` opts in.
    let truthy = graph(
        vec![
            node("t", "seed"),
            node("g", "user_input").with_config("autoAnswerFromInput", Value::from("yes")),
        ],
        vec![edge("e", "t", "g").to_port("values")],
    );
    let result =
        Coordinator::new(&truthy, &executors, identity("truthy"), &CLOCK).run_to_completion(None);
    assert!(result.paused());
}

#[test]
fn an_approval_gate_pauses_even_with_an_approved_flag_on_its_input() {
    // Weigh this one harder than a form: auto-answering means the graph, not a
    // person, approves.
    let approve = graph(
        vec![
            node("t", "seed"),
            node("g", "human_approval").with_outputs(vec![
                PortDescriptor::new("approved"),
                PortDescriptor::new("denied"),
            ]),
        ],
        vec![edge("e", "t", "g").to_port("approved")],
    );
    let executors = gate_executors(Value::Bool(true), &Submissions::shared());

    let result =
        Coordinator::new(&approve, &executors, identity("approve"), &CLOCK).run_to_completion(None);
    assert!(result.paused());
    assert!(result.pause.expect("paused").is_approval());
}

#[test]
fn recording_an_answer_for_the_wrong_node_is_refused() {
    // A queued answer for a node that never paused is a write nobody reads --
    // and from the outside it looks exactly like a submission that worked.
    let mut submissions = Submissions::new();
    submissions.park("g");

    let refused = submissions
        .record("somewhere-else", json(r#"{"answer":1}"#))
        .expect_err("not the node the run is parked on");
    assert!(refused.message.contains("somewhere-else"));
    assert!(!submissions.answered("somewhere-else"));
    assert_eq!(
        submissions.awaiting(),
        Some("g"),
        "still parked where it was"
    );
}

#[test]
fn recording_an_answer_when_nothing_is_waiting_is_refused() {
    assert!(Submissions::new()
        .record("g", json(r#"{"answer":1}"#))
        .is_err());
}

#[test]
fn an_empty_submission_is_a_real_answer() {
    // A truthiness test pauses forever on an empty form.
    let mut submissions = Submissions::new();
    submissions.park("g");
    submissions.record("g", json("{}")).expect("parked on g");
    assert!(submissions.answered("g"));
    assert_eq!(submissions.answer("g"), Some(&json("{}")));
    assert_eq!(submissions.awaiting(), None);
}

// -- the pause wire format -------------------------------------------------
//
// Python's durable suite pins the pause encoding too. Most of it is already
// pinned against the engine in `tests/engine_invariants.rs` (a colon in a node
// id, the `awaiting-approval:` prefix, an ordinary failure, a corrupt payload);
// the cases below are the ones that file does not repeat.

#[test]
fn a_pause_round_trips_through_its_reason_string() {
    let signal = PauseSignal::new("node-1", "input", Some(json(r#"{"fields":["a"]}"#)));
    assert_eq!(Pause::decode(Some(&Pause::encode(&signal))), Some(signal));
}

#[test]
fn the_legacy_input_prefix_stays_decodable_forever() {
    assert_eq!(
        Pause::decode(Some("awaiting-input:node-1")),
        Some(PauseSignal::new("node-1", "input", None))
    );
}

#[test]
fn a_corrupt_pause_payload_is_not_given_an_invented_node_id() {
    assert_eq!(
        Pause::decode(Some(&format!("{}{{not json", Pause::PREFIX))),
        None
    );
    assert_eq!(
        Pause::decode(Some(&format!(
            "{}{}",
            Pause::PREFIX,
            r#"{"awaiting":"input"}"#
        ))),
        None
    );
    assert!(!Pause::is_pause(None));
}
