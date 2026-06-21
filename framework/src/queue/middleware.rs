//! Job middleware — wraps `Job::handle` with reusable cross-cutting logic.
//!
//! Mirrors Laravel 13's `Illuminate\Queue\Middleware\*`. A middleware sees
//! the popped envelope **before** dispatch and decides whether to forward
//! to the next layer (eventually the typed handler), release the job back
//! to the queue without burning an attempt, drop it entirely, or
//! dead-letter immediately. Middleware are registered per `Job` via the
//! [`Job::middleware`](super::job::Job::middleware) method.
//!
//! Concrete middleware bundled here:
//! - [`WithoutOverlapping`] — `Cache::lock` around the handler, release on
//!   contention.
//! - [`RateLimited`] — `RateLimiter::hit_and_check` against a key (atomic
//!   increment-and-test), release on over-budget.
//! - [`ThrottlesExceptions`] — exponential back-off on consecutive
//!   failures (rate-limit on errors, not requests).
//! - [`Skip`] — apply `when`/`unless` conditions before reaching the
//!   handler.
//! - [`FailOnException`] — convert specific exception classes to permanent
//!   failures even within `max_tries`.

use crate::cache::Cache;
use crate::error::FrameworkError;
use crate::queue::envelope::Envelope;
use crate::queue::outcome::JobOutcome;
use crate::rate_limit::RateLimiter;
use async_trait::async_trait;
use futures::future::BoxFuture;
use std::sync::Arc;
use std::time::Duration;

/// The "next layer" in the middleware pipeline — call this to run the
/// remaining middleware and (eventually) the typed handler.
///
/// `Box<dyn FnOnce>` so closures can capture by move; the resulting
/// future is `Send` for tokio-multi-thread compatibility.
pub type Next = Box<
    dyn FnOnce(Envelope) -> BoxFuture<'static, Result<JobOutcome, FrameworkError>> + Send + Sync,
>;

/// Implement this trait to write reusable job middleware. The middleware
/// pipeline runs in the order [`Job::middleware`](super::job::Job::middleware)
/// returns them (outermost first), wrapping the handler call.
#[async_trait]
pub trait JobMiddleware: Send + Sync + 'static {
    /// Wrap the next layer of the pipeline. Return `Ok(JobOutcome::*)` to
    /// settle the attempt without forwarding, or call `next(env).await` to
    /// continue toward the typed handler.
    async fn handle(&self, env: Envelope, next: Next) -> Result<JobOutcome, FrameworkError>;
}

// ---------------------------------------------------------------------------
// WithoutOverlapping
// ---------------------------------------------------------------------------

/// Hold a `Cache::lock` for the duration of the job; release-with-delay
/// on lock contention. Mirrors `Illuminate\Queue\Middleware\WithoutOverlapping`.
///
/// `key` is the lock key tail; the actual cache key is composed as
/// `"laravel-queue-overlap:{job_name}:{key}"` (matching Laravel's prefix),
/// or `"laravel-queue-overlap:{key}"` when [`Self::shared`] is enabled.
pub struct WithoutOverlapping {
    /// User-supplied lock key tail (job-name-scoped unless [`Self::shared`]).
    pub key: String,
    /// Delay before re-enqueue on lock contention; `None` drops the job.
    pub release_after: Option<Duration>,
    /// Maximum time the cache lock may be held while the handler runs.
    pub expires_after: Duration,
    /// Cache-key prefix; defaults to `"laravel-queue-overlap:"`.
    pub prefix: String,
    /// When `true`, the bare `key` is used (no job-name segment) so
    /// different `Job` types can share the same overlap lock.
    pub share_key: bool,
}

impl WithoutOverlapping {
    /// New middleware bound to `key`. Defaults: release-after = 0s
    /// (re-enqueue immediately on contention), expires-after = 60s.
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            release_after: Some(Duration::ZERO),
            expires_after: Duration::from_secs(60),
            prefix: "laravel-queue-overlap:".into(),
            share_key: false,
        }
    }

    /// Delay before re-enqueue on contention. Mirrors Laravel's
    /// `releaseAfter($delay)`.
    pub fn release_after(mut self, delay: Duration) -> Self {
        self.release_after = Some(delay);
        self
    }

    /// Drop the job entirely on contention rather than re-enqueueing.
    /// Mirrors Laravel's `dontRelease()`.
    pub fn dont_release(mut self) -> Self {
        self.release_after = None;
        self
    }

    /// Max time the lock can be held. Mirrors Laravel's `expireAfter($ttl)`.
    pub fn expire_after(mut self, ttl: Duration) -> Self {
        self.expires_after = ttl;
        self
    }

    /// Override the cache-key prefix. Mirrors Laravel's `withPrefix($prefix)`.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Use the bare `key` (no job-name segment) so different `Job` types
    /// can share the same overlap lock. Mirrors Laravel's `shared()`.
    pub fn shared(mut self) -> Self {
        self.share_key = true;
        self
    }

    fn lock_key(&self, job_name: &str) -> String {
        if self.share_key {
            format!("{}{}", self.prefix, self.key)
        } else {
            format!("{}{}:{}", self.prefix, job_name, self.key)
        }
    }
}

#[async_trait]
impl JobMiddleware for WithoutOverlapping {
    async fn handle(&self, env: Envelope, next: Next) -> Result<JobOutcome, FrameworkError> {
        let key = self.lock_key(&env.job_name);
        match Cache::lock(&key, self.expires_after).await? {
            Some(guard) => {
                let result = next(env).await;
                guard.release().await?;
                result
            }
            None => match self.release_after {
                Some(delay) => Ok(JobOutcome::Released { delay }),
                None => Ok(JobOutcome::Deleted),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// RateLimited
// ---------------------------------------------------------------------------

/// Release the job back to the queue if the configured rate-limit key is
/// already over-budget. Mirrors `Illuminate\Queue\Middleware\RateLimited`.
///
/// Uses [`RateLimiter`] (the Laravel-style facade backed by Suprnova's
/// cache). The middleware keys the limit per-job-name by default; use
/// [`Self::by`] to override.
pub struct RateLimited {
    /// Max attempts permitted per `decay` window before releasing.
    pub max_attempts: i64,
    /// Sliding-window length used by the underlying [`RateLimiter`].
    pub decay: Duration,
    /// Override key for the limiter; defaults to `"job-rate:{job_name}"`.
    pub key: Option<String>,
    /// Override release delay; default uses `RateLimiter::available_in`.
    pub release_after: Option<Duration>,
}

impl RateLimited {
    /// New middleware allowing `max_attempts` runs per `decay` window.
    pub fn new(max_attempts: i64, decay: Duration) -> Self {
        Self {
            max_attempts,
            decay,
            key: None,
            release_after: None,
        }
    }

    /// Override the rate-limit key. Default keys per-job-name.
    pub fn by(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Override the release delay. Default uses `RateLimiter::available_in`.
    pub fn release_after(mut self, delay: Duration) -> Self {
        self.release_after = Some(delay);
        self
    }
}

#[async_trait]
impl JobMiddleware for RateLimited {
    async fn handle(&self, env: Envelope, next: Next) -> Result<JobOutcome, FrameworkError> {
        let key = self
            .key
            .clone()
            .unwrap_or_else(|| format!("job-rate:{}", env.job_name));
        // Atomic increment-and-test: a separate `too_many_attempts` check
        // followed by a `hit` lets concurrent workers all pass the check
        // before any of them increments, admitting more than `max_attempts`
        // jobs per window. `hit_and_check` burns the attempt and tests the
        // budget in one round-trip, so exactly `max_attempts` are admitted.
        if RateLimiter::hit_and_check(&key, self.max_attempts, self.decay.as_secs()).await? {
            let delay = match self.release_after {
                Some(d) => d,
                None => {
                    let secs = RateLimiter::available_in(&key).await?;
                    Duration::from_secs(secs.max(1) as u64)
                }
            };
            return Ok(JobOutcome::Released { delay });
        }
        next(env).await
    }
}

// ---------------------------------------------------------------------------
// ThrottlesExceptions
// ---------------------------------------------------------------------------

/// Rate-limit on consecutive failures: after `max_attempts` failures
/// within `decay`, release the job for the cool-off period instead of
/// burning normal retries. Mirrors
/// `Illuminate\Queue\Middleware\ThrottlesExceptions`.
pub struct ThrottlesExceptions {
    /// Max consecutive failures permitted within `decay` before throttling.
    pub max_attempts: i64,
    /// Sliding-window length used to count failures.
    pub decay: Duration,
    /// Delay between retries while still under the failure budget.
    pub backoff: Duration,
    /// Override key for the throttle counter; defaults to `"job-throttle:{job_name}"`.
    pub key: Option<String>,
}

impl ThrottlesExceptions {
    /// New middleware that tolerates `max_attempts` failures within `decay`
    /// before releasing the job for the cool-off period.
    pub fn new(max_attempts: i64, decay: Duration) -> Self {
        Self {
            max_attempts,
            decay,
            backoff: Duration::from_secs(0),
            key: None,
        }
    }

    /// Delay between retries while under the failure budget. Mirrors
    /// Laravel's `backoff($minutes)`.
    pub fn backoff(mut self, b: Duration) -> Self {
        self.backoff = b;
        self
    }

    /// Override the throttle key. Defaults to `"job-throttle:{job_name}"`.
    pub fn by(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }
}

#[async_trait]
impl JobMiddleware for ThrottlesExceptions {
    async fn handle(&self, env: Envelope, next: Next) -> Result<JobOutcome, FrameworkError> {
        let key = self
            .key
            .clone()
            .unwrap_or_else(|| format!("job-throttle:{}", env.job_name));
        if RateLimiter::too_many_attempts(&key, self.max_attempts).await? {
            let secs = RateLimiter::available_in(&key).await?;
            return Ok(JobOutcome::Released {
                delay: Duration::from_secs(secs.max(1) as u64),
            });
        }
        match next(env).await {
            Ok(JobOutcome::Completed) => {
                RateLimiter::clear(&key).await?;
                Ok(JobOutcome::Completed)
            }
            Ok(other) => Ok(other),
            Err(err) => {
                RateLimiter::hit(&key, self.decay.as_secs()).await?;
                if self.backoff.is_zero() {
                    Err(err)
                } else {
                    // Release with backoff: middleware-driven retry, not a
                    // failed-attempt bump.
                    Ok(JobOutcome::Released {
                        delay: self.backoff,
                    })
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Skip
// ---------------------------------------------------------------------------

/// Skip the handler call when the configured condition is true. Mirrors
/// `Illuminate\Queue\Middleware\Skip`.
pub struct Skip {
    skip: bool,
}

impl Skip {
    /// Skip if `condition` is true.
    pub fn when(condition: bool) -> Self {
        Self { skip: condition }
    }

    /// Skip unless `condition` is true.
    pub fn unless(condition: bool) -> Self {
        Self { skip: !condition }
    }
}

#[async_trait]
impl JobMiddleware for Skip {
    async fn handle(&self, env: Envelope, next: Next) -> Result<JobOutcome, FrameworkError> {
        if self.skip {
            Ok(JobOutcome::Deleted)
        } else {
            next(env).await
        }
    }
}

// ---------------------------------------------------------------------------
// FailOnException
// ---------------------------------------------------------------------------

/// Promote specific error patterns to permanent failures (skip retries,
/// dead-letter immediately). Mirrors
/// `Illuminate\Queue\Middleware\FailOnException`.
///
/// Rust doesn't have first-class exception class matching, so the predicate
/// receives the formatted error and returns `true` for "this is permanent".
pub struct FailOnException {
    matcher: Arc<dyn Fn(&FrameworkError) -> bool + Send + Sync>,
}

impl FailOnException {
    /// New middleware that dead-letters whenever `matcher` returns `true`
    /// for the raised error.
    pub fn new<F>(matcher: F) -> Self
    where
        F: Fn(&FrameworkError) -> bool + Send + Sync + 'static,
    {
        Self {
            matcher: Arc::new(matcher),
        }
    }

    /// Convenience: fail on every error whose display contains one of the
    /// given substrings. Mirrors Laravel's class-array form (since we
    /// can't dispatch on PHP class names, we match on the formatted
    /// message — call sites can use `FailOnException::new(|e| matches!
    /// (...))` for typed matching).
    pub fn on_substring<S: Into<String>>(substrings: Vec<S>) -> Self {
        let needles: Vec<String> = substrings.into_iter().map(Into::into).collect();
        Self::new(move |err| {
            let display = err.to_string();
            needles.iter().any(|n| display.contains(n))
        })
    }
}

#[async_trait]
impl JobMiddleware for FailOnException {
    async fn handle(&self, env: Envelope, next: Next) -> Result<JobOutcome, FrameworkError> {
        match next(env).await {
            Ok(outcome) => Ok(outcome),
            Err(err) => {
                if (self.matcher)(&err) {
                    Ok(JobOutcome::Failed {
                        reason: err.to_string(),
                    })
                } else {
                    Err(err)
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SkipIfBatchCancelled
// ---------------------------------------------------------------------------

/// Skip the handler if the envelope's batch was cancelled. Mirrors
/// `Illuminate\Queue\Middleware\SkipIfBatchCancelled`.
pub struct SkipIfBatchCancelled;

#[async_trait]
impl JobMiddleware for SkipIfBatchCancelled {
    async fn handle(&self, env: Envelope, next: Next) -> Result<JobOutcome, FrameworkError> {
        if let Some(batch_id) = env.batch_id.as_deref()
            && let Some(repo) = crate::queue::batch::current_repository()
            && repo.is_cancelled(batch_id).await?
        {
            return Ok(JobOutcome::Deleted);
        }
        next(env).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::App;
    use crate::cache::{CacheStore, InMemoryCache};
    use crate::queue::{BackoffSchedule, CURRENT_SCHEMA_VERSION};
    use chrono::Utc;
    use serial_test::serial;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use uuid::Uuid;

    fn cache_bootstrap() {
        if !crate::cache::Cache::is_initialized() {
            App::bind::<dyn CacheStore>(Arc::new(InMemoryCache::new()));
        }
    }

    fn fresh_env(name: &str) -> Envelope {
        Envelope {
            schema_version: CURRENT_SCHEMA_VERSION,
            id: Uuid::new_v4(),
            job_name: name.into(),
            payload: serde_json::json!({}),
            dispatched_at: Utc::now(),
            available_at: Utc::now(),
            attempts: 0,
            max_tries: 3,
            backoff: BackoffSchedule::default(),
            timeout_secs: None,
            fail_on_timeout: false,
            idempotency_key: None,
            batch_id: None,
            chain_remaining: Vec::new(),
        }
    }

    fn ok_next() -> Next {
        Box::new(|_env| Box::pin(async { Ok(JobOutcome::Completed) }))
    }

    fn err_next() -> Next {
        Box::new(|_env| Box::pin(async { Err(FrameworkError::internal("boom")) }))
    }

    #[tokio::test]
    #[serial]
    async fn without_overlapping_passes_when_lock_available() {
        cache_bootstrap();
        let mw = WithoutOverlapping::new("test_overlap_pass");
        let r = mw.handle(fresh_env("J"), ok_next()).await.unwrap();
        assert!(matches!(r, JobOutcome::Completed));
    }

    #[tokio::test]
    #[serial]
    async fn without_overlapping_releases_on_contention() {
        cache_bootstrap();
        let key = "test_overlap_contend";
        let mw = WithoutOverlapping::new(key);
        // Hold a competing lock manually.
        let held = Cache::lock(
            &format!("laravel-queue-overlap:J:{key}"),
            Duration::from_secs(30),
        )
        .await
        .unwrap()
        .expect("first lock");
        let r = mw.handle(fresh_env("J"), ok_next()).await.unwrap();
        assert!(matches!(r, JobOutcome::Released { .. }));
        held.release().await.unwrap();
    }

    #[tokio::test]
    #[serial]
    async fn rate_limited_passes_under_budget() {
        cache_bootstrap();
        let mw = RateLimited::new(2, Duration::from_secs(60)).by("test_rl_pass");
        RateLimiter::clear("test_rl_pass").await.unwrap();
        let r = mw.handle(fresh_env("J"), ok_next()).await.unwrap();
        assert!(matches!(r, JobOutcome::Completed));
    }

    #[tokio::test]
    #[serial]
    async fn rate_limited_releases_over_budget() {
        cache_bootstrap();
        let key = "test_rl_over";
        RateLimiter::clear(key).await.unwrap();
        // Burn through the budget.
        RateLimiter::hit(key, 60).await.unwrap();
        RateLimiter::hit(key, 60).await.unwrap();
        let mw = RateLimited::new(2, Duration::from_secs(60))
            .by(key)
            .release_after(Duration::from_secs(5));
        let r = mw.handle(fresh_env("J"), ok_next()).await.unwrap();
        assert!(matches!(r, JobOutcome::Released { .. }));
    }

    #[tokio::test]
    #[serial]
    async fn rate_limited_admits_exactly_max_under_concurrency() {
        cache_bootstrap();
        let key = "test_rl_concurrent";
        RateLimiter::clear(key).await.unwrap();
        let max = 3_i64;
        let mw = RateLimited::new(max, Duration::from_secs(60)).by(key);

        // Fire many more dispatches than the budget, all concurrently against
        // the same limiter key. With the old check-then-hit pair these would
        // interleave their checks before any hit and over-admit; the atomic
        // `hit_and_check` admits exactly `max` and releases the rest.
        let dispatches = 12;
        let futs = (0..dispatches).map(|_| mw.handle(fresh_env("J"), ok_next()));
        let results = futures::future::join_all(futs).await;

        let completed = results
            .iter()
            .filter(|r| matches!(r.as_ref().unwrap(), JobOutcome::Completed))
            .count();
        let released = results
            .iter()
            .filter(|r| matches!(r.as_ref().unwrap(), JobOutcome::Released { .. }))
            .count();

        assert_eq!(
            completed, max as usize,
            "exactly the budget may run; admitted {completed}"
        );
        assert_eq!(
            released,
            dispatches - max as usize,
            "every dispatch over budget is released; released {released}"
        );
    }

    #[tokio::test]
    async fn skip_when_true_drops() {
        let r = Skip::when(true)
            .handle(fresh_env("J"), ok_next())
            .await
            .unwrap();
        assert!(matches!(r, JobOutcome::Deleted));
    }

    #[tokio::test]
    async fn skip_unless_true_forwards() {
        let r = Skip::unless(true)
            .handle(fresh_env("J"), ok_next())
            .await
            .unwrap();
        assert!(matches!(r, JobOutcome::Completed));
    }

    #[tokio::test]
    async fn fail_on_exception_promotes_match() {
        let calls = Arc::new(AtomicU32::new(0));
        let c2 = calls.clone();
        let mw = FailOnException::new(move |_| {
            c2.fetch_add(1, Ordering::SeqCst);
            true
        });
        let r = mw.handle(fresh_env("J"), err_next()).await.unwrap();
        assert!(matches!(r, JobOutcome::Failed { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fail_on_exception_propagates_non_match() {
        let mw = FailOnException::new(|_| false);
        let r = mw.handle(fresh_env("J"), err_next()).await;
        assert!(r.is_err());
    }
}
