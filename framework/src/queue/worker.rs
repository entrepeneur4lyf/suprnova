//! Worker registry + dispatch by job_name.
//!
//! Each `Job` impl registers a deserialize-and-run shim keyed by its
//! `job_name`. Drivers call `dispatch_by_name` to run an inbound payload.
//! Re-registering the same name is allowed (last writer wins) — useful
//! for tests; deterministic in production because each Job has exactly
//! one registration site.
//!
//! # At-least-once delivery and job idempotency
//!
//! Redis-backed queue drivers cannot make `nack` atomic — the
//! re-publish (XADD) and ack (XACK) are two separate commands. A
//! crash between them re-delivers the message. The in-memory driver
//! and database driver are exactly-once-per-attempt, but the worker
//! loop itself doesn't distinguish drivers, so **every job handler
//! in a production deployment must be idempotent**.
//!
//! For typical command-style jobs, wrap the handler body in
//! [`Idempotency::once`](crate::idempotency::Idempotency::once) or
//! [`Idempotency::commit_on_success`](crate::idempotency::Idempotency::commit_on_success)
//! keyed by a stable per-operation key (e.g. the entity id or a
//! caller-supplied request id). Without this, a re-delivered job may
//! execute the same side effect twice. When a retry must return the
//! original outcome rather than merely skip re-execution, use
//! [`Idempotency::remember`](crate::idempotency::Idempotency::remember),
//! which records the success value and replays it to later deliveries.

use crate::error::FrameworkError;
use crate::lock;
use crate::queue::Job;
use crate::queue::driver::QueueDriver;
use crate::queue::retry::next_delay;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

type Dispatcher =
    Arc<dyn Fn(serde_json::Value) -> BoxFuture<'static, Result<(), FrameworkError>> + Send + Sync>;

static REGISTRY: RwLock<Option<HashMap<String, Dispatcher>>> = RwLock::new(None);

pub fn register_job<J: Job>() {
    let f: Dispatcher = Arc::new(|payload: serde_json::Value| {
        Box::pin(async move {
            let job: J = serde_json::from_value(payload)
                .map_err(|e| FrameworkError::internal(format!("decode job: {e}")))?;
            job.handle().await
        })
    });
    let mut g = lock::write(&REGISTRY).expect("queue registry poisoned");
    let name = J::job_name();
    let map = g.get_or_insert_with(HashMap::new);
    if map.insert(name.to_string(), f).is_some() {
        // Keep last-writer-wins (tests rely on re-registration) but make it
        // observable: silently rerouting in-flight messages is a foot-gun in
        // production where the same `job_name` should have exactly one
        // registration site.
        tracing::warn!(
            job = name,
            "register_job replaced an existing dispatcher for this job_name; \
             duplicate registration may indicate inventory + manual registration \
             of the same job (last writer wins)"
        );
    }
}

pub async fn dispatch_by_name(
    name: &str,
    payload: serde_json::Value,
) -> Result<(), FrameworkError> {
    let dispatcher = {
        let g = lock::read(&REGISTRY)?;
        let map = g
            .as_ref()
            .ok_or_else(|| FrameworkError::internal(format!("unknown job: {name}")))?;
        map.get(name)
            .cloned()
            .ok_or_else(|| FrameworkError::internal(format!("unknown job: {name}")))?
    };
    dispatcher(payload).await
}

/// Return all registered job names. Used by admin inspectors and
/// `cargo run --bin app -- jobs:list` (Phase 6B).
pub fn registered_job_names() -> Vec<String> {
    lock::read(&REGISTRY)
        .expect("queue registry poisoned")
        .as_ref()
        .map(|m| {
            let mut v: Vec<_> = m.keys().cloned().collect();
            v.sort();
            v
        })
        .unwrap_or_default()
}

// ============================================================================
// Worker loop (Task 8)
// ============================================================================

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub visibility_timeout: Duration,
    pub poll_interval: Duration,
    /// Optional hard cap on jobs processed by this worker before it exits
    /// cleanly. `None` runs until cancelled. Used by `queue:work --max-jobs N`
    /// for periodic restart strategies (e.g. release-on-restart deploys).
    pub max_jobs: Option<u64>,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            visibility_timeout: Duration::from_secs(60),
            poll_interval: Duration::from_millis(100),
            max_jobs: None,
        }
    }
}

/// One job's terminal state for the worker's settlement match.
///
/// Carries the dispatch result by type, not by string-matching the error
/// message: a job whose own failure body legitimately contains the words
/// "timed out after" can no longer be misclassified, and a real timeout
/// is observable without parsing.
enum DispatchOutcome {
    Ok,
    Failed(FrameworkError),
    TimedOut(Duration),
}

/// Pull-loop worker: pops one reservation at a time, dispatches by job_name,
/// acks on success, requeues with backoff on failure, drops after max_tries.
///
/// The worker bumps `env.attempts` locally before dispatch. The memory driver's
/// `nack` also bumps `attempts` on its stored copy so the next `pop` returns
/// the correct incremented count (preventing the worker from treating every
/// retry as attempt 1).
///
/// Returns when `shutdown` is cancelled or when `cfg.max_jobs` is reached.
/// A cancel signal interrupts pop polling but never an in-flight handler:
/// a job that's already been popped is allowed to finish (bounded by its
/// own per-job `timeout()` if set) before the worker exits, so in-flight
/// side effects don't get torn mid-stride. Designed to run under
/// `tokio::spawn`.
pub async fn run_worker(
    driver: Arc<dyn QueueDriver>,
    cfg: WorkerConfig,
    shutdown: CancellationToken,
) {
    let mut processed: u64 = 0;
    loop {
        // Stop accepting new work the moment shutdown fires; the current
        // in-flight job (if any) has already been popped above and will run
        // to completion below before the next iteration sees the cancel.
        if shutdown.is_cancelled() {
            return;
        }
        if let Some(max) = cfg.max_jobs
            && processed >= max
        {
            tracing::info!(
                processed,
                max_jobs = max,
                "queue worker reached max_jobs, exiting cleanly"
            );
            return;
        }

        // Pop OR cancel — whichever happens first. `biased` makes cancel win
        // a tie so a queue under load can still exit promptly.
        let popped = tokio::select! {
            biased;
            _ = shutdown.cancelled() => return,
            res = driver.pop(cfg.visibility_timeout) => res,
        };

        let popped = match popped {
            Ok(opt) => opt,
            Err(e) => {
                tracing::error!(error = %e, driver = driver.name(), "queue pop failed");
                tokio::select! {
                    _ = shutdown.cancelled() => return,
                    _ = tokio::time::sleep(cfg.poll_interval) => {}
                }
                continue;
            }
        };
        let Some(res) = popped else {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(cfg.poll_interval) => {}
            }
            continue;
        };

        let mut env = res.envelope;
        env.attempts += 1;
        let timeout_opt = env.timeout_secs.map(Duration::from_secs);
        let dispatch_fut = dispatch_by_name(&env.job_name, env.payload.clone());

        let outcome = match timeout_opt {
            Some(t) => match tokio::time::timeout(t, dispatch_fut).await {
                Ok(Ok(())) => DispatchOutcome::Ok,
                Ok(Err(e)) => DispatchOutcome::Failed(e),
                Err(_elapsed) => DispatchOutcome::TimedOut(t),
            },
            None => match dispatch_fut.await {
                Ok(()) => DispatchOutcome::Ok,
                Err(e) => DispatchOutcome::Failed(e),
            },
        };

        match outcome {
            DispatchOutcome::Ok => {
                if let Err(e) = driver.ack(&res.token).await {
                    tracing::error!(
                        job = %env.job_name,
                        id = %env.id,
                        driver = driver.name(),
                        error = %e,
                        "queue ack failed after successful run; \
                         job may be re-delivered (at-least-once)"
                    );
                } else {
                    tracing::debug!(job = %env.job_name, id = %env.id, "queue job ok");
                }
            }
            DispatchOutcome::Failed(e) => {
                if env.attempts >= env.max_tries {
                    tracing::error!(
                        job = %env.job_name,
                        id = %env.id,
                        attempts = env.attempts,
                        error = %e,
                        "queue job dead-lettered (max_tries exhausted)"
                    );
                    if let Err(ack_err) = driver.ack(&res.token).await {
                        tracing::error!(
                            job = %env.job_name,
                            id = %env.id,
                            driver = driver.name(),
                            error = %ack_err,
                            "queue ack failed for dead-lettered job; \
                             reservation may stay until visibility expiry"
                        );
                    }
                } else {
                    let delay = next_delay(&env.backoff, env.attempts, None);
                    tracing::warn!(
                        job = %env.job_name,
                        id = %env.id,
                        attempt = env.attempts,
                        retry_in = ?delay,
                        error = %e,
                        "queue job failed, will retry"
                    );
                    if let Err(nack_err) = driver.nack(&res.token, delay).await {
                        tracing::error!(
                            job = %env.job_name,
                            id = %env.id,
                            driver = driver.name(),
                            error = %nack_err,
                            retry_in = ?delay,
                            "queue nack failed; reservation may be redelivered \
                             after visibility expiry without bumped attempts"
                        );
                    }
                }
            }
            DispatchOutcome::TimedOut(t) => {
                let exhausted = env.fail_on_timeout || env.attempts >= env.max_tries;
                if exhausted {
                    tracing::error!(
                        job = %env.job_name,
                        id = %env.id,
                        attempts = env.attempts,
                        timeout_secs = t.as_secs(),
                        fail_on_timeout = env.fail_on_timeout,
                        "queue job dead-lettered (timed out)"
                    );
                    if let Err(ack_err) = driver.ack(&res.token).await {
                        tracing::error!(
                            job = %env.job_name,
                            id = %env.id,
                            driver = driver.name(),
                            error = %ack_err,
                            "queue ack failed for timed-out dead-lettered job; \
                             reservation may stay until visibility expiry"
                        );
                    }
                } else {
                    let delay = next_delay(&env.backoff, env.attempts, None);
                    tracing::warn!(
                        job = %env.job_name,
                        id = %env.id,
                        attempt = env.attempts,
                        retry_in = ?delay,
                        timeout_secs = t.as_secs(),
                        "queue job timed out, will retry"
                    );
                    if let Err(nack_err) = driver.nack(&res.token, delay).await {
                        tracing::error!(
                            job = %env.job_name,
                            id = %env.id,
                            driver = driver.name(),
                            error = %nack_err,
                            retry_in = ?delay,
                            "queue nack failed after timeout; reservation may be \
                             redelivered after visibility expiry without bumped attempts"
                        );
                    }
                }
            }
        }

        // One settlement = one processed job for the max_jobs cap, regardless
        // of outcome (success/failure/timeout). Settlement-failure logging
        // above is separate from this accounting.
        processed = processed.saturating_add(1);
    }
}
