//! Worker emits lifecycle events through Event::dispatch.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use suprnova::error::FrameworkError;
use suprnova::events::{EventFacade, Listener};
use suprnova::queue::events::{JobProcessed, JobProcessing, JobQueued, WorkerStarting};
use suprnova::queue::{
    Job, MemoryQueueDriver, Queue,
    worker::{WorkerConfig, register_job, run_worker},
};
use tokio_util::sync::CancellationToken;

static EV_QUEUED: AtomicU32 = AtomicU32::new(0);
static EV_PROCESSING: AtomicU32 = AtomicU32::new(0);
static EV_PROCESSED: AtomicU32 = AtomicU32::new(0);
static EV_STARTING: AtomicU32 = AtomicU32::new(0);

struct CountQueued;
#[async_trait]
impl Listener<JobQueued> for CountQueued {
    async fn handle(&self, _e: &JobQueued) -> Result<(), FrameworkError> {
        EV_QUEUED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct CountProcessing;
#[async_trait]
impl Listener<JobProcessing> for CountProcessing {
    async fn handle(&self, _e: &JobProcessing) -> Result<(), FrameworkError> {
        EV_PROCESSING.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct CountProcessed;
#[async_trait]
impl Listener<JobProcessed> for CountProcessed {
    async fn handle(&self, _e: &JobProcessed) -> Result<(), FrameworkError> {
        EV_PROCESSED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct CountStarting;
#[async_trait]
impl Listener<WorkerStarting> for CountStarting {
    async fn handle(&self, _e: &WorkerStarting) -> Result<(), FrameworkError> {
        EV_STARTING.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct EventJob;

#[async_trait]
impl Job for EventJob {
    fn job_name() -> &'static str {
        "queue_events::EventJob"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        Ok(())
    }
}

#[tokio::test]
#[serial]
async fn worker_emits_lifecycle_events() {
    EV_QUEUED.store(0, Ordering::SeqCst);
    EV_PROCESSING.store(0, Ordering::SeqCst);
    EV_PROCESSED.store(0, Ordering::SeqCst);
    EV_STARTING.store(0, Ordering::SeqCst);

    register_job::<EventJob>();
    EventFacade::listen::<JobQueued, _>(Arc::new(CountQueued)).await;
    EventFacade::listen::<JobProcessing, _>(Arc::new(CountProcessing)).await;
    EventFacade::listen::<JobProcessed, _>(Arc::new(CountProcessed)).await;
    EventFacade::listen::<WorkerStarting, _>(Arc::new(CountStarting)).await;

    let driver = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());
    Queue::push(EventJob).await.unwrap();

    let cfg = WorkerConfig {
        visibility_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(5),
        max_jobs: Some(1),
    };
    let cancel = CancellationToken::new();
    run_worker(driver, cfg, cancel).await;

    assert_eq!(EV_QUEUED.load(Ordering::SeqCst), 1);
    assert_eq!(EV_PROCESSING.load(Ordering::SeqCst), 1);
    assert_eq!(EV_PROCESSED.load(Ordering::SeqCst), 1);
    assert_eq!(EV_STARTING.load(Ordering::SeqCst), 1);
}
