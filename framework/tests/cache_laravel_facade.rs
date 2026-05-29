//! Parity tests for the Laravel-side `Cache` facade methods shipped in the
//! Module 7 (cache) parity sweep: `missing`, `pull`, `add`, `sear`, plus the
//! `LockGuard::owner` alias.
//!
//! Each method is a facade-only composition over the existing `CacheStore`
//! contract; the tests run against `InMemoryCache` via `TestContainer` for
//! parallel-safety.

use std::sync::Arc;
use std::time::Duration;
use suprnova::cache::{Cache, CacheStore, InMemoryCache};
use suprnova::container::testing::TestContainer;

fn install_fresh_cache() -> suprnova::container::testing::TestContainerGuard {
    let guard = TestContainer::fake();
    TestContainer::bind::<dyn CacheStore>(Arc::new(InMemoryCache::with_prefix("facade:")));
    guard
}

// --- Cache::missing -------------------------------------------------------

#[tokio::test]
async fn missing_is_true_when_key_absent() {
    let _g = install_fresh_cache();
    assert!(Cache::missing("nope").await.unwrap());
}

#[tokio::test]
async fn missing_is_false_when_key_present() {
    let _g = install_fresh_cache();
    Cache::put("present", &42i32, None).await.unwrap();
    assert!(!Cache::missing("present").await.unwrap());
}

#[tokio::test]
async fn missing_is_true_after_forget() {
    let _g = install_fresh_cache();
    Cache::put("temp", &"x", None).await.unwrap();
    Cache::forget("temp").await.unwrap();
    assert!(Cache::missing("temp").await.unwrap());
}

// --- Cache::pull ----------------------------------------------------------

#[tokio::test]
async fn pull_returns_some_and_deletes_the_key() {
    let _g = install_fresh_cache();
    Cache::put("p", &"value", None).await.unwrap();

    let pulled: Option<String> = Cache::pull("p").await.unwrap();
    assert_eq!(pulled.as_deref(), Some("value"));
    assert!(
        !Cache::has("p").await.unwrap(),
        "pull must forget the key after returning it"
    );
}

#[tokio::test]
async fn pull_returns_none_for_absent_key() {
    let _g = install_fresh_cache();
    let pulled: Option<String> = Cache::pull("never-stored").await.unwrap();
    assert!(pulled.is_none());
}

#[tokio::test]
async fn pull_does_not_forget_when_key_absent() {
    // A no-op forget on an absent key is fine, but pull should not
    // perform extra work either. We assert the post-state matches.
    let _g = install_fresh_cache();
    Cache::put("other", &"keep", None).await.unwrap();

    let _: Option<String> = Cache::pull("absent").await.unwrap();

    let other: Option<String> = Cache::get("other").await.unwrap();
    assert_eq!(other.as_deref(), Some("keep"));
}

// --- Cache::add -----------------------------------------------------------

#[tokio::test]
async fn add_writes_when_key_is_absent_and_returns_true() {
    let _g = install_fresh_cache();
    let written = Cache::add("first", &"a", None).await.unwrap();
    assert!(written);

    let got: Option<String> = Cache::get("first").await.unwrap();
    assert_eq!(got.as_deref(), Some("a"));
}

#[tokio::test]
async fn add_does_not_overwrite_existing_key_and_returns_false() {
    let _g = install_fresh_cache();
    Cache::put("k", &"original", None).await.unwrap();

    let written = Cache::add("k", &"new", None).await.unwrap();
    assert!(
        !written,
        "add must return false when the key is already present"
    );

    let got: Option<String> = Cache::get("k").await.unwrap();
    assert_eq!(
        got.as_deref(),
        Some("original"),
        "the original value must be preserved"
    );
}

#[tokio::test]
async fn add_after_forget_succeeds() {
    let _g = install_fresh_cache();
    Cache::put("k", &"v1", None).await.unwrap();
    Cache::forget("k").await.unwrap();

    let written = Cache::add("k", &"v2", None).await.unwrap();
    assert!(written);

    let got: Option<String> = Cache::get("k").await.unwrap();
    assert_eq!(got.as_deref(), Some("v2"));
}

#[tokio::test]
async fn add_threads_ttl_through_to_put() {
    let _g = install_fresh_cache();
    let written = Cache::add("ttl-key", &"x", Some(Duration::from_millis(50)))
        .await
        .unwrap();
    assert!(written);
    assert!(Cache::has("ttl-key").await.unwrap());

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !Cache::has("ttl-key").await.unwrap(),
        "the TTL passed to add must apply to the underlying put"
    );
}

#[tokio::test]
async fn add_raw_is_atomic_against_concurrent_writers_on_in_memory() {
    // Direct CacheStore::add_raw exercise — the in-memory backend must
    // hold its write lock across the existence check + insert. Two
    // racing add_raw calls for the same key must yield exactly one
    // winner.
    let store: Arc<dyn CacheStore> = Arc::new(InMemoryCache::with_prefix("atomic-add:"));

    // 32 racers attempting to write to the same key with distinct
    // values; collect the boolean returns.
    let mut handles = Vec::new();
    for i in 0..32 {
        let s = Arc::clone(&store);
        let v = format!("racer-{i}");
        handles.push(tokio::spawn(async move {
            s.add_raw("contested", &v, None).await.unwrap()
        }));
    }

    let mut wins = 0usize;
    for h in handles {
        if h.await.unwrap() {
            wins += 1;
        }
    }
    assert_eq!(
        wins, 1,
        "exactly one concurrent add_raw must win; got {wins} winners"
    );

    // The stored value must be one of the racers, not a torn write.
    let stored = store.get_raw("contested").await.unwrap().unwrap();
    assert!(
        stored.starts_with("racer-"),
        "stored value `{stored}` must be one of the racer-N values"
    );
}

// --- Cache::sear (alias of remember_forever) -----------------------------

#[tokio::test]
async fn sear_computes_and_stores_when_key_absent() {
    let _g = install_fresh_cache();
    let computed_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let calls_clone = computed_calls.clone();
    let value: i32 = Cache::sear("sear-key", move || {
        let calls = calls_clone.clone();
        async move {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(42)
        }
    })
    .await
    .unwrap();

    assert_eq!(value, 42);
    assert_eq!(computed_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn sear_returns_cached_value_without_computing() {
    let _g = install_fresh_cache();
    Cache::forever("cached", &7i32).await.unwrap();

    let computed_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_clone = computed_calls.clone();
    let value: i32 = Cache::sear("cached", move || {
        let calls = calls_clone.clone();
        async move {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(99)
        }
    })
    .await
    .unwrap();

    assert_eq!(value, 7, "must return the cached value, not the closure's");
    assert_eq!(
        computed_calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the closure must not run when the key is cached"
    );
}

// --- LockGuard::owner (alias of token) -----------------------------------

#[tokio::test]
async fn lock_guard_owner_matches_token() {
    let _g = install_fresh_cache();
    let guard = Cache::lock("printer", Duration::from_secs(5))
        .await
        .unwrap()
        .expect("first acquire wins");

    assert_eq!(
        guard.owner(),
        guard.token(),
        "Laravel-side owner() must return the same string as Rust-side token()"
    );

    // Cleanup so we don't leak the lock through the test container teardown.
    let _ = guard.release().await;
}
