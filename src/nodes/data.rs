//! Data executors — memory, a keyed store, and workflow-scoped values.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use fancy_json::{Map, Value};

use crate::error::RunAborted;
use crate::nodes::support::clients::KeyValueStore;
use crate::nodes::support::expr;
use crate::runtime::ExecutionContext;

/// `memory_store` — read / write / append per-conversation memory.
pub struct MemoryStore {
    store: Rc<dyn KeyValueStore>,
}

impl MemoryStore {
    /// Bind the executor to a store.
    #[must_use]
    pub fn new(store: Rc<dyn KeyValueStore>) -> Self {
        Self { store }
    }
}

impl crate::executors::Executor for MemoryStore {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
        let operation = ctx.option_string("operation", "read");
        let key = ctx.option_string("key", "");

        match operation.as_str() {
            "write" => {
                let value = expr::evaluate_in(ctx.option("value"), ctx.inputs());
                self.store.set(&key, value.clone());
                Ok(value)
            }
            "append" => {
                let value = expr::evaluate_in(ctx.option("value"), ctx.inputs());
                // `into_array` rather than a `match` that moves: `Value`
                // implements `Drop` (its tree is dismantled iteratively), so it
                // cannot be destructured by move. The accessors exist for this.
                let mut items = match self.store.get(&key) {
                    // An absent key appends into a fresh list rather than into
                    // `[null]`, which is what a `get(key, [])` default gives on
                    // the peers.
                    None => Vec::new(),
                    Some(existing) => match existing.into_array() {
                        Some(items) => items,
                        // A non-list value becomes the first element, so an
                        // append never silently discards what was there.
                        None => alloc::vec![self.store.get(&key).unwrap_or(Value::Null)],
                    },
                };
                items.push(value);
                let out = Value::Array(items);
                self.store.set(&key, out.clone());
                Ok(out)
            }
            _ => Ok(self.store.get(&key).unwrap_or(Value::Null)),
        }
    }
}

/// `data_store` — get / set / delete / query / list against a host store.
///
/// Keys are namespaced by `table` as `table/key`. `query` and `list` scan the
/// table; `query` additionally filters rows by the `where` map.
pub struct DataStore {
    store: Rc<dyn KeyValueStore>,
}

impl DataStore {
    /// Bind the executor to a store.
    #[must_use]
    pub fn new(store: Rc<dyn KeyValueStore>) -> Self {
        Self { store }
    }

    fn namespaced(table: &str, key: &str) -> String {
        alloc::format!("{table}/{key}")
    }

    fn rows(&self, table: &str) -> Map {
        let prefix = alloc::format!("{table}/");
        let mut out = Map::new();
        for (key, value) in self.store.all() {
            if let Some(stripped) = key.strip_prefix(prefix.as_str()) {
                out.insert(stripped, value);
            }
        }
        out
    }
}

impl crate::executors::Executor for DataStore {
    fn execute(&self, ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
        let operation = ctx.option_string("operation", "get");
        let table = ctx.option_string("table", "default");
        let key = expr::text(ctx.option("key"));

        match operation.as_str() {
            "set" => {
                let value = expr::evaluate_in(ctx.option("value"), ctx.inputs());
                self.store
                    .set(&Self::namespaced(&table, &key), value.clone());
                Ok(value)
            }
            "delete" => {
                self.store.delete(&Self::namespaced(&table, &key));
                let mut out = Map::new();
                out.insert("deleted", Value::from(key.as_str()));
                Ok(Value::Object(out))
            }
            "list" => Ok(Value::Object(self.rows(&table))),
            "query" => {
                let empty = Map::new();
                let filter = ctx
                    .option("where")
                    .and_then(Value::as_object)
                    .unwrap_or(&empty);
                let matched: Vec<Value> = self
                    .rows(&table)
                    .values()
                    .filter(|row| matches_filter(row, filter))
                    .cloned()
                    .collect();
                Ok(Value::Array(matched))
            }
            _ => Ok(self
                .store
                .get(&Self::namespaced(&table, &key))
                .unwrap_or(Value::Null)),
        }
    }
}

fn matches_filter(row: &Value, filter: &Map) -> bool {
    if filter.is_empty() {
        return true;
    }
    let Some(row) = row.as_object() else {
        return false;
    };
    filter
        .iter()
        .all(|(field, expected)| row.get(field) == Some(expected))
}

/// `variable` — a workflow-scoped value, resolved and emitted.
///
/// # Errors
///
/// Never.
pub fn variable(ctx: &mut ExecutionContext<'_>) -> Result<Value, RunAborted> {
    Ok(expr::evaluate_in(ctx.option("value"), ctx.inputs()))
}
