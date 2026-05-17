//! Pure sliding-window logic — separate from storage so both drivers reuse it.

use std::collections::VecDeque;
use std::time::Duration;
use tokio::time::Instant;

pub struct Bucket {
    hits: VecDeque<Instant>,
}

impl Bucket {
    pub fn new() -> Self {
        Self {
            hits: VecDeque::new(),
        }
    }

    pub fn evict_old(&mut self, window: Duration, now: Instant) {
        while let Some(front) = self.hits.front() {
            if now.saturating_duration_since(*front) >= window {
                self.hits.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn try_record(&mut self, max: u32, window: Duration, now: Instant) -> bool {
        self.evict_old(window, now);
        if self.hits.len() < max as usize {
            self.hits.push_back(now);
            true
        } else {
            false
        }
    }

    pub fn retry_after(&self, max: u32, window: Duration, now: Instant) -> Option<Duration> {
        if (self.hits.len() as u32) < max {
            return None;
        }
        let oldest = self.hits.front().copied()?;
        let elapsed = now.saturating_duration_since(oldest);
        Some(window.saturating_sub(elapsed))
    }
}

impl Default for Bucket {
    fn default() -> Self {
        Self::new()
    }
}
