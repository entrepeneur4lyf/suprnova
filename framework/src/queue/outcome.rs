//! Job outcomes — the worker's settlement decision for a single attempt.
//!
//! Job handlers return `Result<(), FrameworkError>`; middleware speak the
//! richer [`JobOutcome`] vocabulary. A middleware can release a job back to
//! the queue **without** incrementing the failed-attempt counter (the
//! `WithoutOverlapping` / `RateLimited` shape), delete the job entirely, or
//! mark it as failed-now. These outcomes are how Laravel's
//! `$job->release()`, `$job->delete()`, `$job->fail()` semantics translate
//! to a return-by-value Rust handler model.

use std::time::Duration;

/// One settlement decision for a single popped envelope.
///
/// Returned by [`JobMiddleware::handle`](super::middleware::JobMiddleware::handle)
/// (and by the wrapped handler, internally). The worker matches on the
/// outcome to decide whether to `ack` (success/delete/fail), re-enqueue
/// without bumping attempts (release), or run the retry path (handler error).
#[derive(Debug)]
pub enum JobOutcome {
    /// Handler ran to completion and reported success. Worker `ack`s.
    Completed,

    /// Re-enqueue the job after `delay` **without** counting this as a
    /// failed attempt. Used by `WithoutOverlapping` (couldn't get the
    /// lock — try again later) and `RateLimited` (over budget — try
    /// again later). The worker emits a `JobReleasedAfterException`-free
    /// re-enqueue path; the envelope's `attempts` counter is held at its
    /// pre-dispatch value, not bumped.
    Released {
        /// How long the worker should wait before re-claiming the envelope.
        delay: Duration,
    },

    /// Dead-letter the job now without retry. Worker `ack`s the reservation
    /// AND writes a failed-jobs record carrying `reason`. Used by
    /// `FailOnException` and by handlers that decide a failure is
    /// permanent.
    Failed {
        /// Operator-facing reason captured on the failed-job record.
        reason: String,
    },

    /// Drop the job entirely without retry and without a failed-job
    /// record. Worker `ack`s the reservation only. Used by `Skip` (the
    /// condition said don't run this).
    Deleted,
}

impl JobOutcome {
    /// `true` if the worker should not increment `attempts` for this outcome.
    pub fn is_release(&self) -> bool {
        matches!(self, JobOutcome::Released { .. })
    }

    /// `true` if the outcome counts as a successful settlement (no retry).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobOutcome::Completed | JobOutcome::Failed { .. } | JobOutcome::Deleted
        )
    }
}
