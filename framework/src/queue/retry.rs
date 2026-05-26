//! Backoff calculator.
//!
//! `next_delay(schedule, attempts, deterministic_jitter)`:
//! - `attempts` is 1-indexed: 1 = first retry after the original failure.
//! - `deterministic_jitter` is `Some(x)` where x ∈ [-1.0, 1.0] for tests
//!   that need a known result; `None` draws from the thread RNG.

use crate::queue::BackoffSchedule;
use rand::RngExt;
use std::time::Duration;

pub fn next_delay(
    schedule: &BackoffSchedule,
    attempts: u32,
    deterministic_jitter: Option<f32>,
) -> Duration {
    let attempts = attempts.max(1);
    match schedule {
        BackoffSchedule::Fixed { secs } => Duration::from_secs(*secs),
        BackoffSchedule::Exponential {
            base_secs,
            cap_secs,
            jitter_ratio,
        } => {
            // delay = min(base * 2^(attempts-1), cap)
            let raw = (*base_secs as u128).saturating_mul(1u128 << (attempts - 1).min(63));
            let capped = raw.min(*cap_secs as u128) as u64;
            let jitter = deterministic_jitter
                .unwrap_or_else(|| rand::rng().random_range(-1.0_f32..=1.0_f32))
                .clamp(-1.0, 1.0)
                * *jitter_ratio;
            let scaled = (capped as f32 * (1.0 + jitter)).max(0.0);
            Duration::from_secs(scaled.round() as u64)
        }
        BackoffSchedule::Sequence { secs } => {
            let idx = (attempts as usize - 1).min(secs.len().saturating_sub(1));
            let value = secs.get(idx).copied().unwrap_or(0);
            Duration::from_secs(value)
        }
    }
}
