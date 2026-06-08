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
use crate::events::EventFacade;
use crate::lock;
use crate::queue::Job;
use crate::queue::batch::resolve_callback;
use crate::queue::chain::ChainLink;
use crate::queue::driver::QueueDriver;
use crate::queue::envelope::Envelope;
use crate::queue::events as queue_events;
use crate::queue::middleware::{JobMiddleware, Next};
use crate::queue::outcome::JobOutcome;
use crate::queue::retry::next_delay;
use crate::telemetry::Metrics;
use chrono::Utc;
use futures::FutureExt;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Counter name for settlement (ack/nack) failures. Operators can alert on a
/// non-zero rate here: a single failure means at-least-once delivery may
/// re-deliver a successful side effect (ack) or lose attempt accounting (nack).
///
/// Emitted with attributes `operation` (`"ack"` | `"nack"`), `driver`
/// (driver type-name from `QueueDriver::name`), `job` (the `Job::job_name`),
/// and `outcome` (`"success"` for a successful run whose ack failed,
/// `"dead_letter"` for a settled-failed job whose ack failed, `"retry"` for
/// a retried-failure whose nack failed, `"timeout_dead_letter"` for a
/// timeout-exhausted ack failure, `"timeout_retry"` for a timeout-nack
/// failure).
const METRIC_SETTLEMENT_FAILURES: &str = "queue.settlement.failures";

type Dispatcher =
    Arc<dyn Fn(serde_json::Value) -> BoxFuture<'static, Result<(), FrameworkError>> + Send + Sync>;

/// Factory that produces the per-job middleware stack each time a job is
/// dispatched. Middleware can hold per-instance state (lock keys, throttle
/// keys), so we call the factory once per pop rather than caching the
/// stack across runs.
type MiddlewareFactory = Arc<dyn Fn() -> Vec<Arc<dyn JobMiddleware>> + Send + Sync>;

struct Registration {
    dispatcher: Dispatcher,
    middleware: MiddlewareFactory,
}

static REGISTRY: RwLock<Option<HashMap<String, Registration>>> = RwLock::new(None);

/// Register `J` so the worker can dispatch envelopes carrying its
/// `job_name`. Last-write-wins; re-registering the same name replaces
/// the prior dispatcher and emits a `warn` trace event.
pub fn register_job<J: Job>() {
    let dispatcher: Dispatcher = Arc::new(|payload: serde_json::Value| {
        Box::pin(async move {
            let job: J = serde_json::from_value(payload)
                .map_err(|e| FrameworkError::internal(format!("decode job: {e}")))?;
            job.handle().await
        })
    });
    let middleware: MiddlewareFactory = Arc::new(|| J::middleware());
    let mut g = lock::write(&REGISTRY, "queue job registry").expect("queue registry poisoned");
    let name = J::job_name();
    let map = g.get_or_insert_with(HashMap::new);
    if map
        .insert(
            name.to_string(),
            Registration {
                dispatcher,
                middleware,
            },
        )
        .is_some()
    {
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

/// Look up the dispatcher registered under `name` and run it against
/// `payload`. Returns `Err` if no job is registered under that name.
pub async fn dispatch_by_name(
    name: &str,
    payload: serde_json::Value,
) -> Result<(), FrameworkError> {
    let dispatcher = {
        let g = lock::read(&REGISTRY, "queue job registry")?;
        let map = g
            .as_ref()
            .ok_or_else(|| FrameworkError::internal(format!("unknown job: {name}")))?;
        map.get(name)
            .map(|r| r.dispatcher.clone())
            .ok_or_else(|| FrameworkError::internal(format!("unknown job: {name}")))?
    };
    dispatcher(payload).await
}

/// Look up the middleware factory for a job name. Returns an empty list
/// for unregistered jobs (the dispatcher itself will error in that case).
fn middleware_for(name: &str) -> Vec<Arc<dyn JobMiddleware>> {
    let g = match lock::read(&REGISTRY, "queue job registry") {
        Ok(g) => g,
        Err(_) => return Vec::new(),
    };
    g.as_ref()
        .and_then(|m| m.get(name).map(|r| (r.middleware)()))
        .unwrap_or_default()
}

/// Run the middleware pipeline ending in the raw dispatcher. Returns the
/// terminal [`JobOutcome`] OR a handler error (which the worker translates
/// into retry / dead-letter).
///
/// Exposed for test harnesses that want to settle one envelope without
/// running the full worker loop; production code goes through
/// [`run_worker`].
pub async fn run_through_middleware(env: Envelope) -> Result<JobOutcome, FrameworkError> {
    let job_name = env.job_name.clone();
    let mw_stack = middleware_for(&job_name);
    // Build the innermost layer: actually dispatch the job, lift result
    // into JobOutcome::Completed.
    let innermost: Next = Box::new(move |env: Envelope| {
        Box::pin(async move {
            let payload = env.payload.clone();
            dispatch_by_name(&env.job_name, payload).await?;
            Ok(JobOutcome::Completed)
        })
    });

    // Fold middleware in reverse so the first entry runs outermost.
    let chained =
        mw_stack
            .into_iter()
            .rev()
            .fold(innermost, |next: Next, mw: Arc<dyn JobMiddleware>| {
                Box::new(move |env: Envelope| {
                    let mw = mw.clone();
                    Box::pin(async move { mw.handle(env, next).await })
                })
            });

    chained(env).await
}

/// Return all registered job names. Used by admin inspectors and
/// `cargo run --bin app -- jobs:list` (Phase 6B).
pub fn registered_job_names() -> Vec<String> {
    lock::read(&REGISTRY, "queue job registry")
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

/// Runtime tuning for [`run_worker`].
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// How long a reservation stays held before another worker may re-claim
    /// the envelope. Drivers that lack lease semantics ignore this.
    pub visibility_timeout: Duration,
    /// Sleep duration when the driver returns no envelope on a poll.
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
    /// Middleware pipeline returned a typed outcome.
    Settled(JobOutcome),
    /// Handler returned `Err(...)` and middleware didn't convert it.
    /// Worker decides retry vs dead-letter from `attempts`/`max_tries`.
    Failed(FrameworkError),
    /// Dispatch exceeded the per-job timeout budget.
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
    let connection = crate::queue::Queue::connection_name();
    let worker_started_at = Utc::now().timestamp_millis();
    let _ = EventFacade::dispatch(queue_events::WorkerStarting {
        connection: connection.clone(),
    })
    .await;

    let mut processed: u64 = 0;
    let exit_with = |reason: &'static str, processed: u64, connection: &str| {
        tracing::info!(
            reason,
            processed,
            connection = connection,
            "queue worker exiting"
        );
    };

    let result = loop {
        // Stop accepting new work the moment shutdown fires; the current
        // in-flight job (if any) has already been popped above and will run
        // to completion below before the next iteration sees the cancel.
        if shutdown.is_cancelled() {
            exit_with("cancelled", processed, &connection);
            break ExitReason::Cancelled;
        }
        if let Some(max) = cfg.max_jobs
            && processed >= max
        {
            tracing::info!(
                processed,
                max_jobs = max,
                "queue worker reached max_jobs, exiting cleanly"
            );
            break ExitReason::MaxJobs;
        }
        if let Ok(Some(ts)) = crate::queue::Queue::restart_signal().await
            && ts > worker_started_at
        {
            tracing::info!(
                processed,
                "queue worker received restart signal, exiting cleanly"
            );
            let _ = EventFacade::dispatch(queue_events::WorkerInterrupted {
                connection: connection.clone(),
                processed,
            })
            .await;
            break ExitReason::Restart;
        }

        // Emit per-iteration Looping event before the pop so listeners
        // see the cadence even on empty queues.
        let _ = EventFacade::dispatch(queue_events::Looping {
            connection: connection.clone(),
        })
        .await;

        // Pop OR cancel — whichever happens first. `biased` makes cancel win
        // a tie so a queue under load can still exit promptly.
        let popped = tokio::select! {
            biased;
            _ = shutdown.cancelled() => {
                exit_with("cancelled", processed, &connection);
                break ExitReason::Cancelled;
            }
            res = driver.pop(cfg.visibility_timeout) => res,
        };

        let popped = match popped {
            Ok(opt) => opt,
            Err(e) => {
                tracing::error!(error = %e, driver = driver.name(), "queue pop failed");
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        exit_with("cancelled", processed, &connection);
                        break ExitReason::Cancelled;
                    }
                    _ = tokio::time::sleep(cfg.poll_interval) => {}
                }
                continue;
            }
        };
        let Some(res) = popped else {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    exit_with("cancelled", processed, &connection);
                    break ExitReason::Cancelled;
                }
                _ = tokio::time::sleep(cfg.poll_interval) => {}
            }
            continue;
        };

        let mut env = res.envelope;
        env.attempts += 1;
        let identity_pre = queue_events::JobIdentity::from_env(&env, &connection);
        let _ = EventFacade::dispatch(queue_events::JobProcessing {
            job: identity_pre.clone(),
        })
        .await;

        let timeout_opt = env.timeout_secs.map(Duration::from_secs);
        let env_for_dispatch = env.clone();
        // Wrap dispatch in a panic boundary so a panicking handler (or panicking
        // middleware) is converted to a `DispatchOutcome::Failed` and flows
        // through the existing retry / dead-letter path. Without the boundary,
        // a panic would unwind out of `run_worker`, kill the worker task, and
        // strand the envelope's reservation until visibility expiry.
        let dispatch_fut =
            AssertUnwindSafe(run_through_middleware(env_for_dispatch)).catch_unwind();

        let outcome = match timeout_opt {
            Some(t) => match tokio::time::timeout(t, dispatch_fut).await {
                Ok(Ok(Ok(o))) => DispatchOutcome::Settled(o),
                Ok(Ok(Err(e))) => DispatchOutcome::Failed(e),
                Ok(Err(panic_payload)) => {
                    DispatchOutcome::Failed(FrameworkError::internal(format!(
                        "job panicked: {}",
                        crate::server::panic_payload_message(&panic_payload)
                    )))
                }
                Err(_elapsed) => DispatchOutcome::TimedOut(t),
            },
            None => match dispatch_fut.await {
                Ok(Ok(o)) => DispatchOutcome::Settled(o),
                Ok(Err(e)) => DispatchOutcome::Failed(e),
                Err(panic_payload) => DispatchOutcome::Failed(FrameworkError::internal(format!(
                    "job panicked: {}",
                    crate::server::panic_payload_message(&panic_payload)
                ))),
            },
        };

        match outcome {
            DispatchOutcome::Settled(JobOutcome::Completed) => {
                handle_completed(&*driver, &res.token, &env, &connection).await;
            }
            DispatchOutcome::Settled(JobOutcome::Released { delay }) => {
                handle_released(
                    &*driver,
                    &res.token,
                    &mut env,
                    delay,
                    &connection,
                    "middleware",
                )
                .await;
            }
            DispatchOutcome::Settled(JobOutcome::Failed { reason }) => {
                handle_dead_letter(&*driver, &res.token, &env, &connection, &reason, false).await;
            }
            DispatchOutcome::Settled(JobOutcome::Deleted) => {
                // Middleware decided to drop the job without dead-letter.
                if let Err(e) = driver.ack(&res.token).await {
                    settlement_failure(&*driver, &env, "ack", "deleted", &e);
                }
                tracing::debug!(job = %env.job_name, id = %env.id, "queue job dropped by middleware");

                // If this envelope belonged to a batch, the batch's
                // pending_jobs still has to decrement so callbacks can
                // fire. The batch saw the job; the batch must see it
                // settled, even if its handler never ran. Without this,
                // `SkipIfBatchCancelled` would leave a cancelled batch
                // stuck with pending_jobs > 0 forever.
                if let Some(batch_id) = env.batch_id.as_deref()
                    && let Some(repo) = crate::queue::batch::current_repository()
                {
                    let counts = repo.record_successful_job(batch_id, env.id).await;
                    if let Ok(c) = counts
                        && c.pending_jobs == 0
                        && let Ok(Some(b)) = repo.find(batch_id).await
                    {
                        let _ = repo.mark_finished(batch_id).await;
                        let phase = if b.failed_jobs > 0 {
                            BatchPhase::Catch
                        } else {
                            BatchPhase::Then
                        };
                        fire_batch_callbacks(&b, phase).await;
                        fire_batch_callbacks(&b, BatchPhase::Finally).await;
                    }
                }
            }
            DispatchOutcome::Failed(e) => {
                if env.attempts >= env.max_tries {
                    handle_dead_letter(
                        &*driver,
                        &res.token,
                        &env,
                        &connection,
                        &e.to_string(),
                        false,
                    )
                    .await;
                } else {
                    let _ = EventFacade::dispatch(queue_events::JobExceptionOccurred {
                        job: identity_pre.clone(),
                        exception: e.to_string(),
                    })
                    .await;
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
                        settlement_failure(&*driver, &env, "nack", "retry", &nack_err);
                    } else {
                        let _ = EventFacade::dispatch(queue_events::JobReleasedAfterException {
                            job: identity_pre.clone(),
                            exception: e.to_string(),
                            delay_secs: delay.as_secs(),
                        })
                        .await;
                    }
                }
            }
            DispatchOutcome::TimedOut(t) => {
                let _ = EventFacade::dispatch(queue_events::JobTimedOut {
                    job: identity_pre.clone(),
                    timeout: t,
                })
                .await;
                let exhausted = env.fail_on_timeout || env.attempts >= env.max_tries;
                if exhausted {
                    let reason = format!(
                        "job exceeded per-attempt timeout of {} seconds",
                        t.as_secs()
                    );
                    handle_dead_letter(&*driver, &res.token, &env, &connection, &reason, true)
                        .await;
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
                        settlement_failure(&*driver, &env, "nack", "timeout_retry", &nack_err);
                    }
                }
            }
        }

        // One settlement = one processed job for the max_jobs cap, regardless
        // of outcome (success/failure/timeout). Settlement-failure logging
        // above is separate from this accounting.
        processed = processed.saturating_add(1);
    };

    let _ = EventFacade::dispatch(queue_events::WorkerStopping {
        connection: connection.clone(),
        processed,
    })
    .await;
    let _ = result;
}

#[derive(Debug)]
enum ExitReason {
    Cancelled,
    MaxJobs,
    Restart,
}

async fn handle_completed(
    driver: &dyn QueueDriver,
    token: &crate::queue::driver::ReservationToken,
    env: &Envelope,
    connection: &str,
) {
    if let Err(e) = driver.ack(token).await {
        settlement_failure(driver, env, "ack", "success", &e);
    } else {
        tracing::debug!(job = %env.job_name, id = %env.id, "queue job ok");
    }
    let _ = EventFacade::dispatch(queue_events::JobProcessed {
        job: queue_events::JobIdentity::from_env(env, connection),
    })
    .await;
    let _ = EventFacade::dispatch(queue_events::JobAttempted {
        job: queue_events::JobIdentity::from_env(env, connection),
    })
    .await;

    // Notify batch repository.
    if let Some(batch_id) = env.batch_id.as_deref()
        && let Some(repo) = crate::queue::batch::current_repository()
    {
        let counts = repo.record_successful_job(batch_id, env.id).await;
        if let Ok(c) = counts
            && c.pending_jobs == 0
        {
            let _ = repo.mark_finished(batch_id).await;
            if let Ok(Some(b)) = repo.find(batch_id).await {
                // If any prior job failed or the batch was cancelled
                // mid-flight, finalize via Catch — Then is only correct
                // when the entire batch succeeded. Whichever settlement
                // path drives pending to 0 must agree on the callback.
                let phase = if b.failed_jobs > 0 || b.cancelled() {
                    BatchPhase::Catch
                } else {
                    BatchPhase::Then
                };
                fire_batch_callbacks(&b, phase).await;
                fire_batch_callbacks(&b, BatchPhase::Finally).await;
            }
        }
    }

    // Dispatch next link in chain (if any) onto the SAME driver that
    // settled this job. The worker is bound to a specific
    // `Arc<dyn QueueDriver>` at `run_worker(driver, ...)`; resolving
    // through `current_driver()` would re-pick whichever driver is
    // registered globally, which differs from the bound one under
    // multi-connection setups (e.g. one worker per connection) and
    // would silently land the next link on the wrong queue.
    if !env.chain_remaining.is_empty() {
        let mut tail = env.chain_remaining.clone();
        let next: ChainLink = tail.remove(0);
        let mut next_env = next.to_envelope();
        next_env.chain_remaining = tail;
        next_env.batch_id = env.batch_id.clone();
        if let Err(e) = driver.push(next_env).await {
            tracing::error!(
                job = %env.job_name,
                id = %env.id,
                error = %e,
                "queue chain: failed to dispatch next link"
            );
        }
    }
}

async fn handle_released(
    driver: &dyn QueueDriver,
    token: &crate::queue::driver::ReservationToken,
    env: &mut Envelope,
    delay: Duration,
    connection: &str,
    reason: &str,
) {
    // Released means "try again WITHOUT burning an attempt". A naive
    // `driver.nack(token, delay)` would re-publish the driver's stored
    // copy with `attempts += 1` (per the trait contract), defeating the
    // purpose. So instead:
    //   1. Decrement the local copy back to its pre-dispatch attempt count.
    //   2. ACK the original reservation (drop the driver's copy).
    //   3. PUSH the local copy with `available_at` shifted by `delay`.
    // This is at-least-once: a crash between ack and push re-delivers the
    // original via visibility expiry. For the release case (lock busy,
    // throttle exceeded) that re-delivery is benign — it produces another
    // release attempt — and is strictly better than incrementing the
    // attempts counter on every contention cycle.
    env.attempts = env.attempts.saturating_sub(1);
    if let Err(e) = driver.ack(token).await {
        settlement_failure(driver, env, "ack", "released", &e);
        return;
    }
    let new_available = Utc::now()
        + match chrono::Duration::from_std(delay) {
            Ok(d) => d,
            Err(_) => chrono::Duration::seconds(0),
        };
    env.available_at = new_available;
    if let Err(e) = driver.push(env.clone()).await {
        tracing::error!(
            job = %env.job_name,
            id = %env.id,
            driver = driver.name(),
            error = %e,
            "queue released-push failed; reservation has been ack'd, the job is now lost"
        );
        return;
    }
    let _ = EventFacade::dispatch(queue_events::JobReleased {
        job: queue_events::JobIdentity::from_env(env, connection),
        delay_secs: delay.as_secs(),
        reason: reason.into(),
    })
    .await;
    tracing::debug!(
        job = %env.job_name,
        id = %env.id,
        retry_in = ?delay,
        "queue job released without burning attempt"
    );
}

async fn handle_dead_letter(
    driver: &dyn QueueDriver,
    token: &crate::queue::driver::ReservationToken,
    env: &Envelope,
    connection: &str,
    reason: &str,
    is_timeout: bool,
) {
    tracing::error!(
        job = %env.job_name,
        id = %env.id,
        attempts = env.attempts,
        reason = %reason,
        "queue job dead-lettered"
    );
    if let Err(ack_err) = driver.ack(token).await {
        let outcome = if is_timeout {
            "timeout_dead_letter"
        } else {
            "dead_letter"
        };
        settlement_failure(driver, env, "ack", outcome, &ack_err);
    }

    // Persist to failed-jobs store.
    if let Some(store) = crate::queue::failed::current()
        && let Err(e) = store.log(connection, "default", env, reason).await
    {
        tracing::error!(
            job = %env.job_name,
            id = %env.id,
            error = %e,
            "queue failed-jobs store rejected the record"
        );
    }

    let _ = EventFacade::dispatch(queue_events::JobFailed {
        job: queue_events::JobIdentity::from_env(env, connection),
        exception: reason.to_string(),
    })
    .await;

    // Notify batch repository of failure (and cancel if !allow_failures).
    if let Some(batch_id) = env.batch_id.as_deref()
        && let Some(repo) = crate::queue::batch::current_repository()
    {
        let counts = repo.record_failed_job(batch_id, env.id).await;
        if let Ok(c) = counts {
            // Cancel-on-first-failure unless allow_failures is set.
            if let Ok(Some(b)) = repo.find(batch_id).await {
                if !b.options.allow_failures {
                    let _ = repo.cancel(batch_id).await;
                }
                if c.pending_jobs == 0 {
                    let _ = repo.mark_finished(batch_id).await;
                    fire_batch_callbacks(&b, BatchPhase::Catch).await;
                    fire_batch_callbacks(&b, BatchPhase::Finally).await;
                }
            }
        }
    }
}

fn settlement_failure(
    driver: &dyn QueueDriver,
    env: &Envelope,
    operation: &'static str,
    outcome: &'static str,
    err: &FrameworkError,
) {
    let msg = match (operation, outcome) {
        ("ack", "success") => {
            "queue ack failed after successful run; \
             job may be re-delivered (at-least-once)"
        }
        ("ack", "dead_letter") => {
            "queue ack failed for dead-lettered job; \
             reservation may stay until visibility expiry"
        }
        ("ack", "timeout_dead_letter") => {
            "queue ack failed for timed-out dead-lettered job; \
             reservation may stay until visibility expiry"
        }
        ("ack", "deleted") => {
            "queue ack failed for middleware-dropped job; \
             reservation may stay until visibility expiry"
        }
        ("nack", "retry") => {
            "queue nack failed; reservation may be redelivered \
             after visibility expiry without bumped attempts"
        }
        ("nack", "timeout_retry") => {
            "queue nack failed after timeout; reservation may be \
             redelivered after visibility expiry without bumped attempts"
        }
        ("nack", "released") => {
            "queue nack failed for released job; \
             reservation may be redelivered after visibility expiry"
        }
        _ => "queue settlement failed",
    };
    tracing::error!(
        job = %env.job_name,
        id = %env.id,
        driver = driver.name(),
        error = %err,
        operation,
        outcome,
        "{msg}"
    );
    Metrics::counter(METRIC_SETTLEMENT_FAILURES).inc_with(&[
        ("operation", operation),
        ("driver", driver.name()),
        ("job", env.job_name.as_str()),
        ("outcome", outcome),
    ]);
}

enum BatchPhase {
    Then,
    Catch,
    Finally,
}

async fn fire_batch_callbacks(batch: &crate::queue::batch::Batch, phase: BatchPhase) {
    let names = match phase {
        BatchPhase::Then => &batch.options.then_callbacks,
        BatchPhase::Catch => &batch.options.catch_callbacks,
        BatchPhase::Finally => &batch.options.finally_callbacks,
    };
    let error = if matches!(phase, BatchPhase::Catch) {
        Some("one or more jobs in the batch failed".to_string())
    } else {
        None
    };
    for name in names {
        if let Some(cb) = resolve_callback(name) {
            if let Err(e) = cb.handle(batch.clone(), error.clone()).await {
                tracing::error!(
                    batch = %batch.id,
                    callback = name,
                    error = %e,
                    "batch callback returned an error"
                );
            }
        } else {
            tracing::warn!(
                batch = %batch.id,
                callback = name,
                "batch callback name has no registered handler"
            );
        }
    }
}
