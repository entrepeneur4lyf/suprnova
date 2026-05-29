//! Queue introspection + bulk + clear tests.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use suprnova::error::FrameworkError;
use suprnova::queue::{Job, MemoryQueueDriver, Queue, QueueDriver};

#[derive(Serialize, Deserialize, Clone)]
struct Marker {
    x: u32,
}

#[async_trait]
impl Job for Marker {
    fn job_name() -> &'static str {
        "queue_introspection::Marker"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        Ok(())
    }
}

#[tokio::test]
#[serial]
async fn bulk_pushes_every_job() {
    let driver = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());
    Queue::bulk(vec![Marker { x: 1 }, Marker { x: 2 }, Marker { x: 3 }])
        .await
        .unwrap();
    assert_eq!(driver.pending_size().await.unwrap(), 3);
    assert_eq!(Queue::pending_size().await.unwrap(), 3);
    assert_eq!(Queue::size().await.unwrap(), 3);
}

#[tokio::test]
#[serial]
async fn clear_removes_every_envelope() {
    let driver = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());
    Queue::bulk(vec![Marker { x: 1 }, Marker { x: 2 }])
        .await
        .unwrap();
    let removed = Queue::clear().await.unwrap();
    assert_eq!(removed, 2);
    assert_eq!(Queue::size().await.unwrap(), 0);
}
