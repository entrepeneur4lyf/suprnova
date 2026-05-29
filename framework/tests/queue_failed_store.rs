//! Failed-jobs store wiring: worker dead-letters write through to the
//! configured store.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use std::time::Duration;
use suprnova::error::FrameworkError;
use suprnova::queue::{
    FailedJobStore, Job, MemoryFailedJobStore, MemoryQueueDriver, Queue,
    worker::{WorkerConfig, register_job, run_worker},
};
use tokio_util::sync::CancellationToken;

#[derive(Serialize, Deserialize, Clone)]
struct DeadJob;

#[async_trait]
impl Job for DeadJob {
    fn job_name() -> &'static str {
        "queue_failed_store::DeadJob"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        Err(FrameworkError::internal("permanent failure"))
    }
    fn max_tries() -> u32 {
        1
    }
}

#[tokio::test]
#[serial]
async fn worker_writes_dead_letter_to_failed_store() {
    register_job::<DeadJob>();
    let store = Arc::new(MemoryFailedJobStore::new());
    Queue::set_failed_store(store.clone());

    let driver = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());
    Queue::push(DeadJob).await.unwrap();

    let cfg = WorkerConfig {
        visibility_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(5),
        max_jobs: Some(1),
    };
    let cancel = CancellationToken::new();
    run_worker(driver, cfg, cancel).await;

    let all = store.all().await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].job_name, DeadJob::job_name());
    assert!(all[0].exception.contains("permanent failure"));
}

#[derive(Serialize, Deserialize, Clone)]
struct FlakyJob;

#[async_trait]
impl Job for FlakyJob {
    fn job_name() -> &'static str {
        "queue_failed_store::FlakyJob"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        Err(FrameworkError::internal(
            "transient that retries didn't fix",
        ))
    }
    fn max_tries() -> u32 {
        1
    }
}

#[tokio::test]
#[serial]
async fn retry_failed_re_enqueues_and_clears_the_record() {
    register_job::<FlakyJob>();
    let store = Arc::new(MemoryFailedJobStore::new());
    Queue::set_failed_store(store.clone());

    let driver = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());
    Queue::push(FlakyJob).await.unwrap();

    let cfg = WorkerConfig {
        visibility_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(5),
        max_jobs: Some(1),
    };
    let cancel = CancellationToken::new();
    run_worker(driver.clone(), cfg, cancel).await;

    let all = store.all().await.unwrap();
    assert_eq!(all.len(), 1);
    let failed_id = all[0].id;

    // Retry it. Driver should hold the envelope, store should be empty.
    let retried = Queue::retry_failed(failed_id).await.unwrap();
    assert!(
        retried,
        "retry_failed should return true for a found record"
    );
    assert_eq!(store.count().await.unwrap(), 0);
    assert_eq!(Queue::pending_size().await.unwrap(), 1);
    // Re-popping the retried envelope shows attempts back at 0.
    use suprnova::queue::QueueDriver;
    let popped = driver
        .pop(Duration::from_secs(5))
        .await
        .unwrap()
        .expect("retried envelope should pop");
    assert_eq!(popped.envelope.attempts, 0);
    assert_eq!(popped.envelope.job_name, FlakyJob::job_name());

    // Retry on an unknown id returns false.
    let again = Queue::retry_failed(failed_id).await.unwrap();
    assert!(!again);
}
