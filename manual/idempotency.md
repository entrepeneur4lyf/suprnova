# Idempotency

When a client retries a POST, you want the second call to be safe. The
network is unreliable and clients retry — but `POST /charges` should never
charge the card twice, and `POST /orders` should never produce two orders
for one click. Idempotency keys are the contract that says "if you see this
same key again, give me the original answer; don't redo the work."

Suprnova's `Idempotency` is a thin facade over `Cache::lock` that gives you
three escalating guarantees: dedupe-only, dedupe with retry-on-failure, and
Stripe-style result replay. All three keep the lock's lease alive for as
long as the body runs, so a slow body can never let the lock expire and a
duplicate slip past.

```rust
use std::time::Duration;
use suprnova::{Idempotency, Idempotent};

let outcome: Idempotent<OrderId> = Idempotency::once(
    "create-order:user-42:client-key-abc",
    Duration::from_secs(86_400),
    || async {
        // Runs exactly once per key within the 24-hour window.
        place_order(&user, &cart).await
    },
)
.await?;

match outcome {
    Idempotent::Fresh(id) => /* first call — id is the new order */ {},
    Idempotent::Duplicate => /* same key already used */ {},
}
```

## The three primitives

| Method | Body runs | Duplicate sees | Failure releases lock? | Use when |
|---|---|---|---|---|
| `Idempotency::once` | exactly once per window | `Duplicate` marker | no | side effects must NEVER repeat (mail sent, charge attempted) |
| `Idempotency::commit_on_success` | once per success per window | `Duplicate` marker | yes | transient failures should be retryable, but a success holds |
| `Idempotency::remember` | once per success per window | the original return value | yes | duplicates must receive the original payload, not a marker |

All three live under `suprnova::idempotency` and are re-exported from the
crate root as `Idempotency`, `Idempotent`, and `Replay`. They share the
same key-hashing, lease-renewal, and lock semantics — only the
success/failure policy differs.

### `Idempotency::once` — at-most-once

The strictest contract. The first caller in the TTL window runs the body
and gets `Fresh(value)`. Every subsequent caller within the window gets
`Duplicate` and the body does NOT run again — even if the first caller's
body returned `Err`. The TTL IS the dedupe window.

```rust
use std::time::Duration;
use suprnova::{Idempotency, Idempotent};

// Send a welcome email exactly once per signup, regardless of how many
// times the signup callback retries.
let result = Idempotency::once(
    &format!("welcome-mail:{}", user.id),
    Duration::from_secs(7 * 24 * 3600),
    || async {
        Mail::to(&user.email).send(WelcomeMail { user: user.clone() }).await
    },
)
.await?;
```

Reach for `once` when the side effect is the kind where "I tried; even
if I errored after the side effect, don't try again" — sending an email,
posting to an external API that doesn't honour idempotency keys of its
own, writing an audit log entry whose double-write would corrupt
downstream analytics.

### `Idempotency::commit_on_success` — at-least-once on success, retry on failure

Like `once`, but if the body returns `Err`, the dedupe lock is released so
the next caller within the TTL window can retry. A successful body keeps
the lock for the rest of the window.

```rust
use std::time::Duration;
use suprnova::{Idempotency, Idempotent};

let outcome = Idempotency::commit_on_success(
    &format!("publish-post:{}", post.id),
    Duration::from_secs(300),
    || async {
        // Posts a message to an upstream service. Network errors are
        // transient — the next retry should re-enter, not be told
        // "already done" when nothing actually happened.
        social_media_client.post(&post).await
    },
)
.await?;
```

Use `commit_on_success` when the body has retryable failure modes
(transient network errors, upstream rate limits, expired credentials
that a refresh would fix) and you want at-least-once on success but
the lock to surrender on a failure so a retry can re-enter.

### `Idempotency::remember` — Stripe-style result replay

The contract the HTTP `Idempotency-Key` header was invented for. The first
caller runs the body, stores the success value, and gets `Replay::Fresh`.
A later caller within the window gets `Replay::Replayed(<original value>)`
— the recorded return value, not a marker. A concurrent caller that
arrives *while* the first is still running gets `Replay::InProgress`.

```rust
use std::time::Duration;
use suprnova::{
    handler, Auth, FrameworkError, HttpResponse, Idempotency, Replay, Request, Response,
};

#[handler]
pub async fn create_charge(req: Request) -> Response {
    // Extract the header to an owned String before consuming `req` for the body.
    let key = req
        .header("Idempotency-Key")
        .ok_or_else(|| FrameworkError::bad_request("Idempotency-Key header required"))?
        .to_string();

    let user = Auth::user_as::<User>()
        .await?
        .ok_or_else(|| FrameworkError::unauthorized("login required"))?;

    let form: ChargeForm = req.json().await?;

    let outcome = Idempotency::remember(
        &format!("charge:{}:{}", user.id, key),
        Duration::from_secs(24 * 3600),
        || async {
            let charge = StripeClient::charge(&form).await?;
            Ok(ChargeResponse {
                id: charge.id,
                amount: charge.amount,
                status: charge.status,
            })
        },
    )
    .await?;

    match outcome {
        Replay::Fresh(body) | Replay::Replayed(body) => {
            let json = serde_json::to_value(&body)
                .map_err(|e| FrameworkError::internal(format!("serialize: {e}")))?;
            Ok(HttpResponse::json(json))
        }
        Replay::InProgress => Ok(HttpResponse::text("retry")
            .status(409)
            .header("Retry-After", "1")),
    }
}
```

Notice that `Fresh` and `Replayed` are handled identically by the
client-facing response — the whole point of `remember` is that the second
caller can't tell whether they were the one who ran the body or whether
they got the recorded result.

`InProgress` is the case worth thinking about: a duplicate arrived while
the first caller's body was still executing, so there's no recorded result
to hand back yet. `409 Conflict` with a `Retry-After: 1` header is the
canonical answer — the client backs off briefly, then retries, and the
second attempt either races the original to the `Cache::get` short-circuit
or hits `Replayed`.

## Key material

All three methods accept an arbitrary `&str` for the key. Before it
touches the cache backend, the key is SHA-256 hashed into a 64-character
hex digest. This buys you three things:

1. **Bounded backend key length.** A client that POSTs a 10 KB
   `Idempotency-Key` header still produces a 64-byte cache key.
2. **Raw identifiers don't leak into cache tooling.** If the key contains
   an email address, a session id, or an internal user id, those don't
   show up in `redis-cli KEYS idem:*`.
3. **No character-class collisions.** Whatever the cache backend
   interprets specially (colons, glob characters, control bytes) is
   already gone — the hash is hex-only.

The hash is over the user-provided key, not the cache key prefix —
`Idempotency::once("k", …)` and `Idempotency::once("k", …)` from two
different call sites in the same process collide on purpose. Namespace
your keys yourself if you don't want that:

```rust
Idempotency::once(
    &format!("billing:charge:{}:{}", tenant_id, client_key),
    Duration::from_secs(86_400),
    || async { /* … */ },
)
.await?;
```

## Lease renewal — the slow-body problem

A naive lock + TTL combination has a window bug: if the body runs longer
than the TTL, the lock expires while the body is still running, and a
second caller can acquire a fresh lock and run the body again
concurrently. The dedupe contract breaks for exactly the operations
slow enough to need it.

Suprnova solves this by spawning a background task that refreshes the
lock at one-third of the TTL (floored at 50 ms) for the entire duration
of the body. If the refresh ever fails (the lock token was lost,
the backend is unreachable), the renewal task logs once and stops — but
the body itself is unaffected. A `tokio::select!` with `biased` ordering
guarantees the body branch is the only one that ever resolves the future.

The practical upshot: pick a TTL based on your dedupe window
(`how long should a duplicate request be deduped?`), not your
worst-case body duration. A 30-minute body with a 1-minute TTL is fine —
the lock will be refreshed about ninety times during the body's run.

A test that exercises this: a 200 ms TTL with a body that blocks for
500 ms, and a second caller arriving at 400 ms. Without renewal, the
second caller would re-execute the body. With renewal, it sees
`Duplicate`. The lock holds.

## Shared backend

Cross-process dedupe requires a cross-process cache. The in-memory backend
holds locks in a per-process `HashMap`, so two `cargo run` instances on
the same machine won't see each other's idempotency keys. Production
deployments where any of these matter — multiple app processes,
horizontal scaling, blue/green deploys with overlapping traffic windows —
must set `CACHE_DRIVER=redis` and provide a reachable `REDIS_URL`.

The bootstrap is fail-closed: if `CACHE_DRIVER=redis` and Redis is
unreachable, the app refuses to start rather than silently downgrading
to per-process memory. See [cache.md](cache.md) for the full cache
backend contract.

## Error handling

The body's `FrameworkError` propagates up through `Idempotency`
unchanged. A lock-acquisition failure (Redis is down mid-request, the
backend returns an error) propagates as a `FrameworkError` from the
cache layer — there is no silent fallback. The error type is the
framework's standard `FrameworkError`, so handlers can `?` it through
to their controller's error converter:

```rust
use std::time::Duration;
use suprnova::{handler, FrameworkError, HttpResponse, Idempotency, Replay, Response};

#[handler]
pub async fn handler(order_id: i64) -> Response {
    let outcome: Replay<MyDto> = Idempotency::remember(
        &format!("order:{order_id}"),
        Duration::from_secs(60),
        || async move {
            let row = MyRow::find(order_id)
                .await?
                .ok_or_else(|| FrameworkError::not_found("missing"))?;
            Ok(MyDto::from(row))
        },
    )
    .await?;

    match outcome {
        Replay::Fresh(dto) | Replay::Replayed(dto) => {
            let json = serde_json::to_value(&dto)
                .map_err(|e| FrameworkError::internal(format!("serialize: {e}")))?;
            Ok(HttpResponse::json(json))
        }
        Replay::InProgress => Ok(HttpResponse::text("retry")
            .status(409)
            .header("Retry-After", "1")),
    }
}
```

A release failure on the `Err` path of `commit_on_success` or `remember`
is **logged, never returned** — the body's error is the only error the
caller sees on that path. A failed release means the lock will hold
until the TTL lapses; a retry within the window will see `Duplicate`
or `InProgress` until then. Logs include the hashed key (never the raw
key material) so operators can correlate without leaking PII.

## Cancellation

If the caller drops the `Idempotency::remember` future before the body
completes, the body is cancelled like any other `tokio::select!` branch —
the lock is **not** released, and a duplicate arriving before the TTL
lapses sees `InProgress` (then, after the TTL, `Fresh` again). This is
the safe default: a half-finished body whose effects you don't know
about should not be presumed safe to retry. Wrap bodies that hold
unmanaged side effects in `tokio::spawn` and join the handle if you
need to make the body uncancellable.

## Queue integration

The queue layer uses `Idempotency::commit_on_success` internally to
implement `Queue::push_unique`. If you want a job to be enqueued at most
once per `Job::unique_for()` window per `Job::unique_id(&self)`, you
don't need to call `Idempotency::*` yourself:

```rust
use suprnova::{Job, Queue};

let was_pushed = Queue::push_unique(SendReceipt { order_id: 42 }).await?;
if was_pushed {
    // We won the race; the job is on the queue.
} else {
    // Another caller already enqueued this; treat as success.
}
```

See [queues.md](queues.md) for the full job-uniqueness contract.

## Payment webhook ingress

The payments webhook handler does NOT use `Idempotency::*`. Webhook
ingress has a stricter requirement — every event must be auditable, even
on first delivery, so the audit row is the source of truth and the
de-dupe key is the database `UNIQUE(provider, provider_event_id)`
constraint. `Idempotency::remember` would store the response payload in
the cache; the webhook handler stores the *full event envelope plus
processing outcome* in `payments_webhook_events`, which means an
operator can replay or re-process events offline by reading the table.

The two patterns are complementary. Use `Idempotency::*` for client-driven
keys with TTL-scoped dedupe; use a `UNIQUE`-indexed audit table for
provider-driven webhook ingress that needs auditability past the cache
TTL. See [payments.md](payments.md) for the webhook contract.

### Why Suprnova diverges

Laravel's `Cache::lock` is a primitive; the Stripe-style idempotency
contract (record the result, replay it, distinguish in-progress from
duplicate) is left as a userland recipe. Every Laravel project that
needs it ends up writing the same lock-and-cache dance, usually with
one of these three bugs:

1. **No lease renewal.** A body that outlives the TTL re-executes
   concurrently in a duplicate caller. The lock was there; it just
   expired at the wrong moment.
2. **Release on the success path.** Releasing the lock when the body
   succeeds opens a window between `body() -> Ok` and the next caller
   acquiring a fresh lock — the very window the dedupe was supposed to
   close.
3. **Raw keys in the cache backend.** Client-supplied `Idempotency-Key`
   headers go straight into Redis keys, leaking PII into operator
   tooling and producing unbounded key sizes.

Suprnova ships the recipe as a first-class primitive so every caller
gets the same lease renewal, the same fail-closed release semantics,
the same hashed-key safety. The three methods (`once`,
`commit_on_success`, `remember`) name the three policies you actually
have to choose between — pick the one that matches your body's failure
model and move on.

## Testing

`Idempotency` resolves its `CacheStore` through the container, so tests
that bind an `InMemoryCache` get a fresh, isolated cache per test:

```rust
use std::sync::Arc;
use std::time::Duration;
use suprnova::cache::InMemoryCache;
use suprnova::cache::store::CacheStore;
use suprnova::container::testing::TestContainer;
use suprnova::idempotency::{Idempotency, Replay};

#[tokio::test]
async fn duplicate_remember_replays_the_first_result() {
    let _guard = TestContainer::fake();
    let store: Arc<dyn CacheStore> = Arc::new(InMemoryCache::with_prefix("idem:"));
    TestContainer::bind::<dyn CacheStore>(store);

    let r1: Replay<i32> = Idempotency::remember(
        "k",
        Duration::from_secs(60),
        || async { Ok(7) },
    )
    .await
    .unwrap();
    assert_eq!(r1, Replay::Fresh(7));

    let r2: Replay<i32> = Idempotency::remember(
        "k",
        Duration::from_secs(60),
        || async { Ok(999) },
    )
    .await
    .unwrap();
    assert_eq!(r2, Replay::Replayed(7));
}
```

The framework's own `framework/tests/idempotency.rs` covers the
contract surface: duplicate suppression, TTL expiry, error-vs-success
release policy, lease renewal across body durations that outlive the
TTL, the `InProgress` race, and the case where the cache's
`release_lock` itself errors. Read those tests if you want to see the
exact behaviour you can rely on.

## Gotchas

- **`Idempotency::once` consumes the window on error.** A failing first
  caller still holds the lock until the TTL lapses. Use
  `commit_on_success` if you want retries within the window.
- **`Idempotency::remember` stores `T` in the cache backend.** The key
  is hashed, but the *payload* is serialized with serde and written to
  the backend. Don't put secrets in a replayed value that must not
  appear in your cache store.
- **Two processes need a shared cache.** In-memory dedupe is
  per-process. Cross-process correctness requires `CACHE_DRIVER=redis`
  (or another cross-process store).
- **TTLs under 150 ms are not lease-tested.** The renewal floor is
  50 ms, so a 100 ms TTL refreshes about every 50 ms — fine for the
  contract, but the framework's lease tests run at `ttl >= 1s`. Use
  realistic dedupe windows; an idempotency window measured in
  milliseconds usually means the contract isn't quite the right tool.
- **The body's cancellation does not release the lock.** A cancelled
  body leaves the lock holding until the TTL lapses. This is the
  fail-closed choice; arrange your timeouts so the cancellation
  matches what a duplicate caller should see.

## Next

- [cache.md](cache.md) — the underlying lock primitive and the
  `CACHE_DRIVER` selection.
- [queues.md](queues.md) — how `Queue::push_unique` builds on
  `Idempotency::commit_on_success` for job-level dedupe.
- [payments.md](payments.md) — webhook ingress that uses
  database-row idempotency instead of cache-keyed dedupe, and when to
  reach for which.
- [rate-limiting.md](rate-limiting.md) — adjacent middleware that uses
  the same `Cache` backend for sliding-window enforcement.
- [middleware.md](middleware.md) — how to factor idempotency-key
  extraction into a reusable middleware over your POST/PUT routes.
