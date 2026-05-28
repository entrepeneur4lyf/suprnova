//! `Queue::push_unique` enqueue gating.
//!
//! The dedupe key is `queue-unique:<job_name>:<id>`; a second `push_unique`
//! for the same key within `Job::unique_for()` returns `Ok(false)` and does
//! NOT publish a second envelope to the driver. The TTL test below uses a
//! short-lived `unique_for` so the second call escapes the dedupe window
//! without sleeping for minutes.

use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use std::time::Duration;
use suprnova::App;
use suprnova::cache::{CacheStore, InMemoryCache};
use suprnova::queue::Queue;
use suprnova::queue::driver::QueueDriver;
use suprnova::queue::memory::MemoryQueueDriver;
use suprnova::{FrameworkError, Job, async_trait};

#[derive(Serialize, Deserialize, Clone)]
struct UniqueJob {
    id: u32,
}

#[async_trait]
impl Job for UniqueJob {
    fn job_name() -> &'static str {
        "UniqueJob"
    }
    fn unique_id(&self) -> Option<String> {
        Some(self.id.to_string())
    }
    fn unique_for() -> Duration {
        // Long enough to test "second push is rejected" reliably; the TTL
        // test below uses a different short-lived job to avoid sleeping.
        Duration::from_secs(60)
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct ShortTtlJob {
    id: u32,
}

#[async_trait]
impl Job for ShortTtlJob {
    fn job_name() -> &'static str {
        "ShortTtlJob"
    }
    fn unique_id(&self) -> Option<String> {
        Some(self.id.to_string())
    }
    fn unique_for() -> Duration {
        Duration::from_millis(700)
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct NoUniqueIdJob;

#[async_trait]
impl Job for NoUniqueIdJob {
    fn job_name() -> &'static str {
        "NoUniqueIdJob"
    }
    // Inherits the default `unique_id() -> None`.
    async fn handle(self) -> Result<(), FrameworkError> {
        Ok(())
    }
}

async fn install_memory_drivers() {
    // `Cache::bootstrap` is `pub(crate)` because it reads `CacheConfig` from
    // env; in tests we bind the in-memory store directly so the dedupe
    // lock has a backing store without depending on env state.
    App::bind::<dyn CacheStore>(Arc::new(InMemoryCache::new()));
    Queue::set_driver(Arc::new(MemoryQueueDriver::new()));
}

async fn pop_all(driver: &Arc<dyn QueueDriver>) -> usize {
    let mut n = 0;
    while let Some(res) = driver.pop(Duration::from_millis(50)).await.unwrap() {
        driver.ack(&res.token).await.unwrap();
        n += 1;
    }
    n
}

#[tokio::test]
#[serial]
async fn push_unique_suppresses_a_duplicate_within_the_window() {
    install_memory_drivers().await;
    // Pre-cleanup: a prior test may have left an envelope in the registered
    // driver (we re-install per-test, but the test order isn't guaranteed).
    let drv = Queue::driver().unwrap();
    let _ = pop_all(&drv).await;

    let first = Queue::push_unique(UniqueJob { id: 1 }).await.unwrap();
    assert!(first, "first push must enqueue (Fresh)");

    let second = Queue::push_unique(UniqueJob { id: 1 }).await.unwrap();
    assert!(
        !second,
        "second push within unique_for must be suppressed (Duplicate)"
    );

    let drained = pop_all(&drv).await;
    assert_eq!(
        drained, 1,
        "exactly one envelope was published to the driver"
    );
}

#[tokio::test]
#[serial]
async fn push_unique_lets_different_ids_through() {
    install_memory_drivers().await;
    let drv = Queue::driver().unwrap();
    let _ = pop_all(&drv).await;

    assert!(Queue::push_unique(UniqueJob { id: 10 }).await.unwrap());
    assert!(Queue::push_unique(UniqueJob { id: 11 }).await.unwrap());
    let drained = pop_all(&drv).await;
    assert_eq!(drained, 2, "different unique_ids enqueue independently");
}

#[tokio::test]
#[serial]
async fn push_unique_re_enqueues_after_ttl_expires() {
    install_memory_drivers().await;
    let drv = Queue::driver().unwrap();
    let _ = pop_all(&drv).await;

    assert!(Queue::push_unique(ShortTtlJob { id: 1 }).await.unwrap());

    // Within the 700ms window — still a duplicate.
    assert!(!Queue::push_unique(ShortTtlJob { id: 1 }).await.unwrap());

    // Past the window — the dedupe key has expired so a fresh push lands.
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(Queue::push_unique(ShortTtlJob { id: 1 }).await.unwrap());

    let drained = pop_all(&drv).await;
    assert_eq!(drained, 2, "two envelopes after the TTL window elapses");
}

#[tokio::test]
#[serial]
async fn push_unique_errors_when_unique_id_returns_none() {
    install_memory_drivers().await;
    let err = Queue::push_unique(NoUniqueIdJob).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("unique_id"),
        "error must name the missing trait method: {msg}"
    );
}

#[tokio::test]
#[serial]
async fn push_unique_populates_envelope_idempotency_key() {
    install_memory_drivers().await;
    let drv = Queue::driver().unwrap();
    let _ = pop_all(&drv).await;

    assert!(Queue::push_unique(UniqueJob { id: 42 }).await.unwrap());

    let res = drv
        .pop(Duration::from_millis(50))
        .await
        .unwrap()
        .expect("envelope present");
    assert_eq!(
        res.envelope.idempotency_key.as_deref(),
        Some("42"),
        "envelope must carry the unique_id for log correlation"
    );
    drv.ack(&res.token).await.unwrap();
}
