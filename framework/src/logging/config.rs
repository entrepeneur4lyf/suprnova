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
    /// (`"pretty"` | `"json"`, default `"pretty"`).
    pub fn from_env() -> Self {
        let level = env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
        let format = match env::var("LOG_FORMAT").as_deref() {
            Ok("json") => LogFormat::Json,
            _ => LogFormat::Pretty,
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
        }
        let cfg = LogConfig::from_env();
        assert_eq!(cfg.level, "info");
        assert!(matches!(cfg.format, LogFormat::Pretty));
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
