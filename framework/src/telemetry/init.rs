//! `init_telemetry` — the unified entry point that wires `tracing` and
//! (optionally) the OpenTelemetry SDK pipelines into a single subscriber.
//!
//! See [`crate::telemetry`] for the high-level design.

use crate::logging::config::LogConfig;
use crate::logging::init::install_base_subscriber;
use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Environment-driven OpenTelemetry configuration.
///
/// Mirrors the standard OTel environment variables:
///
/// | Field            | Env var                          | Default                         |
/// |------------------|----------------------------------|---------------------------------|
/// | `endpoint`       | `OTEL_EXPORTER_OTLP_ENDPOINT`    | _unset_ → telemetry disabled    |
/// | `service_name`   | `OTEL_SERVICE_NAME`              | `"suprnova"`                    |
/// | `service_version`| `OTEL_SERVICE_VERSION`           | `CARGO_PKG_VERSION` at compile  |
/// | `disabled`       | `OTEL_SDK_DISABLED=true`         | `false`                         |
///
/// Telemetry is "enabled" when `endpoint` is `Some` **and** `disabled` is
/// `false`. The endpoint is read once at process start; runtime mutation
/// is unsupported.
#[derive(Debug, Clone)]
pub struct OtelConfig {
    /// OTLP collector base URL (e.g. `http://localhost:4318`).
    pub endpoint: Option<String>,
    /// `service.name` resource attribute reported on every span / metric / log.
    pub service_name: String,
    /// `service.version` resource attribute.
    pub service_version: String,
    /// Honors the standard `OTEL_SDK_DISABLED=true` kill switch.
    pub disabled: bool,
}

impl OtelConfig {
    /// Read configuration from the environment. Never panics; missing
    /// vars fall back to defaults and the caller can inspect
    /// [`Self::is_enabled`] to decide whether to install exporters.
    pub fn from_env() -> Self {
        let endpoint = env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok().and_then(|s| {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
        let service_name =
            env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "suprnova".to_string());
        let service_version = env::var("OTEL_SERVICE_VERSION")
            .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
        let disabled = matches!(
            env::var("OTEL_SDK_DISABLED").as_deref(),
            Ok("true") | Ok("TRUE") | Ok("1")
        );
        Self {
            endpoint,
            service_name,
            service_version,
            disabled,
        }
    }

    /// Sentinel value: telemetry is explicitly off. Used by
    /// [`crate::logging::init_subscriber`] for the legacy non-OTel path.
    pub fn disabled() -> Self {
        Self {
            endpoint: None,
            service_name: "suprnova".to_string(),
            service_version: env!("CARGO_PKG_VERSION").to_string(),
            disabled: true,
        }
    }

    /// Telemetry is enabled iff an endpoint is configured **and** the
    /// `OTEL_SDK_DISABLED` kill switch is not set.
    pub fn is_enabled(&self) -> bool {
        self.endpoint.is_some() && !self.disabled
    }
}

/// RAII handle returned from [`init_telemetry`]. Owns the SDK provider
/// instances so they can be flushed deterministically on shutdown.
///
/// Call [`shutdown`](Self::shutdown) before the process exits. Dropping
/// the guard without calling `shutdown` emits a warning via `tracing`
/// because batch processors buffer span/metric/log payloads in memory —
/// silently dropping the guard would silently drop telemetry.
///
/// The guard is `Send + Sync` so it can be moved into spawned tasks if
/// needed (e.g. the server keeps it on the main task and flushes on
/// signal).
pub struct TelemetryGuard {
    shutdown_called: Arc<AtomicBool>,
    /// True for guards produced by the legacy `init_subscriber` path —
    /// those have no providers to flush and shouldn't emit a drop warning.
    legacy: bool,
    #[cfg(feature = "otel")]
    tracer_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
    #[cfg(feature = "otel")]
    meter_provider: Option<opentelemetry_sdk::metrics::SdkMeterProvider>,
    #[cfg(feature = "otel")]
    logger_provider: Option<opentelemetry_sdk::logs::SdkLoggerProvider>,
}

impl TelemetryGuard {
    /// Mark this guard as "shutdown" without actually invoking provider
    /// flush — used by the legacy `init_subscriber` path which has no
    /// providers to flush. Suppresses the Drop warning.
    pub(crate) fn mark_shutdown_for_legacy(mut self) {
        // Setting `legacy` ensures Drop is silent.
        self.legacy = true;
        self.shutdown_called.store(true, Ordering::SeqCst);
    }

    /// Flush and shut down all installed OpenTelemetry providers.
    ///
    /// This is async because the batch processors flush buffered data
    /// to the collector over HTTP. It is safe to call exactly once;
    /// subsequent calls are no-ops.
    pub async fn shutdown(self) {
        if self.shutdown_called.swap(true, Ordering::SeqCst) {
            return;
        }
        #[cfg(feature = "otel")]
        {
            if let Some(provider) = &self.tracer_provider {
                if let Err(err) = provider.shutdown() {
                    tracing::warn!(?err, "OTel tracer provider shutdown error");
                }
            }
            if let Some(provider) = &self.meter_provider {
                if let Err(err) = provider.shutdown() {
                    tracing::warn!(?err, "OTel meter provider shutdown error");
                }
            }
            if let Some(provider) = &self.logger_provider {
                if let Err(err) = provider.shutdown() {
                    tracing::warn!(?err, "OTel logger provider shutdown error");
                }
            }
        }
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if !self.legacy && !self.shutdown_called.load(Ordering::SeqCst) {
            tracing::warn!(
                "TelemetryGuard dropped without shutdown() — buffered \
                 telemetry may be lost. Call guard.shutdown().await before \
                 exiting."
            );
        }
    }
}

/// Build a [`TelemetryGuard`] with no provider handles. Used by the
/// disabled / no-feature paths.
fn empty_guard() -> TelemetryGuard {
    TelemetryGuard {
        shutdown_called: Arc::new(AtomicBool::new(false)),
        legacy: false,
        #[cfg(feature = "otel")]
        tracer_provider: None,
        #[cfg(feature = "otel")]
        meter_provider: None,
        #[cfg(feature = "otel")]
        logger_provider: None,
    }
}

/// Install the global `tracing` subscriber and (later) the OpenTelemetry
/// SDK pipelines.
///
/// Currently installs the standard fmt layer driven by [`LogConfig`].
/// The OTel provider installation is wired in the next sub-task.
///
/// Idempotent: a second call is a no-op (the subscriber install returns
/// an error which we silently absorb so tests can call this repeatedly).
pub fn init_telemetry(log_config: LogConfig, otel_config: OtelConfig) -> TelemetryGuard {
    let _ = &otel_config; // silence unused warning until the OTel block lands
    install_base_subscriber(&log_config);
    empty_guard()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Same env-serialization pattern as `crate::logging::config` —
    // tests in this module touch global env so they must run sequentially.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_env() {
        // SAFETY: ENV_LOCK guards concurrent env mutation within this module.
        unsafe {
            std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
            std::env::remove_var("OTEL_SERVICE_NAME");
            std::env::remove_var("OTEL_SERVICE_VERSION");
            std::env::remove_var("OTEL_SDK_DISABLED");
        }
    }

    #[test]
    fn otel_config_disabled_sentinel() {
        let cfg = OtelConfig::disabled();
        assert!(!cfg.is_enabled());
        assert!(cfg.disabled);
        assert!(cfg.endpoint.is_none());
    }

    #[test]
    fn otel_config_from_env_no_endpoint() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        let cfg = OtelConfig::from_env();
        assert!(!cfg.is_enabled());
        assert_eq!(cfg.service_name, "suprnova");
        assert_eq!(cfg.service_version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn otel_config_from_env_with_endpoint() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        // SAFETY: ENV_LOCK serializes env access.
        unsafe {
            std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", "http://localhost:4318");
            std::env::set_var("OTEL_SERVICE_NAME", "test-service");
        }
        let cfg = OtelConfig::from_env();
        assert!(cfg.is_enabled());
        assert_eq!(cfg.endpoint.as_deref(), Some("http://localhost:4318"));
        assert_eq!(cfg.service_name, "test-service");
        clear_env();
    }

    #[test]
    fn otel_config_sdk_disabled_flag() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        // SAFETY: ENV_LOCK serializes env access.
        unsafe {
            std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", "http://localhost:4318");
            std::env::set_var("OTEL_SDK_DISABLED", "true");
        }
        let cfg = OtelConfig::from_env();
        // Endpoint set but kill switch wins.
        assert!(!cfg.is_enabled());
        assert!(cfg.disabled);
        clear_env();
    }

}
