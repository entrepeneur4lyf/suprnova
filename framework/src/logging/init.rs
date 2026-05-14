//! Initializes the global tracing subscriber. Called once from
//! `Server::serve()`. Safe to call multiple times; the second call
//! returns silently.

use super::config::{LogConfig, LogFormat};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Install the global tracing subscriber from a `LogConfig`. Honors
/// the `LOG_LEVEL` env-filter syntax (e.g. `"info,sqlx=warn"`).
///
/// Idempotent. Calling more than once is a no-op (the second
/// install fails inside tracing-subscriber and we ignore the error
/// — convenient for tests).
pub fn init_subscriber(config: LogConfig) {
    let env_filter =
        EnvFilter::try_new(&config.level).unwrap_or_else(|_| EnvFilter::new("info"));

    let registry = tracing_subscriber::registry().with(env_filter);

    let result = match config.format {
        LogFormat::Pretty => registry
            .with(
                fmt::layer()
                    .with_target(true)
                    .with_thread_ids(false)
                    .pretty(),
            )
            .try_init(),
        LogFormat::Json => registry
            .with(
                fmt::layer()
                    .json()
                    .with_target(true)
                    .with_current_span(true),
            )
            .try_init(),
    };

    // Ignore "a global default subscriber has already been set" — safe to re-init.
    let _ = result;
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
