//! End-to-end tests for job middleware: pipeline ordering, release-without-
//! burning-an-attempt, dead-letter promotion via FailOnException.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use suprnova::App;
use suprnova::cache::{Cache, CacheStore, InMemoryCache};
use suprnova::error::FrameworkError;
use suprnova::queue::middleware::{JobMiddleware, Next};
use suprnova::queue::{
    BackoffSchedule, FailOnException, Job, JobOutcome, MemoryQueueDriver, Queue, QueueDriver, Skip,
    WithoutOverlapping,
    worker::{WorkerConfig, register_job, run_through_middleware, run_worker},
};
use tokio_util::sync::CancellationToken;

fn ensure_cache() {
    if !Cache::is_initialized() {
        App::bind::<dyn CacheStore>(Arc::new(InMemoryCache::new()));
    }
}

static OK_RUNS: AtomicU32 = AtomicU32::new(0);

#[derive(Serialize, Deserialize)]
struct SkipJob;

#[async_trait]
impl Job for SkipJob {
    fn job_name() -> &'static str {
        "queue_middleware_pipeline::SkipJob"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        OK_RUNS.fetch_add(100, Ordering::SeqCst);
        Ok(())
    }
    fn middleware() -> Vec<Arc<dyn JobMiddleware>> {
        vec![Arc::new(Skip::when(true))]
    }
}

#[derive(Serialize, Deserialize)]
struct OrderedJob;

struct RecordingMw {
    label: &'static str,
}

#[async_trait]
impl JobMiddleware for RecordingMw {
    async fn handle(
        &self,
        env: suprnova::queue::Envelope,
        next: Next,
    ) -> Result<JobOutcome, FrameworkError> {
        ORDER.lock().unwrap().push(self.label);
        next(env).await
    }
}

static ORDER: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());

#[async_trait]
impl Job for OrderedJob {
    fn job_name() -> &'static str {
        "queue_middleware_pipeline::OrderedJob"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        ORDER.lock().unwrap().push("handler");
        Ok(())
    }
    fn middleware() -> Vec<Arc<dyn JobMiddleware>> {
        vec![
            Arc::new(RecordingMw { label: "outer" }),
            Arc::new(RecordingMw { label: "middle" }),
            Arc::new(RecordingMw { label: "inner" }),
        ]
    }
}

#[tokio::test]
#[serial]
async fn skip_middleware_drops_job_without_running_handler() {
    OK_RUNS.store(0, Ordering::SeqCst);
    register_job::<SkipJob>();
    let env = suprnova::queue::Envelope {
        schema_version: suprnova::queue::CURRENT_SCHEMA_VERSION,
        id: uuid::Uuid::new_v4(),
        job_name: SkipJob::job_name().into(),
        payload: serde_json::to_value(SkipJob).unwrap(),
        dispatched_at: chrono::Utc::now(),
        available_at: chrono::Utc::now(),
        attempts: 0,
        max_tries: 1,
        backoff: BackoffSchedule::default(),
        timeout_secs: None,
        fail_on_timeout: false,
        idempotency_key: None,
        batch_id: None,
        chain_remaining: Vec::new(),
    };
    let outcome = run_through_middleware(env).await.unwrap();
    assert!(matches!(outcome, JobOutcome::Deleted));
    assert_eq!(OK_RUNS.load(Ordering::SeqCst), 0);
}

#[tokio::test]
#[serial]
async fn middleware_runs_outermost_first() {
    ORDER.lock().unwrap().clear();
    register_job::<OrderedJob>();
    let env = suprnova::queue::Envelope {
        schema_version: suprnova::queue::CURRENT_SCHEMA_VERSION,
        id: uuid::Uuid::new_v4(),
        job_name: OrderedJob::job_name().into(),
        payload: serde_json::to_value(OrderedJob).unwrap(),
        dispatched_at: chrono::Utc::now(),
        available_at: chrono::Utc::now(),
        attempts: 0,
        max_tries: 1,
        backoff: BackoffSchedule::default(),
        timeout_secs: None,
        fail_on_timeout: false,
        idempotency_key: None,
        batch_id: None,
        chain_remaining: Vec::new(),
    };
    let outcome = run_through_middleware(env).await.unwrap();
    assert!(matches!(outcome, JobOutcome::Completed));
    let seen: Vec<&'static str> = ORDER.lock().unwrap().clone();
    assert_eq!(seen, vec!["outer", "middle", "inner", "handler"]);
}

#[derive(Serialize, Deserialize)]
struct LockedJob;

#[async_trait]
impl Job for LockedJob {
    fn job_name() -> &'static str {
        "queue_middleware_pipeline::LockedJob"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        Ok(())
    }
    fn middleware() -> Vec<Arc<dyn JobMiddleware>> {
        vec![Arc::new(
            WithoutOverlapping::new("lock-test").release_after(Duration::ZERO),
        )]
    }
}

#[tokio::test]
#[serial]
async fn without_overlapping_releases_without_burning_attempt() {
    ensure_cache();
    register_job::<LockedJob>();

    // Hold a competing lock so the middleware can't acquire one.
    let lock_key = "laravel-queue-overlap:queue_middleware_pipeline::LockedJob:lock-test";
    let held = Cache::lock(lock_key, Duration::from_secs(30))
        .await
        .unwrap()
        .expect("acquire the competing lock");

    // Drive the worker by hand: push, pop, settle.
    let driver = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());
    Queue::push(LockedJob).await.unwrap();

    let cfg = WorkerConfig {
        visibility_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(5),
        max_jobs: Some(1),
    };
    let cancel = CancellationToken::new();
    run_worker(driver.clone(), cfg, cancel.clone()).await;

    // After the worker exits via max_jobs, the released job should be back
    // on the driver (delayed by release_after), and its attempts must be 0
    // — never bumped, because release isn't a failure.
    let delayed = driver.delayed_size().await.unwrap();
    let pending = driver.pending_size().await.unwrap();
    assert!(
        delayed + pending >= 1,
        "released job should be re-enqueued (delayed={delayed}, pending={pending})"
    );

    // Discriminating check (the contract): pop the released envelope and
    // assert its attempts counter is still at 0. A bug here would surface
    // as attempts = 1, meaning every contention burn one of the job's
    // retry budgets — `WithoutOverlapping` would silently break.
    held.release().await.unwrap();
    // Wait until the released-with-delay envelope becomes visible. The
    // memory driver's drain runs on virtual+real clocks; in real time
    // the delay was 0 so it should be visible on the next pop.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let popped = driver
        .pop(Duration::from_secs(5))
        .await
        .unwrap()
        .expect("released envelope should be popped after lock release");
    assert_eq!(
        popped.envelope.attempts, 0,
        "release MUST NOT bump attempts (got {})",
        popped.envelope.attempts
    );
    driver.ack(&popped.token).await.unwrap();
}

#[derive(Serialize, Deserialize)]
struct BadJob;

#[async_trait]
impl Job for BadJob {
    fn job_name() -> &'static str {
        "queue_middleware_pipeline::BadJob"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        Err(FrameworkError::internal("validation failed: bad input"))
    }
    fn middleware() -> Vec<Arc<dyn JobMiddleware>> {
        vec![Arc::new(FailOnException::on_substring(vec![
            "validation failed",
        ]))]
    }
}

#[tokio::test]
#[serial]
async fn fail_on_exception_dead_letters_without_retries() {
    register_job::<BadJob>();
    let env = suprnova::queue::Envelope {
        schema_version: suprnova::queue::CURRENT_SCHEMA_VERSION,
        id: uuid::Uuid::new_v4(),
        job_name: BadJob::job_name().into(),
        payload: serde_json::to_value(BadJob).unwrap(),
        dispatched_at: chrono::Utc::now(),
        available_at: chrono::Utc::now(),
        attempts: 0,
        max_tries: 5,
        backoff: BackoffSchedule::default(),
        timeout_secs: None,
        fail_on_timeout: false,
        idempotency_key: None,
        batch_id: None,
        chain_remaining: Vec::new(),
    };
    let outcome = run_through_middleware(env).await.unwrap();
    assert!(matches!(outcome, JobOutcome::Failed { .. }));
}
