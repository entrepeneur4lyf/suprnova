# Observability

Three layers of operator-visible signal ship in the framework: structured
logs (always on), per-request id correlation (always on, propagates into
spawned tasks), and an opt-in OpenTelemetry bridge that turns every
`tracing` span into an exported OTel span. The same `#[tracing::instrument]`
you'd write for local logs becomes a distributed-trace span when the OTel
feature is on — no second instrumentation API.

```rust
use suprnova::telemetry::{init_telemetry, OtelConfig};
use suprnova::logging::LogConfig;

#[tokio::main]
async fn main() {
    let guard = init_telemetry(LogConfig::from_env(), OtelConfig::from_env());

    // ... run the app ...

    // Flush buffered telemetry before exit. The OTel batch processors hold
    // spans/metrics/logs in memory; dropping the guard without `shutdown`
    // loses whatever hasn't been exported yet.
    guard.shutdown().await;
}
```

A scaffolded app's `Server` already calls `init_telemetry` for you and
flushes the guard on the shutdown signal — you only wire it by hand when
embedding Suprnova in your own runtime.

## The three layers

| Layer | Always on | What it gives you |
|---|---|---|
| Structured logging (`tracing`) | Yes | Stdout logs in `pretty` (dev) or `json` (production) format, environment-aware |
| Request-id correlation | Yes | Per-request id scoped through a `tokio::task_local!`, echoed on `X-Request-Id`, propagates into `spawn_with_request_id` tasks |
| OpenTelemetry export | `otel` feature + collector endpoint | OTLP HTTP/proto export of traces, metrics, and logs; W3C `traceparent` propagation both ways |

The OTel layer is **opt-in at compile time** so default builds carry no
OpenTelemetry dependencies and the [`Metrics`](#metrics) facade compiles to
inert no-ops. With the feature off, "trace" and "metric export" silently
become no-ops — your logs still work.

### Why Suprnova diverges

Laravel's observability story splits between in-framework events
(`QueryExecuted`, `MessageSent`, `JobProcessed`) and runtime concerns
delegated to PHP extensions (OpenTelemetry, Sentry, New Relic) plugged in
at the FPM layer. The event surface is rich; the runtime surface is
"install the extension your APM vendor needs."

Suprnova is a single async process, so it owns both halves. The event
surface is parity (same `QueryExecuted`/`NotificationSent`/`ErrorOccurred`
shape), and the runtime surface is a `tracing` → OpenTelemetry bridge
inside the framework. You don't install an extension; you flip a feature
flag and the same spans you already emit become OTel-exported.

## Structured logging

`LogConfig::from_env()` reads two env vars:

| Var | Default | Notes |
|---|---|---|
| `LOG_LEVEL` | `"info"` | `tracing-subscriber` env-filter syntax (e.g. `"debug,sqlx=warn,hyper=warn"`) |
| `LOG_FORMAT` | environment-aware | `"json"` in production, `"pretty"` everywhere else; explicit value always wins |

The format default is detected from `APP_ENV` via `Environment::detect()`:
a production deploy gets one-JSON-object-per-line output for log
aggregators by default, local/dev runs get human-readable multi-line
output. An explicit `LOG_FORMAT=pretty` will override the production
default if you want raw stdout in production.

```bash
# Local dev — explicit overrides win
LOG_LEVEL=debug,sqlx=warn,hyper=warn LOG_FORMAT=pretty cargo run

# Production — APP_ENV=production flips the format default to json
APP_ENV=production LOG_LEVEL=info cargo run --release
```

A malformed `LOG_LEVEL` directive does not crash boot — it falls back to
`"info"` and prints a one-line warning on stderr so the misconfiguration
is operator-visible.

### Span context in every line

Every routed HTTP request runs inside a `request` span created by the
framework's outermost middleware. The span carries three fields —
`request_id`, `method`, `path` — and the JSON formatter nests them under
`span` on every event emitted inside the request. Your application code
doesn't need to read or record the id on every line; the span carries it
implicitly:

```rust
use tracing::info;

pub async fn show(req: suprnova::Request) -> suprnova::Response {
    info!(user_id = 42, "loaded dashboard");
    // JSON line carries span.request_id / span.method / span.path
    // without the call site having to thread anything in.
    Ok(suprnova::json_response!({ "ok": true }))
}
```

## Request-id correlation

Every request gets a 36-character lowercase UUID v4 id, scoped through a
`tokio::task_local!`. The middleware reuses an inbound `X-Request-Id`
when the header value passes a strict safety check (ASCII alphanumeric
plus `-_.:`, max 128 bytes); anything outside that charset is rejected
and replaced with a fresh UUID so an attacker cannot inject control
characters into log output or balloon downstream pipelines.

The same id is echoed on **every** response — success, error, and panic
recovery — as the `X-Request-Id` header, so a frontend or upstream
service can include it in bug reports and operators can grep for it in
the structured log.

### Reading the id

```rust
use suprnova::{current_request_id, spawn_with_request_id};

pub async fn checkout(req: suprnova::Request) -> suprnova::Response {
    // Inside a request, the id is always present.
    let id = current_request_id().expect("inside a request");
    tracing::info!(request_id = %id, "checkout starting");

    // Background work spawned from a handler. `tokio::spawn` starts a
    // task with empty task-locals — the spawned future would lose the
    // request id without help. `spawn_with_request_id` captures the
    // caller's id and re-scopes it for the spawned future, and attaches
    // the current `tracing` span so the task's events inherit
    // `request_id` the same way in-request events do.
    spawn_with_request_id(async move {
        // This log line carries the originating request's id.
        tracing::info!("post-checkout fanout running");
    });

    Ok(suprnova::ok!())
}
```

`current_request_id()` returns `None` outside a request — background
jobs, scheduled tasks, and tests without the middleware see no id, and
the helper does not invent one. `spawn_with_request_id` outside a
request scope is exactly `tokio::spawn`; nothing magical happens.

### Where the id is also available

| Surface | How |
|---|---|
| `tracing` events | `span.request_id` on every line inside the request |
| Response header | `X-Request-Id` on success, error, and panic-recovered responses |
| `Context` bag | `Context::get("_request_id")` — readable from observers, listeners, jobs that consult `Context` |
| Spawned tasks | `current_request_id()` after `spawn_with_request_id` |

## Built-in events for observability

The framework dispatches typed events at the points an operator usually
wants to instrument. Each is a `suprnova::Event` you can `listen` for via
`EventFacade::listen::<E, _>(...)` and ship to Sentry, Datadog, Slack, or
your metrics pipeline. All of them run through `dispatch_best_effort`, so
a failing listener does not break the request that triggered it.

| Event | When it fires | Carries |
|---|---|---|
| `ErrorOccurred` | Any `FrameworkError` → 5xx conversion (including panic recovery) | error context + request id |
| `QueryExecuted` | Every query routed through the instrumented executor helpers | sql, bindings, duration, connection, read/write classification, result |
| `ConnectionEstablished` | `DbConnection::connect` succeeded | connection name |
| `TransactionBeginning` / `TransactionCommitted` / `TransactionRolledBack` | Closure-form `DB::transaction` + manual handles | connection name |
| `NotificationSending` / `NotificationSent` / `NotificationFailed` | Per-channel before/after/error of `Notification::send` | notification + channel + recipient |

`ErrorOccurred` is the hook for shipping 5xx exceptions; `QueryExecuted`
is the hook for slow-query alerts; the notification trio is the hook for
delivery dashboards. See [Events](events.md) for the listener API and
[Lifecycle](lifecycle.md) for where in the request path each event fires.

### Direct DB query observation

`DB::listen` is a second, synchronous hook tailored specifically for
`QueryExecuted`. It fires inline inside the executor, so a slow listener
slows the query — keep it light. The dispatcher path
(`EventFacade::listen::<QueryExecuted, _>`) is run-them-all
best-effort and tolerates errors; prefer it for anything that can fail.

```rust
use suprnova::DB;

// In bootstrap.rs:
DB::listen(|q| {
    if q.time > std::time::Duration::from_millis(100) {
        tracing::warn!(
            sql = %q.sql,
            ms = q.time.as_millis(),
            "slow query"
        );
    }
})?;
```

A listener that itself issues a database query will **not** re-fire
`QueryExecuted` for the nested call — a task-local re-entrancy guard
prevents the "log-to-DB listener → emits event → log-to-DB → ..." loop.

### Capturing a query log for tests / debug

For test assertions or one-off "what ran during this block?" debugging:

```rust
use suprnova::DB;

DB::enable_query_log()?;
// ... run the code you want to inspect ...
let queries = DB::get_query_log()?;
for q in &queries {
    println!("{:>4}ms  {}", q.time.as_millis(), q.to_raw_sql());
}
DB::disable_query_log()?;
DB::flush_query_log()?;
```

The buffer is **unbounded** — every captured query grows it. Use it for
tests and one-shot investigation, flush periodically if you leave it on
in production.

## Distributed tracing (OTel)

Add the `otel` feature to opt in:

```toml
[dependencies]
suprnova = { git = "...", features = ["otel"] }
```

Configure via the standard OTel environment variables:

```bash
# Minimum: where the collector lives.
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
OTEL_SERVICE_NAME=my-app          # defaults to "suprnova"
OTEL_SERVICE_VERSION=1.4.2        # defaults to your crate version
```

Telemetry is **enabled** only when `OTEL_EXPORTER_OTLP_ENDPOINT` is set
**and** the kill switch `OTEL_SDK_DISABLED` is not on. With no endpoint
the logging layer runs alone, and the returned guard holds no providers,
so dropping it without `shutdown()` is silent (no spurious "buffered
telemetry may be lost" warning on every test process).

### Trace context joins automatically

**Inbound.** When a request arrives carrying a W3C
[`traceparent`](https://www.w3.org/TR/trace-context/) header — i.e. it
was made by another traced service — the middleware extracts that
context and reparents the request span onto the caller's span. Your
server span shows up as a child in the *same* distributed trace, not a
fresh root. A request without `traceparent` (a direct browser hit) stays
a clean root span.

**Outbound.** The framework HTTP client ([`Http`](http-client.md))
injects the active trace context as `traceparent` on every outbound
call, so the downstream service continues the same trace.

Together: `upstream service → your handler → downstream service` is one
connected trace, with no manual span plumbing in your handlers.

**Error status.** When a handler returns a 5xx, the request span is
marked errored so the OTel backend shows `Status::Error`. (A handler
*panic* is caught and turned into a 500 with an error-level log and an
`ErrorOccurred` event, but the OTel span status is not set on that path
— the panic unwinds the span's future before the marker runs.)

### Adding your own spans

Because the bridge turns every `tracing` span into an OTel span, you
instrument with plain `tracing` — no OTel-specific API in your code:

```rust
use suprnova::DatabaseConnection;

#[tracing::instrument(skip(db))]
async fn load_dashboard(db: &DatabaseConnection, user_id: i64) -> anyhow::Result<()> {
    // This span nests under the request span automatically, and exports
    // to your collector when the `otel` feature is on.
    Ok(())
}
```

### Environment variables Suprnova reads

| Var | Effect |
|---|---|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Collector base URL. Unset → telemetry disabled. |
| `OTEL_SERVICE_NAME` | `service.name` resource attribute (default `"suprnova"`). |
| `OTEL_SERVICE_VERSION` | `service.version` resource attribute (default: crate version). |
| `OTEL_SDK_DISABLED` | Kill switch. Case-insensitive `true` or `1` disables export even with an endpoint set. |

The rest of the standard OTLP knobs are read by the SDK itself, so
configure them the normal way:

| Var | Read by |
|---|---|
| `OTEL_EXPORTER_OTLP_HEADERS` | exporter (collector auth, e.g. `Authorization=Bearer ...`) |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | exporter (`http/protobuf`, etc.) |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | exporter |
| `OTEL_EXPORTER_OTLP_COMPRESSION` | exporter |

Per-signal endpoint overrides (`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`,
`_METRICS_ENDPOINT`, `_LOGS_ENDPOINT`) are currently shadowed by the
base endpoint — all three signals go to `OTEL_EXPORTER_OTLP_ENDPOINT`.
If you need to fan signals to different collectors, run a local
collector that routes them.

## Metrics

`Metrics` is the facade for counters, histograms, and gauges. Handles
are cheap to clone and resolve the global meter on each construction:

```rust
use suprnova::telemetry::Metrics;

// Counter — monotonic.
let signups = Metrics::counter("user.signups");
signups.inc();                                  // +1
signups.inc_by(3);                              // +3
signups.inc_with(&[("plan", "pro")]);           // +1 with a label

// Histogram — distributions (latency, sizes).
let latency = Metrics::histogram("request.latency_ms");
latency.record(42.0);
latency.record_with(42.0, &[("route", "/checkout")]);

// Gauge — point-in-time value.
let queue_depth = Metrics::gauge("jobs.pending");
queue_depth.set(17.0);
queue_depth.set_with(17.0, &[("queue", "emails")]);
```

Without the `otel` feature every call above is a no-op with zero
allocation — leave instrumentation in hot paths and pay nothing in
default builds.

Metric handles bind to whichever meter provider is active when the
underlying instrument is first resolved. Create handles **after**
`init_telemetry` has run (or lazily at first use) — a handle constructed
before initialization resolves against the no-op provider and stays
inert. The idiomatic pattern is a `once_cell` / `LazyLock` handle
resolved on first emit, well after boot.

Attribute values are string-typed (`&[(&'static str, &str)]`). Numeric
and boolean attributes are a planned enhancement; format them as strings
at the call site for now.

Naming: stable, ASCII, dot-delimited (e.g. `"http.requests.total"`,
`"http.request.duration"`). The standard OTel semantic conventions live
in `opentelemetry-semantic-conventions::metric::*`.

## The shutdown contract

`init_telemetry` returns a `TelemetryGuard` that owns the SDK provider
handles. The OTel batch processors buffer spans / metrics / logs in
memory and flush asynchronously, so you must `guard.shutdown().await`
before the process exits or you lose whatever is still buffered.

- Calling `shutdown()` flushes and is safe to call once (it takes
  `self`).
- Dropping the guard **without** `shutdown()` logs a warning — but only
  when the guard actually holds providers. A telemetry-disabled run (no
  endpoint, or `OTEL_SDK_DISABLED`, or a non-`otel` build) hands back a
  provider-less guard whose drop is silent, so collector-less dev and
  test runs don't get spammed.

## Summary

| Task | API |
|---|---|
| Enable OTel | `features = ["otel"]` + `OTEL_EXPORTER_OTLP_ENDPOINT` |
| Initialize | `init_telemetry(LogConfig::from_env(), OtelConfig::from_env())` |
| Flush on exit | `guard.shutdown().await` |
| Disable at runtime | `OTEL_SDK_DISABLED=true` |
| Custom span | `#[tracing::instrument]` (auto-bridged to OTel) |
| Counter / histogram / gauge | `Metrics::counter/histogram/gauge(name)` |
| Distributed trace join | Automatic — inbound `traceparent` extracted, outbound injected |
| Read current request id | `current_request_id()` |
| Propagate id into spawn | `spawn_with_request_id(future)` |
| Synchronous query observer | `DB::listen(|q| { ... })` |
| Best-effort query observer | `EventFacade::listen::<QueryExecuted, _>(...)` |
| Capture queries for tests | `DB::enable_query_log()` → `DB::get_query_log()` |

## Next

- [Events](events.md) — listener API, dispatch modes, `EventFacade::fake()` for tests
- [Lifecycle](lifecycle.md) — where in the request path each event fires and where the request span is constructed
- [Error Handling](errors.md) — `ErrorOccurred`, `HttpError`, sanitised 5xx bodies
- [Database](database.md) — `QueryExecuted`, `DB::transaction`, the executor helpers that fire the events
- [HTTP Client](http-client.md) — outbound `traceparent` injection that closes the distributed-trace loop
