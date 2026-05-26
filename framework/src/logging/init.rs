//! Initializes the global tracing subscriber. Historically called once
//! from `Server::serve()` via [`init_subscriber`]. New code should use
//! [`crate::telemetry::init_telemetry`] which also wires in OpenTelemetry
//! when the `otel` feature is enabled and an OTLP endpoint is configured.
//!
//! [`init_subscriber`] is preserved as a thin wrapper that delegates to
//! `init_telemetry` with telemetry disabled. Both entry points are
//! idempotent: a second call is a silent no-op.

use super::config::{LogConfig, LogFormat};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Build an [`EnvFilter`] from a config string, falling back to `"info"`
/// on parse failure so a malformed env var never crashes boot.
pub(crate) fn build_env_filter(level: &str) -> EnvFilter {
    EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"))
}

/// Install the global tracing subscriber from a `LogConfig`. Honors
/// the `LOG_LEVEL` env-filter syntax (e.g. `"info,sqlx=warn"`).
///
/// Idempotent. Calling more than once is a no-op (the second
/// install fails inside tracing-subscriber and we ignore the error
/// — convenient for tests).
///
/// Equivalent to calling
/// [`crate::telemetry::init_telemetry`] with
/// [`crate::telemetry::OtelConfig::disabled`].
pub fn init_subscriber(config: LogConfig) {
    let _guard = crate::telemetry::init_telemetry(config, crate::telemetry::OtelConfig::disabled());
    // The guard's Drop emits a warning if shutdown() was not called, but
    // for the legacy callers we explicitly mark it as shutdown-acknowledged
    // by suppressing the drop warning. Disabled-mode guards have no
    // providers to flush, so we just forget the in-process bookkeeping.
    _guard.mark_shutdown_for_legacy();
}

/// Internal helper used by `init_telemetry` to install the (non-OTel) part
/// of the subscriber. Returns whether install actually succeeded — a
/// duplicate install (e.g. inside tests) is silently ignored.
pub(crate) fn install_base_subscriber(config: &LogConfig) -> bool {
    let env_filter = build_env_filter(&config.level);
    let result = match config.format {
        LogFormat::Pretty => tracing_subscriber::registry()
            .with(env_filter)
            .with(
                fmt::layer()
                    .with_target(true)
                    .with_thread_ids(false)
                    .pretty(),
            )
            .try_init(),
        LogFormat::Json => tracing_subscriber::registry()
            .with(env_filter)
            .with(
                fmt::layer()
                    .json()
                    .with_target(true)
                    .with_current_span(true),
            )
            .try_init(),
    };
    result.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_is_idempotent() {
        // Calling twice must not panic. (tracing-subscriber returns
        // Err on duplicate global default; we swallow it.)
        init_subscriber(LogConfig::default());
        init_subscriber(LogConfig::default());
    }
}
