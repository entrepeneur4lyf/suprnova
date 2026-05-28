//! W3C trace-context propagation.
//!
//! Installs the standard `traceparent`/`tracestate` propagator into the
//! OTel global so the framework can inject context on outbound HTTP
//! client calls and extract it from inbound server requests — the two
//! halves of joining a distributed trace.
//!
//! - Outbound: `http_client::inject_w3c_trace_context` injects the active
//!   context into request headers via [`HeaderInjector`].
//! - Inbound: [`extract_w3c_trace_context`] reads `traceparent`/`tracestate`
//!   off the incoming request, and [`join_upstream_trace`] reparents the
//!   per-request tracing span onto the extracted upstream span so server
//!   spans appear as children of the caller's span instead of starting a
//!   fresh root trace.
//!
//! Also re-exports the header bridge types from `opentelemetry-http` —
//! `HeaderExtractor` / `HeaderInjector` adapt a `http::HeaderMap` to the
//! `TextMapPropagator` interface.

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

/// Extract an OpenTelemetry [`Context`] from inbound request headers using
/// the globally-registered text-map propagator (`TraceContextPropagator`,
/// installed by `init_telemetry`).
///
/// When the request carries a valid `traceparent`, the returned context's
/// span context is valid and carries the upstream trace/span ids — pass it
/// to [`join_upstream_trace`] (or `OpenTelemetrySpanExt::set_parent`) to
/// continue the distributed trace. When no usable trace header is present
/// (the common direct-browser-hit case), the propagator returns a context
/// whose span context is **invalid** — callers MUST check
/// `ctx.span().span_context().is_valid()` before using it as a parent so an
/// untraced request stays a fresh root span rather than depending on the
/// SDK's treatment of an invalid parent.
///
/// This is the pure, testable counterpart to the outbound
/// `inject_w3c_trace_context`. It does not touch any span — it only reads
/// headers and returns a context.
///
/// [`Context`]: opentelemetry::Context
#[cfg(feature = "otel")]
pub fn extract_w3c_trace_context(headers: &http::HeaderMap) -> opentelemetry::Context {
    use opentelemetry::global;
    let extractor = HeaderExtractor(headers);
    global::get_text_map_propagator(|propagator| propagator.extract(&extractor))
}

/// Reparent `span` onto the upstream trace described by `headers`, if any.
///
/// Extracts the inbound W3C context and — only when it carries a **valid**
/// remote span context — sets it as `span`'s parent via
/// `tracing-opentelemetry`. The validity guard is the correctness contract:
///
/// - request **with** a usable `traceparent` → `span` becomes a child of
///   the caller's span (distributed trace joins);
/// - request **without** one → no parent is set, `span` stays a normal root
///   span (every untraced browser hit takes this branch).
///
/// # Ordering
///
/// Must be called **after** the span is constructed and **before** it is
/// first entered / `.instrument`-ed. The `tracing-opentelemetry` bridge
/// materializes the OTel span lazily on first poll, so a `set_parent` issued
/// after entry is dropped on the floor.
#[cfg(feature = "otel")]
pub fn join_upstream_trace(span: &tracing::Span, headers: &http::HeaderMap) {
    use opentelemetry::trace::TraceContextExt;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let parent = extract_w3c_trace_context(headers);
    if parent.span().span_context().is_valid() {
        // `set_parent` errs only when no `tracing-opentelemetry` bridge
        // layer is registered on the active subscriber — i.e. the `otel`
        // feature is on but `init_telemetry` never installed the bridge.
        // There's no upstream to join in that case, so downgrade to a
        // debug line rather than failing the request.
        if let Err(err) = span.set_parent(parent) {
            tracing::debug!(
                target: "suprnova::telemetry",
                %err,
                "could not reparent request span onto the inbound trace; \
                 is the OpenTelemetry layer installed?",
            );
        }
    }
}

/// No-op stub when the `otel` feature is disabled — there is no propagator
/// installed and no OTel span to reparent, so inbound extraction has
/// nothing to do. The per-request `tracing` span is still created by
/// `RequestIdMiddleware` for plain structured logging.
#[cfg(not(feature = "otel"))]
pub fn join_upstream_trace(_span: &tracing::Span, _headers: &http::HeaderMap) {}

#[cfg(all(test, feature = "otel"))]
mod tests {
    use super::*;
    use opentelemetry::trace::{SpanId, TraceContextExt, TraceId};

    // A fixed, spec-valid traceparent: version 00, a known 16-byte trace id,
    // a known 8-byte parent span id, sampled flag. Lifted from the W3C
    // trace-context examples so the parsed ids are easy to eyeball.
    const TRACEPARENT: &str = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";

    fn install() {
        // The propagator is a process-global. It is stateless and the
        // install is idempotent, so every test can (re)install it without
        // racing — they all end up with the same `TraceContextPropagator`.
        install_trace_context_propagator();
    }

    #[test]
    fn extract_reads_a_valid_traceparent_into_a_valid_remote_context() {
        install();
        let mut headers = http::HeaderMap::new();
        headers.insert("traceparent", TRACEPARENT.parse().unwrap());

        let cx = extract_w3c_trace_context(&headers);
        let sc = cx.span().span_context().clone();

        assert!(
            sc.is_valid(),
            "a well-formed traceparent must extract to a valid span context",
        );
        assert_eq!(
            sc.trace_id(),
            TraceId::from_hex("0af7651916cd43dd8448eb211c80319c").unwrap(),
            "extracted trace id must match the inbound traceparent",
        );
        assert_eq!(
            sc.span_id(),
            SpanId::from_hex("b7ad6b7169203331").unwrap(),
            "extracted parent span id must match the inbound traceparent",
        );
        assert!(sc.is_sampled(), "the `-01` flags byte means sampled");
    }

    #[test]
    fn extract_with_no_trace_header_yields_an_invalid_context() {
        install();
        // The common case: a direct browser hit with no `traceparent`.
        // Extraction MUST NOT fabricate a parent — the span context stays
        // invalid so `join_upstream_trace` leaves the request a root span.
        let headers = http::HeaderMap::new();

        let cx = extract_w3c_trace_context(&headers);

        assert!(
            !cx.span().span_context().is_valid(),
            "absent traceparent must yield an invalid (non-joinable) context",
        );
    }

    #[test]
    fn extract_ignores_a_malformed_traceparent() {
        install();
        let mut headers = http::HeaderMap::new();
        // Wrong shape (not 4 hyphen-delimited fields of the right widths).
        headers.insert("traceparent", "garbage-not-a-traceparent".parse().unwrap());

        let cx = extract_w3c_trace_context(&headers);

        assert!(
            !cx.span().span_context().is_valid(),
            "a malformed traceparent must not produce a joinable context",
        );
    }

    #[test]
    fn inject_then_extract_round_trips_the_trace_id() {
        use opentelemetry::Context;
        use opentelemetry::global;
        use opentelemetry::trace::{SpanContext, TraceFlags, TraceState};
        install();

        // Build a context carrying a known remote span, inject it into a
        // fresh header map exactly as the outbound HTTP client does, then
        // extract it back. Trace id surviving the round trip proves the
        // inbound and outbound halves agree on the wire format.
        let trace_id = TraceId::from_hex("0af7651916cd43dd8448eb211c80319c").unwrap();
        let span_id = SpanId::from_hex("b7ad6b7169203331").unwrap();
        let sc = SpanContext::new(
            trace_id,
            span_id,
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        );
        let cx = Context::new().with_remote_span_context(sc);

        let mut headers = http::HeaderMap::new();
        global::get_text_map_propagator(|propagator| {
            let mut injector = HeaderInjector(&mut headers);
            propagator.inject_context(&cx, &mut injector);
        });

        assert!(
            headers.contains_key("traceparent"),
            "injection must emit a traceparent header",
        );

        let extracted = extract_w3c_trace_context(&headers);
        assert_eq!(
            extracted.span().span_context().trace_id(),
            trace_id,
            "trace id must survive an inject -> extract round trip",
        );
    }
}
