//! [`FeatureSync`] — keeps live evaluators consistent with persisted flag state.
//!
//! featureflag's `is_enabled` is synchronous, which means every concrete
//! evaluator we ship keeps some flavour of read-side state ahead of the
//! database: [`DatabaseEvaluator`](crate::features::DatabaseEvaluator)
//! holds an in-memory snapshot, [`CachedEvaluator`](crate::features::CachedEvaluator)
//! holds per-`(feature, scope)` TTL entries. The moment
//! [`admin::upsert`](crate::features::admin::upsert) (or any other
//! mutation path) writes a row, those in-memory views go stale until
//! something resets them.
//!
//! **The whole point of feature flags is sub-second propagation for
//! kill-switch toggles.** Waiting on a TTL or a polling reload isn't
//! good enough — by the time the change is visible, the incident is
//! already in production. This module is the synchronous-fan-out hook
//! every write path calls into so live evaluators reflect the persisted
//! truth before the mutation returns.
//!
//! # Trait shape
//!
//! [`FeatureSync`] carries a single async method,
//! [`FeatureSync::on_flag_changed`], invoked with the
//! `(feature, scope_key)` that was just written. Implementors decide
//! what "react to the change" means:
//!
//! * `DatabaseEvaluator` does a full `reload()` (refetch every row).
//! * `CachedEvaluator` does `invalidate(feature)` (drop entries for
//!   that name, all scopes).
//!
//! The `scope_key` argument is reserved for future targeted-invalidation
//! impls and is currently ignored by both shipped evaluators.
//!
//! # Ordering inside a chain
//!
//! Order is load-bearing. If `Cached.invalidate` runs **before**
//! `Database.reload`, a concurrent reader can hit the empty cache,
//! fall through to the still-stale database snapshot, repopulate the
//! cache with the **old** value, and the chain stays stuck-stale until
//! the next TTL boundary.
//!
//! [`CompositeFeatureSync`] guarantees the correct order by splitting
//! implementors into two named slots:
//!
//! 1. `data_sources` — anything whose state is the source-of-truth
//!    refresh (e.g. `DatabaseEvaluator::reload`).
//! 2. `caches` — anything memoizing the data-source's answers
//!    (e.g. `CachedEvaluator::invalidate`).
//!
//! The composite executes all data-source `on_flag_changed` calls
//! first, awaits them, then executes all cache calls. A
//! `Vec<Arc<dyn FeatureSync>>` whose order is "whatever the caller
//! passed" would be a bug magnet — the slot-typed shape prevents it
//! at the call site.
//!
//! # Wiring
//!
//! Apps wire the chain by binding the composite into the App container
//! under `dyn FeatureSync`:
//!
//! ```rust,no_run
//! # use suprnova::{App, features::{CompositeFeatureSync, DatabaseEvaluator, CachedEvaluator, FeatureSync}};
//! # use std::sync::Arc;
//! # use std::time::Duration;
//! # async fn ex() -> Result<(), Box<dyn std::error::Error>> {
//! let database = Arc::new(DatabaseEvaluator::new().await?);
//! let cached = Arc::new(CachedEvaluator::new(database.clone(), Duration::from_secs(60)));
//! let composite = Arc::new(CompositeFeatureSync::new(
//!     vec![database.clone() as Arc<dyn FeatureSync>],
//!     vec![cached.clone() as Arc<dyn FeatureSync>],
//! ));
//! App::bind::<dyn FeatureSync>(composite);
//! # Ok(()) }
//! ```
//!
//! Most apps don't need to do this by hand — [`bootstrap_database_cached`](crate::features::bootstrap_database_cached)
//! sets up the whole chain in one call. Reach for `CompositeFeatureSync`
//! directly only when you've got a non-default evaluator topology (a
//! Redis-backed cache, a remote sync source, etc.).
//!
//! # The `notify` entrypoint
//!
//! [`notify`] is the function every mutation path calls. It resolves
//! `Arc<dyn FeatureSync>` from the App container (falling back to
//! [`TestContainer`](crate::testing::TestContainer) overrides for tests)
//! and awaits `on_flag_changed`. If no `FeatureSync` is bound — which
//! is the expected state for apps that don't run an in-process
//! evaluator (e.g. an admin CLI tool that only writes the DB) — the
//! call is a no-op.

use async_trait::async_trait;
use std::sync::Arc;

/// Implementors react to feature-flag mutations.
///
/// Implementations are invoked from [`notify`] every time a flag row
/// is created, updated, or deleted. The `feature` and `scope_key`
/// arguments identify the affected row exactly — implementors can use
/// them for targeted invalidation, or ignore them and do a full
/// refresh.
///
/// # Async + object-safe
///
/// Uses `#[async_trait]` so implementors can `.await` inside
/// `on_flag_changed` (typical: `DatabaseEvaluator::reload()` issues a
/// SELECT). The trait is intentionally object-safe — apps store and
/// resolve it as `Arc<dyn FeatureSync>` through the App container.
#[async_trait]
pub trait FeatureSync: Send + Sync + 'static {
    /// Called from [`notify`] once per flag mutation. `feature` is the
    /// flag name; `scope_key` is `""` for a global flag or
    /// `"user:{id}"` / `"team:{name}"` for a scoped override.
    /// Implementors that refresh wholesale can ignore both arguments;
    /// implementors that support targeted invalidation use them.
    async fn on_flag_changed(&self, feature: &str, scope_key: &str);
}

/// Composes [`FeatureSync`] implementors with strict ordering: every
/// `data_source` runs first (and is awaited) before any `cache`.
///
/// See the module docs for why the slot split exists. Briefly: caches
/// must invalidate **after** data sources refresh, or a concurrent
/// reader can repopulate the cache with the stale data-source value.
pub struct CompositeFeatureSync {
    data_sources: Vec<Arc<dyn FeatureSync>>,
    caches: Vec<Arc<dyn FeatureSync>>,
}

impl CompositeFeatureSync {
    /// Construct a composite with the two slots populated explicitly.
    ///
    /// The slot order is part of the API contract — passing an empty
    /// slot is fine ("I have a database but no in-process cache"), but
    /// crossing the slots (putting a cache in `data_sources`) breaks
    /// the staleness invariant the type is here to enforce. The
    /// argument names plus the module docs make the contract loud.
    pub fn new(data_sources: Vec<Arc<dyn FeatureSync>>, caches: Vec<Arc<dyn FeatureSync>>) -> Self {
        Self {
            data_sources,
            caches,
        }
    }
}

#[async_trait]
impl FeatureSync for CompositeFeatureSync {
    async fn on_flag_changed(&self, feature: &str, scope_key: &str) {
        // Data sources first: refresh the source-of-truth snapshot so
        // a concurrent reader that hits a cache miss in the gap below
        // sees the new value.
        for sync in &self.data_sources {
            sync.on_flag_changed(feature, scope_key).await;
        }
        // Caches second: invalidate after the data source has caught
        // up. Any subsequent miss re-fetches from the fresh snapshot.
        for sync in &self.caches {
            sync.on_flag_changed(feature, scope_key).await;
        }
    }
}

/// Resolve `Arc<dyn FeatureSync>` from the App container and dispatch
/// `on_flag_changed`. If no sync is bound, this is a no-op — the most
/// common reason for a missing binding is an out-of-process admin tool
/// that only mutates the DB and has no live evaluator to refresh.
///
/// Checks the [`TestContainer`](crate::testing::TestContainer) thread-
/// local override first, so parallel tests can install isolated fakes.
pub async fn notify(feature: &str, scope_key: &str) {
    if let Some(sync) = crate::container::App::make::<dyn FeatureSync>() {
        sync.on_flag_changed(feature, scope_key).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Records every (feature, scope_key, order) tuple it receives so
    /// tests can assert ordering between multiple instances.
    struct RecordingSync {
        label: &'static str,
        log: Arc<std::sync::Mutex<Vec<String>>>,
        delay_ms: u64,
    }

    #[async_trait]
    impl FeatureSync for RecordingSync {
        async fn on_flag_changed(&self, feature: &str, scope_key: &str) {
            if self.delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            }
            self.log
                .lock()
                .unwrap()
                .push(format!("{}:{}:{}", self.label, feature, scope_key));
        }
    }

    #[tokio::test]
    async fn composite_runs_data_sources_then_caches() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let db = Arc::new(RecordingSync {
            label: "db",
            log: log.clone(),
            delay_ms: 5,
        }) as Arc<dyn FeatureSync>;
        let cache = Arc::new(RecordingSync {
            label: "cache",
            log: log.clone(),
            delay_ms: 0,
        }) as Arc<dyn FeatureSync>;

        let composite = CompositeFeatureSync::new(vec![db], vec![cache]);
        composite.on_flag_changed("flag-a", "user:1").await;

        let recorded = log.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec![
                "db:flag-a:user:1".to_string(),
                "cache:flag-a:user:1".to_string()
            ],
            "data sources must complete before caches invalidate — \
             the db delay shouldn't let the cache run first",
        );
    }

    #[tokio::test]
    async fn composite_with_empty_slots_is_a_noop() {
        let composite = CompositeFeatureSync::new(vec![], vec![]);
        composite.on_flag_changed("anything", "").await;
        // No panic, no allocation observable from the test — the
        // composite tolerates either slot being empty.
    }

    #[tokio::test]
    async fn composite_with_only_data_sources() {
        // Apps without an in-process cache (e.g. a single-replica
        // deployment using the DB snapshot directly) bind only the
        // data-source slot.
        let calls = Arc::new(AtomicU32::new(0));
        let calls_clone = calls.clone();

        struct Counting(Arc<AtomicU32>);
        #[async_trait]
        impl FeatureSync for Counting {
            async fn on_flag_changed(&self, _feature: &str, _scope_key: &str) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let counter: Arc<dyn FeatureSync> = Arc::new(Counting(calls_clone));
        let composite = CompositeFeatureSync::new(vec![counter], vec![]);
        composite.on_flag_changed("flag", "").await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn notify_is_noop_when_no_sync_bound() {
        // Without a TestContainer::fake() guard, App::make returns
        // None and notify just returns — no panic, no error to
        // propagate. This matches the "admin CLI tool only writes
        // the DB" use case.
        notify("flag", "").await;
    }
}
