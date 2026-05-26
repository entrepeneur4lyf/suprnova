//! End-to-end test for the Queue facade's delayed-dispatch path.
//! Drives Queue::later through the MemoryQueueDriver and asserts the
//! envelope is invisible until tokio's virtual clock advances past the
//! delay deadline.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use suprnova::queue::driver::QueueDriver;
use suprnova::queue::memory::MemoryQueueDriver;
use suprnova::{FrameworkError, Job, Queue, async_trait};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct ScheduledNote {
    body: String,
}

#[async_trait]
impl Job for ScheduledNote {
    fn job_name() -> &'static str {
        "ScheduledNote"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn queue_later_dispatches_via_driver_and_honors_delay() {
    let driver = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());

    Queue::later(
        Duration::from_secs(60),
        ScheduledNote {
            body: "later".into(),
        },
    )
    .await
    .unwrap();

    // Immediately after dispatch, the driver MUST not surface the message.
    let nothing = driver.pop(Duration::from_millis(10)).await.unwrap();
    assert!(
        nothing.is_none(),
        "delayed job must not be visible before its deadline"
    );

    // Advance Tokio's virtual clock past available_at.
    tokio::time::advance(Duration::from_secs(61)).await;

    // Pop should succeed and the envelope should match the dispatched job.
    let reservation = driver
        .pop(Duration::from_millis(10))
        .await
        .unwrap()
        .expect("delayed job must be visible after available_at");
    assert_eq!(reservation.envelope.job_name, "ScheduledNote");
    assert_eq!(reservation.envelope.payload["body"], "later");

    driver.ack(&reservation.token).await.unwrap();
}
