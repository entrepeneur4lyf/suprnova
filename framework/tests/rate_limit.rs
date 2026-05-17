use std::sync::Arc;
use std::time::Duration;
use suprnova::rate_limit::memory::InMemoryRateLimiter;
use suprnova::rate_limit::{RateLimiter, SlidingWindowConfig};

fn cfg(max: u32, window_secs: u64) -> SlidingWindowConfig {
    SlidingWindowConfig {
        max_requests: max,
        window: Duration::from_secs(window_secs),
    }
}

#[tokio::test(start_paused = true)]
async fn allows_up_to_max_within_window_then_rejects() {
    let limiter: Arc<dyn RateLimiter> = Arc::new(InMemoryRateLimiter::new());
    let key = "user:1";
    let c = cfg(3, 10);

    assert!(limiter.try_acquire(key, &c).await.unwrap());
    assert!(limiter.try_acquire(key, &c).await.unwrap());
    assert!(limiter.try_acquire(key, &c).await.unwrap());
    assert!(
        !limiter.try_acquire(key, &c).await.unwrap(),
        "4th must be rejected"
    );
}

#[tokio::test(start_paused = true)]
async fn window_slides_so_old_hits_expire() {
    let limiter: Arc<dyn RateLimiter> = Arc::new(InMemoryRateLimiter::new());
    let key = "user:2";
    let c = cfg(2, 10);

    assert!(limiter.try_acquire(key, &c).await.unwrap());
    assert!(limiter.try_acquire(key, &c).await.unwrap());
    assert!(!limiter.try_acquire(key, &c).await.unwrap());

    tokio::time::advance(Duration::from_secs(11)).await;

    assert!(limiter.try_acquire(key, &c).await.unwrap());
    assert!(limiter.try_acquire(key, &c).await.unwrap());
    assert!(!limiter.try_acquire(key, &c).await.unwrap());
}

#[tokio::test(start_paused = true)]
async fn distinct_keys_have_independent_buckets() {
    let limiter: Arc<dyn RateLimiter> = Arc::new(InMemoryRateLimiter::new());
    let c = cfg(1, 60);
    assert!(limiter.try_acquire("a", &c).await.unwrap());
    assert!(limiter.try_acquire("b", &c).await.unwrap());
    assert!(!limiter.try_acquire("a", &c).await.unwrap());
    assert!(!limiter.try_acquire("b", &c).await.unwrap());
}

#[tokio::test(start_paused = true)]
async fn retry_after_reflects_oldest_entry_in_window() {
    let limiter: Arc<dyn RateLimiter> = Arc::new(InMemoryRateLimiter::new());
    let c = cfg(1, 30);
    assert!(limiter.try_acquire("k", &c).await.unwrap());

    tokio::time::advance(Duration::from_secs(10)).await;
    let retry = limiter.retry_after("k", &c).await.unwrap();
    // window=30, oldest entry is 10s old → retry-after = 20s.
    assert_eq!(retry, Some(Duration::from_secs(20)));
}
