//! Durable workflow engine
//!
//! Provides a Postgres-backed durable workflow system with step persistence
//! and automatic retries. Inspired by Laravel queues and DBOS.
//!
//! # Example
//!
//! ```rust,ignore
//! use suprnova::{workflow, workflow_step, start_workflow, FrameworkError};
//!
//! #[workflow_step]
//! async fn fetch_user(user_id: i64) -> Result<String, FrameworkError> {
//!     Ok(format!("user:{}", user_id))
//! }
//!
//! #[workflow_step]
//! async fn send_email(user: String) -> Result<(), FrameworkError> {
//!     println!("Sending email to {}", user);
//!     Ok(())
//! }
//!
//! #[workflow]
//! async fn welcome_flow(user_id: i64) -> Result<(), FrameworkError> {
//!     let user = fetch_user(user_id).await?;
//!     send_email(user).await?;
//!     Ok(())
//! }
//!
//! // Enqueue a workflow
//! // let handle = start_workflow!(welcome_flow, 123).await?;
//! // handle.wait().await?;
//!
//! // Run worker (separate process):
//! // suprnova workflow:work
//! ```

pub mod config;
pub mod context;
pub mod entities;
#[doc(hidden)]
pub mod registry;
pub mod store;
pub mod types;

pub use config::WorkflowConfig;
pub use context::WorkflowContext;
pub use types::{StepStatus, WorkflowHandle, WorkflowStatus};

use crate::config::Config;
use crate::error::FrameworkError;
use crate::workflow::types::ClaimedWorkflow;
use chrono::{Duration as ChronoDuration, Utc};
use futures::FutureExt;
use rand::RngExt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

/// RAII guard that aborts the wrapped task on drop.
///
/// Wraps the workflow heartbeat task so the lease-renewal loop is guaranteed
/// to stop the moment `process_claimed_workflow` returns or panics — even if
/// a later `?` early-returns from one of the settlement arms. Without this,
/// a leaked heartbeat would keep extending `locked_until` for a workflow no
/// worker is actually running, blocking reclamation forever.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Spawn the heartbeat task that extends the workflow lease at half the
/// lock-timeout interval while a workflow body executes.
///
/// Returns an `AbortOnDrop` guard. Drop or let-go-of-scope to stop the
/// heartbeat. The interval is `max(lock_timeout / 2, 1s)` so very small
/// timeouts still produce sane tick rates instead of busy-looping.
fn spawn_lease_heartbeat(workflow_id: i64, lock_timeout: Duration) -> AbortOnDrop {
    let interval = std::cmp::max(lock_timeout / 2, Duration::from_secs(1));
    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // First tick fires immediately; skip it so we don't refresh the
        // lease the worker just set in `claim_next_workflow`.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if let Err(err) = store::refresh_lock(workflow_id, lock_timeout).await {
                tracing::warn!(
                    workflow_id,
                    error = %err,
                    "workflow lease heartbeat failed; another worker may reclaim this row"
                );
            }
        }
    });
    AbortOnDrop(handle)
}

/// Start a workflow by name with serialized input JSON
pub async fn start_named(name: &str, input: &str) -> Result<WorkflowHandle, FrameworkError> {
    if registry::find(name).is_none() {
        return Err(FrameworkError::internal(format!(
            "Workflow '{}' is not registered",
            name
        )));
    }

    let config = Config::get::<WorkflowConfig>().unwrap_or_default();
    store::insert_workflow(name, input, config.max_attempts).await
}

/// Normalize a workflow name to module_path::fn_name form
pub fn normalize_workflow_name(name: &str) -> String {
    let trimmed = name.replace(' ', "");
    if trimmed.contains("::") {
        trimmed
    } else {
        format!("{}::{}", module_path!(), trimmed)
    }
}

/// Workflow worker daemon
pub struct WorkflowWorker {
    config: Arc<WorkflowConfig>,
    worker_id: String,
}

impl Default for WorkflowWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkflowWorker {
    /// Create a worker with config from environment
    pub fn new() -> Self {
        let config = Config::get::<WorkflowConfig>().unwrap_or_default();
        Self::with_config(config)
    }

    /// Create a worker with a custom config
    pub fn with_config(config: WorkflowConfig) -> Self {
        let random: u64 = rand::rng().random();
        let worker_id = format!("{}-{}", std::process::id(), random);
        Self {
            config: Arc::new(config),
            worker_id,
        }
    }

    /// Run the worker loop indefinitely
    pub async fn work_loop() -> Result<(), FrameworkError> {
        Self::new().run().await
    }

    async fn run(self) -> Result<(), FrameworkError> {
        let poll = Duration::from_millis(self.config.poll_interval_ms);
        let semaphore = Arc::new(Semaphore::new(self.config.concurrency));

        loop {
            let permit = semaphore.clone().acquire_owned().await.unwrap();
            let claim = store::claim_next_workflow(&self.worker_id, &self.config).await;

            match claim {
                Ok(Some(claimed)) => {
                    let config = self.config.clone();
                    let worker_id = self.worker_id.clone();
                    tokio::spawn(async move {
                        if let Err(err) =
                            process_claimed_workflow(claimed, config, &worker_id).await
                        {
                            eprintln!("Workflow execution error: {}", err);
                        }
                        drop(permit);
                    });
                }
                Ok(None) => {
                    drop(permit);
                    tokio::time::sleep(poll).await;
                }
                Err(err) => {
                    eprintln!("Workflow claim error: {}", err);
                    drop(permit);
                    tokio::time::sleep(poll).await;
                }
            }
        }
    }
}

async fn process_claimed_workflow(
    claimed: ClaimedWorkflow,
    config: Arc<WorkflowConfig>,
    _worker_id: &str,
) -> Result<(), FrameworkError> {
    let entry = match registry::find(&claimed.name) {
        Some(entry) => entry,
        None => {
            store::mark_failed(claimed.id, "Workflow not registered").await?;
            return Ok(());
        }
    };

    let lock_timeout = Duration::from_secs(config.lock_timeout_secs);
    let ctx = WorkflowContext::new(claimed.id, lock_timeout);

    // Extend the workflow lease while the body runs so long-running steps
    // do not get reclaimed mid-flight by another worker. The pre/post-step
    // refreshes in `WorkflowContext::run_step_with_input` cover the step
    // boundaries, but they do nothing while a step future is awaiting
    // (network I/O, sleeps, retries). Without this, a step that takes
    // longer than `lock_timeout_secs` (default 30s) lets
    // `claim_next_workflow` reclaim the workflow under our feet.
    //
    // The guard aborts the heartbeat task on drop. That's load-bearing —
    // each settle arm uses `?`, so an early return must not leak the
    // heartbeat task and have it keep extending `locked_until` for a
    // workflow nobody is running.
    let _heartbeat = spawn_lease_heartbeat(claimed.id, lock_timeout);

    // Run the workflow body inside a panic boundary so a panicking handler
    // does not strand the row. The spawn site only logs Err returns; a panic
    // would otherwise unwind the spawned task and skip the requeue/mark_failed
    // path entirely, leaving status='running' until the lease expires —
    // and the lease itself only matters now that `claim_next_workflow`
    // reclaims expired-running rows. The boundary mirrors the request-path
    // pattern in `server::execute_chain_safely`: catch the unwind, downcast
    // the payload, fold into the existing Err arm so the row goes through
    // the same retry/fail accounting as a returned `FrameworkError`.
    let body = AssertUnwindSafe(ctx.enter(async { (entry.run)(&claimed.input).await }));
    let result = match body.catch_unwind().await {
        Ok(inner) => inner,
        Err(panic) => {
            let msg = crate::server::panic_payload_message(&panic);
            tracing::error!(
                workflow_id = claimed.id,
                workflow_name = %claimed.name,
                attempts = claimed.attempts,
                max_attempts = claimed.max_attempts,
                panic = %msg,
                "workflow handler panicked — routing through retry/fail path"
            );
            Err(FrameworkError::internal(format!(
                "workflow handler panicked: {msg}"
            )))
        }
    };

    match result {
        Ok(output) => {
            store::mark_succeeded(claimed.id, &output).await?;
        }
        Err(err) => {
            if claimed.attempts < claimed.max_attempts {
                let backoff = config.retry_backoff_secs * claimed.attempts as i64;
                let next_run_at = Utc::now().naive_utc() + ChronoDuration::seconds(backoff);
                store::requeue(claimed.id, &err.to_string(), next_run_at).await?;
            } else {
                store::mark_failed(claimed.id, &err.to_string()).await?;
            }
        }
    }

    Ok(())
}

/// Enqueue a workflow by function name with serialized args
///
/// Example:
/// ```rust,ignore
/// let handle = start_workflow!(my_workflow, 42, "hello").await?;
/// ```
#[macro_export]
macro_rules! start_workflow {
    ($workflow:path $(, $arg:expr)* $(,)?) => {{
        async {
            let __name = stringify!($workflow);
            let __name = if __name.contains("::") {
                __name.to_string()
            } else {
                format!("{}::{}", module_path!(), __name)
            };
            let __name = __name.replace(' ', "");
            let __input = ::suprnova::serde_json::to_string(&( $($arg,)* ))
                .map_err(|e| ::suprnova::FrameworkError::internal(format!("Workflow input serialize error: {}", e)))?;
            ::suprnova::workflow::start_named(&__name, &__input).await
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestDatabase;
    use sea_orm_migration::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use suprnova_macros::{workflow, workflow_step};

    static ALWAYS_CALLS: AtomicUsize = AtomicUsize::new(0);
    static FLAKY_CALLS: AtomicUsize = AtomicUsize::new(0);
    static CACHE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static INPUT_MISMATCH_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[workflow_step]
    async fn always_step() -> Result<i32, FrameworkError> {
        ALWAYS_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(1)
    }

    #[workflow_step]
    async fn flaky_step() -> Result<i32, FrameworkError> {
        let attempt = FLAKY_CALLS.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 {
            Err(FrameworkError::internal("flaky"))
        } else {
            Ok(2)
        }
    }

    #[workflow]
    async fn test_workflow() -> Result<i32, FrameworkError> {
        let a = always_step().await?;
        let b = flaky_step().await?;
        Ok(a + b)
    }

    #[workflow]
    async fn name_norm_workflow(value: i32) -> Result<i32, FrameworkError> {
        Ok(value)
    }

    #[workflow]
    async fn panicking_workflow() -> Result<i32, FrameworkError> {
        panic!("boom");
    }

    // Sleep duration for the heartbeat regression test below.
    // Long enough to outlive the 2s lease the test sets, short enough to
    // keep the test snappy.
    const SLOW_STEP_SLEEP_MS: u64 = 2_500;

    #[workflow_step]
    async fn slow_step() -> Result<i32, FrameworkError> {
        tokio::time::sleep(Duration::from_millis(SLOW_STEP_SLEEP_MS)).await;
        Ok(7)
    }

    #[workflow]
    async fn slow_workflow() -> Result<i32, FrameworkError> {
        let v = slow_step().await?;
        Ok(v)
    }

    #[tokio::test]
    async fn test_step_caching() {
        let _db = setup_db().await;
        CACHE_CALLS.store(0, Ordering::SeqCst);

        let handle = store::insert_workflow("cache", "{}", 3)
            .await
            .expect("workflow insert");

        let ctx = WorkflowContext::new(handle.id(), Duration::from_secs(30));
        let ctx_inner = ctx.clone();
        let _ = ctx
            .enter(async move {
                ctx_inner
                    .run_step_with_input(
                        "cache-step",
                        serde_json::to_string(&()).unwrap(),
                        || async {
                            CACHE_CALLS.fetch_add(1, Ordering::SeqCst);
                            Ok::<_, FrameworkError>(42)
                        },
                    )
                    .await
                    .unwrap()
            })
            .await;

        let ctx2 = WorkflowContext::new(handle.id(), Duration::from_secs(30));
        let ctx2_inner = ctx2.clone();
        let value = ctx2
            .enter(async move {
                ctx2_inner
                    .run_step_with_input(
                        "cache-step",
                        serde_json::to_string(&()).unwrap(),
                        || async {
                            CACHE_CALLS.fetch_add(1, Ordering::SeqCst);
                            Ok::<_, FrameworkError>(99)
                        },
                    )
                    .await
                    .unwrap()
            })
            .await;

        assert_eq!(value, 42);
        assert_eq!(CACHE_CALLS.load(Ordering::SeqCst), 1);
    }

    // Replaying the same step name+index with a *different* serialized input
    // must fail loud rather than silently returning the cached output from
    // the prior input. Without the determinism guard, the second call would
    // return the cached `42` even though the caller passed input `7` —
    // corrupting any downstream step that branches on this step's output.
    #[tokio::test]
    async fn test_step_replay_with_mismatched_input_errors() {
        let _db = setup_db().await;
        INPUT_MISMATCH_CALLS.store(0, Ordering::SeqCst);

        let handle = store::insert_workflow("input-mismatch", "{}", 3)
            .await
            .expect("workflow insert");

        // First pass: record a succeeded step with input `5`.
        let ctx = WorkflowContext::new(handle.id(), Duration::from_secs(30));
        let ctx_inner = ctx.clone();
        let first = ctx
            .enter(async move {
                ctx_inner
                    .run_step_with_input(
                        "mismatch-step",
                        serde_json::to_string(&5_i32).unwrap(),
                        || async {
                            INPUT_MISMATCH_CALLS.fetch_add(1, Ordering::SeqCst);
                            Ok::<_, FrameworkError>(42_i32)
                        },
                    )
                    .await
            })
            .await
            .expect("first run records the step");
        assert_eq!(first, 42);
        assert_eq!(INPUT_MISMATCH_CALLS.load(Ordering::SeqCst), 1);

        // Replay with a different input at the same step name+index.
        // Must return an error rather than the stale `42`.
        let ctx2 = WorkflowContext::new(handle.id(), Duration::from_secs(30));
        let ctx2_inner = ctx2.clone();
        let replayed = ctx2
            .enter(async move {
                ctx2_inner
                    .run_step_with_input(
                        "mismatch-step",
                        serde_json::to_string(&7_i32).unwrap(),
                        || async {
                            INPUT_MISMATCH_CALLS.fetch_add(1, Ordering::SeqCst);
                            Ok::<_, FrameworkError>(999_i32)
                        },
                    )
                    .await
            })
            .await;

        let err = replayed.expect_err(
            "replay with mismatched input must error, not silently return the cached output",
        );
        let msg = err.to_string();
        assert!(
            msg.contains("input mismatch"),
            "error must explain the determinism violation, got: {msg}"
        );
        assert!(
            msg.contains("deterministic"),
            "error must reference the determinism contract, got: {msg}"
        );
        // The step closure must NOT have run on the failed replay — the
        // guard short-circuits before the user function is invoked.
        assert_eq!(
            INPUT_MISMATCH_CALLS.load(Ordering::SeqCst),
            1,
            "step closure must not run when input mismatch is detected"
        );
    }

    #[tokio::test]
    async fn test_retry_flow() {
        let _db = setup_db().await;
        ALWAYS_CALLS.store(0, Ordering::SeqCst);
        FLAKY_CALLS.store(0, Ordering::SeqCst);

        let input = serde_json::to_string(&()).unwrap();
        let handle = start_named(&format!("{}::{}", module_path!(), "test_workflow"), &input)
            .await
            .expect("start workflow");

        let claimed = store::mark_running(handle.id(), "test-worker", Duration::from_secs(30))
            .await
            .expect("mark running");

        let config = WorkflowConfig::from_env();
        process_claimed_workflow(claimed, Arc::new(config), "test-worker")
            .await
            .expect("process workflow");

        let status = store::get_workflow_status(handle.id()).await.unwrap();
        assert_eq!(status, WorkflowStatus::Pending);

        let claimed = store::mark_running(handle.id(), "test-worker", Duration::from_secs(30))
            .await
            .expect("mark running again");

        let config = WorkflowConfig::from_env();
        process_claimed_workflow(claimed, Arc::new(config), "test-worker")
            .await
            .expect("process workflow again");

        let status = store::get_workflow_status(handle.id()).await.unwrap();
        assert_eq!(status, WorkflowStatus::Succeeded);
        assert_eq!(ALWAYS_CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(FLAKY_CALLS.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_name_normalization() {
        let _db = setup_db().await;

        let handle = start_workflow!(name_norm_workflow, 5)
            .await
            .expect("start workflow macro");

        let record = store::get_workflow_record(handle.id()).await.unwrap();
        let expected = format!("{}::{}", module_path!(), "name_norm_workflow");
        assert_eq!(record.name, expected);
    }

    // A panicking workflow handler must NOT strand the row in 'running'.
    // With attempts < max_attempts, the panic is routed through the same
    // requeue arm as a returned Err, so the row goes back to Pending with
    // the panic message stamped in the error column. When the attempt
    // budget is exhausted, the row lands in Failed instead. Verifies
    // `process_claimed_workflow` returns Ok(()) in both cases (the panic
    // was caught and folded into the result accounting).
    #[tokio::test]
    async fn test_panic_requeues_under_budget() {
        let _db = setup_db().await;

        let workflow_name = format!("{}::{}", module_path!(), "panicking_workflow");
        let input = serde_json::to_string(&()).unwrap();

        // max_attempts = 3, attempts will increment to 1 after mark_running,
        // so 1 < 3 — the requeue arm fires.
        let handle = store::insert_workflow(&workflow_name, &input, 3)
            .await
            .expect("insert workflow");

        let claimed = store::mark_running(handle.id(), "test-worker", Duration::from_secs(30))
            .await
            .expect("mark running");
        assert_eq!(claimed.attempts, 1);
        assert_eq!(claimed.max_attempts, 3);

        let config = WorkflowConfig::from_env();
        process_claimed_workflow(claimed, Arc::new(config), "test-worker")
            .await
            .expect(
                "process_claimed_workflow returned Err — the panic boundary should have caught it",
            );

        let status = store::get_workflow_status(handle.id()).await.unwrap();
        assert_eq!(status, WorkflowStatus::Pending, "row must be requeued");

        let record = store::get_workflow_record(handle.id()).await.unwrap();
        let err = record
            .error
            .expect("error column should carry panic message");
        assert!(
            err.contains("boom"),
            "panic payload 'boom' must reach the error column, got: {err}"
        );
        assert!(
            err.contains("panicked"),
            "error must record that it came from a panic, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_panic_marks_failed_when_budget_exhausted() {
        let _db = setup_db().await;

        let workflow_name = format!("{}::{}", module_path!(), "panicking_workflow");
        let input = serde_json::to_string(&()).unwrap();

        // max_attempts = 1: after mark_running, attempts = 1, so 1 < 1 is
        // false and the mark_failed arm fires.
        let handle = store::insert_workflow(&workflow_name, &input, 1)
            .await
            .expect("insert workflow");

        let claimed = store::mark_running(handle.id(), "test-worker", Duration::from_secs(30))
            .await
            .expect("mark running");
        assert_eq!(claimed.attempts, 1);
        assert_eq!(claimed.max_attempts, 1);

        let config = WorkflowConfig::from_env();
        process_claimed_workflow(claimed, Arc::new(config), "test-worker")
            .await
            .expect(
                "process_claimed_workflow returned Err — the panic boundary should have caught it",
            );

        let status = store::get_workflow_status(handle.id()).await.unwrap();
        assert_eq!(status, WorkflowStatus::Failed, "row must be marked failed");

        let record = store::get_workflow_record(handle.id()).await.unwrap();
        let err = record
            .error
            .expect("error column should carry panic message");
        assert!(
            err.contains("boom"),
            "panic payload 'boom' must reach the error column, got: {err}"
        );
    }

    // A workflow body that outlives the lock-timeout window must not
    // strand its row to reclamation. The fix: a heartbeat task spawned
    // inside `process_claimed_workflow` extends `locked_until` at half
    // the lock-timeout interval until the body resolves. Without the
    // heartbeat, the only mid-body lease refreshes are the per-step
    // pre/post refreshes in `WorkflowContext::run_step_with_input` —
    // a single step that runs longer than `lock_timeout_secs` would
    // therefore go the entire `f().await` window with the lease frozen
    // at the value set by the pre-step refresh, and another worker can
    // reclaim it under our feet.
    //
    // The regression check counts DISTINCT `locked_until` values seen
    // during the workflow body, excluding the pre-step refresh (which
    // happens before the step starts and is unrelated to the heartbeat).
    // Snapshot strategy:
    //
    //   * baseline = locked_until once the pre-step refresh has landed
    //     (status='running' on a step row and step started_at populated).
    //     This factors out the per-step refresh path so its single bump
    //     can't false-pass the test.
    //   * Then poll the row while the step is sleeping and record every
    //     distinct locked_until > baseline that appears before the body
    //     completes.
    //
    // With heartbeat: at least one tick fires during the 2.5s sleep
    // (interval = lock_timeout/2 = 1s), so at least one post-baseline
    // value lands → advances ≥ 1.
    //
    // Without heartbeat: nothing refreshes the lease between the
    // pre-step refresh and the step's completion, so no post-baseline
    // value appears → advances = 0 and the assertion fails.
    //
    // Backend-agnostic: this test never calls `claim_next_workflow`
    // (Postgres-only), only `process_claimed_workflow` + `refresh_lock`,
    // both SQLite-compatible.
    #[tokio::test]
    async fn test_long_running_step_extends_lease() {
        let _db = setup_db().await;

        let workflow_name = format!("{}::{}", module_path!(), "slow_workflow");
        let input = serde_json::to_string(&()).unwrap();

        let handle = store::insert_workflow(&workflow_name, &input, 3)
            .await
            .expect("insert workflow");

        // Mark the row running with a short 2s lease.
        let claimed = store::mark_running(handle.id(), "test-worker", Duration::from_secs(2))
            .await
            .expect("mark running");

        // Drive the body in the background so we can poll the row from
        // this task while the step is still sleeping.
        let mut config = WorkflowConfig::from_env();
        config.lock_timeout_secs = 2;
        let worker_id = "test-worker".to_string();
        let workflow_id = handle.id();
        let body = tokio::spawn(async move {
            process_claimed_workflow(claimed, Arc::new(config), &worker_id).await
        });

        // Wait for the pre-step refresh to land (the step row appears
        // with status='running' and started_at set). That value of
        // locked_until becomes our baseline — anything strictly greater
        // than this in the polling loop below can only have been
        // written by the heartbeat.
        let baseline_lock = {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                if std::time::Instant::now() >= deadline {
                    panic!("step row never appeared with status='running'");
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
                let step = store::load_step(workflow_id, 0, "slow_step")
                    .await
                    .expect("load step");
                if let Some(s) = step
                    && s.status == StepStatus::Running.as_str()
                    && s.started_at.is_some()
                {
                    // Step has started — capture the workflow lease as
                    // it stands after the pre-step refresh.
                    let record = store::get_workflow_record(workflow_id)
                        .await
                        .expect("load workflow record");
                    break record
                        .locked_until
                        .expect("pre-step refresh should set locked_until");
                }
            }
        };

        // Count distinct post-baseline locked_until values that appear
        // while the body is still running. Heartbeat firings show up
        // here; pre-step / post-step refreshes do not (pre-step is
        // baseline, post-step lands after status changes away from
        // 'running').
        let mut post_baseline_advances: std::collections::BTreeSet<chrono::NaiveDateTime> =
            std::collections::BTreeSet::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
            let record = store::get_workflow_record(workflow_id)
                .await
                .expect("poll workflow record");
            if record.status != WorkflowStatus::Running.as_str() {
                // Body has settled — post-step refresh and mark_succeeded
                // have either fired or are about to. Stop counting; we
                // only care about mid-body advances.
                break;
            }
            if let Some(current) = record.locked_until
                && current > baseline_lock
            {
                post_baseline_advances.insert(current);
            }
        }

        assert!(
            !post_baseline_advances.is_empty(),
            "expected heartbeat to extend locked_until at least once while the long-running step \
             was executing; baseline (post-pre-step-refresh) = {baseline_lock}, no advance observed"
        );

        // The body must still settle cleanly — the heartbeat guard
        // must abort the renewal task on drop, leaving the final
        // `mark_succeeded` write authoritative and the row in
        // Succeeded.
        body.await
            .expect("workflow body task panicked")
            .expect("process_claimed_workflow returned Err");

        let status = store::get_workflow_status(workflow_id).await.unwrap();
        assert_eq!(
            status,
            WorkflowStatus::Succeeded,
            "workflow must reach Succeeded after the heartbeat-guarded body completes"
        );
    }

    // Crash recovery: a worker that died mid-flight leaves a row in
    // status='running' whose `locked_until` lease eventually expires.
    // `claim_next_workflow` must reclaim that row so another worker can
    // pick the workflow up. SQLite is filtered out at the top of
    // `claim_next_workflow` (the SQL uses FOR UPDATE SKIP LOCKED +
    // returning, Postgres-only), so this test is env-gated on a real
    // Postgres reachable via `DATABASE_URL`. Ignored by default; ran in
    // CI environments that provision a Postgres for the workflow suite.
    #[tokio::test]
    #[ignore = "requires Postgres at DATABASE_URL"]
    async fn test_claim_reclaims_expired_running_row() {
        use crate::container::testing::TestContainer;
        use crate::database::DbConnection;
        use crate::database::config::DatabaseConfig;
        use sea_orm::ConnectionTrait;

        let Some(pg_url) = postgres_url_or_skip("claim_reclaims_expired_running_row") else {
            return;
        };

        let _guard = TestContainer::fake();
        let config = DatabaseConfig::builder()
            .url(&pg_url)
            .max_connections(2)
            .min_connections(1)
            .logging(false)
            .build();
        let conn = DbConnection::connect(&config).await.expect("pg connect");

        // The migrator's `create_index` calls are not `if_not_exists`,
        // so re-running against the same database fails on duplicate
        // index names. Drop the tables first so this test is idempotent
        // against a long-lived Postgres instance.
        conn.inner()
            .execute_unprepared("DROP TABLE IF EXISTS workflow_steps")
            .await
            .ok();
        conn.inner()
            .execute_unprepared("DROP TABLE IF EXISTS workflows")
            .await
            .ok();

        TestMigrator::up(conn.inner(), None)
            .await
            .expect("migrate workflows tables");

        TestContainer::singleton(conn.clone());

        // Insert a workflow row, then manually mark it 'running' with an
        // already-expired lease — simulating a worker that crashed and
        // never released its lock.
        let handle = store::insert_workflow("recoverable", "{}", 3)
            .await
            .expect("insert workflow");

        conn.inner()
            .execute_unprepared(&format!(
                "UPDATE workflows
                 SET status='running',
                     attempts=1,
                     worker_id='dead-worker',
                     locked_until=NOW() - INTERVAL '1 hour',
                     started_at=NOW() - INTERVAL '1 hour'
                 WHERE id={}",
                handle.id()
            ))
            .await
            .expect("simulate crashed worker");

        let cfg = WorkflowConfig::from_env();
        let claimed = store::claim_next_workflow("recovery-worker", &cfg)
            .await
            .expect("claim_next_workflow")
            .expect("expected to reclaim the expired-running row");

        assert_eq!(claimed.id, handle.id());
        assert_eq!(
            claimed.attempts, 2,
            "reclaimed row must have its attempt counter incremented"
        );

        let record = store::get_workflow_record(handle.id()).await.unwrap();
        assert_eq!(record.status, WorkflowStatus::Running.as_str());
        assert_eq!(record.worker_id.as_deref(), Some("recovery-worker"));
    }

    fn postgres_url_or_skip(test_name: &str) -> Option<String> {
        match std::env::var("DATABASE_URL") {
            Ok(url) if url.starts_with("postgres://") || url.starts_with("postgresql://") => {
                Some(url)
            }
            Ok(_) => {
                eprintln!("[{test_name}] skipping: DATABASE_URL is not a Postgres URL");
                None
            }
            Err(_) => {
                eprintln!("[{test_name}] skipping: DATABASE_URL not set");
                None
            }
        }
    }

    async fn setup_db() -> TestDatabase {
        TestDatabase::fresh::<TestMigrator>()
            .await
            .expect("test db")
    }

    pub struct TestMigrator;

    #[async_trait::async_trait]
    impl MigratorTrait for TestMigrator {
        fn migrations() -> Vec<Box<dyn MigrationTrait>> {
            vec![
                Box::new(CreateWorkflowsTable),
                Box::new(CreateWorkflowStepsTable),
            ]
        }
    }

    pub struct CreateWorkflowsTable;

    impl MigrationName for CreateWorkflowsTable {
        // Explicit, file-stable version. `DeriveMigrationName` derives from
        // the parent module path, which collides with `CreateWorkflowStepsTable`
        // because both live in the same `tests` module.
        fn name(&self) -> &str {
            "m20240101_000001_create_workflows"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for CreateWorkflowsTable {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .create_table(
                    Table::create()
                        .table(Workflows::Table)
                        .if_not_exists()
                        .col(
                            ColumnDef::new(Workflows::Id)
                                .big_integer()
                                .not_null()
                                .auto_increment()
                                .primary_key(),
                        )
                        .col(ColumnDef::new(Workflows::Name).string().not_null())
                        .col(ColumnDef::new(Workflows::Status).string().not_null())
                        .col(ColumnDef::new(Workflows::Input).text().not_null())
                        .col(ColumnDef::new(Workflows::Output).text().null())
                        .col(ColumnDef::new(Workflows::Error).text().null())
                        .col(ColumnDef::new(Workflows::Attempts).integer().not_null())
                        .col(ColumnDef::new(Workflows::MaxAttempts).integer().not_null())
                        .col(ColumnDef::new(Workflows::NextRunAt).timestamp().null())
                        .col(ColumnDef::new(Workflows::LockedUntil).timestamp().null())
                        .col(ColumnDef::new(Workflows::WorkerId).string().null())
                        .col(
                            ColumnDef::new(Workflows::CreatedAt)
                                .timestamp()
                                .not_null()
                                .default(Expr::current_timestamp()),
                        )
                        .col(
                            ColumnDef::new(Workflows::UpdatedAt)
                                .timestamp()
                                .not_null()
                                .default(Expr::current_timestamp()),
                        )
                        .col(ColumnDef::new(Workflows::StartedAt).timestamp().null())
                        .col(ColumnDef::new(Workflows::CompletedAt).timestamp().null())
                        .to_owned(),
                )
                .await?;

            manager
                .create_index(
                    Index::create()
                        .name("idx_workflows_status")
                        .table(Workflows::Table)
                        .col(Workflows::Status)
                        .to_owned(),
                )
                .await?;

            manager
                .create_index(
                    Index::create()
                        .name("idx_workflows_next_run_at")
                        .table(Workflows::Table)
                        .col(Workflows::NextRunAt)
                        .to_owned(),
                )
                .await?;

            manager
                .create_index(
                    Index::create()
                        .name("idx_workflows_locked_until")
                        .table(Workflows::Table)
                        .col(Workflows::LockedUntil)
                        .to_owned(),
                )
                .await
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .drop_table(Table::drop().table(Workflows::Table).to_owned())
                .await
        }
    }

    pub struct CreateWorkflowStepsTable;

    impl MigrationName for CreateWorkflowStepsTable {
        fn name(&self) -> &str {
            "m20240101_000002_create_workflow_steps"
        }
    }

    #[async_trait::async_trait]
    impl MigrationTrait for CreateWorkflowStepsTable {
        async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .create_table(
                    Table::create()
                        .table(WorkflowSteps::Table)
                        .if_not_exists()
                        .col(
                            ColumnDef::new(WorkflowSteps::Id)
                                .big_integer()
                                .not_null()
                                .auto_increment()
                                .primary_key(),
                        )
                        .col(
                            ColumnDef::new(WorkflowSteps::WorkflowId)
                                .big_integer()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(WorkflowSteps::StepIndex)
                                .integer()
                                .not_null(),
                        )
                        .col(ColumnDef::new(WorkflowSteps::StepName).string().not_null())
                        .col(ColumnDef::new(WorkflowSteps::Status).string().not_null())
                        .col(ColumnDef::new(WorkflowSteps::Input).text().not_null())
                        .col(ColumnDef::new(WorkflowSteps::Output).text().null())
                        .col(ColumnDef::new(WorkflowSteps::Error).text().null())
                        .col(ColumnDef::new(WorkflowSteps::Attempts).integer().not_null())
                        .col(
                            ColumnDef::new(WorkflowSteps::CreatedAt)
                                .timestamp()
                                .not_null()
                                .default(Expr::current_timestamp()),
                        )
                        .col(
                            ColumnDef::new(WorkflowSteps::UpdatedAt)
                                .timestamp()
                                .not_null()
                                .default(Expr::current_timestamp()),
                        )
                        .col(ColumnDef::new(WorkflowSteps::StartedAt).timestamp().null())
                        .col(
                            ColumnDef::new(WorkflowSteps::CompletedAt)
                                .timestamp()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;

            manager
                .create_index(
                    Index::create()
                        .name("idx_workflow_steps_workflow_id")
                        .table(WorkflowSteps::Table)
                        .col(WorkflowSteps::WorkflowId)
                        .to_owned(),
                )
                .await?;

            manager
                .create_index(
                    Index::create()
                        .name("idx_workflow_steps_unique")
                        .table(WorkflowSteps::Table)
                        .col(WorkflowSteps::WorkflowId)
                        .col(WorkflowSteps::StepIndex)
                        .unique()
                        .to_owned(),
                )
                .await
        }

        async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
            manager
                .drop_table(Table::drop().table(WorkflowSteps::Table).to_owned())
                .await
        }
    }

    #[derive(DeriveIden)]
    enum Workflows {
        Table,
        Id,
        Name,
        Status,
        Input,
        Output,
        Error,
        Attempts,
        MaxAttempts,
        NextRunAt,
        LockedUntil,
        WorkerId,
        CreatedAt,
        UpdatedAt,
        StartedAt,
        CompletedAt,
    }

    #[derive(DeriveIden)]
    enum WorkflowSteps {
        Table,
        Id,
        WorkflowId,
        StepIndex,
        StepName,
        Status,
        Input,
        Output,
        Error,
        Attempts,
        CreatedAt,
        UpdatedAt,
        StartedAt,
        CompletedAt,
    }
}
