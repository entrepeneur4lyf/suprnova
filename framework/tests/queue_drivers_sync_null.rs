//! Sync + Null driver smoke tests.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use suprnova::error::FrameworkError;
use suprnova::queue::{Job, NullQueueDriver, Queue, SyncQueueDriver, worker::register_job};

static SYNC_RAN: AtomicU32 = AtomicU32::new(0);

#[derive(Serialize, Deserialize, Clone)]
struct SyncDriverJob;

#[async_trait]
impl Job for SyncDriverJob {
    fn job_name() -> &'static str {
        "queue_drivers_sync_null::SyncDriverJob"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        SYNC_RAN.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
#[serial]
async fn sync_driver_executes_inline_on_push() {
    SYNC_RAN.store(0, Ordering::SeqCst);
    register_job::<SyncDriverJob>();
    Queue::set_driver(Arc::new(SyncQueueDriver::new()));
    Queue::push(SyncDriverJob).await.unwrap();
    assert_eq!(SYNC_RAN.load(Ordering::SeqCst), 1);
}

#[derive(Serialize, Deserialize, Clone)]
struct NullDriverJob;

#[async_trait]
impl Job for NullDriverJob {
    fn job_name() -> &'static str {
        "queue_drivers_sync_null::NullDriverJob"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        unreachable!("null driver must not run the handler");
    }
}

#[tokio::test]
#[serial]
async fn null_driver_discards_pushes_without_running() {
    register_job::<NullDriverJob>();
    Queue::set_driver(Arc::new(NullQueueDriver::new()));
    Queue::push(NullDriverJob).await.unwrap();
    Queue::push(NullDriverJob).await.unwrap();
    // Null driver reports size 0 always.
    assert_eq!(Queue::size().await.unwrap(), 0);
}
