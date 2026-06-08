//! Queue lifecycle events.
//!
//! Mirrors Laravel 13's `Illuminate\Queue\Events\*`. The worker emits these
//! through the standard [`crate::events::Event`] facade so
//! observers (admin dashboards, custom listeners) can hook in via
//! `Event::listen`. Events carry envelope metadata (not the typed job
//! instance) because the worker is type-erased over JSON payloads.
//!
//! `FrameworkError` doesn't implement `Clone`, so failure events carry the
//! error as a `String` (the formatted display). That's enough for logging
//! and listener-side classification (string prefix / contains checks)
//! without forcing every listener to hold the full error chain.
//!
//! These events are best-effort — `Event::dispatch` with no listeners
//! registered is a no-op `Ok(())`, so workers that emit them in
//! deployments without `Event::init()` pay nothing.

use crate::events::Event;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;

/// Snapshot of the envelope's identity, carried by every queue event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobIdentity {
    /// Unique envelope identifier assigned by the driver.
    pub id: Uuid,
    /// Fully-qualified job type name (e.g. `"App\\Jobs\\SendInvoice"`).
    pub job_name: String,
    /// Number of times the worker has dispatched this job, including the current attempt.
    pub attempts: u32,
    /// Maximum dispatch attempts before the worker dead-letters the job.
    pub max_tries: u32,
    /// Driver connection name the envelope lives on.
    pub connection: String,
}

impl JobIdentity {
    pub(crate) fn from_env(env: &crate::queue::Envelope, connection: &str) -> Self {
        Self {
            id: env.id,
            job_name: env.job_name.clone(),
            attempts: env.attempts,
            max_tries: env.max_tries,
            connection: connection.to_string(),
        }
    }
}

/// Fired before the envelope is committed to the driver (sync path of
/// `Queue::push`). Mirrors `Illuminate\Queue\Events\JobQueueing`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobQueueing {
    /// Fully-qualified job type name (e.g. `"App\\Jobs\\SendInvoice"`).
    pub job_name: String,
    /// Driver connection name the envelope is bound for.
    pub connection: String,
}

impl Event for JobQueueing {
    fn event_name() -> &'static str {
        "queue::JobQueueing"
    }
}

/// Fired after the envelope is successfully committed to the driver.
/// Mirrors `Illuminate\Queue\Events\JobQueued`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobQueued {
    /// Unique envelope identifier assigned by the driver.
    pub id: Uuid,
    /// Fully-qualified job type name (e.g. `"App\\Jobs\\SendInvoice"`).
    pub job_name: String,
    /// Driver connection name the envelope was committed to.
    pub connection: String,
}

impl Event for JobQueued {
    fn event_name() -> &'static str {
        "queue::JobQueued"
    }
}

/// Fired when the worker pops an envelope and is about to dispatch it.
/// Mirrors `Illuminate\Queue\Events\JobProcessing`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobProcessing {
    /// Identity of the job about to be dispatched.
    pub job: JobIdentity,
}

impl Event for JobProcessing {
    fn event_name() -> &'static str {
        "queue::JobProcessing"
    }
}

/// Fired after a successful run. Mirrors
/// `Illuminate\Queue\Events\JobProcessed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobProcessed {
    /// Identity of the job that completed successfully.
    pub job: JobIdentity,
}

impl Event for JobProcessed {
    fn event_name() -> &'static str {
        "queue::JobProcessed"
    }
}

/// Fired immediately after a job attempt resolves to a terminal outcome
/// (success / fail / timeout — not retry). Mirrors
/// `Illuminate\Queue\Events\JobAttempted`. Distinct from [`JobProcessed`]:
/// `JobAttempted` fires for every terminal settlement, while
/// `JobProcessed` only fires on a clean success.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobAttempted {
    /// Identity of the job whose attempt just settled.
    pub job: JobIdentity,
}

impl Event for JobAttempted {
    fn event_name() -> &'static str {
        "queue::JobAttempted"
    }
}

/// Fired when a job throws and the worker is about to decide retry vs
/// dead-letter. Mirrors `Illuminate\Queue\Events\JobExceptionOccurred`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobExceptionOccurred {
    /// Identity of the job that threw.
    pub job: JobIdentity,
    /// Formatted display of the error that was raised.
    pub exception: String,
}

impl Event for JobExceptionOccurred {
    fn event_name() -> &'static str {
        "queue::JobExceptionOccurred"
    }
}

/// Fired when the worker dead-letters a job (max_tries exhausted, fatal
/// timeout, manual fail). Mirrors `Illuminate\Queue\Events\JobFailed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobFailed {
    /// Identity of the job that was dead-lettered.
    pub job: JobIdentity,
    /// Formatted display of the final error that caused the failure.
    pub exception: String,
}

impl Event for JobFailed {
    fn event_name() -> &'static str {
        "queue::JobFailed"
    }
}

/// Fired after the worker re-enqueues a failed job (not on release via
/// middleware, which uses [`JobReleased`] instead). Mirrors
/// `Illuminate\Queue\Events\JobReleasedAfterException`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobReleasedAfterException {
    /// Identity of the job being retried.
    pub job: JobIdentity,
    /// Formatted display of the error that triggered the back-off.
    pub exception: String,
    /// Computed back-off in seconds before the next attempt.
    pub delay_secs: u64,
}

impl Event for JobReleasedAfterException {
    fn event_name() -> &'static str {
        "queue::JobReleasedAfterException"
    }
}

/// Fired when middleware (or manual `release(delay)`) re-enqueues a job
/// **without** counting it as a failed attempt. Distinct from
/// [`JobReleasedAfterException`] — the original Laravel split, kept here
/// so listeners can distinguish "back-off after error" from "retry later
/// because lock/throttle was busy".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobReleased {
    /// Identity of the job that was released back to the queue.
    pub job: JobIdentity,
    /// Delay in seconds before the job becomes eligible for re-claim.
    pub delay_secs: u64,
    /// Reason supplied by the middleware (e.g. `"rate_limited"`, `"locked"`).
    pub reason: String,
}

impl Event for JobReleased {
    fn event_name() -> &'static str {
        "queue::JobReleased"
    }
}

/// Fired when a job times out during dispatch. Mirrors
/// `Illuminate\Queue\Events\JobTimedOut`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobTimedOut {
    /// Identity of the job that exceeded its timeout.
    pub job: JobIdentity,
    /// Timeout budget the job blew past.
    pub timeout: Duration,
}

impl Event for JobTimedOut {
    fn event_name() -> &'static str {
        "queue::JobTimedOut"
    }
}

/// Fired every iteration of the worker loop, after pop+dispatch settles.
/// Mirrors `Illuminate\Queue\Events\Looping`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Looping {
    /// Driver connection name the worker just polled.
    pub connection: String,
}

impl Event for Looping {
    fn event_name() -> &'static str {
        "queue::Looping"
    }
}

/// Fired once when [`run_worker`](crate::queue::worker::run_worker) starts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerStarting {
    /// Driver connection name the worker is starting on.
    pub connection: String,
}

impl Event for WorkerStarting {
    fn event_name() -> &'static str {
        "queue::WorkerStarting"
    }
}

/// Fired once when [`run_worker`](crate::queue::worker::run_worker) exits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerStopping {
    /// Driver connection name the worker was draining.
    pub connection: String,
    /// Total jobs the worker settled before exiting.
    pub processed: u64,
}

impl Event for WorkerStopping {
    fn event_name() -> &'static str {
        "queue::WorkerStopping"
    }
}

/// Fired when a `Queue::restart()` signal causes a running worker to exit
/// cleanly without claiming additional work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerInterrupted {
    /// Driver connection name the worker was draining.
    pub connection: String,
    /// Total jobs the worker settled before honoring the restart signal.
    pub processed: u64,
}

impl Event for WorkerInterrupted {
    fn event_name() -> &'static str {
        "queue::WorkerInterrupted"
    }
}
