//! Typed queue errors mirroring Laravel 13's queue exception classes.
//!
//! These are constructed by the worker and middleware so callers can pattern
//! match on the cause (timeout, max-attempts exhausted, manual fail). They
//! convert into `FrameworkError::internal(...)` for callers that handle queue
//! errors structurally; the worker keeps a typed copy alongside the message
//! for event emission.

use std::time::Duration;
use thiserror::Error;

/// Thrown by the worker when a job exhausts its `max_tries` budget. Mirrors
/// `Illuminate\Queue\MaxAttemptsExceededException`. Carries the job name and
/// the attempt count for the failed-job record.
#[derive(Debug, Clone, Error)]
#[error("queue job '{job_name}' exhausted max_tries after {attempts} attempts: {reason}")]
pub struct MaxAttemptsExceeded {
    pub job_name: String,
    pub attempts: u32,
    pub reason: String,
}

/// Thrown when a job's per-attempt `timeout()` budget is exceeded. Mirrors
/// `Illuminate\Queue\TimeoutExceededException`.
#[derive(Debug, Clone, Error)]
#[error("queue job '{job_name}' exceeded its per-attempt timeout of {timeout:?}")]
pub struct TimeoutExceeded {
    pub job_name: String,
    pub timeout: Duration,
}

/// Thrown when a job middleware (or the handler itself) manually marked the
/// job as failed via `JobContext::fail`. Mirrors
/// `Illuminate\Queue\ManuallyFailedException`.
#[derive(Debug, Clone, Error)]
#[error("queue job '{job_name}' was manually failed: {reason}")]
pub struct ManuallyFailed {
    pub job_name: String,
    pub reason: String,
}
