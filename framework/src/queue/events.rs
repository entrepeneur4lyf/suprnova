//! Queue lifecycle events.
//!
//! Mirrors Laravel 13's `Illuminate\Queue\Events\*`. The worker emits these
//! through the standard [`Event`](crate::events::Event) facade so
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
    pub id: Uuid,
    pub job_name: String,
    pub attempts: u32,
    pub max_tries: u32,
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
    pub job_name: String,
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
    pub id: Uuid,
    pub job_name: String,
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
    pub job: JobIdentity,
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
    pub job: JobIdentity,
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
    pub job: JobIdentity,
    pub exception: String,
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
    pub job: JobIdentity,
    pub delay_secs: u64,
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
    pub job: JobIdentity,
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
    pub connection: String,
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
    pub connection: String,
    pub processed: u64,
}

impl Event for WorkerInterrupted {
    fn event_name() -> &'static str {
        "queue::WorkerInterrupted"
    }
}
