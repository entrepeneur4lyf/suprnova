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
//! When a mutation falls on no scope it emits a `tracing::trace!`
//! event on the `suprnova::context` target so misordered middleware
//! and missing-propagation bugs are observable in instrumented runs
//! without changing the no-panic contract.
//!
//! ### Propagating into spawned tasks
//!
//! Tokio task-locals do not flow through [`tokio::spawn`]: a child
//! task starts with an empty `CONTEXT` and `Context::get` returns
//! `None`. Use [`Context::current`] to snapshot the live store and
//! [`Context::scope`] to re-enter it inside the child:
//!
//! ```ignore
//! if let Some(store) = suprnova::context::Context::current() {
//!     tokio::spawn(suprnova::context::Context::scope(store, async move {
//!         // `Context::get`, `query_param`, etc. now see the parent bag.
//!     }));
//! }
//! ```
//!
//! [`ContextStore`] holds `Arc<DashMap>` handles, so the propagated
//! store is *live-shared* with the parent — writes from either side
//! are visible to the other for as long as the child holds the clone.
//! This is what audit/logging spawns want; if you need an isolated
//! snapshot, clone the maps explicitly.

use dashmap::DashMap;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

/// The backing store inside a request's context scope. Two maps —
/// visible (`data`) and hidden (`hidden`) — so logging serializers
/// can dump `all()` without leaking secrets.
///
/// `query` is the request's query-parameter snapshot. Populated by the
/// request middleware from the URL query string at scope-entry; read
/// by [`Context::query_param`] downstream. Stored separately from
/// `data` so paginate / scope-aware code can't accidentally collide
/// with user-set context keys.
#[derive(Default, Debug, Clone)]
pub struct ContextStore {
    data: Arc<DashMap<String, Value>>,
    hidden: Arc<DashMap<String, Value>>,
    query: Arc<DashMap<String, String>>,
}

impl ContextStore {
    /// Construct a store pre-populated with the supplied query map.
    /// Used by the request middleware so `Context::query_param` reads
    /// the real request's `?key=value` pairs.
    pub fn with_query(query: HashMap<String, String>) -> Self {
        let q = DashMap::with_capacity(query.len());
        for (k, v) in query {
            q.insert(k, v);
        }
        Self {
            data: Arc::new(DashMap::new()),
            hidden: Arc::new(DashMap::new()),
            query: Arc::new(q),
        }
    }
}

tokio::task_local! {
    pub static CONTEXT: ContextStore;
}

// Test-only override for `Context::query_param`.
//
// Per-thread so parallel tests don't collide — `#[tokio::test]` uses a
// current-thread runtime by default, so the future is driven on the
// calling OS thread and `thread_local!` isolates each test.
//
// Tests outside a `CONTEXT.scope` (the common case for unit tests of
// pure-function paginate logic) can install query params via
// `Context::test_set_query` without paying the cost of wrapping every
// async block in a context scope. Reads from the override take
// priority over the scoped `CONTEXT.query` bag.
//
// Production code never touches this — the setter is `#[cfg(test)]`-gated
// (only compiled in test builds) but the reader is always compiled so the
// fast path stays uniform.
thread_local! {
    static QUERY_OVERRIDE: RefCell<Option<HashMap<String, String>>> =
        const { RefCell::new(None) };
}

/// Facade for the per-request key/value bag.
pub struct Context;

impl Context {
    /// Snapshot the active context store, or `None` if no scope is
    /// installed on the current task.
    ///
    /// The returned [`ContextStore`] shares the parent's underlying
    /// maps via `Arc`, so it can be moved into a [`tokio::spawn`]ed
    /// task and re-entered via [`Self::scope`] to give the child task
    /// access to the same `data` / `hidden` / `query` bags. See the
    /// module-level docs for the propagation pattern.
    pub fn current() -> Option<ContextStore> {
        CONTEXT.try_with(|store| store.clone()).ok()
    }

    /// Enter `store` as the active context for the duration of `fut`.
    ///
    /// Thin wrapper around `CONTEXT.scope(store, fut)` so callers can
    /// hand the spawned future to [`tokio::spawn`] without naming the
    /// task-local directly:
    ///
    /// ```ignore
    /// if let Some(store) = Context::current() {
    ///     tokio::spawn(Context::scope(store, async move { /* ... */ }));
    /// }
    /// ```
    pub fn scope<F>(
        store: ContextStore,
        fut: F,
    ) -> tokio::task::futures::TaskLocalFuture<ContextStore, F>
    where
        F: std::future::Future,
    {
        CONTEXT.scope(store, fut)
    }

    /// Set `key` to `value` (replacing any existing entry).
    pub fn add<K, V>(key: K, value: V)
    where
        K: Into<String>,
        V: Serialize,
    {
        if CONTEXT
            .try_with(|store| {
                if let Ok(v) = serde_json::to_value(value) {
                    store.data.insert(key.into(), v);
                }
            })
            .is_err()
        {
            tracing::trace!(
                target: "suprnova::context",
                op = "add",
                "Context mutation discarded: no active scope on this task",
            );
        }
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
        if CONTEXT
            .try_with(|store| {
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
            })
            .is_err()
        {
            tracing::trace!(
                target: "suprnova::context",
                op = "push",
                "Context mutation discarded: no active scope on this task",
            );
        }
    }

    /// True if `key` is set in the visible bag.
    pub fn has(key: &str) -> bool {
        CONTEXT
            .try_with(|store| store.data.contains_key(key))
            .unwrap_or(false)
    }

    /// Remove `key` from both the visible and hidden bags.
    pub fn forget(key: &str) {
        if CONTEXT
            .try_with(|store| {
                store.data.remove(key);
                store.hidden.remove(key);
            })
            .is_err()
        {
            tracing::trace!(
                target: "suprnova::context",
                op = "forget",
                "Context mutation discarded: no active scope on this task",
            );
        }
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
        if CONTEXT
            .try_with(|store| {
                if let Ok(v) = serde_json::to_value(value) {
                    store.hidden.insert(key.into(), v);
                }
            })
            .is_err()
        {
            tracing::trace!(
                target: "suprnova::context",
                op = "hidden_add",
                "Context mutation discarded: no active scope on this task",
            );
        }
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

    /// Read a query parameter from the current request.
    ///
    /// Resolution order:
    /// 1. The thread-local test override (set via
    ///    [`Self::test_set_query`]) — non-empty in tests only.
    /// 2. The active [`CONTEXT`] scope's `query` bag — populated by
    ///    the request middleware from the URL's `?key=value` pairs.
    ///
    /// Returns `None` when the key is absent in both, including when
    /// called outside any context scope (the case for early-boot code,
    /// background workers without an installed scope, and tests
    /// without a query override).
    pub fn query_param(name: &str) -> Option<String> {
        // Test override (per-thread) wins over the scoped query bag.
        // Outside tests this branch always misses and falls through.
        let from_override = QUERY_OVERRIDE.with(|cell| {
            cell.borrow()
                .as_ref()
                .and_then(|map| map.get(name).cloned())
        });
        if from_override.is_some() {
            return from_override;
        }

        CONTEXT
            .try_with(|store| store.query.get(name).map(|v| v.value().clone()))
            .ok()
            .flatten()
    }

    /// **Test-only.** Install a query-parameter override on the current
    /// thread so [`Self::query_param`] reads it without requiring a
    /// wrapping [`CONTEXT::scope`][CONTEXT] call.
    ///
    /// Repeated calls overlay onto the same map. Use
    /// [`Self::test_clear_query`] to wipe between tests; otherwise an
    /// override from a previous `#[tokio::test]` body could leak into
    /// the next test scheduled onto the same OS thread (Cargo reuses
    /// threads across the per-binary thread pool).
    ///
    /// Compiled only in test builds; absent from release binaries.
    #[cfg(any(test, feature = "testing"))]
    pub fn test_set_query(name: impl Into<String>, value: impl Into<String>) {
        QUERY_OVERRIDE.with(|cell| {
            let mut slot = cell.borrow_mut();
            let map = slot.get_or_insert_with(HashMap::new);
            map.insert(name.into(), value.into());
        });
    }

    /// **Test-only.** Wipe the thread-local query override. Pair with
    /// [`Self::test_set_query`] to keep tests on the same OS thread
    /// from leaking query params into each other.
    ///
    /// Compiled only in test builds; absent from release binaries.
    #[cfg(any(test, feature = "testing"))]
    pub fn test_clear_query() {
        QUERY_OVERRIDE.with(|cell| {
            *cell.borrow_mut() = None;
        });
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

    #[tokio::test]
    async fn query_param_reads_scoped_store() {
        // Wipe any override leaked from a sibling test on the same OS
        // thread — the per-thread override otherwise wins over the
        // scoped store and would mask a real read-from-scope bug.
        Context::test_clear_query();
        let mut q = HashMap::new();
        q.insert("page".to_string(), "3".to_string());
        q.insert("sort".to_string(), "name".to_string());
        let store = ContextStore::with_query(q);
        CONTEXT
            .scope(store, async {
                assert_eq!(Context::query_param("page"), Some("3".to_string()));
                assert_eq!(Context::query_param("sort"), Some("name".to_string()));
                assert_eq!(Context::query_param("missing"), None);
            })
            .await;
    }

    #[tokio::test]
    async fn query_param_outside_scope_is_none() {
        // Clear any override that may have leaked in from a previous
        // test on the same OS thread.
        Context::test_clear_query();
        assert_eq!(Context::query_param("page"), None);
    }

    #[tokio::test]
    async fn test_set_query_overrides_outside_scope() {
        Context::test_clear_query();
        Context::test_set_query("page", "7");
        assert_eq!(Context::query_param("page"), Some("7".to_string()));
        Context::test_clear_query();
        assert_eq!(Context::query_param("page"), None);
    }

    #[tokio::test]
    async fn test_set_query_overrides_scoped_store() {
        // The override should win even when a scope is installed.
        let mut q = HashMap::new();
        q.insert("page".to_string(), "1".to_string());
        let store = ContextStore::with_query(q);
        Context::test_clear_query();
        Context::test_set_query("page", "42");
        let result = CONTEXT
            .scope(store, async { Context::query_param("page") })
            .await;
        assert_eq!(result, Some("42".to_string()));
        Context::test_clear_query();
    }

    #[tokio::test]
    async fn current_returns_none_outside_scope() {
        assert!(Context::current().is_none());
    }

    #[tokio::test]
    async fn current_snapshots_active_store() {
        CONTEXT
            .scope(ContextStore::default(), async {
                Context::add("k", "v");
                let snap = Context::current().expect("scope active");
                // The snapshot shares the same backing Arc, so writes
                // made *before* the snapshot are visible through it.
                assert_eq!(
                    snap.data.get("k").map(|v| v.value().clone()),
                    Some(json!("v")),
                );
            })
            .await;
    }

    #[tokio::test]
    async fn spawn_without_propagation_loses_context() {
        // Locks the gap the propagation helper closes: a bare
        // `tokio::spawn` inside a scope sees an empty `CONTEXT`.
        CONTEXT
            .scope(ContextStore::default(), async {
                Context::add("request_id", "abc-123");

                let child = tokio::spawn(async {
                    // No scope inherited — reads see nothing.
                    Context::get::<String>("request_id")
                });

                let observed = child.await.expect("spawned task joined");
                assert_eq!(observed, None);
            })
            .await;
    }

    #[tokio::test]
    async fn spawn_with_current_then_scope_propagates_context() {
        // The recommended propagation pattern: snapshot via
        // `Context::current()` and re-enter via `Context::scope` in
        // the child. The shared `Arc<DashMap>` makes the parent's
        // writes visible to the child.
        CONTEXT
            .scope(ContextStore::default(), async {
                Context::add("request_id", "abc-123");
                Context::hidden_add("user_id", 99i64);

                let store = Context::current().expect("scope active");
                let child = tokio::spawn(Context::scope(store, async {
                    (
                        Context::get::<String>("request_id"),
                        Context::hidden_get::<i64>("user_id"),
                    )
                }));

                let (rid, uid) = child.await.expect("spawned task joined");
                assert_eq!(rid, Some("abc-123".to_string()));
                assert_eq!(uid, Some(99));
            })
            .await;
    }

    #[tokio::test]
    async fn propagated_store_shares_query_bag() {
        // `query_param` reads should also flow through, since the
        // query bag is part of the same `ContextStore`.
        Context::test_clear_query();
        let mut q = HashMap::new();
        q.insert("page".to_string(), "5".to_string());
        let store = ContextStore::with_query(q);

        CONTEXT
            .scope(store, async {
                let snap = Context::current().expect("scope active");
                let child =
                    tokio::spawn(Context::scope(snap, async { Context::query_param("page") }));
                assert_eq!(child.await.unwrap(), Some("5".to_string()));
            })
            .await;
    }

    #[tokio::test]
    #[tracing_test::traced_test]
    async fn out_of_scope_mutations_emit_trace_event() {
        // Locks the "silent loss" gap: mutating ops still no-op as
        // documented, but they emit a `tracing::trace!` so the bug is
        // observable in instrumented runs.
        Context::add("k", "v");
        Context::push("stack", json!(1));
        Context::hidden_add("secret", "x");
        Context::forget("k");

        assert!(logs_contain("Context mutation discarded"));
        assert!(logs_contain("op=\"add\""));
        assert!(logs_contain("op=\"push\""));
        assert!(logs_contain("op=\"hidden_add\""));
        assert!(logs_contain("op=\"forget\""));
    }

    #[tokio::test]
    #[tracing_test::traced_test]
    async fn in_scope_mutations_do_not_emit_trace_event() {
        // The trace event is gated on the no-scope branch; the happy
        // path stays silent.
        CONTEXT
            .scope(ContextStore::default(), async {
                Context::add("k", "v");
                Context::push("stack", json!(1));
                Context::hidden_add("secret", "x");
                Context::forget("k");
            })
            .await;
        assert!(!logs_contain("Context mutation discarded"));
    }
}
