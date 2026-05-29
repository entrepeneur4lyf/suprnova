//! In-memory sliding-window rate limiter driver.

use crate::error::FrameworkError;
use crate::rate_limit::algorithm::Bucket;
use crate::rate_limit::{RateLimiterDriver, SlidingWindowConfig};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use tokio::time::Instant;

pub struct InMemoryRateLimiter {
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl InMemoryRateLimiter {
    pub fn new() -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RateLimiterDriver for InMemoryRateLimiter {
    async fn try_acquire(
        &self,
        key: &str,
        config: &SlidingWindowConfig,
    ) -> Result<bool, FrameworkError> {
        let now = Instant::now();
        let mut g = self
            .buckets
            .lock()
            .map_err(|_| FrameworkError::internal("rate limiter poisoned"))?;
        let b = g.entry(key.to_string()).or_insert_with(Bucket::new);
        Ok(b.try_record(config.max_requests, config.window, now))
    }

    async fn retry_after(
        &self,
        key: &str,
        config: &SlidingWindowConfig,
    ) -> Result<Option<Duration>, FrameworkError> {
        let now = Instant::now();
        let g = self
            .buckets
            .lock()
            .map_err(|_| FrameworkError::internal("rate limiter poisoned"))?;
        Ok(g.get(key)
            .and_then(|b| b.retry_after(config.max_requests, config.window, now)))
    }
}
