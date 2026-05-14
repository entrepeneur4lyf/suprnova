//! Smoke tests for the [`TelemetryGuard`] shutdown path used by
//! [`suprnova::Server::run`]. We don't bring up a real server here —
//! signal handling is exercised manually. These tests pin down the
//! invariant that shutdown is safe to call when no providers are
//! installed (the default-feature, no-endpoint configuration).

use suprnova::{init_telemetry, LogConfig, OtelConfig};

#[tokio::test]
async fn telemetry_guard_shutdown_is_safe_without_providers() {
    let guard = init_telemetry(LogConfig::default(), OtelConfig::disabled());
    // Disabled-mode guards have nothing to flush; shutdown must not
    // panic and must not block indefinitely.
    guard.shutdown().await;
}

#[tokio::test]
async fn telemetry_guard_repeated_shutdown_is_noop() {
    // Two consecutive shutdowns on freshly-built guards must both
    // succeed. (Each call consumes self; we build two separate guards
    // and shut both down — this validates the "safe to call once" path
    // back-to-back from independent boot sequences.)
    let g1 = init_telemetry(LogConfig::default(), OtelConfig::disabled());
    g1.shutdown().await;
    let g2 = init_telemetry(LogConfig::default(), OtelConfig::disabled());
    g2.shutdown().await;
}
