# Cache

Suprnova ships a Laravel-shape `Cache` facade backed by one of two
drivers — in-memory or Redis — picked explicitly at boot via
`CACHE_DRIVER`. The facade is a thin layer over a `CacheStore` trait, so
custom backends plug in the same way the built-ins do.

## The facade

```rust
use suprnova::Cache;
use std::time::Duration;

Cache::put("user:1", &user, Some(Duration::from_secs(3600))).await?;

let cached: Option<User> = Cache::get("user:1").await?;

if Cache::has("user:1").await? {
    // hit
}

Cache::forget("user:1").await?;
```

Every method serialises through `serde_json` at the facade boundary, so
any `T: Serialize + DeserializeOwned` round-trips. The trait under the
facade (`CacheStore`) only sees opaque JSON strings.

## Bootstrap

The cache is bound during `Server::run()`'s driver-bootstrap step (see
[Request Lifecycle](lifecycle.md)). `Cache::bootstrap` reads the
configured `CacheConfig` (or constructs one from env) and dispatches on
`CacheConfig::driver`:

- `Memory` — bind an `InMemoryCache` with the configured prefix and
  default TTL. Always succeeds.
- `Redis` — connect to `REDIS_URL` and bind the resulting `RedisCache`.
  **Fails closed** if the URL is unreachable. There is no silent
  downgrade to memory.

Workers (`queue:work`, `schedule:run`, `workflow:work`) go through the
same bootstrap, so a job using `Cache::get` sees the same backend the
HTTP handler does.

### Why Suprnova diverges

Laravel's `cache.php` config picks a default store and Laravel will
quietly swap to `array` (in-process) when a misconfigured backend fails
in some code paths. That's a productive default for `php artisan tinker`
and a footgun in production — a single Redis miss silently changes the
guarantees of every tag flush and lock acquisition in the app.

Suprnova picks the opposite default. `CACHE_DRIVER=memory` is explicit
(and the default for `cargo run`), and `CACHE_DRIVER=redis` against an
unreachable Redis returns an error from `Server::from_config`. The
binary exits non-zero with a remediation message; supervisord/systemd
sees a boot failure instead of a half-working app.

## Configuration

| Env | Meaning | Default |
|---|---|---|
| `CACHE_DRIVER` | `memory` or `redis` | `memory` |
| `REDIS_URL` | Redis URL (consulted only when `driver=redis`) | `redis://127.0.0.1:6379` |
| `REDIS_PREFIX` | Key prefix applied to every store operation | `suprnova_cache:` |
| `CACHE_DEFAULT_TTL` | Default TTL in seconds for `Cache::put(None)`; `0` means no default | `3600` |

Unset `CACHE_DRIVER` parses to `Memory`; any other value (case-
insensitive, trimmed) that isn't `memory`/`in-memory`/`inmemory`/`redis`
returns an error at boot.

You can also build the config programmatically when you don't want env
parsing:

```rust
use suprnova::{Config, CacheConfig, cache::CacheDriver};

Config::register(
    CacheConfig::builder()
        .driver(CacheDriver::Redis)
        .url("redis://cache.internal:6379")
        .prefix("myapp:")
        .default_ttl(7200)
        .build(),
);
```

`CacheConfigBuilder::build` is deterministic — unset fields fall back
to `CacheConfig::default()` rather than re-reading env.

### The `forever` contract holds across backends

`Cache::forever` and `Cache::remember_forever` bypass
`CACHE_DEFAULT_TTL` entirely; the value never expires regardless of the
configured default. `Cache::put(key, value, None)` does apply the
default — that's the point of having one.

The default-TTL resolution happens at the facade layer. Both `CacheStore`
backends honour `None` literally at the store boundary (no expiration),
which is why `forever` actually means forever on both memory and Redis.

## Reads, writes, deletes

```rust
use suprnova::Cache;
use std::time::Duration;

// Write with an explicit TTL
Cache::put("session:42", &session, Some(Duration::from_secs(1800))).await?;

// Write forever — bypasses CACHE_DEFAULT_TTL
Cache::forever("config:features", &features).await?;

// Read (None on miss or expired)
let session: Option<Session> = Cache::get("session:42").await?;

// Existence — true means present and not expired
if Cache::has("session:42").await? { /* … */ }

// Laravel-spelled negation
if Cache::missing("session:42").await? { /* warm */ }

// Read-and-delete in one call
let one_shot: Option<String> = Cache::pull("notice:welcome:42").await?;

// Returns true if the key existed and was removed
Cache::forget("session:42").await?;

// Wipe everything (prefix-scoped on both backends)
Cache::flush().await?;
```

`Cache::pull` is **not** atomic — it's a `get` followed by a `forget`,
same shape as Laravel's `Repository::pull`. For atomic dequeue use
`Cache::lock` (see below).

### Refresh a TTL without rewriting

```rust
let refreshed = Cache::touch("session:42", Duration::from_secs(1800)).await?;
```

`touch` returns `true` if the key existed and the TTL was extended,
`false` otherwise. The stored value is untouched.

## Add — write-if-absent (atomic)

```rust
let won = Cache::add(
    "daily:winner",
    &user_id,
    Some(Duration::from_secs(86_400)),
).await?;
if won {
    send_winner_email(user_id).await?;
}
```

`Cache::add` writes only if the key is empty (or has expired). Returns
`true` on write, `false` on contention. **Atomic** on both built-in
backends:

- `InMemoryCache` holds a write-lock across the existence check + insert
- `RedisCache` uses `SET key value NX EX ttl` (or `NX` without `EX`)

Custom `CacheStore` implementations that don't override `add_raw` fall
back to a non-atomic check-then-put, matching Laravel's
`Repository::add` fallback for stores without a native `add`.

## Remember — get-or-compute

```rust
let user = Cache::remember(
    "user:1",
    Some(Duration::from_secs(3600)),
    || async { User::find(1).await },
).await?;

let cfg = Cache::remember_forever("config:app", || async {
    load_config_from_db().await
}).await?;
```

`remember` calls your closure only on miss, then stores the result. The
closure returns `Result<T, FrameworkError>`, so domain failures bubble
through `?` rather than poisoning the cache.

`Cache::sear(key, default)` is the Laravel-spelled alias for
`remember_forever`. Same body, same semantics — ships under both names
so migrated code reads the same way.

### Remember is NOT stampede-safe

`remember` is a non-atomic `get`-then-`put` pair. N concurrent misses
for the same cold key run the closure N times and write N results. That
matches Laravel's `Repository::remember` exactly, and it's fine for the
common case (the closure is idempotent, the writes are identical).

It is not fine when:

- The closure is expensive (1s+ to compute or hits a slow upstream)
- The key is popular enough that a cold-cache event sends N requests at
  once at the backing store
- The closure has side effects beyond computing the value

For those, wrap with `Cache::lock`:

```rust
use suprnova::Cache;
use std::time::Duration;

let key = "rebuild:user:1";

if let Some(guard) = Cache::lock(key, Duration::from_secs(10)).await? {
    let user = Cache::remember(
        "user:1",
        Some(Duration::from_secs(3600)),
        || async { User::find(1).await },
    ).await?;
    guard.release().await?;
    return Ok(user);
}

// Lost the race — the winner is computing. Read whatever they wrote,
// or fall back to a stale value.
let user = Cache::get::<User>("user:1").await?
    .ok_or_else(|| FrameworkError::internal("cache miss after losing rebuild lock"))?;
```

## Locks

`Cache::lock` returns a `LockGuard` holding the ownership token. Locks
are advisory and cross-process when backed by Redis.

```rust
use suprnova::Cache;
use std::time::Duration;

if let Some(guard) = Cache::lock("job:42", Duration::from_secs(30)).await? {
    do_exclusive_work().await?;
    guard.release().await?;
}
// Some(guard) means we own it. None means another holder beat us.
```

The guard exposes:

| Method | Use for |
|---|---|
| `guard.token()` | Read the ownership token (Rust-side name) |
| `guard.owner()` | Same value, Laravel-spelled alias |
| `guard.refresh(ttl)` | Extend the TTL — returns `false` if we no longer own the lock |
| `guard.release()` | Release if we still own the lock — returns `false` if the token no longer matches |

There is intentionally **no `Drop` auto-release**. A Redis lock must be
acknowledged across process boundaries; auto-release on drop would
either silently steal a stolen lock back (wrong) or hide release
failures in destructor panics (worse). The release is explicit so
errors propagate.

`refresh` lets a long-running job extend its own lock to avoid a
self-inflicted timeout — see [Idempotency](idempotency.md) for the
in-tree consumer.

## Atomic counters

```rust
// Initialises to 0 if absent, then increments. Returns the new value.
let visits = Cache::increment("page:visits", 1).await?;

// Same shape for negative steps
let remaining = Cache::decrement("quota:remaining", 1).await?;

// Custom amount
let total = Cache::increment("stats:downloads", 10).await?;
```

Atomic on both built-in backends: `InMemoryCache` uses a write-locked
`HashMap::entry`; `RedisCache` uses `INCRBY`/`DECRBY`. The stored value
is a JSON-encoded integer, so `Cache::get::<i64>("page:visits")` round-
trips with the same key.

## Tagged cache

Tags let you invalidate a whole family of related entries with one
call. The classic use case is per-resource caches that have to flush
together when the resource changes.

```rust
use suprnova::Cache;
use std::time::Duration;

// Store under one or more tags
Cache::tags_put(
    &["users", "user:1"],
    "user:1:profile",
    &profile,
    Some(Duration::from_secs(3600)),
).await?;

Cache::tags_put(
    &["users", "user:1"],
    "user:1:posts",
    &posts,
    Some(Duration::from_secs(600)),
).await?;

// Update path: drop every key tagged `user:1`
Cache::flush_tags(&["user:1"]).await?;
```

Tag membership is **per-entry**: each tagged write installs that
write's tag set as the entry's source of truth, replacing any prior
tags. Two consequences worth knowing:

- An untagged `Cache::put` over a previously tagged key **clears** the
  entry's tags. A subsequent `flush_tags` of the old tag will not
  delete the live untagged value.
- Overwriting `tags_put(&["a"], …)` with `tags_put(&["b"], …)` makes
  the entry respond only to `flush_tags(&["b"])`.

Stale forward-index references are pruned during the flush walk and on
`flush()`, so they don't accumulate indefinitely for tags that are
written but never flushed.

## Two backends

| Feature | `InMemoryCache` | `RedisCache` |
|---|---|---|
| Shared across processes | No | Yes |
| Persistence | No | Yes, if Redis is configured for it |
| Atomic `add` | Yes (write-lock) | Yes (`SET NX`) |
| Atomic `increment`/`decrement` | Yes (write-lock) | Yes (`INCRBY`/`DECRBY`) |
| Tagged cache | Yes | Yes |
| Locks | Yes | Yes (cross-process) |
| Sub-second TTL | Yes (`tokio::time::Instant`) | Yes (`PX`/`PEXPIRE`) |
| Selected via | `CACHE_DRIVER=memory` (default) | `CACHE_DRIVER=redis` |

There is no Database cache driver — the two backends above are the
ones the framework ships. Custom backends can implement `CacheStore`
and bind into the container directly; see the test-injection pattern
below.

### In-memory expiration

`InMemoryCache` evicts expired entries **lazily on read**: `get_raw`,
`has`, and `add_raw` purge an entry the first time they observe it
expired. Re-accessed keys never accumulate corpses.

A workload that writes a high-cardinality set of short-lived keys and
never reads them back has no such trigger. Call
`InMemoryCache::purge_expired()` from a periodic task in that case —
it returns the count of entries removed. Redis handles its own
expiration server-side; the equivalent isn't needed there.

### Redis TTL precision

Every Redis TTL goes through `PX` / `PEXPIRE`, not `EX` / `EXPIRE`.
That avoids two pitfalls:

- Sub-second `Duration`s would truncate to `0 seconds` under `EX`,
  which Redis rejects (`SET … EX 0`) or, worse, interprets as
  "delete the key" (`EXPIRE key 0`).
- `Duration::ZERO` is clamped to 1 ms before the call, so neither
  rejection path is reachable from user code.

## Testing

Bind an `InMemoryCache` into the `TestContainer` and the facade
resolves it like any other store:

```rust
use std::sync::Arc;
use suprnova::{Cache, CacheStore, InMemoryCache};
use suprnova::container::testing::TestContainer;

#[tokio::test]
async fn cache_round_trips() {
    let _guard = TestContainer::fake();
    TestContainer::bind::<dyn CacheStore>(Arc::new(InMemoryCache::new()));

    Cache::put("k", &"v", None).await.unwrap();

    let v: Option<String> = Cache::get("k").await.unwrap();
    assert_eq!(v.as_deref(), Some("v"));
}
```

`TestContainer::bind` writes to the thread-local scope, so parallel
tests do not leak cache state into each other. See the
[Service Container](container.md) chapter for the three-layer lookup
model.

## Patterns

A few recurring shapes worth naming:

```rust
// Hierarchical, colon-separated keys — same convention Laravel uses
Cache::put("users:1:profile", &profile, None).await?;
Cache::put("posts:123:comments:count", &count, None).await?;

// TTL by data volatility
Cache::put("stats:active", &count, Some(Duration::from_secs(60))).await?;
Cache::put("config:features", &features, Some(Duration::from_secs(3600))).await?;
Cache::forever("translations:en", &translations).await?;

// Cache-by-tag invalidation around a write
async fn update_user(id: i64, data: UserUpdate) -> Result<User, FrameworkError> {
    let user = User::update(id, data).await?;
    Cache::flush_tags(&[&format!("user:{}", id)]).await?;
    Ok(user)
}
```

## Next

- [Configuration](configuration.md) — how `Config::register` and env
  vars combine
- [Rate Limiting](rate-limiting.md) — the Laravel-shape `RateLimiter`
  facade is built on top of `Cache`
- [Idempotency](idempotency.md) — the request-dedupe middleware uses
  `Cache::lock` end-to-end
- [Service Container](container.md) — how `CacheStore` is bound and
  resolved
- [Error Model](error-model.md) — what `Cache::*` returns when Redis
  is unreachable mid-request
