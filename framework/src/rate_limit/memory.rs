//! In-memory sliding-window rate limiter driver.
//!
//! ## Bucket-map growth and the periodic sweep
//!
//! The driver records one bucket per distinct key. When the keying
//! closure is attacker-controlled — historically the documented
//! "rate-limit by `X-Forwarded-For`" pattern, which an attacker could
//! abuse by rotating the header on a deployment without trusted-proxy
//! gating — the map can grow without bound.
//!
//! [`InMemoryRateLimiter::purge_inactive`] is the manual sweep
//! primitive: it drops every bucket whose last recorded hit aged out
//! past the supplied window. The constructor pair makes the choice
//! explicit:
//!
//! - [`InMemoryRateLimiter::new`] is sweep-free and runtime-free —
//!   the default for unit tests, `bootstrap_default`, and any other
//!   call site that doesn't need (or want) a background task.
//! - [`InMemoryRateLimiter::with_periodic_sweep`] spawns a `tokio`
//!   task that calls `purge_inactive` on a fixed interval. The task
//!   holds a `Weak` reference back to the shared bucket map, so the
//!   sweep self-terminates once the last `Arc<Self>` drops — no
//!   leaked task, no shutdown plumbing needed by the embedder.

use crate::error::FrameworkError;
use crate::rate_limit::algorithm::Bucket;
use crate::rate_limit::{RateLimiterDriver, SlidingWindowConfig};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::Instant;

type BucketMap = Mutex<HashMap<String, Bucket>>;

/// In-process sliding-window rate limiter. Default driver for
/// single-process apps; use [`RedisRateLimiter`](crate::rate_limit::redis::RedisRateLimiter)
/// for multi-instance deployments.
pub struct InMemoryRateLimiter {
    buckets: Arc<BucketMap>,
    sweep_handle: Mutex<Option<JoinHandle<()>>>,
}

impl InMemoryRateLimiter {
    /// Build a sweep-free driver. The bucket map grows as new keys
    /// arrive; call [`Self::purge_inactive`] manually, or use
    /// [`Self::with_periodic_sweep`] when running inside a tokio
    /// runtime that owns the driver for the process lifetime.
    pub fn new() -> Self {
        Self {
            buckets: Arc::new(Mutex::new(HashMap::new())),
            sweep_handle: Mutex::new(None),
        }
    }

    /// Build a driver that spawns a background sweep task. The task
    /// calls [`Self::purge_inactive`] every `interval`, dropping any
    /// bucket whose last recorded hit is older than `inactivity_window`.
    ///
    /// The task holds a [`Weak`] reference to the shared bucket map,
    /// so it self-terminates the next interval after the last `Arc`
    /// to the driver drops. Use this constructor from
    /// `bootstrap_from_env` and any other long-running embedder where
    /// the driver lives in the [`crate::container::App`] container
    /// for the process lifetime.
    ///
    /// Must be called from within a tokio runtime — the spawn happens
    /// via [`tokio::spawn`] and will panic if no runtime is
    /// installed. The sweep-free [`Self::new`] constructor is the
    /// right choice for unit tests that don't already have a runtime.
    pub fn with_periodic_sweep(interval: Duration, inactivity_window: Duration) -> Arc<Self> {
        let limiter = Arc::new(Self::new());
        let weak: Weak<BucketMap> = Arc::downgrade(&limiter.buckets);
        let handle = tokio::spawn(async move {
            // Sleep first so a freshly-constructed driver doesn't
            // immediately attempt a sweep on an empty map.
            let mut ticker = tokio::time::interval(interval);
            // Skip the immediate tick; we want the first wake to be
            // `interval` away from construction.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let Some(buckets) = weak.upgrade() else {
                    // Driver dropped — self-terminate.
                    break;
                };
                let now = Instant::now();
                if let Ok(mut g) = buckets.lock() {
                    g.retain(|_, bucket| !bucket.is_inactive(inactivity_window, now));
                }
            }
        });
        *limiter
            .sweep_handle
            .lock()
            .expect("rate limiter sweep handle poisoned at construction") = Some(handle);
        limiter
    }

    /// Drop every bucket whose last recorded hit aged out past
    /// `window`. Returns the number of buckets removed; useful for
    /// metrics and the unit test that asserts the sweep ran.
    ///
    /// Safe to call from a non-async context (it takes `&self` and
    /// no `.await` happens inside the lock).
    pub fn purge_inactive(&self, window: Duration, now: Instant) -> usize {
        let Ok(mut g) = self.buckets.lock() else {
            return 0;
        };
        let before = g.len();
        g.retain(|_, bucket| !bucket.is_inactive(window, now));
        before - g.len()
    }

    /// Current count of buckets in the map. Useful for tests and
    /// metrics that want to observe sweep progress without driving
    /// the limiter's public SPI surface.
    pub fn bucket_count(&self) -> usize {
        self.buckets.lock().map(|g| g.len()).unwrap_or(0)
    }
}

impl Drop for InMemoryRateLimiter {
    fn drop(&mut self) {
        // Abort the sweep task on drop. The `Weak` self-termination
        // path already covers the "Arc count hits zero" case, but
        // an explicit abort makes the test harness deterministic when
        // a fresh limiter is constructed and dropped without ever
        // hitting the first interval.
        if let Ok(mut handle) = self.sweep_handle.lock()
            && let Some(h) = handle.take()
        {
            h.abort();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn purge_inactive_drops_buckets_past_window() {
        let limiter = InMemoryRateLimiter::new();
        let cfg = SlidingWindowConfig {
            max_requests: 5,
            window: Duration::from_secs(60),
        };
        // Record three distinct keys at t=0.
        for k in ["a", "b", "c"] {
            assert!(limiter.try_acquire(k, &cfg).await.unwrap());
        }
        assert_eq!(limiter.bucket_count(), 3);

        // Jump past the inactivity window (60s) and sweep. Each
        // bucket's most-recent hit is now > 60s in the past, so the
        // sweep must drop all three.
        tokio::time::advance(Duration::from_secs(90)).await;
        let removed = limiter.purge_inactive(Duration::from_secs(60), Instant::now());
        assert_eq!(removed, 3, "all three buckets should have been dropped");
        assert_eq!(limiter.bucket_count(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn purge_inactive_retains_recently_hit_buckets() {
        let limiter = InMemoryRateLimiter::new();
        let cfg = SlidingWindowConfig {
            max_requests: 5,
            window: Duration::from_secs(60),
        };
        // Hit "fresh" at t=0.
        assert!(limiter.try_acquire("fresh", &cfg).await.unwrap());

        // Jump forward 30s — still inside the inactivity window — and
        // hit "fresh" again.
        tokio::time::advance(Duration::from_secs(30)).await;
        assert!(limiter.try_acquire("fresh", &cfg).await.unwrap());

        // Sweep with a 60s inactivity window. "fresh"'s last hit is
        // 0s old; the bucket must survive.
        let removed = limiter.purge_inactive(Duration::from_secs(60), Instant::now());
        assert_eq!(removed, 0);
        assert_eq!(limiter.bucket_count(), 1);
    }

    #[tokio::test]
    async fn periodic_sweep_runs_and_self_terminates_on_drop() {
        // Use REAL time so tokio's `interval` ticker observes the
        // wall-clock advance — `start_paused` makes the interval
        // arm/disarm under the test's control, which loses races
        // against `yield_now`. A short interval keeps the test cheap.
        let limiter = InMemoryRateLimiter::with_periodic_sweep(
            Duration::from_millis(50),
            Duration::from_millis(20),
        );
        let cfg = SlidingWindowConfig {
            max_requests: 5,
            window: Duration::from_secs(60),
        };
        assert!(limiter.try_acquire("expire-me", &cfg).await.unwrap());
        assert_eq!(limiter.bucket_count(), 1);

        // Poll until the sweep observes the bucket inactive and drops
        // it. With a 50 ms interval and 20 ms inactivity window, the
        // first sweep at ~50 ms should evict; allow up to 750 ms for
        // CI scheduling noise before treating it as a real failure.
        let deadline = tokio::time::Instant::now() + Duration::from_millis(750);
        loop {
            if limiter.bucket_count() == 0 {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("the background sweep should have dropped the inactive bucket within 750ms");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // Verify drop cleanly aborts the task — no panic, no leak.
        drop(limiter);
    }
}
