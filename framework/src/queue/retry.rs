//! Backoff calculator.
//!
//! `next_delay(schedule, attempts, deterministic_jitter)`:
//! - `attempts` is 1-indexed: 1 = first retry after the original failure.
//! - `deterministic_jitter` is `Some(x)` where x ∈ [-1.0, 1.0] for tests
//!   that need a known result; `None` draws from the thread RNG.

use crate::queue::BackoffSchedule;
use rand::RngExt;
use std::time::Duration;

/// Compute the next retry delay for `attempts` (1-indexed) under the
/// supplied [`BackoffSchedule`]. When `deterministic_jitter` is
/// `Some(x)` the value is used in place of an RNG draw — tests use
/// this to assert exact delays.
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
            // Clamp `jitter_ratio` before use — the field is a public
            // `f32` so callers can construct any value, and NaN /
            // out-of-range need to fail safe. The cap is then enforced
            // post-multiply too: symmetric jitter with `jr > 0` allows
            // `(1 + jitter)` to land above 1.0, so without a final
            // clamp the delay can exceed `cap_secs`. Pin the ceiling
            // strictly — `cap_secs` is the contract.
            let jr = if jitter_ratio.is_nan() {
                0.0
            } else {
                jitter_ratio.clamp(0.0, 1.0)
            };
            let jitter = deterministic_jitter
                .unwrap_or_else(|| rand::rng().random_range(-1.0_f32..=1.0_f32))
                .clamp(-1.0, 1.0)
                * jr;
            let scaled = (capped as f32 * (1.0 + jitter)).max(0.0);
            let delay = scaled.round() as u64;
            Duration::from_secs(delay.min(*cap_secs))
        }
        BackoffSchedule::Sequence { secs } => {
            let idx = (attempts as usize - 1).min(secs.len().saturating_sub(1));
            let value = secs.get(idx).copied().unwrap_or(0);
            Duration::from_secs(value)
        }
    }
}
