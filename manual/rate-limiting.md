# Rate Limiting

Suprnova ships two complementary rate-limit surfaces:

| Surface | Use when... | Backend |
|---------|-------------|---------|
| `RateLimiterDriver` + `RateLimitMiddleware` | You want strict sliding-window enforcement against arbitrary storage (Redis ZSET, in-memory deque) | `dyn RateLimiterDriver` |
| `RateLimiter` + `ThrottleRequestsMiddleware` | You want Laravel-shape named limiters, `attempt()` workflow callbacks, or `X-RateLimit-*` response headers | `Cache` store (memory or Redis) |

The sliding-window driver is Suprnova's native shape — one slot per request, no separate timer key, atomic Lua eval on Redis. The Laravel facade is what migrated apps reach for and what the named-limiter / response-callback pattern requires. The two coexist by design, and a route can layer both.

## Sliding-window driver SPI

`RateLimiterDriver` is the storage SPI for the sliding-window algorithm. Each key tracks a deque of hit timestamps. On every `try_acquire`, entries older than `now - window` are evicted; if the remaining count is below `max_requests`, `now` is appended and the call accepts. Otherwise it rejects.

```rust
use std::sync::Arc;
use std::time::Duration;
use suprnova::rate_limit::memory::InMemoryRateLimiter;
use suprnova::rate_limit::{RateLimiterDriver, SlidingWindowConfig};

let limiter: Arc<dyn RateLimiterDriver> = Arc::new(InMemoryRateLimiter::new());
let cfg = SlidingWindowConfig {
    max_requests: 60,
    window: Duration::from_secs(60),
};
let ok = limiter.try_acquire("user:42", &cfg).await?;
if !ok {
    let wait = limiter.retry_after("user:42", &cfg).await?;
    // wait is the Option<Duration> until the oldest slot in the bucket
    // ages out.
}
```

### Built-in drivers

| Driver | Storage | Selected via |
|--------|---------|--------------|
| `InMemoryRateLimiter` | Per-process `HashMap<String, Bucket>` with `tokio::time::Instant` so `start_paused` tests can drive the clock | `RATE_LIMIT_DRIVER=memory` (default) |
| `RedisRateLimiter` | Redis ZSET + Lua atomic check-and-record | `RATE_LIMIT_DRIVER=redis` + `RATE_LIMIT_REDIS_URL` |

`bootstrap_from_env()` wires the matching driver into the container. An unknown driver value falls back to memory with a `warn!` log.

### `RateLimitMiddleware`

The HTTP wrapper around the driver. Construct with a `key_fn` closure to drive bucket selection per-request:

```rust
use std::sync::Arc;
use std::time::Duration;
use suprnova::container::App;
use suprnova::rate_limit::{
    BackendErrorPolicy, RateLimitMiddleware, RateLimiterDriver, SlidingWindowConfig,
};

let limiter: Arc<dyn RateLimiterDriver> =
    App::resolve_make::<dyn RateLimiterDriver>().unwrap();

let mw = RateLimitMiddleware::new(
    limiter,
    SlidingWindowConfig {
        max_requests: 100,
        window: Duration::from_secs(60),
    },
    |req| format!("route:{}", req.path()),
)
.on_backend_error(BackendErrorPolicy::FailClosed);
```

On rejection (over quota) it returns HTTP 429 with a `Retry-After` header.

### Backend-error policy

`BackendErrorPolicy` governs what happens when the limiter *backend* itself errors — e.g. Redis is unreachable — as distinct from a request legitimately exceeding its quota. The backend cannot make a decision, so the middleware must choose between availability and the limit's guarantee.

| Policy | Behaviour | When to use |
|--------|-----------|-------------|
| `FailOpen` (default) | Pass the request through; log at `warn` | Most public APIs — a limiter outage should not take down traffic |
| `FailClosed` | Reject with HTTP 503 + `Retry-After: 1`; log at `error` | Sensitive routes (login, password reset, payments) where unbounded traffic during a backend outage is worse than briefly rejecting |

Choose with `.on_backend_error(BackendErrorPolicy::FailClosed)` on the middleware. Quota-exhausted requests are always 429 regardless of the policy — the policy only affects backend-error fallthrough.

## Cache-backed Laravel-shape facade

`RateLimiter` (the struct) mirrors `Illuminate\Cache\RateLimiter`. It's a fixed-window counter built on top of the Suprnova [`Cache`](cache.md) facade. Use it for named limiters, `attempt()` workflows, or any time you want the `X-RateLimit-*` headers Laravel apps expect.

### Storage layout

For an attempt counter key `K` with decay of `D` seconds:

- `K` — i64 counter incremented by every `hit`. Initial seed is 0 (via `Cache::add`).
- `K:timer` — i64 unix-seconds-since-epoch when the window ends, set via `Cache::add` so only the first caller in a window pins the deadline.

Both keys carry the same TTL so the cache cleans them up automatically when the window ends. When the counter has reached `max_attempts` but the `:timer` is gone, `too_many_attempts` resets the counter — this is what makes the window slide forward after a quota-exhausted period.

### Counter API

```rust
use suprnova::RateLimiter;

// Burn one attempt; seeds the window if missing.
let n = RateLimiter::hit("login:1.2.3.4", 60).await?;

// Increment by N; useful for "cost-weighted" limits (each request burns
// more than one attempt).
let n = RateLimiter::increment("api:user:1", 60, 5).await?;

// Read the current count (0 when never hit or expired).
let attempts = RateLimiter::attempts("login:1.2.3.4").await?;

// Number of seconds until the window reopens (0 when no window open).
let secs = RateLimiter::available_in("login:1.2.3.4").await?;

// Retries left before tripping.
let remaining = RateLimiter::remaining("login:1.2.3.4", 5).await?;
// retries_left is the Laravel-spelt alias of remaining.
let remaining = RateLimiter::retries_left("login:1.2.3.4", 5).await?;

// Is the bucket over its limit RIGHT NOW (with window still open)?
let over = RateLimiter::too_many_attempts("login:1.2.3.4", 5).await?;

// Drop only the counter (timer stays — the window is still pinned).
RateLimiter::reset_attempts("login:1.2.3.4").await?;

// Drop both counter and timer.
RateLimiter::clear("login:1.2.3.4").await?;
```

### `attempt()` workflow

Run a callback only when the bucket is under quota; the hit is only burned when the callback runs:

```rust
let result = RateLimiter::attempt(
    "login:1.2.3.4",
    5,
    || async { do_login_work().await },
    60,
).await?;
match result {
    Some(value) => { /* callback ran, attempt counted */ }
    None => { /* over limit, callback was NOT run */ }
}
```

This is the right shape for login forms — you don't burn an attempt unless the work actually reached the callback.

### Named limiters

Register at boot, resolve at request time. The Laravel-side name `for` is a Rust reserved keyword, so the primary Rust-side name is `define`; the literal Laravel alias is exposed via `r#for`.

```rust
use suprnova::{Limit, RateLimiter};

// At boot — `define` is the primary Rust-side name.
RateLimiter::define("api", |req| {
    let key = req.header("x-forwarded-for").unwrap_or("anon");
    Limit::per_minute(60).by(format!("ip:{key}")).into()
});

// Laravel-side alias — same thing under the keyword-escape spelling.
RateLimiter::r#for("uploads", |_req| Limit::per_hour(100).into());

// Resolve.
let cb = RateLimiter::limiter("api").unwrap();
let limit_result = cb(&request);
```

A named-limiter callback returns a [`LimitResult`], constructible from:

- A single `Limit` — apply this limit.
- A `Vec<Limit>` — apply every limit; first to trip wins.
- An `HttpResponse` — short-circuit immediately with this response (used for "admin gets unlimited access" via `Limit::none()`, or to refuse the request outright).

### Sanitising keys

`RateLimiter::clean_rate_limiter_key(key)` strips `&abc;` HTML-entity markers from a key — Laravel uses this for user-supplied strings that round-trip through `htmlentities`. Suprnova reproduces the strip stage exactly but does NOT prepend the `htmlentities` encoding (which only matters for non-UTF-8 inputs, irrelevant for Rust `String`). The function is deterministic and idempotent inside Suprnova; consumers who need byte-identical hashing with a PHP service should run their own `htmlentities` pre-step on the input.

```rust
assert_eq!(RateLimiter::clean_rate_limiter_key("a&amp;b"), "aab");
```

## `Limit` builder

The data type returned by named-limiter callbacks. Shorthand constructors mirror Laravel's `Limit::per*`:

```rust
use suprnova::Limit;
use std::time::Duration;

Limit::per_second(10, 1);           // 10 per 1 second (max_attempts, decay_seconds)
Limit::per_minute(60);              // 60 per minute
Limit::per_minutes(5, 100);         // 100 per 5 minutes (decay-first, Laravel signature)
Limit::per_hour(1_000);             // 1000/hr
Limit::per_hours(6, 5_000);         // 5000 per 6 hours
Limit::per_day(10_000);             // 10000/day
Limit::per_days(7, 50_000);         // 50000 per 7 days
Limit::new(123, Duration::from_secs(45));  // bare ctor

// Builder chain.
let l = Limit::per_minute(5)
    .by("user:42")
    .response(|req| {
        suprnova::HttpResponse::text("blocked").status(429)
    })
    .after(|response| response.status_code() >= 400);
```

- `.by(key)` — set the bucket key. Empty key is "global" (every caller shares one bucket).
- `.response(callback)` — generate a custom response when the limit trips; the default is plain 429 "Too Many Attempts.".
- `.after(callback)` — only burn the attempt when `callback(response)` returns true. Canonical use: only count failed logins (`after(|r| r.status_code() >= 400)`).

`Limit::none()` returns an `Unlimited` (a `GlobalLimit` with `max_attempts = i64::MAX`). Returning it from a named limiter is the Laravel pattern for bypass. `GlobalLimit` itself is a thin wrapper around `Limit` with an empty key, kept for parity with `Illuminate\Cache\RateLimiting\GlobalLimit`.

## `ThrottleRequestsMiddleware`

HTTP wrapper around the Cache-backed facade. Mirrors `Illuminate\Routing\Middleware\ThrottleRequests`. Three constructors:

```rust
use suprnova::{Limit, ThrottleRequestsMiddleware};

// Named limiter — resolves at request time via RateLimiter::limiter(name).
ThrottleRequestsMiddleware::by_name("api");

// Inline max/decay/prefix — the literal Laravel `throttle:60,1` shape.
ThrottleRequestsMiddleware::with(60, 1, "myroute");

// Explicit list of Limits — first-to-trip wins; most Rust-idiomatic.
ThrottleRequestsMiddleware::with_limits(vec![
    Limit::per_hour(5_000).by("user:1"),
    Limit::per_minute(60).by("user:1"),
]);
```

Wire it into a route group:

```rust
use suprnova::{Limit, RateLimiter, Router, ThrottleRequestsMiddleware};

RateLimiter::define("api", |req| {
    Limit::per_minute(60)
        .by(req.header("x-forwarded-for").unwrap_or("anon"))
        .into()
});

let router = Router::new()
    .get("/api/items", list_items)
    .post("/api/items", create_item)
    .middleware(ThrottleRequestsMiddleware::by_name("api"));
```

### Response headers

Every wrapped response carries:

- `X-RateLimit-Limit` — the configured `max_attempts`.
- `X-RateLimit-Remaining` — retries left for this bucket.

429 responses additionally carry:

- `Retry-After` — seconds until the window reopens.
- `X-RateLimit-Reset` — unix-seconds-since-epoch when the bucket reopens.

This matches Laravel's `ThrottleRequests::getHeaders` shape exactly.

### Missing named limiter

When a route is wired to `by_name("X")` but no limiter under `X` has been registered, the middleware returns HTTP 503 with a body that names the missing limiter. Laravel throws `MissingRateLimiterException`; we surface it as an HTTP response so a misconfigured boot does not panic the worker thread.

### Driver-vs-facade composition

The two middlewares can coexist on a single router. Layer the sliding-window driver for low-level fairness, then the Cache-backed throttle for per-endpoint named limits:

```rust
let router = Router::new()
    .get("/api/items", list_items)
    .middleware(RateLimitMiddleware::new(limiter_driver, cfg, key_fn))
    .middleware(ThrottleRequestsMiddleware::by_name("api"));
```

## Configuration

The driver SPI is configured via environment variables; the Cache-backed facade is configured wherever your [`Cache`](cache.md) store is configured (memory or Redis).

| Variable | Used by | Default |
|----------|---------|---------|
| `RATE_LIMIT_DRIVER` | Driver SPI bootstrap | `memory` |
| `RATE_LIMIT_REDIS_URL` | Redis driver | `redis://127.0.0.1:6379` |
| `RATE_LIMIT_PREFIX` | Redis key prefix | `suprnova:` |
| `CACHE_DRIVER` / `REDIS_URL` / `CACHE_DEFAULT_TTL` / `REDIS_PREFIX` | Cache-backed `RateLimiter` facade (see [`Cache`](cache.md)) | various |

## Migration from Laravel

| Laravel | Suprnova |
|---------|----------|
| `RateLimiter::for('api', fn ($req) => Limit::perMinute(60))` | `RateLimiter::define("api", \|req\| Limit::per_minute(60).into())` or `RateLimiter::r#for(...)` |
| `RateLimiter::hit($key, $decay)` | `RateLimiter::hit(key, decay).await?` |
| `RateLimiter::tooManyAttempts($key, $max)` | `RateLimiter::too_many_attempts(key, max).await?` |
| `RateLimiter::availableIn($key)` | `RateLimiter::available_in(key).await?` |
| `RateLimiter::attempt($key, $max, $cb, $decay)` | `RateLimiter::attempt(key, max, \|\| async { ... }, decay).await?` |
| `RateLimiter::retriesLeft($key, $max)` | `RateLimiter::retries_left(key, max).await?` |
| `RateLimiter::cleanRateLimiterKey($key)` | `RateLimiter::clean_rate_limiter_key(key)` |
| `Limit::perMinute(60)->by($ip)->response(fn () => abort(429))` | `Limit::per_minute(60).by(ip).response(\|_\| HttpResponse::text("...").status(429))` |
| `Limit::perMinutes(3, 100)` | `Limit::per_minutes(3, 100)` |
| `Limit::none()` | `Limit::none()` |
| `throttle:api` middleware | `ThrottleRequestsMiddleware::by_name("api")` |
| `throttle:60,1` middleware | `ThrottleRequestsMiddleware::with(60, 1, "")` |
| `X-RateLimit-Limit/Remaining/Reset` + `Retry-After` headers | Same headers, same shape |

### Why Suprnova diverges

Laravel ships one shape: `Illuminate\Cache\RateLimiter` (Cache-backed fixed-window counter) with `Illuminate\Routing\Middleware\ThrottleRequests` as its HTTP wrapper. Suprnova ships both that shape *and* a native sliding-window driver SPI because two real questions need two real answers.

A Cache-backed counter is the right answer to "I have named limiters, response callbacks, after-callbacks for failed-login-only counting, and I want to be source-compatible with Laravel migrations." It's the wrong answer to "I need exact one-slot-per-request sliding-window enforcement against a Redis ZSET with atomic Lua eval and no separate timer key." That second question is what most Rust services hitting Tokio's concurrency limits actually have, so `RateLimiterDriver` + `RateLimitMiddleware` exist alongside, not behind a feature flag.

The backend-error policy is also a Suprnova addition. Laravel's middleware never surfaces a "the limiter is broken" decision because PHP's per-request lifecycle hides it — the next request gets a fresh process. A long-lived Tokio worker that loses Redis for ten seconds must decide what to do with the requests arriving during that window; `BackendErrorPolicy::FailOpen` (default) vs `FailClosed` is that decision exposed explicitly.

## Next

- [Middleware](middleware.md) — how middleware composes, runs, and short-circuits in the request chain
- [Cache](cache.md) — the store the Laravel-shape `RateLimiter` facade is built on
- [Configuration](configuration.md) — typed config for the cache and Redis backends
- [Auth Flows](auth-flows.md) — `LoginThrottleMiddleware` and the brute-force lockout pattern build on this surface
- [Error Model](error-model.md) — why `Result<HttpResponse, HttpResponse>` lets the middleware short-circuit cleanly
