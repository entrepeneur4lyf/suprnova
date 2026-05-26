use std::sync::Arc;
use std::time::Duration;
use suprnova::cache::InMemoryCache;
use suprnova::cache::store::CacheStore;

async fn fresh() -> Arc<dyn CacheStore> {
    Arc::new(InMemoryCache::with_prefix("locks:"))
}

#[tokio::test]
async fn lock_acquire_succeeds_for_first_caller_only() {
    let s = fresh().await;
    let alice = s
        .acquire_lock("printer", Duration::from_secs(5))
        .await
        .unwrap();
    assert!(alice.is_some(), "first caller acquires lock");

    let bob = s
        .acquire_lock("printer", Duration::from_secs(5))
        .await
        .unwrap();
    assert!(bob.is_none(), "second caller while alice holds the lock");

    s.release_lock("printer", alice.as_ref().unwrap())
        .await
        .unwrap();

    let carol = s
        .acquire_lock("printer", Duration::from_secs(5))
        .await
        .unwrap();
    assert!(carol.is_some(), "after release, next caller acquires");
}

#[tokio::test]
async fn release_with_wrong_token_does_nothing() {
    let s = fresh().await;
    let alice = s
        .acquire_lock("printer", Duration::from_secs(5))
        .await
        .unwrap()
        .unwrap();
    let released = s.release_lock("printer", "not-alice-token").await.unwrap();
    assert!(!released, "must reject release with wrong token");
    let released_real = s.release_lock("printer", &alice).await.unwrap();
    assert!(released_real);
}

// Note: Uses real-time short TTLs instead of start_paused + tokio::time::advance
// because CacheEntry uses std::time::Instant which is unaffected by tokio's
// time mocking. Real-time sleep is the only reliable approach here.
#[tokio::test]
async fn lock_expires_after_ttl_and_can_be_reacquired() {
    let s = fresh().await;
    let alice = s
        .acquire_lock("printer", Duration::from_millis(50))
        .await
        .unwrap();
    assert!(alice.is_some());

    tokio::time::sleep(Duration::from_millis(100)).await;

    let bob = s
        .acquire_lock("printer", Duration::from_secs(5))
        .await
        .unwrap();
    assert!(
        bob.is_some(),
        "ttl expiry releases the lock for the next caller"
    );
}

// Note: Same real-time approach for the same reason.
#[tokio::test]
async fn refresh_extends_ttl_only_for_the_owner() {
    let s = fresh().await;
    let alice = s
        .acquire_lock("printer", Duration::from_millis(200))
        .await
        .unwrap()
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Alice refreshes before expiry — lock should last another 200ms from now
    let refreshed = s
        .refresh_lock("printer", &alice, Duration::from_millis(200))
        .await
        .unwrap();
    assert!(refreshed);

    tokio::time::sleep(Duration::from_millis(100)).await;

    // At 200ms total, alice's original TTL would have expired but refresh kept it alive
    let bob = s
        .acquire_lock("printer", Duration::from_millis(50))
        .await
        .unwrap();
    assert!(bob.is_none(), "refresh kept alice's lock alive");

    // Wrong token cannot refresh
    let bob_refresh = s
        .refresh_lock("printer", "junk", Duration::from_millis(10000))
        .await
        .unwrap();
    assert!(!bob_refresh);
}
