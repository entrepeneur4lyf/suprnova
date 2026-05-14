//! Per-request key/value bag scoped via `tokio::task_local!`.
//!
//! Laravel-shaped: `Context::add(key, val)` for visible storage,
//! `Context::hidden_add(key, val)` for storage that doesn't appear in
//! `Context::all()` (sensitive data you want available to deep callers
//! but not serialized into logs). `Context::push(key, val)` appends to
//! a stack at that key. `Context::forget(key)` removes.
//!
//! Operations outside an active scope are silent no-ops — early-boot
//! code, tests without middleware setup, and background tasks that
//! choose not to install a scope all keep working without panics.

use dashmap::DashMap;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// The backing store inside a request's context scope. Two maps —
/// visible (`data`) and hidden (`hidden`) — so logging serializers
/// can dump `all()` without leaking secrets.
#[derive(Default, Debug, Clone)]
pub struct ContextStore {
    data: Arc<DashMap<String, Value>>,
    hidden: Arc<DashMap<String, Value>>,
}

tokio::task_local! {
    pub static CONTEXT: ContextStore;
}

/// Facade for the per-request key/value bag.
pub struct Context;

impl Context {
    /// Set `key` to `value` (replacing any existing entry).
    pub fn add<K, V>(key: K, value: V)
    where
        K: Into<String>,
        V: Serialize,
    {
        let _ = CONTEXT.try_with(|store| {
            if let Ok(v) = serde_json::to_value(value) {
                store.data.insert(key.into(), v);
            }
        });
    }

    /// Read `key` and deserialize. Returns `None` if absent, outside a
    /// scope, or if the stored value isn't of type `T`.
    pub fn get<T: DeserializeOwned>(key: &str) -> Option<T> {
        CONTEXT
            .try_with(|store| {
                store
                    .data
                    .get(key)
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
            })
            .ok()
            .flatten()
    }

    /// Push `value` onto a stack stored at `key`. Initializes an empty
    /// vec on the first push; converts a scalar at `key` into a
    /// `[scalar, value]` array on subsequent push.
    pub fn push<K, V>(key: K, value: V)
    where
        K: Into<String>,
        V: Serialize,
    {
        let _ = CONTEXT.try_with(|store| {
            let key = key.into();
            let new_val = serde_json::to_value(value).ok();
            let Some(new_val) = new_val else { return };
            store
                .data
                .entry(key)
                .and_modify(|existing| {
                    if let Value::Array(arr) = existing {
                        arr.push(new_val.clone());
                    } else {
                        *existing = Value::Array(vec![existing.clone(), new_val.clone()]);
                    }
                })
                .or_insert_with(|| Value::Array(vec![new_val]));
        });
    }

    /// True if `key` is set in the visible bag.
    pub fn has(key: &str) -> bool {
        CONTEXT
            .try_with(|store| store.data.contains_key(key))
            .unwrap_or(false)
    }

    /// Remove `key` from both the visible and hidden bags.
    pub fn forget(key: &str) {
        let _ = CONTEXT.try_with(|store| {
            store.data.remove(key);
            store.hidden.remove(key);
        });
    }

    /// Snapshot the visible bag. Returns an empty map outside a scope.
    pub fn all() -> HashMap<String, Value> {
        CONTEXT
            .try_with(|store| {
                store
                    .data
                    .iter()
                    .map(|kv| (kv.key().clone(), kv.value().clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Set `key` to `value` in the hidden bag (separate from the
    /// visible bag exposed by `all()`).
    pub fn hidden_add<K, V>(key: K, value: V)
    where
        K: Into<String>,
        V: Serialize,
    {
        let _ = CONTEXT.try_with(|store| {
            if let Ok(v) = serde_json::to_value(value) {
                store.hidden.insert(key.into(), v);
            }
        });
    }

    /// Read `key` from the hidden bag.
    pub fn hidden_get<T: DeserializeOwned>(key: &str) -> Option<T> {
        CONTEXT
            .try_with(|store| {
                store
                    .hidden
                    .get(key)
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
            })
            .ok()
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn add_and_get_round_trip_inside_scope() {
        CONTEXT
            .scope(ContextStore::default(), async {
                Context::add("user_id", 42i64);
                assert_eq!(Context::get::<i64>("user_id"), Some(42));
                assert!(Context::has("user_id"));
                assert_eq!(Context::get::<String>("missing"), None);
            })
            .await;
    }

    #[tokio::test]
    async fn push_appends_to_a_stack() {
        CONTEXT
            .scope(ContextStore::default(), async {
                Context::push("trail", json!("home"));
                Context::push("trail", json!("settings"));
                Context::push("trail", json!("billing"));
                let trail: Vec<String> = Context::get("trail").unwrap();
                assert_eq!(trail, vec!["home", "settings", "billing"]);
            })
            .await;
    }

    #[tokio::test]
    async fn forget_removes_key() {
        CONTEXT
            .scope(ContextStore::default(), async {
                Context::add("k", "v");
                Context::forget("k");
                assert!(!Context::has("k"));
            })
            .await;
    }

    #[tokio::test]
    async fn hidden_storage_is_separate_from_visible() {
        CONTEXT
            .scope(ContextStore::default(), async {
                Context::add("public_key", "yes");
                Context::hidden_add("secret_key", "shh");

                // all() returns visible only
                let all = Context::all();
                assert!(all.contains_key("public_key"));
                assert!(!all.contains_key("secret_key"));

                // hidden_get reads from hidden bag
                assert_eq!(
                    Context::hidden_get::<String>("secret_key"),
                    Some("shh".into())
                );
                assert_eq!(Context::hidden_get::<String>("public_key"), None);
            })
            .await;
    }

    #[tokio::test]
    async fn outside_scope_operations_are_silent_noops() {
        // Calling Context::add outside a scope must not panic.
        Context::add("k", "v");
        assert_eq!(Context::get::<String>("k"), None);
        assert!(!Context::has("k"));
        assert!(Context::all().is_empty());
    }
}
