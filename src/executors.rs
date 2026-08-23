//! Maps nodes to the code that runs them.
//!
//! Three-tier lookup, matching every peer runtime:
//!
//! ```text
//! node id  ->  node kind  ->  "*" fallback
//! ```

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use fancy_json::Value;

use crate::error::RunAborted;
use crate::registry::{builtin, kind_id, NodeKindRegistry};
use crate::runtime::ExecutionContext;
use crate::schema::FlowNode;

/// Anything that can execute a node.
///
/// Deliberately **not** `Send + Sync`. This engine is synchronous by design and
/// a chain consumer drives it on one thread; requiring the bounds would force
/// them on every closure a host writes for no benefit it can use.
pub trait Executor {
    /// Run the node.
    ///
    /// # Errors
    ///
    /// [`RunAborted`] stops the run, carrying the reason **verbatim**. A human
    /// gate pauses through this same channel, so nothing may decorate it.
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted>;
}

impl<F> Executor for F
where
    F: Fn(&mut ExecutionContext<'_>) -> Result<Value, RunAborted>,
{
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
        self(ctx)
    }
}

/// A shared handle to an executor, so a registry can be forked cheaply.
pub type SharedExecutor = Rc<dyn Executor>;

/// Wrap a closure as a shared executor.
pub fn executor<F>(run: F) -> SharedExecutor
where
    F: Fn(&mut ExecutionContext<'_>) -> Result<Value, RunAborted> + 'static,
{
    Rc::new(run)
}

/// Wrap a boxed executor as a shared one.
#[must_use]
pub fn shared(executor: Box<dyn Executor>) -> SharedExecutor {
    Rc::from(executor)
}

/// Bind a whole map of kind -> executor in one call.
pub fn bind_many(
    registry: &mut ExecutorRegistry,
    bindings: alloc::vec::Vec<(&str, SharedExecutor)>,
) {
    for (kind, executor) in bindings {
        registry.bind(kind, executor);
    }
}

/// Bindings from node id / kind / `*` to executors.
#[derive(Clone, Default)]
pub struct ExecutorRegistry {
    by_kind: BTreeMap<String, SharedExecutor>,
    by_node: BTreeMap<String, SharedExecutor>,
    kinds: Option<Rc<NodeKindRegistry>>,
}

impl core::fmt::Debug for ExecutorRegistry {
    /// Counts, not contents: an executor is a closure and has nothing useful to
    /// print, and a registry holding two dozen of them would drown any other
    /// field in a `{:?}` of the whole run.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ExecutorRegistry")
            .field("kinds_bound", &self.by_kind.len())
            .field("nodes_bound", &self.by_node.len())
            .field("has_kind_catalogue", &self.kinds.is_some())
            .finish_non_exhaustive()
    }
}

impl ExecutorRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Consult `kinds` for alias expansion instead of the builtin index.
    #[must_use]
    pub fn with_kinds(mut self, kinds: Rc<NodeKindRegistry>) -> Self {
        self.kinds = Some(kinds);
        self
    }

    /// Bind an executor to a node kind, or to the `*` fallback.
    ///
    /// **Alias-aware for kinds this registry knows.** Binding `user_input`
    /// binds `@particle-academy/user_input` and `@fancy/user_input` with it,
    /// because they are the same kind and a caller overriding one means the
    /// kind.
    ///
    /// Keying literally was a silent trap in the PHP twin, and it cost a human
    /// gate: the builtins were bound under all three ids, lookup tries the
    /// node's literal id FIRST, and a durable override bound under the bare
    /// name only never matched a node saved as
    /// `@particle-academy/user_input`. Nothing errored; the run simply went
    /// straight past the person it was meant to stop for.
    ///
    /// An UNKNOWN kind is still bound literally. Expanding one would claim
    /// `@particle-academy/<name>` for somebody else's node, which is the
    /// opposite mistake.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "by value is what lets an Rc<Concrete> coerce to Rc<dyn Executor> \
                  at the call site, which is how all two dozen builtin bindings are \
                  written. A reference kills the unsized coercion and costs every \
                  caller an explicit cast to save one Rc clone."
    )]
    pub fn bind(&mut self, kind: &str, executor: SharedExecutor) -> &mut Self {
        self.by_kind.insert(kind.to_string(), Rc::clone(&executor));

        // `*` is a sentinel, not a kind: it has no aliases and must never be
        // expanded into namespaced spellings.
        if kind == "*" {
            return self;
        }

        for alias in self.alias_ids_for(kind) {
            self.by_kind.insert(alias, Rc::clone(&executor));
        }
        self
    }

    /// Bind an executor to a single node id — highest precedence.
    pub fn bind_node(&mut self, node_id: &str, executor: SharedExecutor) -> &mut Self {
        self.by_node.insert(node_id.to_string(), executor);
        self
    }

    /// Bind the `*` fallback.
    pub fn bind_fallback(&mut self, executor: SharedExecutor) -> &mut Self {
        self.by_kind.insert("*".to_string(), executor);
        self
    }

    /// A copy sharing every executor.
    ///
    /// Bind on the fork to override kinds for a single run without mutating the
    /// shared registry — what a durable driver does when it swaps in a pausing
    /// approval executor, or fences the graph off around one node.
    #[must_use]
    pub fn fork(&self) -> Self {
        self.clone()
    }

    /// Whether a binding exists under ANY id this kind answers to.
    #[must_use]
    pub fn has_kind(&self, kind: &str) -> bool {
        self.kind_candidates(kind)
            .iter()
            .any(|id| self.by_kind.contains_key(id))
    }

    /// Whether a `*` fallback is bound.
    #[must_use]
    pub fn has_fallback(&self) -> bool {
        self.by_kind.contains_key("*")
    }

    /// Resolve the executor for a node, following id -> kind -> `*`.
    ///
    /// The kind step tries EVERY id the kind answers to, not just the one
    /// written in the graph. Canonical ids are namespaced while a host may well
    /// have bound its executor under the bare name — resolving only the literal
    /// string would turn a rename into a breaking change in disguise.
    ///
    /// There is no `data.kind` fallback here and there does not need to be:
    /// [`FlowNode`] has exactly one kind field, so a kind has no second place
    /// to hide. That is what made the TypeScript 0.48.1 bug — a kind-keyed
    /// registry that never fired — unrepresentable in this port.
    #[must_use]
    pub fn resolve_for(&self, node: &FlowNode) -> Option<SharedExecutor> {
        if let Some(found) = self.by_node.get(&node.id) {
            return Some(Rc::clone(found));
        }

        if let Some(kind) = node.kind.as_deref() {
            for candidate in self.kind_candidates(kind) {
                if let Some(found) = self.by_kind.get(&candidate) {
                    return Some(Rc::clone(found));
                }
            }
        }

        self.by_kind.get("*").map(Rc::clone)
    }

    /// Every id a binding for `kind` might have been registered under.
    ///
    /// Explicit aliases from the kind registry come first — a custom kind may
    /// declare any alias it likes — then the naming-convention variants, which
    /// cover bindings made against a kind that was never registered.
    fn kind_candidates(&self, kind: &str) -> Vec<String> {
        let mut ordered: Vec<String> = alloc::vec![kind.to_string()];
        if let Some(kinds) = &self.kinds {
            for id in kinds.ids_for(kind) {
                crate::registry::dedup_push(&mut ordered, id);
            }
        }
        for variant in kind_id::variants(kind) {
            crate::registry::dedup_push(&mut ordered, variant);
        }
        ordered
    }

    /// Every id a KNOWN kind answers to, minus the one just bound.
    ///
    /// Declared aliases come from the kind registry, because convention alone
    /// cannot get you from `llm_branch` to `llm_router` — only the kind's own
    /// alias list does. Empty for a kind nothing has heard of.
    fn alias_ids_for(&self, kind: &str) -> Vec<String> {
        let declared = match &self.kinds {
            Some(kinds) if !kinds.ids_for(kind).is_empty() => kinds.ids_for(kind),
            // The kind registry is not necessarily populated when a binding is
            // made — a forked registry overriding a builtin often has none at
            // all — so fall back to the builtin index, which is the SAME
            // authority the base bindings were expanded from. Agreeing with it
            // by construction is the whole point.
            _ => builtin::kind_ids_for(kind_id::bare(kind)),
        };

        if declared.is_empty() {
            return Vec::new();
        }

        let mut ordered: Vec<String> = Vec::new();
        for id in declared.into_iter().chain(kind_id::variants(kind)) {
            if id != kind {
                crate::registry::dedup_push(&mut ordered, id);
            }
        }
        ordered
    }
}
