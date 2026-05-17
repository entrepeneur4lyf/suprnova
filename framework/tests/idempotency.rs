use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use serial_test::serial;
use suprnova::cache::store::CacheStore;
use suprnova::cache::InMemoryCache;
use suprnova::container::App;
use suprnova::idempotency::{Idempotency, Idempotent};

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
    }).await.unwrap();
    assert!(matches!(r1, Idempotent::Fresh(42)));
    assert_eq!(RAN.load(Ordering::SeqCst), 1);

    let r2: Idempotent<i32> = Idempotency::once("k-1", Duration::from_secs(60), || async {
        RAN.fetch_add(1, Ordering::SeqCst);
        Ok(99_i32)
    }).await.unwrap();
    assert!(matches!(r2, Idempotent::Duplicate));
    assert_eq!(RAN.load(Ordering::SeqCst), 1, "body must not run for duplicate key");
}

#[tokio::test]
#[serial]
async fn key_expires_after_ttl() {
    install_memory_cache();
    let _ = Idempotency::once::<_, _, ()>("k-2", Duration::from_millis(50), || async { Ok(()) }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let r = Idempotency::once::<_, _, i32>("k-2", Duration::from_secs(5), || async { Ok(7) }).await.unwrap();
    assert!(matches!(r, Idempotent::Fresh(7)));
}

#[tokio::test]
#[serial]
async fn commit_on_success_releases_lock_when_body_errors() {
    RAN.store(0, Ordering::SeqCst);
    install_memory_cache();

    // First call — body returns Err, lock must be released.
    let r1 = Idempotency::commit_on_success::<_, _, i32>(
        "cos-1",
        Duration::from_secs(60),
        || async {
            RAN.fetch_add(1, Ordering::SeqCst);
            Err(suprnova::FrameworkError::internal("synthetic"))
        },
    ).await;
    assert!(r1.is_err());
    assert_eq!(RAN.load(Ordering::SeqCst), 1);

    // Second call — lock was released, so body runs again.
    let r2: Idempotent<i32> = Idempotency::commit_on_success(
        "cos-1",
        Duration::from_secs(60),
        || async {
            RAN.fetch_add(1, Ordering::SeqCst);
            Ok(99)
        },
    ).await.unwrap();
    assert!(matches!(r2, Idempotent::Fresh(99)));
    assert_eq!(RAN.load(Ordering::SeqCst), 2, "body must run after a failed predecessor releases the lock");
}

#[tokio::test]
#[serial]
async fn commit_on_success_keeps_lock_when_body_succeeds() {
    RAN.store(0, Ordering::SeqCst);
    install_memory_cache();

    let r1: Idempotent<i32> = Idempotency::commit_on_success(
        "cos-2",
        Duration::from_secs(60),
        || async {
            RAN.fetch_add(1, Ordering::SeqCst);
            Ok(42)
        },
    ).await.unwrap();
    assert!(matches!(r1, Idempotent::Fresh(42)));
    assert_eq!(RAN.load(Ordering::SeqCst), 1);

    // Duplicate caller after success — still Duplicate.
    let r2: Idempotent<i32> = Idempotency::commit_on_success(
        "cos-2",
        Duration::from_secs(60),
        || async {
            RAN.fetch_add(1, Ordering::SeqCst);
            Ok(99)
        },
    ).await.unwrap();
    assert!(matches!(r2, Idempotent::Duplicate));
    assert_eq!(RAN.load(Ordering::SeqCst), 1, "body must not run for duplicate after success");
}
