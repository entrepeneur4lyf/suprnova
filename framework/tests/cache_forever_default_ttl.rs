//! Regression: HIGH audit finding `cache` #252 — `Cache::forever` is
//! not forever when `CACHE_DEFAULT_TTL` is set.
//!
//! Both stores must honour `put_raw(_, _, None)` as "no expiration",
//! independent of any facade-level default. `Cache::forever` reaches
//! the store with `None` directly; `Cache::put(None)` reaches the
//! store with the facade-resolved default TTL applied.
//!
//! These tests exercise the store-level contract (where the audit
//! divergence lived) rather than the facade — that lets them run
//! without an `App` boot, and they catch the exact bug the audit
//! flagged: a store substituting its own default on `None`.

use std::sync::Arc;
use std::time::Duration;
use suprnova::cache::config::CacheConfig;
use suprnova::cache::store::CacheStore;
use suprnova::cache::{CacheDriver, InMemoryCache};

#[tokio::test]
async fn store_put_raw_none_ttl_means_no_expiration_in_memory() {
    // Construct a memory store with a 1-second configured default — the
    // store MUST NOT substitute this when `put_raw` is called with `None`.
    // (Pre-fix Redis did exactly that substitution; in-memory always
    // honoured None, so this test pins the contract uniformly.)
    let config = CacheConfig {
        driver: CacheDriver::Memory,
        url: "unused".into(),
        prefix: "test-forever:".into(),
        default_ttl: 1,
    };
    let store: Arc<dyn CacheStore> = Arc::new(InMemoryCache::with_config(&config));

    store.put_raw("k", "\"v\"", None).await.expect("put_raw");

    // After 1.2s — past the configured default — the key MUST still be
    // present. If the store substituted the default on None, this would
    // already have expired.
    tokio::time::sleep(Duration::from_millis(1_200)).await;

    let v = store.get_raw("k").await.expect("get_raw");
    assert_eq!(
        v.as_deref(),
        Some("\"v\""),
        "InMemoryCache::put_raw with None ttl must mean 'no expiration', \
         not 'apply the configured default'"
    );
}

#[tokio::test]
async fn store_exposes_default_ttl_for_facade_consumption() {
    // The facade reads `default_ttl()` to resolve `Cache::put(None)`.
    // Verify that the in-memory store returns what the config set,
    // converted to Duration. (Pre-fix the in-memory store didn't track
    // a default at all — the facade had no way to apply it uniformly.)
    let config = CacheConfig {
        driver: CacheDriver::Memory,
        url: "unused".into(),
        prefix: "test-default:".into(),
        default_ttl: 42,
    };
    let store: Arc<dyn CacheStore> = Arc::new(InMemoryCache::with_config(&config));
    assert_eq!(store.default_ttl(), Some(Duration::from_secs(42)));

    let zero_config = CacheConfig {
        default_ttl: 0,
        ..config
    };
    let zero_store: Arc<dyn CacheStore> = Arc::new(InMemoryCache::with_config(&zero_config));
    assert_eq!(
        zero_store.default_ttl(),
        None,
        "default_ttl=0 in config must mean None (no facade default)"
    );
}

#[tokio::test]
async fn store_put_raw_some_ttl_still_expires() {
    // Companion test: the bug-fix didn't break ordinary TTL semantics.
    let store: Arc<dyn CacheStore> = Arc::new(InMemoryCache::with_prefix("test-some:"));

    store
        .put_raw("k", "\"v\"", Some(Duration::from_millis(100)))
        .await
        .expect("put_raw");

    tokio::time::sleep(Duration::from_millis(200)).await;

    let v = store.get_raw("k").await.expect("get_raw");
    assert!(
        v.is_none(),
        "put_raw with Some(100ms) ttl must still expire after 200ms"
    );
}
