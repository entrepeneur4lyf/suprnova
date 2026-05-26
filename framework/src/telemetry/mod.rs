//! OpenTelemetry export bridge + Metrics facade.
//!
//! Suprnova wires `tracing` to OpenTelemetry through this module:
//!
//! - [`init_telemetry`] is the single entry point. It installs the global
//!   `tracing` subscriber, and (when the `otel` feature is enabled and a
//!   collector endpoint is configured) also installs SDK tracer / meter
//!   / logger providers backed by OTLP HTTP-proto exporters.
//! - [`OtelConfig`] is the env-driven configuration shape. It mirrors the
//!   standard OTel env vars so operators don't have to learn anything new.
//! - [`TelemetryGuard`] owns the SDK provider handles and flushes them on
//!   `shutdown().await`. Forgetting to call `shutdown` emits a `tracing`
//!   warning on drop.
//! - [`Metrics`] is the user-facing facade for counters / histograms /
//!   gauges. It compiles to inert no-ops when `otel` is disabled, so
//!   instrumentation in user code has zero overhead in default builds.
//!
//! The whole module follows the Suprnova rule: re-export only the API
//! users actually call; keep SDK details internal.

pub mod init;
pub mod metrics;
pub mod propagation;

pub use init::{OtelConfig, TelemetryGuard, init_telemetry};
pub use metrics::{CounterHandle, GaugeHandle, HistogramHandle, Metrics};
