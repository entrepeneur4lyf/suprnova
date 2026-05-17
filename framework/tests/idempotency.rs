use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
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
async fn key_expires_after_ttl() {
    install_memory_cache();
    let _ = Idempotency::once::<_, _, ()>("k-2", Duration::from_millis(50), || async { Ok(()) }).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let r = Idempotency::once::<_, _, i32>("k-2", Duration::from_secs(5), || async { Ok(7) }).await.unwrap();
    assert!(matches!(r, Idempotent::Fresh(7)));
}
