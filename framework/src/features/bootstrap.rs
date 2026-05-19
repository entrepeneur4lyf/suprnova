//! One-call wiring for the canonical feature-flag stack.
//!
//! Production apps want the same three things 95% of the time:
//!
//! 1. A [`DatabaseEvaluator`] hydrated from the `features` table.
//! 2. A [`CachedEvaluator`] wrapping it with a TTL the operator picks.
//! 3. The chain registered as featureflag's global default *and* bound
//!    into the App container under `dyn FeatureSync` so
//!    [`admin::upsert`](crate::features::admin::upsert) / `delete`
//!    refreshes propagate sub-second.
//!
//! Wiring those by hand means resolving a `DB`, building both
//! evaluators with the right `Arc<dyn _>` casts, calling
//! [`set_global_default`](featureflag::evaluator::set_global_default),
//! constructing a [`CompositeFeatureSync`] with the two slots in the
//! right order, calling [`App::bind`](crate::container::App::bind), and
//! flipping the framework's "evaluator is installed" tracking bit so
//! [`FeatureMiddleware`](crate::features::FeatureMiddleware) doesn't
//! log a missing-evaluator warning. Five steps, all easy to get wrong
//! in a way the type system can't catch. This module is one call.
//!
//! Reach past it only when your evaluator topology isn't
//! Cached(Database) — e.g. a Redis-backed cache, a remote sync source,
//! a chain of evaluators. The lower-level primitives
//! ([`DatabaseEvaluator`], [`CachedEvaluator`], [`CompositeFeatureSync`],
//! [`install_evaluator`]) all stay public for that case.
//!
//! # Example
//!
//! ```rust,ignore
//! use std::time::Duration;
//! use suprnova::features;
//!
//! // Inside main / boot after DB::init has run:
//! let features = features::bootstrap_database_cached(Duration::from_secs(60))
//!     .await
//!     .expect("feature flags wired");
//!
//! // Optional: hold onto `features.database` to schedule periodic
//! // reloads or expose admin diff views. Most apps drop the handle
//! // and let `notify`-driven refresh do the work.
//! drop(features);
//! ```

use crate::container::App;
use crate::error::FrameworkError;
use crate::features::sync::{CompositeFeatureSync, FeatureSync};
use crate::features::{CachedEvaluator, DatabaseEvaluator};
use featureflag::evaluator::{set_global_default, Evaluator};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Tracks whether an evaluator was installed via
/// [`install_evaluator`] / [`bootstrap_database_cached`]. Read by
/// [`FeatureMiddleware`](crate::features::FeatureMiddleware) to
/// decide whether to log the "no evaluator installed" warning on
/// first request. Apps that bypass these helpers and call
/// `featureflag::evaluator::set_global_default` directly will trip
/// the warning unless they also call [`mark_installed`] themselves
/// — fine, intentional, the warning is the contract.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Manually mark the evaluator as "installed" for the purposes of the
/// [`FeatureMiddleware`] startup check. Use this when bypassing
/// [`install_evaluator`] (e.g. testing with a `with_default` scope, or
/// wiring a non-`bootstrap_database_cached` topology).
pub fn mark_installed() {
    INSTALLED.store(true, Ordering::Release);
}

/// Query the installation tracker. `false` means no evaluator was
/// registered via the framework's helpers — [`FeatureMiddleware`]
/// uses this to gate its one-shot warning.
pub fn is_installed() -> bool {
    INSTALLED.load(Ordering::Acquire)
}

/// Register `evaluator` as featureflag's global default and flip the
/// framework's installation tracker.
///
/// Equivalent to [`set_global_default`] for the featureflag side, but
/// also records that the framework knows about it — keeps
/// [`FeatureMiddleware`]'s startup warning quiet.
pub fn install_evaluator(evaluator: Arc<dyn Evaluator + Send + Sync>) {
    set_global_default(evaluator);
    mark_installed();
}

/// The wired feature-flag stack returned by
/// [`bootstrap_database_cached`].
///
/// Holds typed `Arc` handles to both layers so callers can:
///
/// * trigger an explicit `database.reload()` from a periodic timer or
///   admin reload button,
/// * inspect snapshot state via `database` for an admin diff view,
/// * peek at `cached.len()` for a cache-size metric.
///
/// Most apps boot and never look at the return value again — the
/// composite is bound into the App container, the global default is
/// set, and `is_enabled!` Just Works from there.
pub struct BootstrappedFeatures {
    /// The DB-backed snapshot evaluator. Reload-able via
    /// [`DatabaseEvaluator::reload`] for periodic refresh tasks.
    pub database: Arc<DatabaseEvaluator>,
    /// The TTL cache wrapping `database`. Live reads in the framework
    /// (the global default) flow through this.
    pub cached: Arc<CachedEvaluator>,
}

/// Wire the canonical `CachedEvaluator(DatabaseEvaluator)` chain.
///
/// 1. Constructs a [`DatabaseEvaluator`] against the primary database
///    pool (the same one `DB::connection()` returns).
/// 2. Wraps it in a [`CachedEvaluator`] with the requested `ttl`.
///    `Duration::ZERO` disables caching — useful for low-flag-count
///    apps that don't want the cache layer at all but still get the
///    `FeatureSync` plumbing.
/// 3. Registers the cached evaluator as featureflag's global default
///    via [`install_evaluator`] (so `is_enabled!` works and the
///    middleware sees an installed evaluator).
/// 4. Binds a [`CompositeFeatureSync`] with `database` in
///    `data_sources` and `cached` in `caches` into the App container
///    so admin mutations propagate sub-second.
///
/// # Errors
///
/// Propagates the underlying [`DatabaseEvaluator::new`] failure —
/// either the App container hasn't initialised a `DB`, or the initial
/// `SELECT * FROM features` failed. Both should surface as a clear
/// startup error, not get swallowed.
///
/// # Idempotency
///
/// Calling twice replaces the global default and rebinds the
/// container entry. The old evaluators stay alive only as long as
/// existing `Arc` clones hold them. There's no double-bootstrap guard
/// because there's no clean meaning for "what to do on conflict" —
/// applications that need re-wire-on-config-change get the right
/// behaviour from a plain second call.
pub async fn bootstrap_database_cached(
    ttl: Duration,
) -> Result<BootstrappedFeatures, FrameworkError> {
    let database = Arc::new(DatabaseEvaluator::new().await?);
    let cached = Arc::new(CachedEvaluator::new(
        database.clone() as Arc<dyn Evaluator + Send + Sync>,
        ttl,
    ));

    install_evaluator(cached.clone() as Arc<dyn Evaluator + Send + Sync>);

    let composite = Arc::new(CompositeFeatureSync::new(
        vec![database.clone() as Arc<dyn FeatureSync>],
        vec![cached.clone() as Arc<dyn FeatureSync>],
    ));
    App::bind::<dyn FeatureSync>(composite);

    Ok(BootstrappedFeatures { database, cached })
}

#[cfg(test)]
mod tests {
    use super::*;

    // INSTALLED is process-wide static. Running these two tests in
    // parallel against the shared bit means one of them could
    // observe state mutated by the other. We serialize through a
    // single test so the assertions remain deterministic without
    // pulling in a test-ordering crate. The combined test covers
    // both behaviours we care about: install flips the bit, and
    // repeated marks don't flip it back off.
    #[test]
    fn tracker_starts_false_install_flips_repeats_stay_true() {
        // Snapshot the current state so we can restore it — other
        // bootstrap tests in the same process may have already
        // installed something.
        let prior = INSTALLED.load(Ordering::Acquire);
        INSTALLED.store(false, Ordering::Release);
        assert!(!is_installed(), "tracker starts false after reset");

        // Use a stand-in evaluator — featureflag's tests use
        // Context::root() panics-on-no-default semantics, but we're
        // not exercising context here.
        struct NoopEvaluator;
        impl Evaluator for NoopEvaluator {
            fn is_enabled(
                &self,
                _feature: &str,
                _context: &featureflag::context::Context,
            ) -> Option<bool> {
                None
            }
        }

        install_evaluator(Arc::new(NoopEvaluator) as Arc<dyn Evaluator + Send + Sync>);
        assert!(is_installed(), "install_evaluator must flip tracker");

        // Idempotent: a second call doesn't toggle off.
        mark_installed();
        mark_installed();
        assert!(is_installed(), "repeated mark_installed stays true");

        // Restore prior state so a downstream test reading the bit
        // observes what it would have observed without us.
        INSTALLED.store(prior, Ordering::Release);
    }
}
