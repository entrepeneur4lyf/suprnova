//! [`CachedEvaluator`] — TTL-bounded memoization in front of any
//! [`Evaluator`].
//!
//! Wraps an inner evaluator (typically [`DatabaseEvaluator`](super::database::DatabaseEvaluator))
//! with a process-local [`DashMap`] cache keyed by
//! `(feature, user_id, team)`. The cache's lookup path is fully
//! synchronous — matching featureflag's [`Evaluator::is_enabled`]
//! contract — so the hot path stays lock-free for concurrent
//! readers and never blocks on an async runtime.
//!
//! # When to use this
//!
//! [`DatabaseEvaluator`] already snapshots flags into an in-memory
//! `HashMap` on construction and reload, so per-request DB queries
//! aren't a concern. `CachedEvaluator` exists to memoize the result
//! of the **scope-resolution walk** (build candidate keys, look each
//! up, fall back to global) when that walk's cost ever becomes
//! material — e.g. an evaluator chain whose links are not all
//! `DatabaseEvaluator`, or a custom evaluator whose `is_enabled`
//! computation is non-trivial.
//!
//! # Cross-replica coherence
//!
//! The cache is per-process. Flag changes on one replica are visible
//! to other replicas as soon as their inner evaluator reloads — there
//! is no cross-cluster cache-coherence protocol in v1. The cache TTL
//! therefore bounds the worst-case staleness across the cluster.
//! Callers who need millisecond propagation should either:
//!
//! * lower the TTL toward zero (and accept the cost of skipping the
//!   memoization), or
//! * call [`CachedEvaluator::invalidate`] from the admin-CRUD path
//!   that mutated the flag (Phase 13 Task 6 — admin handlers will
//!   wire this).
//!
//! # Why DashMap + manual TTL (not our Cache facade)
//!
//! The `Cache` facade is async by design — it has to be, to support
//! Redis as a backend. featureflag's `Evaluator::is_enabled` is sync.
//! Bridging the two via `block_on` inside `is_enabled` would tank
//! request throughput. The right reconciliation is two layers: a
//! sync per-process cache (this struct) for hot reads, and a
//! background invalidator that subscribes to a cross-process channel
//! and clears local entries — the invalidator is out of scope for
//! v1 since flag changes are operator-initiated, infrequent, and
//! already bounded by the TTL.

use crate::features::fields::{TeamField, UserIdField};
use dashmap::DashMap;
use featureflag::{context::Context, evaluator::Evaluator};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// TTL-cached wrapper around any [`Evaluator`].
pub struct CachedEvaluator {
    inner: Arc<dyn Evaluator>,
    ttl: Duration,
    /// Key format: `"{feature}::u={user_id?}::t={team?}"`. Empty
    /// segments encode "field absent in this context."
    cache: DashMap<String, CacheEntry>,
}

#[derive(Copy, Clone)]
struct CacheEntry {
    value: Option<bool>,
    inserted_at: Instant,
}

impl CachedEvaluator {
    /// Construct a new cached evaluator with the given TTL. A TTL of
    /// zero degenerates to "no caching" — every call falls through
    /// to `inner`. A very long TTL bounds the cross-replica staleness
    /// window; tune to taste.
    pub fn new(inner: Arc<dyn Evaluator>, ttl: Duration) -> Self {
        Self {
            inner,
            ttl,
            cache: DashMap::new(),
        }
    }

    /// Reference to the underlying evaluator. Exposed for tests and
    /// for callers that need to dispatch a cache-bypassed lookup
    /// (e.g. admin tooling rendering "current vs cached" diffs).
    pub fn inner(&self) -> &Arc<dyn Evaluator> {
        &self.inner
    }

    /// Drop every cached entry for a specific feature name. Intended
    /// for the admin-CRUD path: after [`DatabaseEvaluator::set_flag`]
    /// mutates a flag, callers invalidate the corresponding cached
    /// entries so the next `is_enabled` re-reads the snapshot.
    pub fn invalidate(&self, feature: &str) {
        let prefix = format!("{feature}::");
        self.cache.retain(|key, _| !key.starts_with(&prefix));
    }

    /// Drop every cached entry. Use sparingly — typically only on a
    /// bulk admin reload or in tests.
    pub fn invalidate_all(&self) {
        self.cache.clear();
    }

    /// Number of entries currently held. Useful for tests + admin
    /// telemetry; not load-bearing.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Test convenience.
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    fn cache_key(feature: &str, context: &Context) -> String {
        let user = context
            .iter()
            .find_map(|c| c.extensions().get::<UserIdField>())
            .map(|UserIdField(id)| id.to_string())
            .unwrap_or_default();
        let team = context
            .iter()
            .find_map(|c| c.extensions().get::<TeamField>())
            .map(|TeamField(team)| team.clone())
            .unwrap_or_default();
        format!("{feature}::u={user}::t={team}")
    }
}

impl Evaluator for CachedEvaluator {
    fn is_enabled(&self, feature: &str, context: &Context) -> Option<bool> {
        // TTL=0 short-circuits the cache entirely. Avoids the
        // insert+evict churn that would otherwise dominate when the
        // caller doesn't want caching.
        if self.ttl.is_zero() {
            return self.inner.is_enabled(feature, context);
        }

        let key = Self::cache_key(feature, context);

        // Fast path: live entry.
        if let Some(entry) = self.cache.get(&key)
            && entry.inserted_at.elapsed() < self.ttl
        {
            return entry.value;
        }

        // Miss or expired — consult inner and store the result. We
        // store None values too: "feature not configured" is itself
        // a stable answer worth caching to avoid re-walking the
        // scope chain on every request.
        let value = self.inner.is_enabled(feature, context);
        self.cache.insert(
            key,
            CacheEntry {
                value,
                inserted_at: Instant::now(),
            },
        );
        value
    }

    fn on_new_context(
        &self,
        context: featureflag::context::ContextRef<'_>,
        fields: featureflag::fields::Fields<'_>,
    ) {
        // Pass through to the inner evaluator so the same field-to-
        // extension translation runs once per context creation. This
        // keeps the cached wrapper transparent: from the caller's
        // perspective, switching DatabaseEvaluator for
        // CachedEvaluator(DatabaseEvaluator) changes only the cache
        // behaviour, not the field-resolution behaviour.
        self.inner.on_new_context(context, fields);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use featureflag::context::Context;
    use featureflag::evaluator::with_default;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Inner evaluator that counts how many times `is_enabled` was
    /// actually invoked. Lets tests assert cache hit/miss behaviour
    /// without relying on timing.
    struct CountingEvaluator {
        return_value: Option<bool>,
        calls: AtomicU32,
    }

    impl CountingEvaluator {
        fn new(return_value: Option<bool>) -> Self {
            Self {
                return_value,
                calls: AtomicU32::new(0),
            }
        }
        fn call_count(&self) -> u32 {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Evaluator for CountingEvaluator {
        fn is_enabled(&self, _feature: &str, _context: &Context) -> Option<bool> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.return_value
        }
    }

    #[test]
    fn cache_hits_on_second_call_with_same_context() {
        let inner = Arc::new(CountingEvaluator::new(Some(true)));
        let cached = CachedEvaluator::new(inner.clone(), Duration::from_secs(60));

        with_default(Arc::new(NoopEvaluator), || {
            let ctx = Context::root();
            assert_eq!(cached.is_enabled("flag", &ctx), Some(true));
            assert_eq!(cached.is_enabled("flag", &ctx), Some(true));
        });

        assert_eq!(
            inner.call_count(),
            1,
            "second call must come from the cache; inner saw {} calls",
            inner.call_count()
        );
    }

    #[test]
    fn ttl_expiry_falls_through_to_inner() {
        let inner = Arc::new(CountingEvaluator::new(Some(false)));
        let cached = CachedEvaluator::new(inner.clone(), Duration::from_millis(20));

        with_default(Arc::new(NoopEvaluator), || {
            let ctx = Context::root();
            assert_eq!(cached.is_enabled("flag", &ctx), Some(false));
            std::thread::sleep(Duration::from_millis(40));
            assert_eq!(cached.is_enabled("flag", &ctx), Some(false));
        });

        assert_eq!(
            inner.call_count(),
            2,
            "second call after TTL expiry must re-hit inner"
        );
    }

    #[test]
    fn ttl_zero_disables_cache() {
        let inner = Arc::new(CountingEvaluator::new(Some(true)));
        let cached = CachedEvaluator::new(inner.clone(), Duration::ZERO);

        with_default(Arc::new(NoopEvaluator), || {
            let ctx = Context::root();
            for _ in 0..5 {
                assert_eq!(cached.is_enabled("flag", &ctx), Some(true));
            }
        });

        assert_eq!(
            inner.call_count(),
            5,
            "TTL=0 must short-circuit caching and call inner every time"
        );
        assert!(
            cached.is_empty(),
            "TTL=0 must not populate the cache map either"
        );
    }

    #[test]
    fn none_is_cached_too() {
        let inner = Arc::new(CountingEvaluator::new(None));
        let cached = CachedEvaluator::new(inner.clone(), Duration::from_secs(60));

        with_default(Arc::new(NoopEvaluator), || {
            let ctx = Context::root();
            for _ in 0..3 {
                assert_eq!(cached.is_enabled("flag", &ctx), None);
            }
        });

        assert_eq!(
            inner.call_count(),
            1,
            "the None response must be cached the same as Some(_)"
        );
    }

    #[test]
    fn invalidate_clears_only_the_named_feature() {
        let inner = Arc::new(CountingEvaluator::new(Some(true)));
        let cached = CachedEvaluator::new(inner.clone(), Duration::from_secs(60));

        with_default(Arc::new(NoopEvaluator), || {
            let ctx = Context::root();
            assert_eq!(cached.is_enabled("flag-a", &ctx), Some(true));
            assert_eq!(cached.is_enabled("flag-b", &ctx), Some(true));
            assert_eq!(cached.len(), 2);

            cached.invalidate("flag-a");
            assert_eq!(cached.len(), 1, "only flag-a's entries should be gone");

            // flag-a re-fetches; flag-b stays cached.
            assert_eq!(cached.is_enabled("flag-a", &ctx), Some(true));
            assert_eq!(cached.is_enabled("flag-b", &ctx), Some(true));
        });

        assert_eq!(
            inner.call_count(),
            3,
            "expected calls: flag-a, flag-b (initial), flag-a (after invalidate)"
        );
    }

    #[test]
    fn invalidate_all_clears_everything() {
        let inner = Arc::new(CountingEvaluator::new(Some(true)));
        let cached = CachedEvaluator::new(inner.clone(), Duration::from_secs(60));

        with_default(Arc::new(NoopEvaluator), || {
            let ctx = Context::root();
            cached.is_enabled("flag-a", &ctx);
            cached.is_enabled("flag-b", &ctx);
            cached.invalidate_all();
            assert!(cached.is_empty());
        });
    }

    /// Stand-in evaluator for the `with_default(...)` scope-default
    /// — featureflag panics if a `Context::root()`-derived context
    /// is used while no global default is installed, so the tests
    /// thread a no-op default through their scope.
    struct NoopEvaluator;

    impl Evaluator for NoopEvaluator {
        fn is_enabled(&self, _feature: &str, _context: &Context) -> Option<bool> {
            None
        }
    }
}
