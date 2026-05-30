---
title: 'Observability'
description: 'Structured logging, distributed tracing, and metrics via OpenTelemetry.'
icon: 'chart-line'
---

Suprnova wires Rust's [`tracing`](https://docs.rs/tracing) ecosystem to
[OpenTelemetry](https://opentelemetry.io/) through one module. Out of the box
you get structured logs with a per-request id; turn on the `otel` feature and
point it at a collector and the same spans become distributed traces, events
become OTel logs, and you can emit metrics — with zero changes to your
instrumentation code.

## The two layers

| Layer | Always on | What it gives you |
|-------|-----------|-------------------|
| `tracing` subscriber | Yes | Structured stdout logs (pretty or JSON), per-request `request` span carrying `request_id` / `method` / `path` |
| OpenTelemetry SDK | `otel` feature + collector endpoint | OTLP export of traces, metrics, and logs; W3C trace-context propagation |

The OTel layer is **opt-in at compile time** (`features = ["otel"]`) so default
builds carry no OpenTelemetry dependencies and the [`Metrics`](#metrics) facade
compiles to inert no-ops. When the feature is off, everything below that
mentions "traces" or "metrics export" is silently a no-op — your logs still
work.

## Enabling OpenTelemetry

Add the feature to your app's `suprnova` dependency:

```toml
[dependencies]
suprnova = { git = "...", features = ["otel"] }
```

Then configure via the standard OTel environment variables and initialize at
boot:

```bash
# Minimum to turn it on: where the collector lives.
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318
OTEL_SERVICE_NAME=my-app          # defaults to "suprnova"
OTEL_SERVICE_VERSION=1.4.2        # defaults to your crate version
```

```rust
use suprnova::telemetry::{init_telemetry, OtelConfig};
use suprnova::logging::LogConfig;

#[tokio::main]
async fn main() {
    let guard = init_telemetry(LogConfig::from_env(), OtelConfig::from_env());

    // ... run the app ...

    // Flush buffered telemetry before exit. The batch processors hold
    // spans/metrics/logs in memory; dropping the guard without this loses
    // whatever hasn't been exported yet.
    guard.shutdown().await;
}
```

Telemetry is **enabled** only when `OTEL_EXPORTER_OTLP_ENDPOINT` is set
**and** the kill switch `OTEL_SDK_DISABLED` is not on. With no endpoint, you
get the logging layer alone — and the returned guard holds no providers, so
dropping it without `shutdown()` is silent (no spurious "buffered telemetry
may be lost" warning).

<Note>
A scaffolded app's `Server` already calls `init_telemetry` for you and flushes
the guard on shutdown signal. You only wire it by hand when embedding Suprnova
in your own runtime.
</Note>

## Distributed tracing

Every routed HTTP request runs inside a `request` span created by the
framework's outermost middleware. The span carries `request_id`, `method`,
and `path`, and the per-request id is echoed on the `X-Request-Id` response
header.

**Inbound trace join.** When a request arrives carrying a W3C
[`traceparent`](https://www.w3.org/TR/trace-context/) header — i.e. it was
made by another traced service — Suprnova extracts that context and reparents
the request span onto the caller's span. Your server span shows up as a child
in the *same* distributed trace, not a fresh root. A request with no
`traceparent` (a direct browser hit) stays a clean root span.

**Outbound trace propagation.** The framework HTTP client
([`Http`](/docs/core/http-client)) injects the active trace context as a
`traceparent` header on every outbound call, so the downstream service
continues the same trace.

Together these close the loop: `upstream service → your handler → downstream
service` is one connected trace, automatically, with no manual span plumbing
in your handlers.

**Error status.** When a handler returns a 5xx, the request span is marked
errored so the OTel backend shows it as `Status::Error`. (A handler *panic* is
caught and turned into a 500 with an error-level log and an `ErrorOccurred`
event, but the span status is not set in that path — the panic unwinds before
the marker runs.)

### Adding your own spans

Because the bridge turns every `tracing` span into an OTel span, you
instrument with plain `tracing` — no OTel-specific API in your code:

```rust
#[tracing::instrument(skip(db))]
async fn load_dashboard(db: &DatabaseConnection, user_id: i64) -> Result<Dashboard> {
    // This span nests under the request span automatically, and exports
    // to your collector when `otel` is on.
    let widgets = fetch_widgets(db, user_id).await?;
    Ok(Dashboard::new(widgets))
}
```

## Environment variables

Suprnova reads only the handful of vars that decide **whether** and **as
whom** to export:

| Var | Effect |
|-----|--------|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Collector base URL. Unset → telemetry disabled. |
| `OTEL_SERVICE_NAME` | `service.name` resource attribute (default `"suprnova"`). |
| `OTEL_SERVICE_VERSION` | `service.version` resource attribute (default: crate version). |
| `OTEL_SDK_DISABLED` | Kill switch. Case-insensitive `true` (or `1`) disables export even with an endpoint set. |

**The rest of the standard OTLP knobs are read by the SDK itself**, not by
Suprnova — so configure them the normal way and they take effect:

| Var | Read by |
|-----|---------|
| `OTEL_EXPORTER_OTLP_HEADERS` | exporter (collector auth, e.g. `Authorization=Bearer ...`) |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | exporter (`http/protobuf`, etc.) |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | exporter |
| `OTEL_EXPORTER_OTLP_COMPRESSION` | exporter |

<Warning>
**Per-signal endpoint override is currently shadowed.** Suprnova sets the
base endpoint explicitly for traces, metrics, and logs, which means
per-signal overrides like `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` do not take
effect — all three signals go to `OTEL_EXPORTER_OTLP_ENDPOINT`. If you need
to fan signals to different collectors, run a local collector that routes
them. This is a known limitation tracked for a future release.
</Warning>

## Metrics

`Metrics` is the facade for counters, histograms, and gauges. Handles are
cheap to clone and resolve the global meter on each construction, so you can
create them wherever you need them:

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

Without the `otel` feature every call above is a no-op with no allocation, so
you can leave instrumentation in hot paths and pay nothing in default builds.

<Note>
Metric handles bind to whichever meter provider is active when the underlying
instrument is first resolved. Create handles **after** `init_telemetry` has
run (or lazily at first use) — a handle constructed before initialization
resolves against the no-op provider and stays inert. The idiomatic pattern is
a `once_cell`/`LazyLock` handle resolved on first emit, well after boot.

Attribute values are string-typed (`&[(&'static str, &str)]`). Numeric and
boolean attributes are a planned enhancement; for now format them as strings
at the call site.
</Note>

## The shutdown contract

`init_telemetry` returns a `TelemetryGuard` that owns the SDK provider
handles. The OTel batch processors buffer spans/metrics/logs in memory and
flush asynchronously, so you must `guard.shutdown().await` before the process
exits or you lose whatever is still buffered.

- Calling `shutdown()` flushes and is safe to call once (it takes `self`).
- Dropping the guard **without** `shutdown()` logs a warning — but only when
  the guard actually holds providers. A telemetry-disabled run (no endpoint,
  or `OTEL_SDK_DISABLED`) hands back a provider-less guard whose drop is
  silent, so collector-less dev and test runs don't get spammed.

## Summary

| Task | API |
|------|-----|
| Enable | `features = ["otel"]` + `OTEL_EXPORTER_OTLP_ENDPOINT` |
| Initialize | `init_telemetry(LogConfig::from_env(), OtelConfig::from_env())` |
| Flush on exit | `guard.shutdown().await` |
| Disable at runtime | `OTEL_SDK_DISABLED=true` |
| Custom span | `#[tracing::instrument]` (auto-bridged to OTel) |
| Counter / histogram / gauge | `Metrics::counter/histogram/gauge(name)` |
| Distributed trace join | Automatic — inbound `traceparent` extracted, outbound injected |
