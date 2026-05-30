//! Initializes the global tracing subscriber. Historically called once
//! from `Server::serve()` via [`init_subscriber`]. New code should use
//! [`crate::telemetry::init_telemetry`] which also wires in OpenTelemetry
//! when the `otel` feature is enabled and an OTLP endpoint is configured.
//!
//! [`init_subscriber`] is preserved as a thin wrapper that delegates to
//! `init_telemetry` with telemetry disabled. Both entry points are
//! idempotent: a second call leaves the existing subscriber in place and
//! emits a `tracing::warn!` so an operator can see when a fresh
//! `LogConfig` was NOT applied.

use super::config::{LogConfig, LogFormat};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Parse a `tracing-subscriber` env-filter directive (the `LOG_LEVEL`
/// syntax, e.g. `"info,sqlx=warn"`), returning the parse error as a
/// string on failure. Split out from [`build_env_filter`] so the
/// validity decision is unit-testable without installing a subscriber.
pub(crate) fn parse_env_filter(level: &str) -> Result<EnvFilter, String> {
    EnvFilter::try_new(level).map_err(|e| e.to_string())
}

/// Build an [`EnvFilter`] from a config string, falling back to `"info"`
/// on parse failure so a malformed env var never crashes boot.
///
/// A malformed directive is a real misconfiguration, so it is reported on
/// stderr: this runs while installing the subscriber, before any global
/// `tracing` subscriber is guaranteed to exist, so a `tracing::warn!`
/// could be silently dropped — stderr is always visible to the operator.
pub(crate) fn build_env_filter(level: &str) -> EnvFilter {
    match parse_env_filter(level) {
        Ok(filter) => filter,
        Err(e) => {
            eprintln!(
                "suprnova: invalid LOG_LEVEL directive {level:?} ({e}); \
                 falling back to \"info\". Fix LOG_LEVEL to silence this."
            );
            EnvFilter::new("info")
        }
    }
}

/// Install the global tracing subscriber from a `LogConfig`. Honors
/// the `LOG_LEVEL` env-filter syntax (e.g. `"info,sqlx=warn"`).
///
/// Idempotent. Calling more than once leaves the existing global
/// subscriber in place; the second attempt is reported through that
/// subscriber as a `tracing::warn!` so the new `LogConfig` not being
/// applied is operator-visible.
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
/// duplicate install (e.g. inside tests) leaves the existing subscriber
/// in place and emits a `tracing::warn!` through it so the operator can
/// see that this `LogConfig` was not applied.
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
    let installed = result.is_ok();
    if !installed {
        // A global subscriber is already installed (common in tests, or an
        // embedder that initialises logging more than once). The existing
        // one wins and this `LogConfig` was NOT applied. `try_init` only
        // fails when a subscriber is already in place, so that subscriber
        // is guaranteed to receive this `warn!` — `warn!` rather than
        // `debug!` so an info-level production filter still surfaces it,
        // since the bool return value tends to be ignored by callers.
        tracing::warn!(
            "tracing subscriber already installed; keeping the existing one (this LogConfig was not applied)"
        );
    }
    installed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_is_idempotent() {
        // Calling twice must not panic. (tracing-subscriber returns
        // Err on duplicate global default; the second attempt logs a
        // warn through the existing subscriber and reports false.)
        init_subscriber(LogConfig::default());
        init_subscriber(LogConfig::default());
    }

    #[test]
    fn parse_env_filter_accepts_valid_directives() {
        assert!(parse_env_filter("info").is_ok());
        assert!(parse_env_filter("debug,hyper=warn,sqlx=info").is_ok());
    }

    #[test]
    fn parse_env_filter_rejects_invalid_directive() {
        // An invalid level name after `=` is a parse error — `build_env_filter`
        // surfaces it on stderr instead of silently falling back unannounced.
        assert!(parse_env_filter("app=notalevel").is_err());
    }

    #[test]
    fn install_base_subscriber_reports_duplicate_install_as_not_applied() {
        // After at least one install in the process, a subsequent attempt
        // must report `false`: the existing global subscriber wins and the
        // new `LogConfig` is not applied. The first call may itself be a
        // duplicate if another test already installed the default — either
        // way the second call is deterministically not-applied.
        let _first = install_base_subscriber(&LogConfig::default());
        let second = install_base_subscriber(&LogConfig::default());
        assert!(
            !second,
            "a duplicate subscriber install must report not-applied"
        );
    }
}
