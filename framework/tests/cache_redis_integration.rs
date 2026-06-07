//! Live-Redis integration tests for the cache module.
//!
//! These tests connect to a running Redis at `CACHE_REDIS_TEST_URL`
//! (default `redis://127.0.0.1:6379`). They are `#[ignore]`d so the
//! default `cargo test` run does not require a Redis. Run them with:
//!
//! ```sh
//! cargo test -p suprnova --test cache_redis_integration -- --ignored
//! ```
//!
//! Each test scopes itself to a unique prefix so concurrent runs and
//! prior failed runs do not see each other's keys.

use std::sync::Arc;
use std::time::Duration;
use suprnova::cache::store::CacheStore;
use suprnova::cache::{CacheConfig, RedisCache};

fn redis_url() -> String {
    std::env::var("CACHE_REDIS_TEST_URL")
        .or_else(|_| std::env::var("REDIS_URL"))
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string())
}

async fn fresh_store(prefix: &str) -> Arc<dyn CacheStore> {
    let cfg = CacheConfig {
        driver: suprnova::cache::CacheDriver::Redis,
        url: redis_url(),
        prefix: format!("{}{}:", prefix, uuid::Uuid::new_v4()),
        default_ttl: 0,
    };
    let cache = RedisCache::connect(&cfg)
        .await
        .expect("connect to test Redis (set CACHE_REDIS_TEST_URL if not on localhost)");
    Arc::new(cache)
}

#[tokio::test]
#[ignore = "requires Redis at CACHE_REDIS_TEST_URL or default localhost"]
async fn redis_put_with_subsecond_ttl_expires_correctly() {
    let s = fresh_store("sub-ttl").await;
    s.put_raw("k", "{\"v\":1}", Some(Duration::from_millis(80)))
        .await
        .unwrap();
    assert!(
        s.has("k").await.unwrap(),
        "value present immediately after put"
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !s.has("k").await.unwrap(),
        "sub-second TTL must be honoured (PX, not EX rounded to 0)"
    );
}

#[tokio::test]
#[ignore = "requires Redis at CACHE_REDIS_TEST_URL or default localhost"]
async fn redis_lock_subsecond_ttl_expires_and_releases() {
    let s = fresh_store("sub-lock").await;
    let alice = s
        .acquire_lock("printer", Duration::from_millis(50))
        .await
        .unwrap();
    assert!(alice.is_some(), "first acquire wins");

    let bob = s
        .acquire_lock("printer", Duration::from_millis(50))
        .await
        .unwrap();
    assert!(bob.is_none(), "contention");

    tokio::time::sleep(Duration::from_millis(120)).await;
    let carol = s
        .acquire_lock("printer", Duration::from_secs(5))
        .await
        .unwrap();
    assert!(
        carol.is_some(),
        "sub-second lock TTL must expire — EX-as-secs would have errored or rounded to 0"
    );
}

#[tokio::test]
#[ignore = "requires Redis at CACHE_REDIS_TEST_URL or default localhost"]
async fn redis_lock_refresh_subsecond_ttl_extends() {
    let s = fresh_store("sub-refresh").await;
    let alice = s
        .acquire_lock("k", Duration::from_millis(200))
        .await
        .unwrap()
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let refreshed = s
        .refresh_lock("k", &alice, Duration::from_millis(300))
        .await
        .unwrap();
    assert!(refreshed, "refresh succeeds with valid token");

    tokio::time::sleep(Duration::from_millis(150)).await;

    let bob = s
        .acquire_lock("k", Duration::from_millis(50))
        .await
        .unwrap();
    assert!(
        bob.is_none(),
        "PEXPIRE extended the lock — EXPIRE with sub-second TTL would have deleted the key"
    );

    s.release_lock("k", &alice).await.unwrap();
}

#[tokio::test]
#[ignore = "requires Redis at CACHE_REDIS_TEST_URL or default localhost"]
async fn redis_touch_subsecond_ttl_extends() {
    let s = fresh_store("sub-touch").await;
    s.put_raw("k", "v", Some(Duration::from_millis(80)))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(40)).await;

    let touched = s.touch("k", Duration::from_millis(300)).await.unwrap();
    assert!(touched, "touch returns true on extant key");

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        s.has("k").await.unwrap(),
        "PEXPIRE extended; EXPIRE 0 would have deleted the key"
    );
}

#[tokio::test]
#[ignore = "requires Redis at CACHE_REDIS_TEST_URL or default localhost"]
async fn redis_flush_uses_scan_and_clears_the_keyspace() {
    let s = fresh_store("scan-flush").await;
    for i in 0..50 {
        s.put_raw(&format!("k:{i}"), &format!("v{i}"), None)
            .await
            .unwrap();
    }
    for i in 0..50 {
        assert!(s.has(&format!("k:{i}")).await.unwrap());
    }
    s.flush().await.unwrap();
    for i in 0..50 {
        assert!(
            !s.has(&format!("k:{i}")).await.unwrap(),
            "flush via SCAN must remove every prefixed key"
        );
    }
}

#[tokio::test]
#[ignore = "requires Redis at CACHE_REDIS_TEST_URL or default localhost"]
async fn redis_tagged_writes_can_be_flushed_by_tag() {
    let s = fresh_store("redis-tags-1").await;
    s.tagged_put_raw(&["users"], "u:1", "{\"id\":1}", None)
        .await
        .unwrap();
    s.tagged_put_raw(&["users", "active"], "u:2", "{\"id\":2}", None)
        .await
        .unwrap();
    s.tagged_put_raw(&["posts"], "p:1", "{\"id\":1}", None)
        .await
        .unwrap();

    s.flush_tags(&["users"]).await.unwrap();

    assert!(!s.has("u:1").await.unwrap());
    assert!(!s.has("u:2").await.unwrap());
    assert!(s.has("p:1").await.unwrap(), "different tag untouched");
}

#[tokio::test]
#[ignore = "requires Redis at CACHE_REDIS_TEST_URL or default localhost"]
async fn redis_untagged_overwrite_after_tagged_survives_flush() {
    let s = fresh_store("redis-tags-2").await;
    s.tagged_put_raw(&["users"], "u:1", "v1", None)
        .await
        .unwrap();
    s.put_raw("u:1", "v2", None).await.unwrap();

    s.flush_tags(&["users"]).await.unwrap();

    assert!(
        s.has("u:1").await.unwrap(),
        "untagged overwrite cleared the tag aux set — flush_tags must skip it"
    );
    let got: Option<String> = s.get_raw("u:1").await.unwrap();
    assert_eq!(got.as_deref(), Some("v2"));
}

#[tokio::test]
#[ignore = "requires Redis at CACHE_REDIS_TEST_URL or default localhost"]
async fn redis_retagging_drops_old_membership() {
    let s = fresh_store("redis-tags-3").await;
    s.tagged_put_raw(&["a"], "k", "v1", None).await.unwrap();
    s.tagged_put_raw(&["b"], "k", "v2", None).await.unwrap();

    s.flush_tags(&["a"]).await.unwrap();
    assert!(
        s.has("k").await.unwrap(),
        "k re-tagged to b — flushing a must not delete it"
    );

    s.flush_tags(&["b"]).await.unwrap();
    assert!(!s.has("k").await.unwrap(), "flushing current tag deletes");
}

#[tokio::test]
#[ignore = "requires Redis at CACHE_REDIS_TEST_URL or default localhost"]
async fn redis_tagged_subsecond_ttl_expires() {
    let s = fresh_store("redis-tags-sub").await;
    s.tagged_put_raw(&["t"], "k", "v", Some(Duration::from_millis(80)))
        .await
        .unwrap();
    assert!(s.has("k").await.unwrap());
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        !s.has("k").await.unwrap(),
        "tagged_put_raw must use PX for sub-second TTL"
    );
}

#[tokio::test]
#[ignore = "requires Redis at CACHE_REDIS_TEST_URL or default localhost"]
async fn redis_add_with_subsecond_ttl_expires() {
    let s = fresh_store("redis-add-sub").await;
    let ok = s
        .add_raw("k", "v", Some(Duration::from_millis(80)))
        .await
        .unwrap();
    assert!(ok);

    // Contention with another add — must fail until the TTL expires.
    let busy = s
        .add_raw("k", "v2", Some(Duration::from_secs(5)))
        .await
        .unwrap();
    assert!(!busy, "contention while value is live");

    tokio::time::sleep(Duration::from_millis(150)).await;

    let free = s
        .add_raw("k", "v3", Some(Duration::from_secs(5)))
        .await
        .unwrap();
    assert!(free, "add succeeds after sub-second TTL expires");
    let v: Option<String> = s.get_raw("k").await.unwrap();
    assert_eq!(v.as_deref(), Some("v3"));
}

/// A regular `Cache::forget("lock:foo")` MUST NOT release a held
/// distributed lock for `foo`. Pre-isolation, the lock value lived at
/// `<prefix>lock:foo` and was indistinguishable from a user-side
/// `forget("lock:foo")` (which also produced `<prefix>lock:foo`).
#[tokio::test]
#[ignore = "requires Redis at CACHE_REDIS_TEST_URL or default localhost"]
async fn redis_forget_with_lock_prefixed_key_does_not_release_held_lock() {
    let s = fresh_store("redis-lock-iso-1").await;
    let token = s
        .acquire_lock("printer", Duration::from_secs(30))
        .await
        .unwrap()
        .expect("lock acquired");

    // User-side `forget("lock:printer")` must NOT touch the lock's
    // internal slot.
    let _ = s.forget("lock:printer").await.unwrap();

    assert!(
        s.acquire_lock("printer", Duration::from_secs(30))
            .await
            .unwrap()
            .is_none(),
        "lock keyspace must be isolated from user `forget(\"lock:...\")`"
    );
    assert!(s.release_lock("printer", &token).await.unwrap());
}

/// A user-side `put("lock:foo", ...)` MUST NOT overwrite a held
/// distributed lock for `foo`.
#[tokio::test]
#[ignore = "requires Redis at CACHE_REDIS_TEST_URL or default localhost"]
async fn redis_put_with_lock_prefixed_key_does_not_overwrite_held_lock() {
    let s = fresh_store("redis-lock-iso-2").await;
    let token = s
        .acquire_lock("job", Duration::from_secs(30))
        .await
        .unwrap()
        .expect("lock acquired");

    s.put_raw("lock:job", "hijacked-token", Some(Duration::from_secs(30)))
        .await
        .unwrap();

    assert!(
        s.acquire_lock("job", Duration::from_secs(30))
            .await
            .unwrap()
            .is_none(),
        "lock keyspace must be isolated from user `put(\"lock:...\")`"
    );
    assert!(s.release_lock("job", &token).await.unwrap());
}

/// A `Cache::forget("tag:users")` MUST NOT clobber the tag forward
/// index for `users`. Pre-isolation, the forward index lived at
/// `<prefix>tag:users` and could be deleted by a user-side
/// `forget("tag:users")`, breaking subsequent `flush_tags(["users"])`.
#[tokio::test]
#[ignore = "requires Redis at CACHE_REDIS_TEST_URL or default localhost"]
async fn redis_forget_with_tag_prefixed_key_does_not_clobber_tag_index() {
    let s = fresh_store("redis-tag-iso").await;
    s.tagged_put_raw(&["users"], "u:1", "{\"id\":1}", None)
        .await
        .unwrap();

    // User-side forget against the same prefix we used to store the
    // forward index — must miss because the internal index lives in
    // a NUL-byte-prefixed slot the user cannot reach.
    let _ = s.forget("tag:users").await.unwrap();

    s.flush_tags(&["users"]).await.unwrap();
    assert!(
        !s.has("u:1").await.unwrap(),
        "flush_tags must still find and delete tagged keys"
    );
}
