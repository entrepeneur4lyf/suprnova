//! Regression: HIGH audit finding `cache` #251 — production Redis
//! failures silently downgrade the app to per-process memory cache.
//!
//! The fix makes `CACHE_DRIVER` an explicit knob (defaulting to
//! `memory`) and makes `Cache::bootstrap` fail closed when the
//! configured Redis is unreachable. These tests exercise the
//! `CacheConfig` + `CacheDriver` parsing path that drives bootstrap;
//! the bootstrap dispatcher itself is exercised via the integration
//! suites that boot a server.

use suprnova::cache::{CacheConfig, CacheDriver};

#[test]
fn cache_driver_parses_known_names_case_insensitively() {
    assert_eq!(CacheDriver::parse("memory").unwrap(), CacheDriver::Memory);
    assert_eq!(CacheDriver::parse("MEMORY").unwrap(), CacheDriver::Memory);
    assert_eq!(CacheDriver::parse("In-Memory").unwrap(), CacheDriver::Memory);
    assert_eq!(CacheDriver::parse("inmemory").unwrap(), CacheDriver::Memory);
    assert_eq!(CacheDriver::parse("redis").unwrap(), CacheDriver::Redis);
    assert_eq!(CacheDriver::parse("REDIS").unwrap(), CacheDriver::Redis);
    assert_eq!(CacheDriver::parse(" redis \t").unwrap(), CacheDriver::Redis);
}

#[test]
fn cache_driver_rejects_unknown_with_descriptive_error() {
    let err = CacheDriver::parse("memcached").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("memcached"),
        "error should echo the bad value back; got {msg}"
    );
    assert!(
        msg.contains("memory") && msg.contains("redis"),
        "error should list the accepted values; got {msg}"
    );
}

#[test]
fn cache_driver_default_is_memory_not_redis() {
    // The pre-fix bootstrap tried Redis first and silently fell back
    // to memory. Post-fix, the DEFAULT is memory (no Redis attempt at
    // all unless the operator explicitly opts in via CACHE_DRIVER=redis).
    assert_eq!(CacheDriver::default(), CacheDriver::Memory);
}

#[test]
fn cache_config_default_uses_memory_driver() {
    let cfg = CacheConfig::default();
    assert_eq!(
        cfg.driver,
        CacheDriver::Memory,
        "production safety: an unconfigured CacheConfig must not silently \
         enable Redis"
    );
}

#[test]
fn cache_config_builder_threads_driver_through() {
    let cfg = CacheConfig::builder()
        .driver(CacheDriver::Redis)
        .url("redis://example.test:6379")
        .prefix("app:")
        .default_ttl(60)
        .build();
    assert_eq!(cfg.driver, CacheDriver::Redis);
    assert_eq!(cfg.url, "redis://example.test:6379");
    assert_eq!(cfg.prefix, "app:");
    assert_eq!(cfg.default_ttl, 60);
}

/// Integration regression for HIGH #251: `Cache::bootstrap` must fail
/// closed when `CACHE_DRIVER=redis` is requested but the Redis URL is
/// unreachable. The previous behaviour was to silently install
/// `InMemoryCache` and pretend everything was fine.
///
/// We exercise this via the `RedisCache::connect` path directly — it's
/// what `Cache::bootstrap` calls, and it lets us check the error
/// without touching the global `App` (which would taint other tests).
#[tokio::test]
async fn redis_connect_fails_loudly_when_url_is_unreachable() {
    let cfg = CacheConfig::builder()
        .driver(CacheDriver::Redis)
        // 127.0.0.1:1 — well-known unbound port; connect must fail
        // promptly rather than hanging.
        .url("redis://127.0.0.1:1/0")
        .prefix("test-bootstrap:")
        .default_ttl(0)
        .build();

    let res = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        suprnova::cache::RedisCache::connect(&cfg),
    )
    .await
    .expect("connect must not hang past 10 s");

    assert!(
        res.is_err(),
        "RedisCache::connect against an unreachable URL must Err — \
         bootstrap relies on this Err to fail closed (HIGH #251)"
    );
}
