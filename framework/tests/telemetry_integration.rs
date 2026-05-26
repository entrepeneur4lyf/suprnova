//! Live-collector smoke test for the OTel export bridge.
//!
//! `#[ignore]` by default — the test reaches out to whatever is at
//! `OTEL_EXPORTER_OTLP_ENDPOINT` and only proves anything if a real
//! collector is listening. Run manually with:
//!
//! ```bash
//! OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318 \
//!   cargo test -p suprnova --features otel \
//!   --test telemetry_integration -- --ignored
//! ```
//!
//! The test produces one named span (`integration.test.span`) and one
//! counter increment (`integration.test.counter`). On the collector
//! side, both should show up tagged with `service.name=suprnova`
//! (or whatever `OTEL_SERVICE_NAME` was set to).

#![cfg(feature = "otel")]

use suprnova::{LogConfig, Metrics, OtelConfig, init_telemetry};

#[tokio::test]
#[ignore = "requires a live OTLP collector reachable at OTEL_EXPORTER_OTLP_ENDPOINT"]
async fn traces_reach_collector() {
    let guard = init_telemetry(LogConfig::default(), OtelConfig::from_env());

    let span = tracing::info_span!("integration.test.span", test_run = true);
    let _enter = span.enter();
    Metrics::counter("integration.test.counter").inc();
    drop(_enter);

    // shutdown() awaits the batch flush so the data lands on the
    // collector before the test process exits.
    guard.shutdown().await;
}
