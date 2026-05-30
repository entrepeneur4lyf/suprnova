# Logging

Suprnova logs through [`tracing`](https://docs.rs/tracing) — every log
line is a structured event with fields, not a formatted string. A
subscriber is installed at boot that reads `LOG_LEVEL` and `LOG_FORMAT`
from the environment, emits pretty multi-line output in dev and one
JSON object per line in production, and propagates a per-request id
into every event a handler emits.

This chapter covers the log surface itself: the subscriber, the
formats, the levels, and the request-id correlation that makes a
production log searchable. For the OpenTelemetry bridge and query
logging see [Observability](observability.md); for the request
`Context` bag that emitters can read alongside the id see
[Context](context.md).

## What gets logged where

Two outputs by default:

| Where | Format | When |
|---|---|---|
| `stdout` | `LogFormat::Pretty` — multi-line, coloured, human-friendly | dev (`APP_ENV` is `local`, `dev`, `testing`, …) |
| `stdout` | `LogFormat::Json` — one JSON object per line | production (`APP_ENV=production` / `prod`) |

The dev/prod default is computed from `APP_ENV` via
`Environment::detect()`. Override with `LOG_FORMAT=pretty` or
`LOG_FORMAT=json` to force one explicitly.

```env
# .env (dev)
LOG_LEVEL=info,sqlx=warn
LOG_FORMAT=pretty   # optional; this is the dev default

# .env.production
LOG_LEVEL=info,sqlx=warn,suprnova::queue=debug
LOG_FORMAT=json     # optional; this is the prod default
```

The framework only writes to `stdout`. In production point your
container runtime, systemd journal, or log aggregator at it
(`docker logs`, `kubectl logs`, `journalctl -u my-app`, a Loki/Vector
agent, etc.). There is no rotating file appender — let the platform
own log persistence.

## Emitting events

Use the `tracing` macros in handlers, jobs, middleware, anywhere:

```rust
use tracing::{debug, info, warn, error, instrument};

pub async fn checkout(req: suprnova::Request) -> suprnova::Response {
    let user_id: i64 = req.session::<i64>("user_id").unwrap_or(0);

    info!(user_id, "checkout starting");

    let order = place_order(user_id).await.map_err(|e| {
        error!(user_id, error = %e, "checkout failed");
        e
    })?;

    info!(user_id, order_id = order.id, total = order.total_cents, "checkout succeeded");

    suprnova::Response::ok().json(&order)
}
```

Each field becomes a top-level key in JSON output and a coloured
`field=value` pair in pretty output. Prefer fields over interpolation —
they're searchable in JSON logs and the formatter handles type-aware
rendering.

To wrap a function in a span and stamp every event inside it with
shared fields, use `#[instrument]`:

```rust
#[instrument(skip(db), fields(user_id = %user_id))]
pub async fn load_dashboard(
    db: &suprnova::DatabaseConnection,
    user_id: i64,
) -> Result<Dashboard, FrameworkError> {
    info!("loading"); // automatically carries user_id from the span
    // … queries …
}
```

The same `#[instrument]` becomes an OpenTelemetry span when the `otel`
feature is enabled — see [Observability](observability.md#opentelemetry).

## Log levels

`LOG_LEVEL` is a [`tracing-subscriber` env-filter
directive](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html),
not a single level. The grammar is comma-separated `target=level`
pairs, where bare values set the default:

```env
LOG_LEVEL=info                                  # everything at info+
LOG_LEVEL=debug                                 # everything at debug+
LOG_LEVEL=info,sqlx=warn                        # info default, sqlx quieter
LOG_LEVEL=warn,suprnova::queue=debug,my_app=info  # warn default, two targets verbose
```

Targets are usually the emitting crate or module path
(`suprnova::queue`, `hyper::server`, `my_app::services::checkout`).
Find a target by reading the JSON log line — the `target` field on
every event is its filter key.

Levels in increasing verbosity: `error` < `warn` < `info` (default) <
`debug` < `trace`. The wire-format error response is always sanitised
to `{"message": "Internal Server Error"}` regardless of level — the
detail goes only to the structured log.

### Invalid directives don't crash boot

A malformed `LOG_LEVEL` (e.g. `LOG_LEVEL=app=notalevel`) falls back to
`"info"` and writes a one-line warning to `stderr`:

```text
suprnova: invalid LOG_LEVEL directive "app=notalevel" (...); falling back to "info". Fix LOG_LEVEL to silence this.
```

This is `stderr` rather than `tracing::warn!` because the subscriber
hasn't been installed yet — a `warn!` would be silently dropped. Fix
the directive and the warning goes away.

## Pretty vs JSON output

The same `info!(user_id = 42, "saved")` renders differently per format.

**Pretty (dev):**

```text
  2026-05-30T22:14:08.221341Z  INFO request{request_id=78a9...} my_app::handlers::checkout: saved
    at src/handlers/checkout.rs:48
    in checkout
    in request with request_id: 78a9..., method: POST, path: /checkout
```

**JSON (prod):**

```json
{
  "timestamp": "2026-05-30T22:14:08.221341Z",
  "level": "INFO",
  "fields": { "message": "saved", "user_id": 42 },
  "target": "my_app::handlers::checkout",
  "span": { "name": "checkout" },
  "spans": [
    { "name": "request", "request_id": "78a9...", "method": "POST", "path": "/checkout" }
  ]
}
```

The JSON shape is what production aggregators (Datadog, Loki,
Honeycomb, CloudWatch, …) parse out of the box. `span.request_id` is
the correlation key — see below.

## Per-request id correlation

Every HTTP request gets a `RequestId` from `RequestIdMiddleware`, the
outermost middleware on every chain. The id is:

- **Reused** from a safe inbound `X-Request-Id` header (alphanumerics
  plus `- _ . :`, up to 128 bytes), or **freshly minted** as a UUID v4
  if absent / unsafe.
- **Echoed** back on the response as `X-Request-Id` (both 2xx and
  5xx variants).
- **Scoped** into a `request` `tracing` span so every event from any
  middleware, handler, or downstream library carries `request_id` in
  its `spans` array automatically.
- **Seeded** into the request `Context` bag as `_request_id`, so
  emitters that want the bare string (jobs, broadcast payloads, error
  reports) can read it by name.

Read it in code with `current_request_id()`:

```rust
use suprnova::current_request_id;
use tracing::info;

if let Some(id) = current_request_id() {
    info!(request_id = %id, "checkpoint reached");
}
```

`current_request_id()` returns `Option<RequestId>` because background
work (jobs, scheduled tasks, tests that didn't install the middleware)
runs outside any request scope.

### Background tasks: spawn with the id

`tokio::spawn` starts a fresh task with empty task-locals — a handler
that spawns side-effect work loses `current_request_id()` and its log
events become orphaned. Use `spawn_with_request_id` instead:

```rust
use suprnova::spawn_with_request_id;
use tracing::info;

pub async fn checkout(req: suprnova::Request) -> suprnova::Response {
    let order = place_order().await?;

    spawn_with_request_id(async move {
        // This task still observes current_request_id().
        // Its log events carry the same request_id as the handler's.
        info!(order_id = order.id, "post-checkout fanout running");
        send_receipt(order.id).await;
        update_analytics(order.id).await;
    });

    suprnova::Response::ok().json(&order)
}
```

The helper propagates both the `RequestId` task-local and the current
`tracing::Span`, so the spawned future's events nest under the same
`request` span in the log. Outside an active request scope it falls
through to a bare `tokio::spawn` — safe to use unconditionally.

Only the request id and tracing span follow the task — the request
`Context` bag deliberately does not, because background work isn't
serving the originating HTTP request.

## The subscriber

The framework installs a global `tracing` subscriber at boot from
`Server::run()`. You almost never call this yourself; it's documented
because tests, embedders, and unusual entry points sometimes need to.

```rust
use suprnova::{LogConfig, init_subscriber};

// Read LOG_LEVEL / LOG_FORMAT from the environment:
init_subscriber(LogConfig::from_env());

// Or programmatic:
init_subscriber(LogConfig {
    level: "info,sqlx=warn".to_string(),
    format: suprnova::LogFormat::Json,
});
```

`init_subscriber` is **idempotent**. A second call leaves the existing
subscriber in place and emits a `tracing::warn!` so an operator can
see that the new `LogConfig` was not applied. This is what lets tests
that each call `init_subscriber` not race each other — the first wins,
the rest are no-ops.

For the OTel-aware variant (the same `LogConfig`, plus
distributed-tracing export), use
[`init_telemetry`](observability.md#opentelemetry).

## Tests

Tests don't need to install a subscriber — the `#[suprnova_test]`
attribute and `TestContainer::fake` set up enough machinery for
handler events to flow. If you want to assert on log output, capture
via `tracing-subscriber`'s
[`tracing_subscriber::fmt::TestWriter`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/fmt/struct.TestWriter.html)
or a custom layer; the framework deliberately does not ship a "capture
all logs in this test" fake because the standard `tracing-subscriber`
test patterns work cleanly.

## Why Suprnova diverges

Laravel uses [Monolog](https://github.com/Seldaek/monolog) — message
strings with optional context arrays, log channels, and per-channel
handlers (file, syslog, Slack, …). PHP's request-per-process model
means a single global static logger is safe: each request gets its
own process and its own context.

Rust's process model is the opposite — one process serves many
concurrent requests on many threads. A global string-formatter would
race on context and require explicit `request_id` plumbing through
every call site. `tracing` solves both with structured fields and
task-local spans: no plumbing, fields stay typed, and correlation is
automatic because the request span is in scope for every event the
chain emits.

`stdout`-only output is also intentional. In containerised
deployments (the only way Suprnova ships) the runtime, not the app,
owns log persistence — file rotation, retention, and shipping all
belong to the platform.

## Next

- [Observability](observability.md) — OpenTelemetry, query log, the
  full operator surface
- [Context](context.md) — the per-request bag where `_request_id` and
  other contextual fields live
- [Error Handling](errors.md) — how the framework's panic boundary
  and 5xx path emit their own structured events
- [Environment Variables](env-vars.md) — `LOG_LEVEL`, `LOG_FORMAT`
  reference
