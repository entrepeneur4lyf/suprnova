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
