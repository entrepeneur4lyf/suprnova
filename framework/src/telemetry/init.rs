//! `init_telemetry` — the unified entry point that wires `tracing` and
//! (optionally) the OpenTelemetry SDK pipelines into a single subscriber.
//!
//! See [`crate::telemetry`] for the high-level design.

use crate::logging::config::LogConfig;
use crate::logging::init::install_base_subscriber;
use std::env;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Environment-driven OpenTelemetry configuration.
///
/// This struct models only the handful of vars Suprnova reads itself to
/// decide *whether* and *as whom* to export:
///
/// | Field            | Env var                          | Default                         |
/// |------------------|----------------------------------|---------------------------------|
/// | `endpoint`       | `OTEL_EXPORTER_OTLP_ENDPOINT`    | _unset_ → telemetry disabled    |
/// | `service_name`   | `OTEL_SERVICE_NAME`              | `"suprnova"`                    |
/// | `service_version`| `OTEL_SERVICE_VERSION`           | `CARGO_PKG_VERSION` at compile  |
/// | `disabled`       | `OTEL_SDK_DISABLED` (case-insensitive `true` / `1`) | `false`     |
///
/// Telemetry is "enabled" when `endpoint` is `Some` **and** `disabled` is
/// `false`. The endpoint is read once at process start; runtime mutation
/// is unsupported.
///
/// **The rest of the standard OTLP knobs are read by the SDK, not here.**
/// `OTEL_EXPORTER_OTLP_HEADERS` (collector auth), `_PROTOCOL`, `_TIMEOUT`,
/// and `_COMPRESSION` are consumed directly by the `opentelemetry-otlp`
/// exporter builders when `init_telemetry` calls `.build()` — so operators
/// get the standard behavior without Suprnova re-modeling each one. The one
/// value Suprnova sets explicitly is the endpoint (via `.with_endpoint`),
/// which means a per-signal override like `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`
/// is currently shadowed by the base `OTEL_EXPORTER_OTLP_ENDPOINT` for all
/// three signals — see `MODULE_REVIEW_NOTES` for that known limitation.
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
        let service_name = env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "suprnova".to_string());
        let service_version = env::var("OTEL_SERVICE_VERSION")
            .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
        let disabled = parse_sdk_disabled(env::var("OTEL_SDK_DISABLED").ok().as_deref());
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

/// Parse the `OTEL_SDK_DISABLED` value into a boolean.
///
/// The OTel spec treats it as a case-insensitive boolean ("true"/"false").
/// We accept any case of `true`, plus the common `1` convention; everything
/// else (including `false`, `0`, empty, and unset) leaves telemetry enabled.
/// Pulled out as a pure function so the parsing contract is unit-testable
/// without mutating process-global environment state.
fn parse_sdk_disabled(value: Option<&str>) -> bool {
    value
        .map(|v| {
            let v = v.trim();
            v.eq_ignore_ascii_case("true") || v == "1"
        })
        .unwrap_or(false)
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
    #[cfg(feature = "otel")]
    tracer_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
    #[cfg(feature = "otel")]
    meter_provider: Option<opentelemetry_sdk::metrics::SdkMeterProvider>,
    #[cfg(feature = "otel")]
    logger_provider: Option<opentelemetry_sdk::logs::SdkLoggerProvider>,
}

impl TelemetryGuard {
    /// `true` when this guard owns at least one live SDK provider that
    /// still needs an explicit flush. The Drop warning is gated on this —
    /// a guard with no providers (the disabled path, the legacy
    /// `init_subscriber` path, or any non-`otel` build) has nothing to
    /// lose on drop and must stay silent.
    #[cfg(feature = "otel")]
    fn owns_providers(&self) -> bool {
        self.tracer_provider.is_some()
            || self.meter_provider.is_some()
            || self.logger_provider.is_some()
    }

    /// Without the `otel` feature there are no providers to own.
    #[cfg(not(feature = "otel"))]
    fn owns_providers(&self) -> bool {
        false
    }

    /// Mark this guard as "shutdown" without invoking provider flush —
    /// used by the legacy `init_subscriber` path. That path holds no
    /// providers, so [`Self::owns_providers`] already keeps Drop silent;
    /// this additionally records the shutdown so the state is unambiguous.
    pub(crate) fn mark_shutdown_for_legacy(self) {
        self.shutdown_called.store(true, Ordering::SeqCst);
    }

    /// Flush and shut down all installed OpenTelemetry providers.
    ///
    /// This is async because the batch processors flush buffered data
    /// to the collector over HTTP. It is safe to call exactly once;
    /// subsequent calls are no-ops.
    pub async fn shutdown(self) {
        // Mark shutdown so the `Drop` impl doesn't warn about a lost flush.
        // `shutdown` takes `self` by value, so it runs at most once.
        self.shutdown_called.store(true, Ordering::SeqCst);
        #[cfg(feature = "otel")]
        {
            if let Some(provider) = &self.tracer_provider
                && let Err(err) = provider.shutdown()
            {
                tracing::warn!(?err, "OTel tracer provider shutdown error");
            }
            if let Some(provider) = &self.meter_provider
                && let Err(err) = provider.shutdown()
            {
                tracing::warn!(?err, "OTel meter provider shutdown error");
            }
            if let Some(provider) = &self.logger_provider
                && let Err(err) = provider.shutdown()
            {
                tracing::warn!(?err, "OTel logger provider shutdown error");
            }
        }
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        // Warn only when we hold providers that were never flushed.
        // Guards with no providers (disabled path, legacy subscriber path,
        // non-`otel` builds) have nothing buffered, so a silent drop is
        // correct — warning there would be pure noise on every process that
        // runs without a collector configured.
        if self.owns_providers() && !self.shutdown_called.load(Ordering::SeqCst) {
            tracing::warn!(
                "TelemetryGuard dropped without shutdown() — buffered \
                 telemetry may be lost. Call guard.shutdown().await before \
                 exiting."
            );
        }
    }
}

/// Build a [`TelemetryGuard`] with no provider handles. Used by the
/// disabled / no-feature paths. Holds no providers, so its Drop is silent.
fn empty_guard() -> TelemetryGuard {
    TelemetryGuard {
        shutdown_called: Arc::new(AtomicBool::new(false)),
        #[cfg(feature = "otel")]
        tracer_provider: None,
        #[cfg(feature = "otel")]
        meter_provider: None,
        #[cfg(feature = "otel")]
        logger_provider: None,
    }
}

/// Install the global `tracing` subscriber and (when applicable) the
/// OpenTelemetry SDK pipelines.
///
/// Behavior:
///
/// 1. Always installs the standard fmt layer driven by [`LogConfig`].
/// 2. When compiled with `feature = "otel"` **and** `otel_config.is_enabled()`,
///    additionally:
///    - builds OTLP HTTP-proto exporters for traces, metrics, and logs;
///    - wraps each in an SDK provider with the configured service-name
///      resource;
///    - installs the providers globally so any code can call
///      `opentelemetry::global::tracer(...)` / `meter(...)`;
///    - installs a `TraceContextPropagator` (from `opentelemetry_sdk::propagation`)
///      for W3C trace-context propagation;
///    - registers a `tracing-opentelemetry` layer so every `tracing::span`
///      becomes an OTel span automatically;
///    - registers the `opentelemetry-appender-tracing` bridge so every
///      `tracing::event` is forwarded to the OTel log pipeline as well.
///
/// Idempotent: a second call is a no-op (the subscriber install returns
/// an error which we silently absorb so tests can call this repeatedly).
pub fn init_telemetry(log_config: LogConfig, otel_config: OtelConfig) -> TelemetryGuard {
    #[cfg(feature = "otel")]
    {
        if otel_config.is_enabled() {
            return init_telemetry_with_otel(log_config, otel_config);
        }
    }
    let _ = otel_config; // silence unused warning when feature is off
    install_base_subscriber(&log_config);
    empty_guard()
}

#[cfg(feature = "otel")]
fn init_telemetry_with_otel(log_config: LogConfig, otel_config: OtelConfig) -> TelemetryGuard {
    use crate::logging::config::LogFormat;
    use crate::logging::init::build_env_filter;
    use opentelemetry::KeyValue;
    use opentelemetry::global;
    use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::logs::SdkLoggerProvider;
    use opentelemetry_sdk::metrics::SdkMeterProvider;
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use opentelemetry_semantic_conventions::resource as semconv;
    use tracing_subscriber::fmt;
    use tracing_subscriber::layer::SubscriberExt;

    // Endpoint is guaranteed Some by is_enabled(); unwrap is safe.
    let endpoint = otel_config.endpoint.clone().unwrap_or_default();

    // Resource is shared across all three signals.
    let resource = Resource::builder()
        .with_attributes(vec![
            KeyValue::new(semconv::SERVICE_NAME, otel_config.service_name.clone()),
            KeyValue::new(
                semconv::SERVICE_VERSION,
                otel_config.service_version.clone(),
            ),
        ])
        .build();

    // --- Traces ---
    let span_exporter = match opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(&endpoint)
        .build()
    {
        Ok(exp) => exp,
        Err(err) => {
            tracing::error!(
                ?err,
                "failed to build OTLP span exporter; continuing without traces"
            );
            install_base_subscriber(&log_config);
            return empty_guard();
        }
    };
    let tracer_provider = SdkTracerProvider::builder()
        .with_batch_exporter(span_exporter)
        .with_resource(resource.clone())
        .build();
    global::set_tracer_provider(tracer_provider.clone());

    // --- Metrics ---
    let metric_exporter = match opentelemetry_otlp::MetricExporter::builder()
        .with_http()
        .with_endpoint(&endpoint)
        .build()
    {
        Ok(exp) => exp,
        Err(err) => {
            tracing::error!(
                ?err,
                "failed to build OTLP metric exporter; continuing without metrics"
            );
            // Tracer is installed; still safe. Keep returning an
            // OTel-enabled guard so traces still flush on shutdown.
            install_base_subscriber(&log_config);
            return TelemetryGuard {
                shutdown_called: Arc::new(AtomicBool::new(false)),
                tracer_provider: Some(tracer_provider),
                meter_provider: None,
                logger_provider: None,
            };
        }
    };
    let meter_provider = SdkMeterProvider::builder()
        .with_periodic_exporter(metric_exporter)
        .with_resource(resource.clone())
        .build();
    global::set_meter_provider(meter_provider.clone());

    // --- Logs ---
    let log_exporter = match opentelemetry_otlp::LogExporter::builder()
        .with_http()
        .with_endpoint(&endpoint)
        .build()
    {
        Ok(exp) => exp,
        Err(err) => {
            tracing::error!(
                ?err,
                "failed to build OTLP log exporter; continuing without log export"
            );
            install_base_subscriber(&log_config);
            return TelemetryGuard {
                shutdown_called: Arc::new(AtomicBool::new(false)),
                tracer_provider: Some(tracer_provider),
                meter_provider: Some(meter_provider),
                logger_provider: None,
            };
        }
    };
    let logger_provider = SdkLoggerProvider::builder()
        .with_batch_exporter(log_exporter)
        .with_resource(resource)
        .build();

    // --- Propagation ---
    crate::telemetry::propagation::install_trace_context_propagator();

    // --- Wire layers into the global subscriber ---
    //
    // `OpenTelemetryLayer<S, T>` is parameterized on the subscriber type
    // `S` it wraps, so we have to build the bridge layers fresh inside
    // each format arm — the inferred `S` differs between Pretty and Json
    // (different concrete fmt::Layer types) and a single layer instance
    // can only commit to one `S`.
    let env_filter = build_env_filter(&log_config.level);
    let tracer = global::tracer("suprnova");

    // try_init() returns Err if a global default is already set (e.g.
    // tests). That's fine; the existing subscriber wins and we still
    // hand back a guard for orderly shutdown of the providers we built.
    match log_config.format {
        LogFormat::Pretty => {
            let subscriber = tracing_subscriber::registry()
                .with(env_filter)
                .with(
                    fmt::layer()
                        .with_target(true)
                        .with_thread_ids(false)
                        .pretty(),
                )
                .with(tracing_opentelemetry::layer().with_tracer(tracer))
                .with(OpenTelemetryTracingBridge::new(&logger_provider));
            let _ = tracing::subscriber::set_global_default(subscriber);
        }
        LogFormat::Json => {
            let subscriber = tracing_subscriber::registry()
                .with(env_filter)
                .with(
                    fmt::layer()
                        .json()
                        .with_target(true)
                        .with_current_span(true),
                )
                .with(tracing_opentelemetry::layer().with_tracer(tracer))
                .with(OpenTelemetryTracingBridge::new(&logger_provider));
            let _ = tracing::subscriber::set_global_default(subscriber);
        }
    }

    TelemetryGuard {
        shutdown_called: Arc::new(AtomicBool::new(false)),
        tracer_provider: Some(tracer_provider),
        meter_provider: Some(meter_provider),
        logger_provider: Some(logger_provider),
    }
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

    #[cfg(feature = "otel")]
    #[test]
    fn init_telemetry_no_endpoint_stays_noop() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        let guard = init_telemetry(LogConfig::default(), OtelConfig::from_env());
        assert!(guard.tracer_provider.is_none());
        assert!(guard.meter_provider.is_none());
        assert!(guard.logger_provider.is_none());
        // Acknowledge the guard so Drop doesn't warn.
        guard.mark_shutdown_for_legacy();
    }

    // ---- OTEL_SDK_DISABLED parse (no env mutation needed) -------------

    #[test]
    fn sdk_disabled_accepts_case_insensitive_true_and_one() {
        for v in ["true", "True", "TRUE", "tRuE", "1"] {
            assert!(parse_sdk_disabled(Some(v)), "{v:?} should disable the SDK",);
        }
    }

    #[test]
    fn sdk_disabled_trims_surrounding_whitespace() {
        assert!(parse_sdk_disabled(Some("  true  ")));
        assert!(parse_sdk_disabled(Some(" 1 ")));
    }

    #[test]
    fn sdk_disabled_leaves_telemetry_enabled_for_other_values() {
        // Unset, explicit false, zero, and arbitrary text all mean "enabled".
        for v in [
            None,
            Some("false"),
            Some("FALSE"),
            Some("0"),
            Some("yes"),
            Some(""),
        ] {
            assert!(!parse_sdk_disabled(v), "{v:?} must NOT disable the SDK",);
        }
    }

    // ---- empty / disabled guard drop is silent ------------------------

    #[test]
    fn empty_guard_owns_no_providers_so_drop_is_silent() {
        // The disabled path returns `empty_guard()`. It holds no providers,
        // so `owns_providers()` is false and Drop must not warn about lost
        // telemetry — there is nothing buffered. Regression guard for the
        // spurious "buffered telemetry may be lost" warning that fired on
        // every collector-less process before this fix.
        let guard = empty_guard();
        assert!(
            !guard.owns_providers(),
            "a guard with no providers must report owns_providers() == false",
        );
        // Drop runs here with shutdown_called still false; the assertion
        // above pins the invariant the Drop warning is gated on. (A
        // subscriber-capture assertion would need global subscriber state,
        // which collides with parallel tests — the owns_providers gate is
        // the deterministic core.)
        drop(guard);
    }
}
