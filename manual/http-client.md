# HTTP Client

The `Http` facade is the outbound side of HTTP — the Rust equivalent of
Laravel's `Http::` helper. You reach for it when your handler, job, or
scheduled task needs to call somebody else's API: a payment gateway, a
geocoder, a webhook target, a Slack message. Fluent builder, JSON in
and out, retries with jitter, deterministic test fakes that record what
you sent. The same surface you used in Laravel, with task-local
isolation so parallel tests don't see each other's fakes.

```rust
use suprnova::Http;
use serde_json::json;

let resp = Http::post("https://api.stripe.com/v1/charges")
    .bearer_token(secret_key)
    .json(&json!({ "amount": 1000, "currency": "usd" }))
    .send()
    .await?;

let body: serde_json::Value = resp.json().await?;
```

That's the shape: `Http::<verb>(url)` returns a `RequestBuilder`; you
chain configuration onto it; `.send().await` returns a
`ClientResponse`. The backing client is one shared `reqwest::Client`
with rustls TLS, a 30s default timeout, and a `suprnova/<version>` user
agent — built lazily on first call.

## The verbs

```rust
Http::get("https://api.example.com/users/42")
Http::post("https://api.example.com/users")
Http::put("https://api.example.com/users/42")
Http::patch("https://api.example.com/users/42")
Http::delete("https://api.example.com/users/42")
```

Every verb returns a `RequestBuilder`. The URL can be any
`impl Into<String>` — a `&str`, a `String`, or a `Cow<str>`. No
URL-building helpers ship in the facade; format the URL yourself or
reach for a query-string crate.

## Bodies

Three ways to attach a body. Each one replaces any previously-set body.

### JSON

```rust
use serde::Serialize;

#[derive(Serialize)]
struct CreateUser {
    name: String,
    email: String,
}

Http::post("https://api.example.com/users")
    .json(&CreateUser {
        name: "Ada".into(),
        email: "ada@example.com".into(),
    })
    .send()
    .await?;
```

`.json(&value)` accepts anything that implements `serde::Serialize`.
The wire `Content-Type` is set to `application/json` automatically.
If serialization fails (e.g. a map with a non-string key), the
builder records the error and `send()` surfaces it instead of
silently sending a `null` body.

### Form

```rust
Http::post("https://login.example.com/oauth/token")
    .form(&serde_json::json!({
        "grant_type": "client_credentials",
        "client_id": id,
        "client_secret": secret,
    }))
    .send()
    .await?;
```

`.form(&value)` serializes the value as `application/x-www-form-urlencoded`.
The value must serialize to a JSON object; the keys become form fields.
Same body-error semantics as `.json` — a serialization failure surfaces
through `send().await?`, never as a silent empty body.

### Raw bytes

```rust
use bytes::Bytes;

let payload: Bytes = compress(report)?;
Http::post("https://collector.example.com/ingest")
    .header("Content-Type", "application/octet-stream")
    .body(payload)
    .send()
    .await?;
```

`.body(bytes)` takes anything `impl Into<Bytes>`. You're responsible
for the `Content-Type` header — `.body` doesn't set one.

## Headers and auth

```rust
Http::get("https://api.example.com/private")
    .header("X-Request-Id", request_id)
    .header("Accept", "application/vnd.api+json")
    .bearer_token(api_key)
    .send()
    .await?;
```

`.header(name, value)` appends; the framework doesn't dedupe, so two
calls with the same name send two headers and reqwest joins them per
HTTP semantics. Two shortcuts for the common auth schemes:

- `.bearer_token(token)` — sets `Authorization: Bearer <token>`
- `.basic_auth(user, password)` — sets `Authorization: Basic <b64>`;
  `password` is `Option<&str>` so `.basic_auth("api-key", None)`
  encodes the `api-key:` form some providers want

## Timeouts

The shared client has a 30-second default timeout. Override per-request
when you need to:

```rust
use std::time::Duration;

Http::get("https://slow.example.com/report")
    .timeout(Duration::from_secs(120))
    .send()
    .await?;
```

`.timeout(dur)` overrides both the connect and the total request
timeout for this one call. There's no separate `connect_timeout`
knob on the builder; the underlying reqwest client uses one combined
timeout.

## Retries

`Http` ships exponential-backoff retries with full jitter — the AWS
recipe, the same one Laravel uses. Two variants, distinguished by
whether they're willing to replay non-idempotent methods.

### `.retry(max_attempts, base_backoff)` — idempotent only

```rust
use std::time::Duration;

let resp = Http::get("https://flaky.example.com/health")
    .retry(4, Duration::from_millis(200))
    .send()
    .await?;
```

`max_attempts` includes the first try, so `retry(4, ...)` retries up
to three times after the initial attempt. The delay before attempt
`n+1` is a uniform random duration in `[0, base_backoff * 2^(n-1)]`,
capped at 30 seconds. Full jitter, not exponential-backoff-plus-fixed-
sleep, so many workers retrying the same outage don't synchronize into
a thundering herd.

A request is retried when:

- The send fails before a response arrives (connect / DNS / timeout), or
- The response status is 5xx

4xx and 2xx/3xx responses are returned as-is. After exhausting retries
the last response (or the last error) is returned to the caller.

The `.retry()` form refuses to retry `POST` or `PATCH`: those methods
are not idempotent, and if the server already committed the write but
the response was lost on the way back, a blind replay would duplicate
the side effect. Calling `.retry()` on a POST/PATCH still works — it
just means "retry on connection errors before the request reaches the
server"; once a 5xx comes back, it's returned to the caller after one
attempt.

### `.retry_non_idempotent(...)` — opt-in for POST/PATCH

```rust
Http::post("https://api.example.com/charges")
    .header("Idempotency-Key", idem_key)
    .retry_non_idempotent(3, Duration::from_millis(200))
    .send()
    .await?;
```

When you've supplied an idempotency key the upstream honors, or you've
otherwise made the request safe to replay, switch to
`.retry_non_idempotent(...)` to opt POST and PATCH into the same
retry behavior. The retry rules are identical — connection errors and
5xx responses are retried; 4xx and 2xx/3xx pass through.

### Retry-After is honored on 503

For a `503 Service Unavailable`, the framework respects a `Retry-After`
header — in either delta-seconds (`Retry-After: 30`) or HTTP-date
(`Retry-After: Tue, 15 Nov 1994 08:12:31 GMT`) form. The actual wait
is the larger of the jittered backoff and the `Retry-After` hint,
still capped at 30 seconds. A hostile or misconfigured server returning
`Retry-After: 86400` won't park your task for a day.

## Reading the response

`ClientResponse` exposes status, headers, and three body-reading
methods. Each body method consumes the response.

```rust
let resp = Http::get("https://api.example.com/users/42").send().await?;

let status: u16 = resp.status();
let etag: Option<String> = resp.header("ETag");

// Pick one — each consumes the response.
let user: User = resp.json().await?;
// let text: String = resp.text().await?;
// let bytes: Bytes = resp.bytes().await?;
```

`.header(name)` is case-insensitive. `.json::<T>()` returns
`Result<T, FrameworkError>` and uses `serde_json` for decoding.
`.text()` enforces UTF-8 and surfaces a `FrameworkError` if the body
isn't valid UTF-8.

### Response body cap

A slow or hostile upstream can otherwise stream an unbounded body into
memory. To protect that, every buffered body read is capped — 25 MiB
by default. Override globally at boot:

```rust
use suprnova::Http;

// Once, somewhere in bootstrap.
Http::set_max_response_bytes(100 * 1024 * 1024); // 100 MiB
```

Or per-request when one call legitimately handles a larger payload:

```rust
let bytes = Http::get("https://example.com/big-export.json")
    .max_response_bytes(500 * 1024 * 1024) // 500 MiB
    .send()
    .await?
    .bytes()
    .await?;
```

A response that declares a `Content-Length` over the cap is rejected
before any body is read; the streaming loop also enforces the cap
against the actual bytes, in case `Content-Length` is absent or lies.

## Escape hatch — raw reqwest

The framework covers the common cases. When you need something we don't
expose — streaming bodies, multipart uploads, redirect policy
inspection, websocket upgrades — call `.into_inner()` to unwrap the
underlying `reqwest::Response`:

```rust
let resp = Http::get("https://example.com/big-stream").send().await?;
let raw: reqwest::Response = resp.into_inner()?;
let mut stream = raw.bytes_stream();
while let Some(chunk) = stream.next().await {
    process(chunk?);
}
```

`into_inner()` returns `Err(FrameworkError::internal(...))` when called
on a fake response — there's no underlying `reqwest::Response` in that
case. The response-body cap also no longer applies once you take the
raw response; you own the read from there.

For outgoing multipart uploads today, drop down to `reqwest::Client`
directly via the same escape route. A future release may add a
`.multipart(...)` builder when the demand pattern shapes itself.

## Testing with `Http::fake`

This is the part you'll use every day. `Http::fake` runs your test body
inside a `tokio::task_local!` scope where every outbound call is
intercepted, captured, and answered with whatever you've queued.

```rust
use suprnova::{Http, fake_response, assert_sent};

#[tokio::test]
async fn creates_a_user_via_api() {
    Http::fake(|| async {
        fake_response(
            "POST",
            "/api/users",
            201,
            serde_json::json!({ "id": 42, "name": "Ada" }),
        );

        let resp = Http::post("https://example.com/api/users")
            .json(&serde_json::json!({ "name": "Ada" }))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 201);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["id"], 42);

        assert_sent(|r| r.method == "POST" && r.url.contains("/api/users"));
    })
    .await;
}
```

### Matching canned responses

`fake_response(method, url_substring, status, body)` queues a canned
response. The first outbound request whose method matches
(case-insensitive) and whose URL contains `url_substring` consumes the
canned entry and returns that response. Use method `"*"` to match any
method.

Subsequent matching requests fall through to the next canned entry of
the same shape, or — if none match — return an empty `200 {}`. Queue
one canned response per expected call:

```rust
fake_response("GET", "/v1/customer", 200, json!({ "id": "cus_1" }));
fake_response("GET", "/v1/customer", 200, json!({ "id": "cus_2" }));
// Two GETs to /v1/customer get distinct responses; a third gets 200 {}.
```

### Assertions

```rust
// Pass if at least one recorded request matches.
assert_sent(|r| r.method == "POST" && r.url.contains("/charges"));

// Pass if no recorded request matches.
assert_not_sent(|r| r.url.contains("/refunds"));
```

`RecordedRequest` exposes `method: String`, `url: String`,
`headers: Vec<(String, String)>`, and `body: Option<Vec<u8>>`. The
predicate runs against every recorded request; assertion failures
print the recorded list with header values and bodies redacted (a
small allowlist of `Content-Type`, `Accept`, and `User-Agent` is shown
in full; everything else is `<redacted>`). That keeps bearer tokens
and webhook payloads out of CI logs even when an assertion blows up.

### Tests run in parallel safely

The fake state lives in a `tokio::task_local!` — every fake scope is
scoped to the task running the test, not the process. Two tests
running concurrently on different tasks each get their own
recorded-requests vec and their own canned-response queue. No shared
mutex, no test ordering, no `#[serial]`.

```rust
#[tokio::test]
async fn first_test() {
    Http::fake(|| async {
        fake_response("GET", "/a", 200, json!({"who": "first"}));
        let _ = Http::get("https://x.test/a").send().await.unwrap();
        assert_sent(|r| r.url.contains("/a"));
        // Sibling test's request to /b is invisible here.
    })
    .await;
}

#[tokio::test]
async fn second_test() {
    Http::fake(|| async {
        fake_response("GET", "/b", 200, json!({"who": "second"}));
        let _ = Http::get("https://x.test/b").send().await.unwrap();
        assert_sent(|r| r.url.contains("/b"));
    })
    .await;
}
```

## The spawned-task gotcha

`tokio::task_local!` is scoped to the current task. Work that goes
through `tokio::spawn` lands on a fresh task and does NOT inherit
the fake — by default, outbound calls from the spawned future hit the
real network. Two helpers address this.

### `Http::fail_on_real_calls()` and `FailOnRealCallsGuard`

Flips a process-global flag that turns any unmatched outbound call
into a `FrameworkError::internal(...)` instead of letting it hit the
network. This is Suprnova's analogue of Laravel's
`Http::preventStrayRequests()` — it catches the exact bug the gotcha
creates.

Use the RAII guard so the flag resets when the test ends, even on
panic:

```rust
use suprnova::FailOnRealCallsGuard;

#[tokio::test]
async fn no_test_makes_a_real_call() {
    let _guard = FailOnRealCallsGuard::install();

    // Any unfaked outbound HTTP call from anywhere inside this test
    // — including from a `tokio::spawn`-ed task — errors with a
    // message naming the URL. No network IO actually happens.
}
```

Nested guards compose correctly: the inner guard's `Drop` restores
the PREVIOUS state, not unconditionally "allowed". So an inner test
helper that installs its own guard inside an outer guarded scope
doesn't disarm the outer guard on the way out.

The flag is process-global by design. The point is catching a
`tokio::spawn`-ed future silently escaping a fake scope and pinging a
real third party from CI. A per-task flag would miss that.

### `Http::spawn_with_fake_inheritance(future)`

When code under test legitimately spawns a task — a queue worker, a
background syncer, a sub-task — and you want its outbound calls to go
through the parent's fake, swap `tokio::spawn` for
`Http::spawn_with_fake_inheritance`:

```rust
Http::fake(|| async {
    fake_response("GET", "/child", 204, json!({}));

    let handle = Http::spawn_with_fake_inheritance(async {
        // Runs on a NEW task, but the parent's fake state is
        // re-installed in this task's task-local scope. The send
        // is intercepted; the response is the 204 above.
        Http::get("https://child.example.com/child").send().await
    });

    let response = handle.await.unwrap().unwrap();
    assert_eq!(response.status(), 204);

    // Recorded requests from the child show up here — the
    // Arc<Mutex<FakeState>> is shared, not snapshotted.
    assert_sent(|r| r.url.contains("/child"));
})
.await;
```

If no fake scope is active when you call
`spawn_with_fake_inheritance`, it's equivalent to `tokio::spawn` — the
child runs without any fake context. So you can use it
unconditionally in code that's sometimes tested with `Http::fake` and
sometimes not.

### Belt-and-braces in test setup

The two combine. A test that wants to be loudly safe pairs them:

```rust
#[tokio::test]
async fn pays_the_invoice() {
    let _guard = FailOnRealCallsGuard::install();

    Http::fake(|| async {
        fake_response("POST", "/v1/charges", 200, json!({ "id": "ch_1" }));

        // If a typo on the URL or method drifts away from the fake,
        // the request falls through to the guard, which errors out
        // with a message naming the URL — instead of silently
        // returning an empty 200 that hides the mismatch.
        pay_invoice(&invoice).await.unwrap();

        assert_sent(|r| r.url.contains("/v1/charges"));
    })
    .await;
}
```

Without the guard, an URL or method that drifts from the fake silently
falls through to a default `200 {}`, and your test passes despite the
production code calling a different endpoint. With the guard, you
fail loudly on the first mismatch.

## OpenTelemetry trace propagation

When the framework is built with the `otel` feature and a W3C
TraceContext propagator is installed, every outbound `Http::*` request
injects `traceparent` (and `tracestate` when non-empty) into its
headers — so downstream services can continue the trace. No
configuration on the call site; the propagator reads
`opentelemetry::Context::current()` at send time.

Without an active OTel context, no headers are injected and outbound
requests look exactly like they did before. See
[Observability](observability.md) for the propagator setup.

## Why Suprnova diverges

Two small divergences from Laravel's `Http::` facade are worth calling
out, both forced by the runtime model.

**Task-local fakes instead of a process-global mock store.** Laravel's
`Http::fake()` mutates a process-wide registry; tests serialize on it,
or you accept that parallel runners can race. Suprnova's `Http::fake`
uses `tokio::task_local!` so two tests on two tasks each see their own
fake — no test ordering, no shared mutex. The price is that
`tokio::spawn`-ed work doesn't inherit the fake by default, which is
why `Http::spawn_with_fake_inheritance` and
`FailOnRealCallsGuard` exist. Together they give you the same
"can't accidentally hit production" guarantee that
`Http::preventStrayRequests()` does in Laravel, with stricter scoping.

**Retries default to refusing POST/PATCH.** Laravel's HTTP client
retries any method by default. Suprnova's `.retry(...)` is idempotent-
only; non-idempotent methods need an explicit
`.retry_non_idempotent(...)` opt-in. The reasoning is that a 5xx
response from a write endpoint frequently means "I committed the
write and then the response was lost" — replaying that blindly
duplicates a charge, a refund, a fan-out. We force the caller to
decide: have you supplied an idempotency key the upstream honors?
If yes, opt POST/PATCH into retries. If no, accept the 5xx.

## Edge cases and small print

- **`Http::*` is closed for v1.** We deliberately don't expose the
  underlying `reqwest::Client`. To grow the surface, add a method to
  the facade rather than reaching for `reqwest` directly — except via
  the documented `into_inner()` escape hatch on a real response.
- **The shared client is built once and lives forever.** Built lazily
  on first call to any `Http::*` verb, kept in a `OnceLock`. The
  rustls TLS stack and the 30s default timeout are baked in.
- **JSON/form serialization failures fail loudly.** A
  `.json(&unserializable)` builder records the error and `send()`
  returns it as `FrameworkError::internal(...)`. The request never
  goes out — we don't degrade to a `null` body.
- **The 30s retry ceiling is hard.** The backoff math caps at 30
  seconds; the `Retry-After` interpretation caps at 30 seconds; no
  single retry sleep parks a task for longer.
- **Process-global cap is one-shot.** `Http::set_max_response_bytes`
  is a write to a process-global atomic — set it once at boot, then
  override per-request as needed. There's no "reset to default" call.

## Next

- [Mail](mail.md) — outbound email, which uses similar fake / driver
  patterns for tests
- [Notifications](notifications.md) — notification channels including
  web push, all share the same test-fake philosophy
- [Queues](queues.md) — jobs that make outbound HTTP calls, plus the
  `spawn_with_fake_inheritance` pattern for testing workers
- [Testing](testing.md) — `#[suprnova_test]`, `TestContainer`, and the
  rest of the fakes surface
- [Observability](observability.md) — OTel propagator setup that makes
  `traceparent` injection light up
