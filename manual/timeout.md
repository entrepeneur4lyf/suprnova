# Request Timeouts

`TimeoutMiddleware` puts a hard deadline on every HTTP request. A slow
handler — a hung database query, an unresponsive upstream API, an
accidental infinite loop in some hot path — would otherwise hold a hyper
connection open until the client gave up or the OS killed the process.
The timeout middleware caps that wait, drops the in-flight handler, and
returns `503 Service Unavailable` so the operator sees the failure
instead of the application silently leaking connections.

Reach for it when you're building anything that talks to the public
internet, anything that fans out to third-party APIs, or anything where
"the database might be slow today" is a realistic Tuesday.

```rust
use suprnova::{global_middleware, TimeoutMiddleware};

pub async fn register() {
    // Every HTTP route gets a 30-second ceiling.
    global_middleware!(TimeoutMiddleware::default());
}
```

That single line gives the whole application the same default ceiling
Suprnova uses for its database connect timeout — pick once, apply
everywhere. Per-route overrides are one line each. The rest of this
chapter explains exactly what the deadline bounds, what it intentionally
doesn't, and how it interacts with the panic boundary, streaming
responses, and WebSockets.

## The middleware

`TimeoutMiddleware` lives at `suprnova::TimeoutMiddleware`. It exposes
three constructors and one accessor:

```rust
use std::time::Duration;
use suprnova::TimeoutMiddleware;

let default_30s = TimeoutMiddleware::default();
let custom      = TimeoutMiddleware::new(Duration::from_millis(2_500));
let whole_secs  = TimeoutMiddleware::seconds(5);

assert_eq!(default_30s.duration(), Duration::from_secs(30));
assert_eq!(custom.duration(),      Duration::from_millis(2_500));
assert_eq!(whole_secs.duration(),  Duration::from_secs(5));
```

`TimeoutMiddleware::default()` uses a 30-second deadline. That number is
not arbitrary — it matches `DB_CONNECT_TIMEOUT` (also 30s) so a request
blocked waiting for a brand-new database connection and a request blocked
inside the handler share one ceiling. If you raise one, raise the other.

`TimeoutMiddleware::seconds(n)` is shorthand for the common whole-seconds
case. `TimeoutMiddleware::new(Duration::…)` is the escape hatch when you
need millisecond precision (an internal health check that should never
take more than 200ms; a synthetic probe with a 50ms budget).

## Installing globally

A global timeout is the right starting point: it gives every route a
ceiling without anyone having to remember to add it. Install it in
`bootstrap.rs` alongside your other global middleware:

```rust
// src/bootstrap.rs
use suprnova::{global_middleware, CorsConfig, CorsMiddleware, DB, TimeoutMiddleware};
use crate::middleware::{LoggingMiddleware, RequestIdMiddleware};

pub async fn register() {
    DB::init().await.expect("database connect");

    // Run-order matters: request-id first (so timeout logs carry it),
    // then logging (so slow requests are still observed), then the
    // timeout itself.
    global_middleware!(RequestIdMiddleware);
    global_middleware!(LoggingMiddleware);
    global_middleware!(TimeoutMiddleware::default());

    global_middleware!(CorsMiddleware::new(
        CorsConfig::allow_origins(["https://app.example"]),
    ));
}
```

The order matters because global middleware wraps the rest of the chain
in registration order: `RequestIdMiddleware` runs first on the way in
and last on the way out, so the request id is in scope while the timeout
fires its `503`. Putting the timeout before logging would hide slow
requests that did eventually complete from the access log.

## Tightening per route

A 30-second global ceiling is generous on purpose — it's there to catch
runaway handlers, not to enforce SLAs. When a specific endpoint should
fail faster, attach a per-route timeout:

```rust
use suprnova::{Router, TimeoutMiddleware};

Router::new()
    // Public report endpoint: must respond in 5s or we'd rather 503
    // and let the client retry than block.
    .get("/report", controllers::report::show)
    .middleware(TimeoutMiddleware::seconds(5));
```

You can attach a tighter timeout to a route group too. This is the
typical shape for a public API where each request should be quick, while
the rest of the app keeps the 30-second default:

```rust
use suprnova::Router;
use suprnova::TimeoutMiddleware;

Router::new()
    .group("/api", |r| {
        r.get("/users",       controllers::api::users::index)
         .post("/users",      controllers::api::users::create)
         .get("/users/{id}",  controllers::api::users::show)
    })
    .middleware(TimeoutMiddleware::seconds(3));
```

### Global is a ceiling; per-route can only tighten

Global middleware runs **outside** route middleware. The chain wraps
inside-out:

```
Global timeout (30s) → Route timeout (3s) → handler
```

Both `tokio::time::timeout` futures are armed; the inner one fires
first because it has the shorter deadline. So a per-route timeout can
only make a route *stricter* than the global, never looser.

If a single endpoint legitimately needs to run *longer* than the global
default — a slow report, a large upload, a long-poll fallback — you
have two options:

1. Raise the global value. Simplest, but it relaxes the ceiling for
   every other route too.
2. Scope the global middleware to a route group that *excludes* the
   long endpoint, and attach a separate timeout (or none) to the slow
   route. This keeps the strict default everywhere else.

The second option is the right shape for one outlier; the first is
right when the whole class of work needs more room.

## What the deadline actually bounds

The deadline races the future returned by `next(request)`. That future
resolves the moment your handler returns its `HttpResponse` — not when
the body finishes streaming. That distinction is load-bearing:

- **Normal handlers** build their full body before returning, so the
  deadline effectively bounds total handler time. A handler that
  serialises a JSON list, renders an Inertia page, or assembles an HTML
  response holds the future until the work is done.
- **Streaming responses** (`HttpResponse::sse(...)`,
  `HttpResponse::stream_bytes(...)`) return *immediately* with a lazy
  body. The middleware chain has already completed by the time hyper
  starts pulling bytes off the stream, so the deadline never observes
  the body's lifetime. An SSE event stream can stay open for hours
  under a 30-second timeout, by design — see
  [Server-Sent Events](sse.md) for the streaming model.
- **WebSocket upgrades** are skipped explicitly. See the next section.

This is the behaviour you almost certainly want. If you wrapped a
long-lived SSE stream in a 30-second timeout, the framework would tear
the connection down mid-stream every 30 seconds and the feature would
be unusable.

## WebSocket carve-out

The middleware inspects the request before arming the deadline:

```rust
if is_websocket_upgrade(request.headers()) {
    return next(request).await;
}
```

Any request carrying `Upgrade: websocket` skips the timeout entirely.
The check is case-insensitive on the token value (`WebSocket`,
`websocket`, `WEBSOCKET` all match), and a bare `Connection: upgrade`
without `Upgrade: websocket` is *not* treated as a WS upgrade — that
flows through the timeout normally.

Today, WebSocket upgrades take a separate server path that doesn't run
global middleware at all, so this guard is defence in depth — it
keeps the timeout from ever bounding a long-lived bidirectional channel
the day that changes. See [WebSockets](websockets.md) for how upgrades
are dispatched and the lifetime of a connected socket.

## What happens at the deadline

When `tokio::time::timeout` elapses before the handler completes, the
middleware does three things, in order:

1. **Drops the in-flight handler future.** The future was being polled
   inside the `timeout` combinator; the combinator returns `Err(Elapsed)`
   and the future is dropped where it was last suspended.
2. **Logs a warning** with the route path and the timeout duration in
   milliseconds:

   ```
   WARN suprnova::timeout request exceeded its timeout; returning 503 Service Unavailable
       route=/report timeout_ms=5000
   ```

   The log is at `WARN` so it surfaces in operator dashboards by
   default, separate from `INFO` access logs of normal requests.
3. **Returns `503 Service Unavailable`** with a plain-text body:

   ```
   HTTP/1.1 503 Service Unavailable
   Content-Type: text/plain
   Content-Length: 42

   Service Unavailable: request timed out
   ```

The 503 is wrapped in `Err(HttpResponse::…)` so it short-circuits the
rest of the chain just like any other middleware-rejected request.
Outer middleware (logging, request-id, CORS) still runs its post-handler
side, so the response goes out with the correct headers.

### Why 503 and not 504

`504 Gateway Timeout` is the right code when *you* are the gateway and
an *upstream* timed out. `503 Service Unavailable` is the right code
when *this* service couldn't produce the response in time. The timeout
middleware is bounding *our own* handler, so it returns 503. If you
want a different shape — a JSON body, a different status, a
machine-readable code — wrap your own outer middleware around the
timeout and translate its 503 response.

## Cancel safety

When the deadline elapses, the handler future is **dropped** at its
current `.await` point. This is normal Tokio cancellation; the same
thing happens when a client closes the connection mid-request. Anything
held across the await boundary is released by its `Drop` impl:

- **Database transactions** roll back. A SeaORM `DatabaseTransaction`
  has a `Drop` impl that issues `ROLLBACK` on the underlying connection.
- **Mutex and RwLock guards** release. A standard library or
  `parking_lot` guard releases on drop; another waiter can take it
  immediately.
- **File handles** close. The OS-level descriptor is released when the
  `tokio::fs::File` is dropped.
- **Network connections** check back into the pool or close, depending
  on the pool's drop behaviour.

The result is that a timed-out handler leaves nothing dangling — the
operator sees the 503, the database sees the rollback, the next request
sees a clean pool.

### What is *not* cancelled

Anything you moved off the request with `tokio::spawn` is **detached**.
Spawned tasks live on the runtime, not the request future, so dropping
the request does not stop them. This matters when you wrote something
like this:

```rust
pub async fn webhook(req: Request) -> Response {
    let payload: WebhookPayload = req.body_json().await?;

    // Fire-and-forget background work. Survives the request timing out.
    tokio::spawn(async move {
        if let Err(e) = process_webhook(payload).await {
            tracing::error!("webhook processing failed: {e}");
        }
    });

    Ok(HttpResponse::no_content())
}
```

If the request times out *before* the `spawn` line runs, the spawn
never happens. If the request times out *after* the spawn, the
background task keeps running — it is not cancelled with the request.
That's almost always what you want for webhook-style work, but it does
mean cleanup after a long `.await` inside the handler is **not**
guaranteed to run:

```rust
pub async fn upload(req: Request) -> Response {
    let temp_path = save_to_temp(&req).await?;

    // If this is what times out, the cleanup below DOES NOT RUN.
    let processed = long_running_processing(&temp_path).await?;

    // Not guaranteed under a timeout.
    tokio::fs::remove_file(&temp_path).await?;

    Ok(HttpResponse::json(&processed)?)
}
```

The fix is to use RAII. Wrap the temporary file in a struct whose
`Drop` impl removes it; then the cleanup runs whether the handler
returns, returns an error, or is dropped mid-`.await` by the timeout.
This is the same discipline you'd apply for any cancellation source —
client disconnect, runtime shutdown, panic recovery.

## Interaction with the panic boundary

The Suprnova server wraps the entire middleware chain in
[`execute_chain_safely`](lifecycle.md), which uses
`AssertUnwindSafe(...).catch_unwind()` to translate panics into a sanitised
`500 Internal Server Error`. A timed-out request is **not** a panic —
the future is dropped cleanly — so the timeout's `503` goes out
without involving the panic boundary at all.

The two boundaries handle different failure modes:

| Failure | Boundary | Status | Body |
|---|---|---|---|
| Handler `.await` exceeds deadline | `TimeoutMiddleware` | `503` | `Service Unavailable: request timed out` |
| Handler panics (`.unwrap()` on `None`, etc.) | `execute_chain_safely` | `500` | `{"message": "Internal Server Error"}` |
| Handler returns `Err(HttpResponse)` | normal `Response` flow | whatever the handler set | whatever the handler set |

You don't have to pick — both boundaries are always installed. A handler
that panics *after* exceeding its timeout still produces a 503 (the
future was dropped before the panic could happen). A handler that
panics *before* exceeding its timeout produces a 500.

## Operational tuning

Three considerations when picking timeout values:

1. **Match your database connect timeout.** If `DB_CONNECT_TIMEOUT=30`
   (the default), a request timeout shorter than 30s will fire before
   a slow connect ever completes — the user sees `503` instead of the
   chance to recover. Either raise the connect timeout or accept that
   "30s" is the floor.
2. **Account for the slowest legitimate handler.** Look at a histogram
   of your `INFO`-level request durations. The p99 of the slow tail
   should sit comfortably below the timeout, with headroom for clock
   skew and event-loop jitter. A timeout that fires routinely on
   healthy traffic is a misconfiguration, not a feature.
3. **Per-route timeouts are observability.** Tightening
   `TimeoutMiddleware::seconds(3)` on `/api/*` turns a degraded API
   into a visible alert (logs full of WARN, 503s in the load balancer)
   instead of a creeping latency problem. Use them where you have an
   SLA and want a hard failure when you miss it.

The framework's own integration tests use durations in the millisecond
range (`TimeoutMiddleware::new(Duration::from_millis(50))`) to exercise
the deadline deterministically. Production deadlines are almost always
in whole seconds.

### Why Suprnova diverges

In a Laravel + PHP-FPM deployment, request timeouts live outside the
application: nginx's `proxy_read_timeout`, PHP-FPM's
`request_terminate_timeout`, the load balancer's idle timeout. The
PHP process is killed when the budget is exhausted, and any open
state — database connections, file handles — leaks until the next
request reuses the worker.

Suprnova bounds the request inside the application because it can. The
handler is a Tokio future, not a PHP process, so dropping it runs `Drop`
impls cleanly: transactions roll back, locks release, descriptors close,
the connection pool stays healthy. The 503 also goes out *as a real HTTP
response* — clients see a proper status code instead of an upstream
reset.

This is also why the middleware doesn't try to be a Tower
`Timeout` layer. Tower's layer is generic over any Tokio service and
returns `tower::timeout::error::Elapsed`, which callers then have to map
to an HTTP status. The Suprnova middleware knows it's wrapping an HTTP
request pipeline; it returns `503` directly, logs the offending route,
and respects the framework's WebSocket and streaming carve-outs without
the caller having to reason about them. The Tower layer is the right
primitive for a generic Tokio service; for an HTTP request, this is the
right shape.

## Next

- [Middleware](middleware.md) — the trait, the chain, global vs per-route registration, terminable hooks
- [Request Lifecycle](lifecycle.md) — where the timeout sits in the chain, and how `execute_chain_safely` handles panics
- [Server-Sent Events](sse.md) — the streaming response model the timeout intentionally doesn't bound
- [WebSockets](websockets.md) — the upgrade path that bypasses the timeout entirely
- [Errors](errors.md) — how 5xx responses are dispatched as `ErrorOccurred` events for observability
