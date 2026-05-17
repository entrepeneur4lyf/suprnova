//! Job trait + BackoffSchedule.

use crate::error::FrameworkError;
use async_trait::async_trait;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackoffSchedule {
    /// Fixed delay between every retry. `secs` is the per-attempt delay.
    Fixed { secs: u64 },
    /// Exponential: `delay = min(base * 2^(attempts-1), cap)`, multiplied
    /// by a random factor in `[1 - jitter_ratio, 1 + jitter_ratio]`.
    Exponential { base_secs: u64, cap_secs: u64, jitter_ratio: f32 },
    /// Explicit schedule, one entry per attempt. If more attempts than
    /// entries, the last entry is reused.
    Sequence { secs: Vec<u64> },
}

impl Default for BackoffSchedule {
    /// Suprnova's default: exponential, base 2s, cap 5min, ±25% jitter.
    fn default() -> Self {
        Self::Exponential { base_secs: 2, cap_secs: 300, jitter_ratio: 0.25 }
    }
}

#[async_trait]
pub trait Job: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// Stable string used in the envelope's `job_name`. Must be unique
    /// per concrete `Job` impl. Renaming breaks in-flight messages.
    fn job_name() -> &'static str
    where
        Self: Sized;

    /// Run the job. Return `Err(...)` to trigger a retry.
    async fn handle(self) -> Result<(), FrameworkError>;

    /// Max attempts including the initial dispatch. Default: 3.
    fn max_tries() -> u32
    where
        Self: Sized,
    {
        3
    }

    /// Backoff schedule. Default: framework default (exponential 2s..5min ±25%).
    fn backoff() -> BackoffSchedule
    where
        Self: Sized,
    {
        BackoffSchedule::default()
    }

    /// Per-attempt timeout. `None` means no timeout. Default: none.
    fn timeout() -> Option<Duration>
    where
        Self: Sized,
    {
        None
    }

    /// If `true`, a timeout counts as a fatal failure (do not retry).
    /// If `false`, a timeout retries up to `max_tries`. Default: false.
    fn fail_on_timeout() -> bool
    where
        Self: Sized,
    {
        false
    }
}
