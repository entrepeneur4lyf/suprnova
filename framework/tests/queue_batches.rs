//! Queued batch tests: dispatch, progress tracking, cancellation, callbacks.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use suprnova::App;
use suprnova::cache::{Cache, CacheStore, InMemoryCache};
use suprnova::error::FrameworkError;
use suprnova::queue::batch::register_callback;
use suprnova::queue::{
    Batch, BatchCallback, Job, MemoryBatchRepository, MemoryQueueDriver, Queue,
    worker::{WorkerConfig, register_job, run_worker},
};
use tokio_util::sync::CancellationToken;

fn cache_init() {
    if !Cache::is_initialized() {
        App::bind::<dyn CacheStore>(Arc::new(InMemoryCache::new()));
    }
}

static BATCHED_RUNS: AtomicU32 = AtomicU32::new(0);

#[derive(Serialize, Deserialize, Clone)]
struct BatchedJob {
    n: u32,
}

#[async_trait]
impl Job for BatchedJob {
    fn job_name() -> &'static str {
        "queue_batches::BatchedJob"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        BATCHED_RUNS.fetch_add(self.n, Ordering::SeqCst);
        Ok(())
    }
}

static CALLBACK_HITS: AtomicU32 = AtomicU32::new(0);

struct CountingCallback {
    name: &'static str,
}

#[async_trait]
impl BatchCallback for CountingCallback {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn handle(&self, _batch: Batch, _err: Option<String>) -> Result<(), FrameworkError> {
        CALLBACK_HITS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
#[serial]
async fn batch_dispatches_every_job_and_fires_then_finally() {
    cache_init();
    BATCHED_RUNS.store(0, Ordering::SeqCst);
    CALLBACK_HITS.store(0, Ordering::SeqCst);
    register_job::<BatchedJob>();
    Queue::set_batch_repository(Arc::new(MemoryBatchRepository::new()));
    register_callback(Arc::new(CountingCallback { name: "then-cb" }));
    register_callback(Arc::new(CountingCallback { name: "finally-cb" }));

    let driver = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());

    let batch_id = Queue::batch()
        .name("test-batch")
        .add(BatchedJob { n: 1 })
        .add(BatchedJob { n: 2 })
        .add(BatchedJob { n: 4 })
        .then("then-cb")
        .finally("finally-cb")
        .dispatch()
        .await
        .unwrap();

    // Drain the queue.
    let cfg = WorkerConfig {
        visibility_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(5),
        max_jobs: Some(3),
    };
    let cancel = CancellationToken::new();
    run_worker(driver.clone(), cfg, cancel).await;

    assert_eq!(BATCHED_RUNS.load(Ordering::SeqCst), 7);
    let repo = Queue::batch_repository().unwrap();
    let snap = repo.find(&batch_id).await.unwrap().unwrap();
    assert_eq!(snap.pending_jobs, 0);
    assert_eq!(snap.failed_jobs, 0);
    assert!(snap.finished());
    // Both callbacks fired once.
    assert_eq!(CALLBACK_HITS.load(Ordering::SeqCst), 2);
}

#[derive(Serialize, Deserialize, Clone)]
struct FailingJob;

#[async_trait]
impl Job for FailingJob {
    fn job_name() -> &'static str {
        "queue_batches::FailingJob"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        Err(FrameworkError::internal("nope"))
    }
    fn max_tries() -> u32 {
        1
    }
}

#[tokio::test]
#[serial]
async fn batch_records_failure_and_cancels_when_allow_failures_off() {
    cache_init();
    register_job::<FailingJob>();
    Queue::set_batch_repository(Arc::new(MemoryBatchRepository::new()));

    let driver = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());

    let batch_id = Queue::batch()
        .name("fail-batch")
        .add(FailingJob)
        .dispatch()
        .await
        .unwrap();

    let cfg = WorkerConfig {
        visibility_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(5),
        max_jobs: Some(1),
    };
    let cancel = CancellationToken::new();
    run_worker(driver.clone(), cfg, cancel).await;

    let repo = Queue::batch_repository().unwrap();
    let snap = repo.find(&batch_id).await.unwrap().unwrap();
    assert_eq!(snap.failed_jobs, 1);
    assert!(snap.cancelled());
}

#[derive(Serialize, Deserialize, Clone)]
struct CancelAwareJob {
    n: u32,
}

#[async_trait]
impl Job for CancelAwareJob {
    fn job_name() -> &'static str {
        "queue_batches::CancelAwareJob"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        if self.n == 1 {
            Err(FrameworkError::internal("first job fails"))
        } else {
            BATCHED_RUNS.fetch_add(self.n, Ordering::SeqCst);
            Ok(())
        }
    }
    fn max_tries() -> u32 {
        1
    }
    fn middleware() -> Vec<Arc<dyn suprnova::queue::JobMiddleware>> {
        vec![Arc::new(suprnova::queue::SkipIfBatchCancelled)]
    }
}

#[tokio::test]
#[serial]
async fn multi_job_batch_fires_finally_even_when_first_fails_and_rest_skipped() {
    cache_init();
    BATCHED_RUNS.store(0, Ordering::SeqCst);
    CALLBACK_HITS.store(0, Ordering::SeqCst);
    register_job::<CancelAwareJob>();
    Queue::set_batch_repository(Arc::new(MemoryBatchRepository::new()));
    register_callback(Arc::new(CountingCallback {
        name: "multi-finally",
    }));

    let driver = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());

    let batch_id = Queue::batch()
        .name("multi-fail")
        .add(CancelAwareJob { n: 1 })
        .add(CancelAwareJob { n: 2 })
        .add(CancelAwareJob { n: 4 })
        .finally("multi-finally")
        .dispatch()
        .await
        .unwrap();

    // Drain. Job 1 dead-letters, cancels batch. Jobs 2-3 settle via
    // `SkipIfBatchCancelled` → `Deleted`.
    let cfg = WorkerConfig {
        visibility_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(5),
        max_jobs: Some(3),
    };
    let cancel = CancellationToken::new();
    run_worker(driver.clone(), cfg, cancel).await;

    let repo = Queue::batch_repository().unwrap();
    let snap = repo.find(&batch_id).await.unwrap().unwrap();
    assert_eq!(
        snap.pending_jobs, 0,
        "every batched job must decrement pending_jobs (cancelled or not)"
    );
    assert!(snap.finished(), "batch should be marked finished");
    // The cancelled-skip path counts as a deleted settlement, so the
    // expected runs total is only the failed job's side effects (none) —
    // no successful runs because the rest were skipped.
    assert_eq!(BATCHED_RUNS.load(Ordering::SeqCst), 0);
    // `finally` MUST fire even when the batch was cancelled mid-flight.
    assert!(
        CALLBACK_HITS.load(Ordering::SeqCst) >= 1,
        "finally callback must run after cancellation"
    );
}
