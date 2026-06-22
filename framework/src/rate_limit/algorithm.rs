//! Pure sliding-window logic — separate from storage so both drivers reuse it.

use std::collections::VecDeque;
use std::time::Duration;
use tokio::time::Instant;

/// Per-key sliding-window hit counter shared by the in-memory and Redis
/// rate-limit drivers.
pub struct Bucket {
    hits: VecDeque<Instant>,
}

impl Bucket {
    /// Construct an empty bucket.
    pub fn new() -> Self {
        Self {
            hits: VecDeque::new(),
        }
    }

    /// Drop every hit older than `window` as of `now`.
    pub fn evict_old(&mut self, window: Duration, now: Instant) {
        while let Some(front) = self.hits.front() {
            if now.saturating_duration_since(*front) >= window {
                self.hits.pop_front();
            } else {
                break;
            }
        }
    }

    /// Evict aged hits, then record a fresh hit at `now` if the bucket
    /// is under `max`. Returns `true` when the hit was recorded,
    /// `false` when the limit was hit.
    pub fn try_record(&mut self, max: u32, window: Duration, now: Instant) -> bool {
        self.evict_old(window, now);
        if self.hits.len() < max as usize {
            self.hits.push_back(now);
            true
        } else {
            false
        }
    }

    /// Time until the oldest in-window hit ages out. Returns `None`
    /// when the bucket is under the limit and a new hit would succeed
    /// immediately.
    pub fn retry_after(&self, max: u32, window: Duration, now: Instant) -> Option<Duration> {
        if self.hits.len() < max as usize {
            return None;
        }
        let oldest = self.hits.front().copied()?;
        let elapsed = now.saturating_duration_since(oldest);
        Some(window.saturating_sub(elapsed))
    }

    /// Whether every recorded hit on this bucket has aged out beyond
    /// `window` as of `now`. Used by the in-memory driver's periodic
    /// sweep to evict buckets that no longer carry any state — without
    /// this, attacker-controlled keying (e.g. `X-Forwarded-For` with
    /// no trusted-proxy gating) can grow the bucket map unboundedly.
    pub fn is_inactive(&self, window: Duration, now: Instant) -> bool {
        match self.hits.back().copied() {
            Some(last) => now.saturating_duration_since(last) >= window,
            None => true,
        }
    }
}

impl Default for Bucket {
    fn default() -> Self {
        Self::new()
    }
}
