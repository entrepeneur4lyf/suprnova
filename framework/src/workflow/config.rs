//! Workflow configuration

use crate::config::env;
use crate::error::FrameworkError;

/// Minimum allowed `concurrency`. Zero would make the worker semaphore
/// permanently saturated (`acquire_owned()` parks forever); the worker
/// would never process anything.
pub const MIN_CONCURRENCY: usize = 1;

/// Minimum allowed `max_attempts`. A value below 1 prevents any attempt
/// from running (`attempts < max_attempts` is never true after the
/// first claim increments `attempts` to 1).
pub const MIN_MAX_ATTEMPTS: i32 = 1;

/// Workflow configuration
///
/// # Environment Variables
///
/// - `WORKFLOW_POLL_INTERVAL_MS` - Worker poll interval in milliseconds (default: 1000)
/// - `WORKFLOW_CONCURRENCY` - Number of workflows to process concurrently (default: 4, min: 1)
/// - `WORKFLOW_LOCK_TIMEOUT_SECS` - Lease duration in seconds (default: 30)
/// - `WORKFLOW_MAX_ATTEMPTS` - Max workflow attempts (default: 3, min: 1)
/// - `WORKFLOW_RETRY_BACKOFF_SECS` - Linear backoff seconds (default: 5, min: 0)
///
/// # Validation
///
/// Out-of-range environment values are clamped (with a structured warning
/// emitted via `tracing`) so a typo in `.env` cannot brick a worker.
/// Use [`WorkflowConfig::validate`] for fail-fast checks on programmatic
/// configs supplied through the typed config registry.
#[derive(Debug, Clone)]
pub struct WorkflowConfig {
    /// Worker poll interval in milliseconds
    pub poll_interval_ms: u64,
    /// Max concurrent workflows processed by a worker
    pub concurrency: usize,
    /// Lease duration in seconds
    pub lock_timeout_secs: u64,
    /// Max attempts per workflow
    pub max_attempts: i32,
    /// Linear backoff seconds per attempt
    pub retry_backoff_secs: i64,
}

impl WorkflowConfig {
    /// Build config from environment variables.
    ///
    /// Out-of-range values are clamped to safe minimums rather than honoured
    /// blindly. Returning a "load-but-useless" config (e.g. concurrency=0)
    /// would deadlock the worker the first time it ran — clamping plus a
    /// structured warning makes the misconfiguration visible while keeping
    /// the worker functional.
    pub fn from_env() -> Self {
        let raw_concurrency = env("WORKFLOW_CONCURRENCY", 4usize);
        let concurrency = if raw_concurrency < MIN_CONCURRENCY {
            tracing::warn!(
                env = "WORKFLOW_CONCURRENCY",
                value = raw_concurrency,
                clamped_to = MIN_CONCURRENCY,
                "WORKFLOW_CONCURRENCY below minimum; clamping (0 would park the worker semaphore forever)"
            );
            MIN_CONCURRENCY
        } else {
            raw_concurrency
        };

        let raw_max_attempts = env("WORKFLOW_MAX_ATTEMPTS", 3i32);
        let max_attempts = if raw_max_attempts < MIN_MAX_ATTEMPTS {
            tracing::warn!(
                env = "WORKFLOW_MAX_ATTEMPTS",
                value = raw_max_attempts,
                clamped_to = MIN_MAX_ATTEMPTS,
                "WORKFLOW_MAX_ATTEMPTS below minimum; clamping (a row with attempts >= max_attempts is failed before its first run)"
            );
            MIN_MAX_ATTEMPTS
        } else {
            raw_max_attempts
        };

        let raw_backoff = env("WORKFLOW_RETRY_BACKOFF_SECS", 5i64);
        let retry_backoff_secs = if raw_backoff < 0 {
            tracing::warn!(
                env = "WORKFLOW_RETRY_BACKOFF_SECS",
                value = raw_backoff,
                clamped_to = 0i64,
                "WORKFLOW_RETRY_BACKOFF_SECS is negative; clamping (negative backoff schedules retries in the past, causing instant tight-loop reclaim)"
            );
            0
        } else {
            raw_backoff
        };

        Self {
            poll_interval_ms: env("WORKFLOW_POLL_INTERVAL_MS", 1000u64),
            concurrency,
            lock_timeout_secs: env("WORKFLOW_LOCK_TIMEOUT_SECS", 30u64),
            max_attempts,
            retry_backoff_secs,
        }
    }

    /// Validate a programmatic config. Returns `Err` for any value the
    /// worker cannot honour. Use this when constructing a `WorkflowConfig`
    /// in code (not from env) to fail fast at boot instead of failing
    /// silently at runtime.
    pub fn validate(&self) -> Result<(), FrameworkError> {
        if self.concurrency < MIN_CONCURRENCY {
            return Err(FrameworkError::internal(format!(
                "WorkflowConfig.concurrency must be >= {MIN_CONCURRENCY}; got {}. \
                 Zero concurrency makes the worker semaphore park forever.",
                self.concurrency
            )));
        }
        if self.max_attempts < MIN_MAX_ATTEMPTS {
            return Err(FrameworkError::internal(format!(
                "WorkflowConfig.max_attempts must be >= {MIN_MAX_ATTEMPTS}; got {}. \
                 The first claim increments attempts to 1, so max_attempts < 1 fails \
                 every workflow before its body runs.",
                self.max_attempts
            )));
        }
        if self.retry_backoff_secs < 0 {
            return Err(FrameworkError::internal(format!(
                "WorkflowConfig.retry_backoff_secs must be >= 0; got {}. \
                 Negative backoff schedules retries in the past, producing tight-loop \
                 reclaim instead of backoff.",
                self.retry_backoff_secs
            )));
        }
        if self.lock_timeout_secs > i64::MAX as u64 {
            return Err(FrameworkError::internal(format!(
                "WorkflowConfig.lock_timeout_secs must be <= {} (i64::MAX); got {}. \
                 Values above this wrap to a negative chrono duration, making every \
                 workflow lease appear expired and causing reclaim thrashing.",
                i64::MAX,
                self.lock_timeout_secs
            )));
        }
        Ok(())
    }
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Helper that restores env vars after the test. Necessary because
    /// `from_env` is process-wide and these tests mutate the same vars.
    struct EnvGuard {
        keys: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            let saved = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            // Clear all keys up front so the test starts from a clean slate.
            for k in keys {
                // SAFETY: tests are gated with #[serial] so no other test
                // concurrently reads or mutates these env vars.
                unsafe {
                    std::env::remove_var(k);
                }
            }
            Self { keys: saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.keys {
                // SAFETY: same as above — serial test, single-threaded env mutation.
                unsafe {
                    match v {
                        Some(value) => std::env::set_var(k, value),
                        None => std::env::remove_var(k),
                    }
                }
            }
        }
    }

    #[test]
    #[serial]
    fn from_env_clamps_zero_concurrency_to_min() {
        let _guard = EnvGuard::new(&[
            "WORKFLOW_CONCURRENCY",
            "WORKFLOW_MAX_ATTEMPTS",
            "WORKFLOW_RETRY_BACKOFF_SECS",
        ]);
        // SAFETY: serial test guarantees no concurrent env mutation.
        unsafe {
            std::env::set_var("WORKFLOW_CONCURRENCY", "0");
        }

        let cfg = WorkflowConfig::from_env();
        assert!(
            cfg.concurrency >= MIN_CONCURRENCY,
            "concurrency=0 must be clamped to >= {MIN_CONCURRENCY}, got {}",
            cfg.concurrency
        );
    }

    #[test]
    #[serial]
    fn from_env_clamps_negative_max_attempts_to_min() {
        let _guard = EnvGuard::new(&[
            "WORKFLOW_CONCURRENCY",
            "WORKFLOW_MAX_ATTEMPTS",
            "WORKFLOW_RETRY_BACKOFF_SECS",
        ]);
        // SAFETY: serial test.
        unsafe {
            std::env::set_var("WORKFLOW_MAX_ATTEMPTS", "-3");
        }

        let cfg = WorkflowConfig::from_env();
        assert!(
            cfg.max_attempts >= MIN_MAX_ATTEMPTS,
            "max_attempts=-3 must be clamped to >= {MIN_MAX_ATTEMPTS}, got {}",
            cfg.max_attempts
        );
    }

    #[test]
    #[serial]
    fn from_env_clamps_negative_backoff_to_zero() {
        let _guard = EnvGuard::new(&[
            "WORKFLOW_CONCURRENCY",
            "WORKFLOW_MAX_ATTEMPTS",
            "WORKFLOW_RETRY_BACKOFF_SECS",
        ]);
        // SAFETY: serial test.
        unsafe {
            std::env::set_var("WORKFLOW_RETRY_BACKOFF_SECS", "-7");
        }

        let cfg = WorkflowConfig::from_env();
        assert!(
            cfg.retry_backoff_secs >= 0,
            "retry_backoff_secs=-7 must be clamped to >= 0, got {}",
            cfg.retry_backoff_secs
        );
    }

    #[test]
    fn validate_rejects_zero_concurrency() {
        let cfg = WorkflowConfig {
            poll_interval_ms: 1000,
            concurrency: 0,
            lock_timeout_secs: 30,
            max_attempts: 3,
            retry_backoff_secs: 5,
        };
        let err = cfg.validate().expect_err("zero concurrency must error");
        assert!(
            err.to_string().contains("concurrency"),
            "error must mention concurrency, got: {err}"
        );
    }

    #[test]
    fn validate_rejects_negative_backoff() {
        let cfg = WorkflowConfig {
            poll_interval_ms: 1000,
            concurrency: 4,
            lock_timeout_secs: 30,
            max_attempts: 3,
            retry_backoff_secs: -1,
        };
        let err = cfg.validate().expect_err("negative backoff must error");
        assert!(
            err.to_string().contains("retry_backoff_secs"),
            "error must mention retry_backoff_secs, got: {err}"
        );
    }

    #[test]
    fn validate_rejects_zero_max_attempts() {
        let cfg = WorkflowConfig {
            poll_interval_ms: 1000,
            concurrency: 4,
            lock_timeout_secs: 30,
            max_attempts: 0,
            retry_backoff_secs: 5,
        };
        let err = cfg.validate().expect_err("zero max_attempts must error");
        assert!(
            err.to_string().contains("max_attempts"),
            "error must mention max_attempts, got: {err}"
        );
    }

    #[test]
    fn validate_passes_for_sane_defaults() {
        let cfg = WorkflowConfig {
            poll_interval_ms: 1000,
            concurrency: 4,
            lock_timeout_secs: 30,
            max_attempts: 3,
            retry_backoff_secs: 5,
        };
        cfg.validate().expect("default config must validate");
    }
}
