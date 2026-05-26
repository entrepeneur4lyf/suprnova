//! Sliding-window rate limiter.
//!
//! Per-key window: each key tracks a deque of hit timestamps. On every
//! `try_acquire`, evict entries older than `now - window`, then if the
//! remaining count is below `max_requests`, append `now` and accept;
//! otherwise reject.
//!
//! The in-memory driver uses `tokio::time::Instant` so `start_paused`
//! tests can use `tokio::time::advance` to drive the clock. The Redis
//! driver uses `chrono::Utc::now().timestamp_millis()` with a Lua
//! script for atomic check-and-record.

pub mod algorithm;
pub mod memory;
pub mod redis;

use crate::error::FrameworkError;
use async_trait::async_trait;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct SlidingWindowConfig {
    pub max_requests: u32,
    pub window: Duration,
}

#[async_trait]
pub trait RateLimiter: Send + Sync {
    /// Try to acquire one slot for `key` under `config`. Returns `Ok(true)`
    /// if accepted (slot consumed); `Ok(false)` if rejected.
    async fn try_acquire(
        &self,
        key: &str,
        config: &SlidingWindowConfig,
    ) -> Result<bool, FrameworkError>;

    /// Compute how long to wait before another `try_acquire` is likely to succeed.
    /// Returns `None` if the bucket has free slots right now.
    async fn retry_after(
        &self,
        key: &str,
        config: &SlidingWindowConfig,
    ) -> Result<Option<Duration>, FrameworkError>;
}

// ============================================================================
// Middleware integration
// ============================================================================

use crate::container::App;

/// Wire the in-memory rate limiter as the default. Idempotent.
pub async fn bootstrap_default() {
    if App::has_binding::<dyn RateLimiter>() {
        return;
    }
    App::bind::<dyn RateLimiter>(std::sync::Arc::new(memory::InMemoryRateLimiter::new()));
}

/// Read `RATE_LIMIT_DRIVER` env and configure the matching driver. Falls back
/// to the in-memory default on any unrecognized value or when the var is unset.
pub async fn bootstrap_from_env() -> Result<(), FrameworkError> {
    let driver = std::env::var("RATE_LIMIT_DRIVER").unwrap_or_else(|_| "memory".into());
    match driver.as_str() {
        "memory" => bootstrap_default().await,
        "redis" => {
            let url = std::env::var("RATE_LIMIT_REDIS_URL")
                .unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
            let prefix = std::env::var("RATE_LIMIT_PREFIX").unwrap_or_else(|_| "suprnova:".into());
            let d = redis::RedisRateLimiter::connect(&url, &prefix).await?;
            App::bind::<dyn RateLimiter>(std::sync::Arc::new(d));
        }
        other => {
            tracing::warn!(driver = %other, "unknown RATE_LIMIT_DRIVER, falling back to memory");
            bootstrap_default().await;
        }
    }
    Ok(())
}

use crate::Request;
use crate::http::{HttpResponse, Response};
use std::sync::Arc;

/// How [`RateLimitMiddleware`] reacts when the rate-limiter *backend* itself
/// errors — e.g. Redis is unreachable — as opposed to a request legitimately
/// exceeding its quota.
///
/// This is distinct from the over-quota path (always HTTP 429). A backend
/// error means the limiter could not make a decision at all, so the
/// middleware must choose between availability and the limit's guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendErrorPolicy {
    /// Pass the request through when the backend errors. Prioritizes
    /// availability: a limiter outage does not take down the API. This is
    /// the default, matching most public-API expectations. The error is
    /// logged at `warn` so the outage is still visible.
    #[default]
    FailOpen,
    /// Reject the request with HTTP 503 (`Retry-After: 1`) when the backend
    /// errors. Prioritizes the limit's guarantee: for sensitive routes
    /// (login, password reset, payments) letting unbounded traffic through
    /// during a limiter outage is worse than briefly returning 503. The
    /// error is logged at `error`.
    FailClosed,
}

/// HTTP middleware that enforces a sliding-window rate limit.
///
/// The bucket key is determined by a caller-supplied closure, making it
/// trivial to rate-limit per-route, per-IP, per-user, or any composite.
///
/// On rejection (the caller is over quota) the middleware short-circuits with
/// HTTP 429 and a `Retry-After` header (seconds until the oldest slot
/// expires).
///
/// When the *backend* errors (e.g. Redis is unreachable) the response is
/// governed by [`BackendErrorPolicy`], chosen via
/// [`RateLimitMiddleware::on_backend_error`]. The default is
/// [`BackendErrorPolicy::FailOpen`] (pass through, log a warning); sensitive
/// routes can opt into [`BackendErrorPolicy::FailClosed`] (HTTP 503).
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use std::time::Duration;
/// use suprnova::rate_limit::{BackendErrorPolicy, RateLimitMiddleware, SlidingWindowConfig};
/// use suprnova::rate_limit::memory::InMemoryRateLimiter;
///
/// let limiter = Arc::new(InMemoryRateLimiter::new());
/// let cfg = SlidingWindowConfig { max_requests: 100, window: Duration::from_secs(60) };
/// let mw = RateLimitMiddleware::new(limiter, cfg, |req| {
///     format!("route:{}", req.path())
/// })
/// // Opt sensitive routes into fail-closed (HTTP 503 if the backend is down):
/// .on_backend_error(BackendErrorPolicy::FailClosed);
/// ```
pub struct RateLimitMiddleware<F>
where
    F: Fn(&Request) -> String + Send + Sync + 'static,
{
    limiter: Arc<dyn RateLimiter>,
    config: SlidingWindowConfig,
    key_fn: F,
    on_backend_error: BackendErrorPolicy,
}

impl<F> RateLimitMiddleware<F>
where
    F: Fn(&Request) -> String + Send + Sync + 'static,
{
    /// Create a new `RateLimitMiddleware`.
    ///
    /// * `limiter` — the rate-limiter backend (in-memory or Redis)
    /// * `config`  — window duration and per-key request cap
    /// * `key_fn`  — closure that maps each incoming request to a bucket key string
    pub fn new(limiter: Arc<dyn RateLimiter>, config: SlidingWindowConfig, key_fn: F) -> Self {
        Self {
            limiter,
            config,
            key_fn,
            on_backend_error: BackendErrorPolicy::default(),
        }
    }

    /// Choose how the middleware reacts to a rate-limiter *backend* error
    /// (e.g. Redis is unreachable), as distinct from a request being over its
    /// quota. Defaults to [`BackendErrorPolicy::FailOpen`].
    ///
    /// Use [`BackendErrorPolicy::FailClosed`] on sensitive routes where letting
    /// unbounded traffic through during a limiter outage is unacceptable.
    pub fn on_backend_error(mut self, policy: BackendErrorPolicy) -> Self {
        self.on_backend_error = policy;
        self
    }
}

#[async_trait]
impl<F> crate::Middleware for RateLimitMiddleware<F>
where
    F: Fn(&Request) -> String + Send + Sync + 'static,
{
    async fn handle(&self, request: Request, next: crate::Next) -> Response {
        let key = (self.key_fn)(&request);
        match self.limiter.try_acquire(&key, &self.config).await {
            Ok(true) => next(request).await,
            Ok(false) => {
                // Compute how long the caller must wait before trying again.
                let secs = self
                    .limiter
                    .retry_after(&key, &self.config)
                    .await
                    .ok()
                    .flatten()
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                Err(HttpResponse::text("429 Too Many Requests")
                    .status(429)
                    .header("retry-after", secs.to_string()))
            }
            // The limiter backend itself errored (e.g. Redis unreachable) —
            // it could not make a decision. Behavior is governed by the
            // configured `BackendErrorPolicy`. Either way the error is now
            // logged (it was previously swallowed silently): `warn` when
            // failing open since it self-limits to backend outages, `error`
            // when failing closed since that path actively rejects live
            // traffic.
            Err(e) => match self.on_backend_error {
                BackendErrorPolicy::FailOpen => {
                    tracing::warn!(
                        error = %e,
                        key = %key,
                        "rate limiter backend error; failing open (request passed through)"
                    );
                    next(request).await
                }
                BackendErrorPolicy::FailClosed => {
                    tracing::error!(
                        error = %e,
                        key = %key,
                        "rate limiter backend error; failing closed with 503"
                    );
                    Err(HttpResponse::text("503 Service Unavailable")
                        .status(503)
                        .header("retry-after", "1"))
                }
            },
        }
    }
}
