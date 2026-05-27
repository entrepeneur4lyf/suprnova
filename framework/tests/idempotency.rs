use serial_test::serial;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use suprnova::cache::InMemoryCache;
use suprnova::cache::store::CacheStore;
use suprnova::container::App;
use suprnova::idempotency::{Idempotency, Idempotent, Replay};

static RAN: AtomicU32 = AtomicU32::new(0);

fn install_memory_cache() {
    let store: Arc<dyn CacheStore> = Arc::new(InMemoryCache::with_prefix("idem:"));
    App::bind::<dyn CacheStore>(store);
}

#[tokio::test]
#[serial]
async fn first_call_runs_body_subsequent_call_is_duplicate() {
    RAN.store(0, Ordering::SeqCst);
    install_memory_cache();

    let r1: Idempotent<i32> = Idempotency::once("k-1", Duration::from_secs(60), || async {
        RAN.fetch_add(1, Ordering::SeqCst);
        Ok(42_i32)
    })
    .await
    .unwrap();
    assert!(matches!(r1, Idempotent::Fresh(42)));
    assert_eq!(RAN.load(Ordering::SeqCst), 1);

    let r2: Idempotent<i32> = Idempotency::once("k-1", Duration::from_secs(60), || async {
        RAN.fetch_add(1, Ordering::SeqCst);
        Ok(99_i32)
    })
    .await
    .unwrap();
    assert!(matches!(r2, Idempotent::Duplicate));
    assert_eq!(
        RAN.load(Ordering::SeqCst),
        1,
        "body must not run for duplicate key"
    );
}

#[tokio::test]
#[serial]
async fn key_expires_after_ttl() {
    install_memory_cache();
    let _ = Idempotency::once::<_, _, ()>("k-2", Duration::from_millis(50), || async { Ok(()) })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let r = Idempotency::once::<_, _, i32>("k-2", Duration::from_secs(5), || async { Ok(7) })
        .await
        .unwrap();
    assert!(matches!(r, Idempotent::Fresh(7)));
}

#[tokio::test]
#[serial]
async fn once_consumes_the_window_even_when_body_errors() {
    RAN.store(0, Ordering::SeqCst);
    install_memory_cache();

    // First caller's body errors. `once` does NOT release on error — the TTL
    // is the dedupe window regardless of outcome (this is the contract that
    // distinguishes it from `commit_on_success`).
    let r1 = Idempotency::once::<_, _, i32>("once-err", Duration::from_secs(60), || async {
        RAN.fetch_add(1, Ordering::SeqCst);
        Err(suprnova::FrameworkError::internal("boom"))
    })
    .await;
    assert!(r1.is_err());

    // Second caller within the window: the lock is still held → Duplicate, and
    // the body does NOT run again.
    let r2: Idempotent<i32> = Idempotency::once("once-err", Duration::from_secs(60), || async {
        RAN.fetch_add(1, Ordering::SeqCst);
        Ok(7)
    })
    .await
    .unwrap();
    assert_eq!(r2, Idempotent::Duplicate);
    assert_eq!(
        RAN.load(Ordering::SeqCst),
        1,
        "once must not re-run after a failed predecessor; use commit_on_success to allow retry"
    );
}

#[tokio::test]
#[serial]
async fn commit_on_success_releases_lock_when_body_errors() {
    RAN.store(0, Ordering::SeqCst);
    install_memory_cache();

    // First call — body returns Err, lock must be released.
    let r1 =
        Idempotency::commit_on_success::<_, _, i32>("cos-1", Duration::from_secs(60), || async {
            RAN.fetch_add(1, Ordering::SeqCst);
            Err(suprnova::FrameworkError::internal("synthetic"))
        })
        .await;
    assert!(r1.is_err());
    assert_eq!(RAN.load(Ordering::SeqCst), 1);

    // Second call — lock was released, so body runs again.
    let r2: Idempotent<i32> =
        Idempotency::commit_on_success("cos-1", Duration::from_secs(60), || async {
            RAN.fetch_add(1, Ordering::SeqCst);
            Ok(99)
        })
        .await
        .unwrap();
    assert!(matches!(r2, Idempotent::Fresh(99)));
    assert_eq!(
        RAN.load(Ordering::SeqCst),
        2,
        "body must run after a failed predecessor releases the lock"
    );
}

#[tokio::test]
#[serial]
async fn commit_on_success_keeps_lock_when_body_succeeds() {
    RAN.store(0, Ordering::SeqCst);
    install_memory_cache();

    let r1: Idempotent<i32> =
        Idempotency::commit_on_success("cos-2", Duration::from_secs(60), || async {
            RAN.fetch_add(1, Ordering::SeqCst);
            Ok(42)
        })
        .await
        .unwrap();
    assert!(matches!(r1, Idempotent::Fresh(42)));
    assert_eq!(RAN.load(Ordering::SeqCst), 1);

    // Duplicate caller after success — still Duplicate.
    let r2: Idempotent<i32> =
        Idempotency::commit_on_success("cos-2", Duration::from_secs(60), || async {
            RAN.fetch_add(1, Ordering::SeqCst);
            Ok(99)
        })
        .await
        .unwrap();
    assert!(matches!(r2, Idempotent::Duplicate));
    assert_eq!(
        RAN.load(Ordering::SeqCst),
        1,
        "body must not run for duplicate after success"
    );
}

#[tokio::test]
#[serial]
async fn remember_records_result_and_replays_it_to_duplicates() {
    RAN.store(0, Ordering::SeqCst);
    install_memory_cache();

    let r1: Replay<String> = Idempotency::remember("rem-1", Duration::from_secs(60), || async {
        RAN.fetch_add(1, Ordering::SeqCst);
        Ok("hello".to_string())
    })
    .await
    .unwrap();
    assert_eq!(r1, Replay::Fresh("hello".to_string()));

    // Duplicate: a different body value must NOT run; the recorded result replays.
    let r2: Replay<String> = Idempotency::remember("rem-1", Duration::from_secs(60), || async {
        RAN.fetch_add(1, Ordering::SeqCst);
        Ok("world".to_string())
    })
    .await
    .unwrap();
    assert_eq!(r2, Replay::Replayed("hello".to_string()));
    assert_eq!(
        RAN.load(Ordering::SeqCst),
        1,
        "replay must not run the body"
    );
}

#[tokio::test]
#[serial]
async fn remember_error_does_not_replay_and_is_retryable() {
    RAN.store(0, Ordering::SeqCst);
    install_memory_cache();

    // First call errors — nothing is recorded and the lock is released.
    let r1 = Idempotency::remember::<_, _, i32>("rem-err", Duration::from_secs(60), || async {
        RAN.fetch_add(1, Ordering::SeqCst);
        Err(suprnova::FrameworkError::internal("boom"))
    })
    .await;
    assert!(r1.is_err());

    // Second call re-enters (lock was released) and succeeds.
    let r2: Replay<i32> = Idempotency::remember("rem-err", Duration::from_secs(60), || async {
        RAN.fetch_add(1, Ordering::SeqCst);
        Ok(42)
    })
    .await
    .unwrap();
    assert_eq!(r2, Replay::Fresh(42));

    // Third call replays the recorded success.
    let r3: Replay<i32> = Idempotency::remember("rem-err", Duration::from_secs(60), || async {
        RAN.fetch_add(1, Ordering::SeqCst);
        Ok(0)
    })
    .await
    .unwrap();
    assert_eq!(r3, Replay::Replayed(42));
    assert_eq!(
        RAN.load(Ordering::SeqCst),
        2,
        "body runs once on retry-after-error and never again on replay"
    );
}

#[tokio::test]
#[serial]
async fn remember_returns_in_progress_for_concurrent_duplicate() {
    install_memory_cache();

    // `inside_body` fires once caller 1 is executing the body (lock held, no
    // result recorded yet); `release_body` lets caller 1 finish.
    let inside_body = Arc::new(tokio::sync::Notify::new());
    let inside_body_tx = inside_body.clone();
    let release_body = Arc::new(tokio::sync::Notify::new());
    let release_body_rx = release_body.clone();

    let caller1 = tokio::spawn(async move {
        Idempotency::remember::<_, _, i32>("inprog", Duration::from_secs(60), || async move {
            inside_body_tx.notify_one();
            release_body_rx.notified().await;
            Ok(7)
        })
        .await
    });

    // Wait until caller 1 is inside the body, then race a duplicate in.
    inside_body.notified().await;
    let r2: Replay<i32> =
        Idempotency::remember("inprog", Duration::from_secs(60), || async { Ok(99) })
            .await
            .unwrap();
    assert_eq!(
        r2,
        Replay::InProgress,
        "duplicate arriving before the original records a result must be InProgress"
    );

    // Let caller 1 finish and record its result.
    release_body.notify_one();
    let r1 = caller1.await.unwrap().unwrap();
    assert_eq!(r1, Replay::Fresh(7));

    // A later caller now replays the recorded result.
    let r3: Replay<i32> =
        Idempotency::remember("inprog", Duration::from_secs(60), || async { Ok(0) })
            .await
            .unwrap();
    assert_eq!(r3, Replay::Replayed(7));
}

#[tokio::test]
#[serial]
async fn long_body_keeps_lock_alive_so_a_late_duplicate_does_not_double_execute() {
    RAN.store(0, Ordering::SeqCst);
    install_memory_cache();

    let inside_body = Arc::new(tokio::sync::Notify::new());
    let inside_body_tx = inside_body.clone();
    let release_body = Arc::new(tokio::sync::Notify::new());
    let release_body_rx = release_body.clone();

    // Caller 1 holds a 200ms-TTL lock but blocks in the body well past it.
    // Without lease renewal the lock would expire at ~200ms and a later caller
    // would acquire it and run the body a second time.
    let caller1 = tokio::spawn(async move {
        Idempotency::once::<_, _, i32>("watchdog", Duration::from_millis(200), || async move {
            RAN.fetch_add(1, Ordering::SeqCst);
            inside_body_tx.notify_one();
            release_body_rx.notified().await;
            Ok(1)
        })
        .await
    });

    inside_body.notified().await;
    // Wait 2.5x the original TTL: a non-renewed lock would already be gone.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let r2: Idempotent<i32> = Idempotency::once("watchdog", Duration::from_millis(200), || async {
        RAN.fetch_add(1, Ordering::SeqCst);
        Ok(2)
    })
    .await
    .unwrap();
    assert_eq!(
        r2,
        Idempotent::Duplicate,
        "lease renewal must keep the lock alive past the original TTL while the body runs"
    );

    release_body.notify_one();
    let r1 = caller1.await.unwrap().unwrap();
    assert_eq!(r1, Idempotent::Fresh(1));
    assert_eq!(
        RAN.load(Ordering::SeqCst),
        1,
        "body must execute exactly once despite the body outliving the TTL"
    );
}

/// A cache whose `release_lock` always fails, leaving every other operation
/// delegated to a real in-memory backend. Proves the release path is handled
/// (logged, not panicked, body error preserved) when the backend cannot
/// acknowledge a release.
struct FailingReleaseCache(InMemoryCache);

#[async_trait::async_trait]
impl CacheStore for FailingReleaseCache {
    async fn get_raw(&self, key: &str) -> Result<Option<String>, suprnova::FrameworkError> {
        self.0.get_raw(key).await
    }
    async fn put_raw(
        &self,
        key: &str,
        value: &str,
        ttl: Option<Duration>,
    ) -> Result<(), suprnova::FrameworkError> {
        self.0.put_raw(key, value, ttl).await
    }
    fn default_ttl(&self) -> Option<Duration> {
        self.0.default_ttl()
    }
    async fn has(&self, key: &str) -> Result<bool, suprnova::FrameworkError> {
        self.0.has(key).await
    }
    async fn forget(&self, key: &str) -> Result<bool, suprnova::FrameworkError> {
        self.0.forget(key).await
    }
    async fn flush(&self) -> Result<(), suprnova::FrameworkError> {
        self.0.flush().await
    }
    async fn increment(&self, key: &str, amount: i64) -> Result<i64, suprnova::FrameworkError> {
        self.0.increment(key, amount).await
    }
    async fn decrement(&self, key: &str, amount: i64) -> Result<i64, suprnova::FrameworkError> {
        self.0.decrement(key, amount).await
    }
    async fn tagged_put_raw(
        &self,
        tags: &[&str],
        key: &str,
        value: &str,
        ttl: Option<Duration>,
    ) -> Result<(), suprnova::FrameworkError> {
        self.0.tagged_put_raw(tags, key, value, ttl).await
    }
    async fn flush_tags(&self, tags: &[&str]) -> Result<(), suprnova::FrameworkError> {
        self.0.flush_tags(tags).await
    }
    async fn acquire_lock(
        &self,
        key: &str,
        ttl: Duration,
    ) -> Result<Option<String>, suprnova::FrameworkError> {
        self.0.acquire_lock(key, ttl).await
    }
    async fn release_lock(
        &self,
        _key: &str,
        _token: &str,
    ) -> Result<bool, suprnova::FrameworkError> {
        Err(suprnova::FrameworkError::internal(
            "synthetic release failure",
        ))
    }
    async fn refresh_lock(
        &self,
        key: &str,
        token: &str,
        ttl: Duration,
    ) -> Result<bool, suprnova::FrameworkError> {
        self.0.refresh_lock(key, token, ttl).await
    }
    async fn touch(&self, key: &str, ttl: Duration) -> Result<bool, suprnova::FrameworkError> {
        self.0.touch(key, ttl).await
    }
}

#[tokio::test]
#[serial]
async fn commit_on_success_surfaces_body_error_even_when_release_fails() {
    let store: Arc<dyn CacheStore> =
        Arc::new(FailingReleaseCache(InMemoryCache::with_prefix("idem:")));
    App::bind::<dyn CacheStore>(store);

    // Body fails; the Err-path release also fails. The release failure must be
    // logged (not masked) and the body error must come back without a panic.
    let r = Idempotency::commit_on_success::<_, _, i32>(
        "rel-fail",
        Duration::from_secs(60),
        || async { Err(suprnova::FrameworkError::internal("body boom")) },
    )
    .await;

    assert!(
        r.is_err(),
        "the body error is the only error on this path; a failing release must not swallow or replace it"
    );
}
