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
            let prefix = std::env::var("RATE_LIMIT_PREFIX")
                .unwrap_or_else(|_| "suprnova:".into());
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

use crate::http::{HttpResponse, Response};
use crate::Request;
use std::sync::Arc;

/// HTTP middleware that enforces a sliding-window rate limit.
///
/// The bucket key is determined by a caller-supplied closure, making it
/// trivial to rate-limit per-route, per-IP, per-user, or any composite.
///
/// On rejection the middleware short-circuits with HTTP 429 and a
/// `Retry-After` header (seconds until the oldest slot expires). On a
/// backend error it fails-open — the request is passed through — to
/// avoid taking down the API when Redis has a hiccup.
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use std::time::Duration;
/// use suprnova::rate_limit::{RateLimitMiddleware, SlidingWindowConfig};
/// use suprnova::rate_limit::memory::InMemoryRateLimiter;
///
/// let limiter = Arc::new(InMemoryRateLimiter::new());
/// let cfg = SlidingWindowConfig { max_requests: 100, window: Duration::from_secs(60) };
/// let mw = RateLimitMiddleware::new(limiter, cfg, |req| {
///     format!("route:{}", req.path())
/// });
/// ```
pub struct RateLimitMiddleware<F>
where
    F: Fn(&Request) -> String + Send + Sync + 'static,
{
    limiter: Arc<dyn RateLimiter>,
    config: SlidingWindowConfig,
    key_fn: F,
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
        }
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
            // Fail-open: a backend error (e.g. Redis down) must not
            // take down the API — pass the request through.
            Err(_) => next(request).await,
        }
    }
}
