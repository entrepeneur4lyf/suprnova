//! Configuration for tracing/log output. Read from environment so
//! consumers can change verbosity without recompiling.

use std::env;

/// Output format for log lines.
#[derive(Debug, Clone, Copy)]
pub enum LogFormat {
    /// Human-friendly multi-line output. Default for dev.
    Pretty,
    /// One-JSON-object-per-line. Default for production / log aggregators.
    Json,
}

/// Logging configuration.
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// `tracing-subscriber` env-filter directive
    /// (e.g. `"info"`, `"debug,hyper=warn,sqlx=info"`).
    pub level: String,
    /// Output format.
    pub format: LogFormat,
}

impl LogConfig {
    /// Read from `LOG_LEVEL` (default `"info"`) and `LOG_FORMAT`
    /// (`"pretty"` | `"json"`).
    ///
    /// When `LOG_FORMAT` is unset, the default is environment-aware:
    /// `json` in production (the log-aggregator-friendly format
    /// [`LogFormat::Json`] documents as the production default) and
    /// `pretty` everywhere else for human-readable local/dev output. An
    /// explicit `LOG_FORMAT` always wins over this default. The
    /// environment is detected from `APP_ENV` via
    /// [`Environment::detect`](crate::config::Environment::detect).
    pub fn from_env() -> Self {
        let level = env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
        let format = match env::var("LOG_FORMAT").as_deref() {
            Ok("json") => LogFormat::Json,
            Ok("pretty") => LogFormat::Pretty,
            _ => {
                if crate::config::Environment::detect().is_production() {
                    LogFormat::Json
                } else {
                    LogFormat::Pretty
                }
            }
        };
        Self { level, format }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // tests in this module touch the global env, so they need to run sequentially.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn from_env_defaults_to_info_pretty() {
        let _g = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK serializes env access within this module
        unsafe {
            std::env::remove_var("LOG_LEVEL");
            std::env::remove_var("LOG_FORMAT");
            // The format default is environment-aware; clear APP_ENV so a
            // leaked `production` from another test can't flip this to JSON.
            std::env::remove_var("APP_ENV");
        }
        let cfg = LogConfig::from_env();
        assert_eq!(cfg.level, "info");
        assert!(matches!(cfg.format, LogFormat::Pretty));
    }

    #[test]
    fn from_env_defaults_to_json_in_production() {
        let _g = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK serializes env access within this module
        unsafe {
            std::env::remove_var("LOG_FORMAT");
            std::env::set_var("APP_ENV", "production");
        }
        let cfg = LogConfig::from_env();
        assert!(
            matches!(cfg.format, LogFormat::Json),
            "production must default to JSON logs when LOG_FORMAT is unset"
        );
        unsafe {
            std::env::remove_var("APP_ENV");
        }
    }

    #[test]
    fn explicit_log_format_wins_over_production_default() {
        let _g = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK serializes env access within this module
        unsafe {
            std::env::set_var("APP_ENV", "production");
            std::env::set_var("LOG_FORMAT", "pretty");
        }
        let cfg = LogConfig::from_env();
        assert!(
            matches!(cfg.format, LogFormat::Pretty),
            "an explicit LOG_FORMAT must override the production JSON default"
        );
        unsafe {
            std::env::remove_var("APP_ENV");
            std::env::remove_var("LOG_FORMAT");
        }
    }

    #[test]
    fn from_env_reads_overrides() {
        let _g = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK serializes env access within this module
        unsafe {
            std::env::set_var("LOG_LEVEL", "debug,hyper=warn");
            std::env::set_var("LOG_FORMAT", "json");
        }
        let cfg = LogConfig::from_env();
        assert_eq!(cfg.level, "debug,hyper=warn");
        assert!(matches!(cfg.format, LogFormat::Json));
        // Cleanup so other tests see a fresh env
        unsafe {
            std::env::remove_var("LOG_LEVEL");
            std::env::remove_var("LOG_FORMAT");
        }
    }
}
