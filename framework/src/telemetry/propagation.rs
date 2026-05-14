//! W3C trace-context propagation.
//!
//! Installs the standard `traceparent`/`tracestate` propagator into the
//! OTel global so downstream HTTP clients (and Phase 2 middleware) can
//! inject and extract trace context on requests and responses.
//!
//! Also re-exports the header bridge types from `opentelemetry-http` —
//! `HeaderExtractor` / `HeaderInjector` adapt a `http::HeaderMap` to the
//! `TextMapPropagator` interface and will be used by request middleware
//! in a later phase.

/// Install the W3C `TraceContextPropagator` as the global text-map
/// propagator. Idempotent — calling repeatedly simply overwrites the
/// previous installation.
#[cfg(feature = "otel")]
pub fn install_trace_context_propagator() {
    use opentelemetry::global;
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    global::set_text_map_propagator(TraceContextPropagator::new());
}

/// Stub for builds without the `otel` feature. Does nothing.
#[cfg(not(feature = "otel"))]
pub fn install_trace_context_propagator() {}

#[cfg(feature = "otel")]
pub use opentelemetry_http::{HeaderExtractor, HeaderInjector};
