//! The built-in node library.
//!
//! The kinds `@particle-academy/fancy-flow` ships, ported kind for kind, plus
//! batteries-included framework-free executors:
//!
//! ```ignore
//! let mut kinds = NodeKindRegistry::new();
//! builtin::register(&mut kinds, true);          // install the kind definitions
//! let executors = builtin::executors(&deps);    // default executors, offline
//! ```
//!
//! On the TypeScript side the built-in kinds ship *without* executors — each
//! host wires where memory, HTTP and AI actually go. Every server twin ships
//! defaults so a flow runs out of the box, while each stays overridable through
//! the same kind + executor path a custom node uses.
//!
//! The literals below are written with BARE names because that reads better and
//! there are two dozen of them; namespacing is applied by [`canonicalize`], so
//! no kind can drift out of the convention by hand.
//!
//! **Quote the SET, not the count.** "27 builtin kinds" was already ambiguous
//! across two runtimes; with four it is meaningless.

use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::executors::ExecutorRegistry;
use crate::nodes::support::clients::ExecutorDeps;
use crate::nodes::{ai, data, human, io, logic, output, structural, trigger};
use crate::registry::{kind_id, ConfigField, NodeKind, NodeKindRegistry};
use crate::schema::PortDescriptor;

/// Give a built-in kind its CANONICAL namespaced id, keeping every previous
/// spelling as an alias.
fn canonicalize(mut kind: NodeKind) -> NodeKind {
    let bare = kind_id::bare(&kind.name).to_string();
    let declared = core::mem::take(&mut kind.aliases);

    kind.name = alloc::format!("{}{bare}", kind_id::NAMESPACE);

    let mut aliases: Vec<String> = Vec::new();
    for alias in kind_id::builtin_aliases(&bare).into_iter().chain(declared) {
        super::dedup_push(&mut aliases, alias);
    }
    kind.aliases = aliases;
    kind
}

fn ports(ids: &[&str]) -> Vec<PortDescriptor> {
    ids.iter().map(|id| PortDescriptor::new(*id)).collect()
}

/// The authorable kinds, with canonical namespaced ids and bare aliases.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "one literal table, read top to bottom against the peers' own. \
              Splitting it by domain would make 'is this kind declared the same \
              way as its TypeScript twin' a search rather than a scroll."
)]
pub fn kinds() -> Vec<NodeKind> {
    let mut out = Vec::new();

    // -- triggers --------------------------------------------------------
    out.push(
        NodeKind::new("manual_trigger", "trigger", "Manual trigger")
            .describe("Starts a run when a person or an agent asks for one.")
            .inputs(Vec::new())
            .outputs(ports(&["out"])),
    );
    out.push(
        NodeKind::new("webhook_trigger", "trigger", "Webhook")
            .describe("Starts a run from an inbound request.")
            .inputs(Vec::new())
            .outputs(ports(&["out"]))
            .config(alloc::vec![ConfigField::new("text", "path", "Path")]),
    );
    out.push(
        NodeKind::new("schedule_trigger", "trigger", "Schedule")
            .describe("Starts a run on a cron schedule.")
            .inputs(Vec::new())
            .outputs(ports(&["out"]))
            .config(alloc::vec![
                ConfigField::new("text", "cron", "Cron expression"),
                ConfigField::new("text", "timezone", "Timezone")
                    .default(fancy_json::Value::from("UTC")),
            ]),
    );

    // -- human -----------------------------------------------------------
    out.push(
        NodeKind::new("user_input", "human", "User input")
            .describe("Stops the run until a person submits the requested values.")
            .inputs(ports(&["in"]))
            .outputs(ports(&["out"]))
            .pauses_for("input")
            .side_effects("none"),
    );
    out.push(
        NodeKind::new("human_approval", "human", "Human approval")
            .describe("Stops the run until a person approves or denies.")
            .inputs(ports(&["in"]))
            .outputs(ports(&["approved", "denied"]))
            .pauses_for("approval")
            .side_effects("none"),
    );
    out.push(
        NodeKind::new("notify", "human", "Notify")
            .describe("Sends a message to a channel.")
            .inputs(ports(&["in"]))
            .outputs(ports(&["out"]))
            // A second attempt sends a second message. A durable driver must
            // not replay it blindly.
            .side_effects("unsafe-to-replay")
            .config(alloc::vec![
                ConfigField::new("select", "channel", "Channel")
                    .options(&[("slack", "Slack"), ("email", "Email"), ("sms", "SMS")])
                    .default(fancy_json::Value::from("slack")),
                ConfigField::new("text", "to", "To"),
                ConfigField::new("textarea", "message", "Message"),
            ]),
    );

    // -- logic -----------------------------------------------------------
    out.push(
        NodeKind::new("branch", "logic", "Branch")
            .describe("Two ports, exactly one taken.")
            .inputs(ports(&["in"]))
            .outputs(ports(&["true", "false"]))
            .config(alloc::vec![ConfigField::new(
                "expression",
                "condition",
                "Condition"
            )
            .required()]),
    );
    out.push(
        NodeKind::new("switch_case", "logic", "Switch")
            .describe("Routes on a key to one of several named ports.")
            .inputs(ports(&["in"]))
            // Config-driven ports: a `switch_case` node's real outputs come
            // from its `cases` map, and the editor serialises the resolved
            // ports into the document. `default` is the floor.
            .outputs(ports(&["default"]))
            .config(alloc::vec![
                ConfigField::new("expression", "value", "Value").required(),
                ConfigField::new("json", "cases", "Cases"),
            ]),
    );
    out.push(
        NodeKind::new("for_each", "logic", "For each")
            .describe("Publishes a collection and its size. Fan-out as DATA, not as jobs.")
            .inputs(ports(&["in"]))
            .outputs(ports(&["item", "done"]))
            .config(alloc::vec![ConfigField::new(
                "expression",
                "source",
                "Source"
            )]),
    );
    out.push(
        NodeKind::new("merge", "logic", "Merge")
            .describe("Several inputs, one value.")
            .inputs(ports(&["a", "b"]))
            .outputs(ports(&["out"]))
            .config(alloc::vec![ConfigField::new("select", "mode", "Mode")
                .options(&[("merge", "Merge"), ("concat", "Concat")])
                .default(fancy_json::Value::from("merge"))]),
    );
    out.push(
        NodeKind::new("wait", "logic", "Wait")
            .describe("A pause point. The framework-free default does not sleep.")
            .inputs(ports(&["in"]))
            .outputs(ports(&["out"]))
            .config(alloc::vec![
                ConfigField::new("select", "mode", "Mode")
                    .options(&[("duration", "Duration"), ("until", "Until")])
                    .default(fancy_json::Value::from("duration")),
                ConfigField::new("text", "duration", "Duration"),
            ]),
    );
    out.push(
        NodeKind::new("transform", "logic", "Transform")
            .describe("Reshape in place.")
            .inputs(ports(&["in"]))
            .outputs(ports(&["out"]))
            .config(alloc::vec![ConfigField::new(
                "expression",
                "expression",
                "Expression"
            )]),
    );
    out.push(
        NodeKind::new("subflow", "logic", "Subflow")
            .describe("Runs another workflow and brings its result home.")
            .inputs(ports(&["in"]))
            .outputs(ports(&["out"]))
            .config(alloc::vec![ConfigField::new(
                "text", "workflow", "Workflow"
            )
            .required()]),
    );

    // -- data ------------------------------------------------------------
    out.push(
        NodeKind::new("memory_store", "data", "Memory")
            .describe("Read, write or append per-conversation memory.")
            .inputs(ports(&["in"]))
            .outputs(ports(&["out"]))
            .side_effects("idempotent")
            .config(alloc::vec![
                ConfigField::new("select", "operation", "Operation")
                    .options(&[("read", "Read"), ("write", "Write"), ("append", "Append")])
                    .default(fancy_json::Value::from("read")),
                ConfigField::new("text", "key", "Key").required(),
                ConfigField::new("expression", "value", "Value"),
            ]),
    );
    out.push(
        NodeKind::new("data_store", "data", "Data store")
            .describe("Get, set, delete, query or list rows in a table.")
            .inputs(ports(&["in"]))
            .outputs(ports(&["out"]))
            .side_effects("idempotent")
            .config(alloc::vec![
                ConfigField::new("select", "operation", "Operation")
                    .options(&[
                        ("get", "Get"),
                        ("set", "Set"),
                        ("delete", "Delete"),
                        ("query", "Query"),
                        ("list", "List"),
                    ])
                    .default(fancy_json::Value::from("get")),
                ConfigField::new("text", "table", "Table")
                    .default(fancy_json::Value::from("default")),
                ConfigField::new("expression", "key", "Key"),
                ConfigField::new("expression", "value", "Value"),
                ConfigField::new("json", "where", "Where"),
            ]),
    );
    out.push(
        NodeKind::new("variable", "data", "Variable")
            .describe("A workflow-scoped value.")
            .inputs(ports(&["in"]))
            .outputs(ports(&["out"]))
            .config(alloc::vec![ConfigField::new(
                "expression",
                "value",
                "Value"
            )]),
    );

    // -- ai --------------------------------------------------------------
    out.push(
        NodeKind::new("llm_call", "ai", "LLM call")
            .describe("A free-form completion.")
            .inputs(ports(&["in"]))
            .outputs(ports(&["out"]))
            .side_effects("unsafe-to-replay")
            .config(alloc::vec![
                ConfigField::new("text", "model", "Model"),
                ConfigField::new("textarea", "prompt", "Prompt").required(),
                ConfigField::new("number", "temperature", "Temperature"),
            ]),
    );
    out.push(
        // Renamed from `llm_branch`. Survivable ONLY because the old spelling
        // stays registered as an alias — no amount of prefix arithmetic gets
        // you from one name to the other.
        NodeKind::new("llm_router", "ai", "LLM router")
            .describe("Routes to one of several named branches.")
            .inputs(ports(&["in"]))
            .outputs(ports(&["default"]))
            .aliases(alloc::vec![
                "llm_branch".to_string(),
                alloc::format!("{}llm_branch", kind_id::NAMESPACE),
                alloc::format!("{}llm_branch", kind_id::LEGACY_NAMESPACE),
            ])
            .config(alloc::vec![ConfigField::new("json", "routes", "Routes")]),
    );
    out.push(
        NodeKind::new("tool_use", "ai", "Tool use")
            .describe("Invokes a named tool with resolved arguments.")
            .inputs(ports(&["in"]))
            .outputs(ports(&["out"]))
            .side_effects("unsafe-to-replay")
            .config(alloc::vec![
                ConfigField::new("text", "tool", "Tool").required(),
                ConfigField::new("expression", "args", "Arguments"),
            ]),
    );
    out.push(
        NodeKind::new("embed_search", "ai", "Embedding search")
            .describe("Embeds a query and searches a vector store.")
            .inputs(ports(&["in"]))
            .outputs(ports(&["out"]))
            .config(alloc::vec![
                ConfigField::new("expression", "query", "Query").required(),
                ConfigField::new("number", "topK", "Top K").default(fancy_json::Value::from(5)),
            ]),
    );

    // -- io --------------------------------------------------------------
    out.push(
        NodeKind::new("api_request", "io", "API request")
            .describe("An HTTP request to any URL.")
            .inputs(ports(&["in"]))
            .outputs(ports(&["out"]))
            .side_effects("unsafe-to-replay")
            .config(alloc::vec![
                ConfigField::new("select", "method", "Method")
                    .options(&[
                        ("GET", "GET"),
                        ("POST", "POST"),
                        ("PUT", "PUT"),
                        ("PATCH", "PATCH"),
                        ("DELETE", "DELETE"),
                    ])
                    .default(fancy_json::Value::from("GET")),
                ConfigField::new("expression", "url", "URL").required(),
                ConfigField::new("json", "headers", "Headers"),
                ConfigField::new("expression", "body", "Body"),
            ]),
    );
    out.push(
        NodeKind::new("webhook_out", "io", "Webhook out")
            .describe("POSTs a payload to a configured URL.")
            .inputs(ports(&["in"]))
            .outputs(ports(&["out"]))
            .side_effects("unsafe-to-replay")
            .config(alloc::vec![
                ConfigField::new("expression", "url", "URL").required(),
                ConfigField::new("json", "headers", "Headers"),
                ConfigField::new("expression", "payload", "Payload"),
            ]),
    );

    // -- output ----------------------------------------------------------
    out.push(
        NodeKind::new("output", "output", "Output")
            .describe("Captures a value into the run's outputs.")
            .inputs(ports(&["in"]))
            // A terminal node: an EMPTY declared list, not an absent one. The
            // engine honours the distinction, and collapsing it is how a
            // terminal node starts publishing on `out`.
            .outputs(Vec::new()),
    );
    out.push(
        NodeKind::new("log", "output", "Log")
            .describe("Writes a line to the run feed.")
            .inputs(ports(&["in"]))
            .outputs(Vec::new())
            .config(alloc::vec![
                ConfigField::new("select", "level", "Level")
                    .options(&[
                        ("info", "Info"),
                        ("warn", "Warn"),
                        ("error", "Error"),
                        ("debug", "Debug"),
                    ])
                    .default(fancy_json::Value::from("info")),
                ConfigField::new("textarea", "message", "Message"),
            ]),
    );

    out.into_iter().map(canonicalize).collect()
}

/// Kinds the engine handles specially.
///
/// `note` is never executed; `subgraph` runs a nested flow. Neither is part of
/// the TypeScript `builtin.ts` registration, so they are opt-in.
#[must_use]
pub fn structural_kinds() -> Vec<NodeKind> {
    alloc::vec![
        canonicalize(
            NodeKind::new("note", super::category::ANNOTATION, "Note")
                .describe("A canvas annotation. Visual only — never fed to a runner.")
                .config(alloc::vec![ConfigField::new("textarea", "text", "Text")]),
        ),
        canonicalize(
            NodeKind::new("subgraph", "structural", "Subgraph")
                .describe("Runs a nested workflow held inline in this node's config.")
                .inputs(ports(&["in"]))
                .outputs(ports(&["out"]))
                .config(alloc::vec![ConfigField::new("json", "graph", "Graph")]),
        ),
    ]
}

/// The `agent` kind — an LLM agent with tools and bounded multi-step reasoning.
///
/// Not part of the TypeScript `builtin.ts` mirror, so it is opt-in. Declared
/// here rather than omitted so [`kind_ids_for`] knows its aliases: an executor
/// bound for it must expand the same way every other builtin does.
///
/// **It ships no executor.** An agent loop without an LLM adapter is a stub,
/// and a stub that runs is worse than a kind that refuses to.
#[must_use]
pub fn agent_kind() -> NodeKind {
    canonicalize(
        NodeKind::new("agent", "ai", "Agent")
            .describe("An LLM agent with tools and bounded multi-step reasoning.")
            .inputs(ports(&["in"]))
            .outputs(ports(&["out"]))
            .side_effects("unsafe-to-replay")
            .config(alloc::vec![
                ConfigField::new("text", "model", "Model"),
                ConfigField::new("textarea", "instructions", "Instructions"),
                ConfigField::new("json", "tools", "Tools"),
                ConfigField::new("number", "max_steps", "Max steps")
                    .default(fancy_json::Value::from(8)),
            ]),
    )
}

/// Install every built-in kind definition into a registry.
pub fn register(registry: &mut NodeKindRegistry, with_structural: bool) -> &mut NodeKindRegistry {
    for kind in kinds() {
        registry.register(kind);
    }
    if with_structural {
        for kind in structural_kinds() {
            registry.register(kind);
        }
    }
    registry
}

/// Every id the built-in kind named `bare` answers to; empty when unknown.
///
/// PUBLIC because an override has to agree with the bindings it is overriding.
/// [`ExecutorRegistry::bind`] consults this so that replacing `user_input`
/// replaces it under all three ids the way the base bindings were made — and
/// the kind registry is not necessarily populated at bind time, so it cannot be
/// the only source.
#[must_use]
pub fn kind_ids_for(bare: &str) -> Vec<String> {
    kinds()
        .into_iter()
        .chain(structural_kinds())
        .chain(core::iter::once(agent_kind()))
        .find(|kind| kind_id::bare(&kind.name) == bare)
        .map(|kind| kind.ids())
        .unwrap_or_default()
}

/// A registry pre-bound with the default executor for every built-in kind.
///
/// Bindings are made under EVERY id each kind answers to, not just the
/// canonical one. Convention-derived variants are not enough: `llm_router` was
/// renamed from `llm_branch`, and no amount of prefix arithmetic gets you from
/// one to the other — only the kind's declared alias list does.
#[must_use]
pub fn executors(deps: &ExecutorDeps) -> ExecutorRegistry {
    let shared = Rc::new(ExecutorDeps {
        memory: Rc::clone(&deps.memory),
        data: Rc::clone(&deps.data),
        http: Rc::clone(&deps.http),
        completions: Rc::clone(&deps.completions),
        tools: Rc::clone(&deps.tools),
        vectors: Rc::clone(&deps.vectors),
        notifier: Rc::clone(&deps.notifier),
    });

    let mut registry = ExecutorRegistry::new();

    registry
        .bind(
            "manual_trigger",
            crate::executors::executor(trigger::manual_trigger),
        )
        .bind(
            "webhook_trigger",
            crate::executors::executor(trigger::webhook_trigger),
        )
        .bind(
            "schedule_trigger",
            crate::executors::executor(trigger::schedule_trigger),
        )
        .bind("user_input", crate::executors::executor(human::user_input))
        .bind(
            "human_approval",
            crate::executors::executor(human::human_approval),
        )
        .bind(
            "notify",
            Rc::new(human::Notify::new(Rc::clone(&deps.notifier))),
        )
        .bind("branch", crate::executors::executor(logic::branch))
        .bind(
            "switch_case",
            crate::executors::executor(logic::switch_case),
        )
        .bind("for_each", crate::executors::executor(logic::for_each))
        .bind("merge", crate::executors::executor(logic::merge))
        .bind("wait", crate::executors::executor(logic::wait))
        .bind("transform", crate::executors::executor(logic::transform))
        .bind(
            "subflow",
            Rc::new(structural::Subflow::new(Rc::clone(&shared), None)),
        )
        .bind(
            "memory_store",
            Rc::new(data::MemoryStore::new(Rc::clone(&deps.memory))),
        )
        .bind(
            "data_store",
            Rc::new(data::DataStore::new(Rc::clone(&deps.data))),
        )
        .bind("variable", crate::executors::executor(data::variable))
        .bind(
            "llm_call",
            Rc::new(ai::LlmCall::new(Rc::clone(&deps.completions))),
        )
        .bind("llm_router", Rc::new(ai::LlmRouter))
        .bind(
            "tool_use",
            Rc::new(ai::ToolUse::new(Rc::clone(&deps.tools))),
        )
        .bind(
            "embed_search",
            Rc::new(ai::EmbedSearch::new(Rc::clone(&deps.vectors))),
        )
        .bind(
            "api_request",
            Rc::new(io::ApiRequest::new(Rc::clone(&deps.http))),
        )
        .bind(
            "webhook_out",
            Rc::new(io::WebhookOut::new(Rc::clone(&deps.http))),
        )
        .bind("output", crate::executors::executor(output::output))
        .bind("log", crate::executors::executor(output::log))
        .bind("subgraph", Rc::new(structural::Subgraph::new(shared)));

    registry
}
