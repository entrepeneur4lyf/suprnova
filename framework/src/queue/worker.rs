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
//! execute the same side effect twice.

use crate::error::FrameworkError;
use crate::lock;
use crate::queue::driver::QueueDriver;
use crate::queue::retry::next_delay;
use crate::queue::Job;
use futures::future::BoxFuture;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

type Dispatcher = Arc<dyn Fn(serde_json::Value) -> BoxFuture<'static, Result<(), FrameworkError>> + Send + Sync>;

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
    g.get_or_insert_with(HashMap::new).insert(J::job_name().to_string(), f);
}

pub async fn dispatch_by_name(name: &str, payload: serde_json::Value) -> Result<(), FrameworkError> {
    let dispatcher = {
        let g = lock::read(&REGISTRY).expect("queue registry poisoned");
        let map = g.as_ref()
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
    lock::read(&REGISTRY).expect("queue registry poisoned")
        .as_ref()
        .map(|m| { let mut v: Vec<_> = m.keys().cloned().collect(); v.sort(); v })
        .unwrap_or_default()
}

// ============================================================================
// Worker loop (Task 8)
// ============================================================================

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub visibility_timeout: Duration,
    pub poll_interval: Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            visibility_timeout: Duration::from_secs(60),
            poll_interval: Duration::from_millis(100),
        }
    }
}

/// Pull-loop worker: pops one reservation at a time, dispatches by job_name,
/// acks on success, requeues with backoff on failure, drops after max_tries.
///
/// The worker bumps `env.attempts` locally before dispatch. The memory driver's
/// `nack` also bumps `attempts` on its stored copy so the next `pop` returns
/// the correct incremented count (preventing the worker from treating every
/// retry as attempt 1).
///
/// Returns when its task is cancelled. Designed to run under `tokio::spawn`.
pub async fn run_worker(driver: Arc<dyn QueueDriver>, cfg: WorkerConfig) {
    loop {
        let popped = match driver.pop(cfg.visibility_timeout).await {
            Ok(opt) => opt,
            Err(e) => {
                tracing::error!(error = %e, driver = driver.name(), "queue pop failed");
                tokio::time::sleep(cfg.poll_interval).await;
                continue;
            }
        };
        let Some(res) = popped else {
            tokio::time::sleep(cfg.poll_interval).await;
            continue;
        };

        let mut env = res.envelope;
        env.attempts += 1;
        let timeout_opt = env.timeout_secs.map(Duration::from_secs);
        let dispatch_fut = dispatch_by_name(&env.job_name, env.payload.clone());

        let outcome: Result<(), FrameworkError> = match timeout_opt {
            Some(t) => match tokio::time::timeout(t, dispatch_fut).await {
                Ok(inner) => inner,
                Err(_) => Err(FrameworkError::internal(format!(
                    "job {} timed out after {}s",
                    env.job_name,
                    t.as_secs()
                ))),
            },
            None => dispatch_fut.await,
        };

        match outcome {
            Ok(()) => {
                let _ = driver.ack(&res.token).await;
                tracing::debug!(job = %env.job_name, id = %env.id, "queue job ok");
            }
            Err(e) => {
                let is_timeout = e.to_string().contains("timed out after");
                let exhausted = env.attempts >= env.max_tries
                    || (is_timeout && env.fail_on_timeout);
                if exhausted {
                    tracing::error!(
                        job = %env.job_name,
                        id = %env.id,
                        attempts = env.attempts,
                        error = %e,
                        "queue job dead-lettered (max_tries exhausted)"
                    );
                    let _ = driver.ack(&res.token).await;
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
                    let _ = driver.nack(&res.token, delay).await;
                }
            }
        }
    }
}
