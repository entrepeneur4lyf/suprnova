//! Regression test for the queue worker's per-dispatch panic boundary.
//!
//! Before the fix, a job handler that panicked would unwind out of
//! `run_through_middleware`, kill the `run_worker` task, and strand the
//! envelope's reservation until the visibility window expired. The boundary
//! converts the panic into `DispatchOutcome::Failed`, so the failure flows
//! through the existing retry / dead-letter accounting and the worker stays
//! up to drain subsequent jobs.

use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use suprnova::queue::driver::QueueDriver;
use suprnova::queue::memory::MemoryQueueDriver;
use suprnova::queue::worker::{WorkerConfig, register_job, run_worker};
use suprnova::queue::{BackoffSchedule, Queue};
use suprnova::{FrameworkError, Job, async_trait};
use tokio_util::sync::CancellationToken;

static ATTEMPTS: AtomicU32 = AtomicU32::new(0);
static SUCCESSES: AtomicU32 = AtomicU32::new(0);

#[derive(Serialize, Deserialize, Debug, Clone)]
struct PanicThenSucceedJob {
    id: u32,
}

#[async_trait]
impl Job for PanicThenSucceedJob {
    fn job_name() -> &'static str {
        "PanicThenSucceedJob"
    }
    fn max_tries() -> u32 {
        5
    }
    fn backoff() -> BackoffSchedule {
        BackoffSchedule::Fixed { secs: 0 }
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        let n = ATTEMPTS.fetch_add(1, Ordering::SeqCst) + 1;
        if n == 1 {
            // First attempt panics; without the worker-side panic boundary
            // this would tear down the spawned worker task entirely.
            panic!("synthetic panic from PanicThenSucceedJob attempt #1");
        }
        SUCCESSES.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// The headline contract for H1: a single panicking attempt must NOT kill the
/// worker. The worker must convert the panic into a typed failure, retry per
/// the job's `BackoffSchedule`/`max_tries` policy, and ultimately settle the
/// second attempt as a success — proving (a) the worker survived, (b) the
/// reservation was nack'd not stranded, and (c) the panic was routed through
/// the existing failure accounting.
#[tokio::test]
#[serial]
async fn worker_survives_handler_panic_and_retries_to_success() {
    ATTEMPTS.store(0, Ordering::SeqCst);
    SUCCESSES.store(0, Ordering::SeqCst);

    let d: Arc<dyn QueueDriver> = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(d.clone());
    register_job::<PanicThenSucceedJob>();

    Queue::push(PanicThenSucceedJob { id: 1 }).await.unwrap();

    let cfg = WorkerConfig {
        visibility_timeout: Duration::from_secs(60),
        poll_interval: Duration::from_millis(5),
        max_jobs: Some(2),
    };
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(run_worker(d.clone(), cfg, cancel.clone()));

    // Wait for the second attempt to settle as success. Without the panic
    // boundary the worker task would have aborted on attempt #1 and SUCCESSES
    // would stay at 0 — the timeout below would fail the assertion.
    for _ in 0..400 {
        if SUCCESSES.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // Worker must still be alive — cancel cleanly, not abort.
    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;

    assert_eq!(
        ATTEMPTS.load(Ordering::SeqCst),
        2,
        "handler must be invoked exactly twice: panic, then success"
    );
    assert_eq!(
        SUCCESSES.load(Ordering::SeqCst),
        1,
        "second attempt must settle as success — proves the panic was retried, \
         not left stuck on a non-acked reservation"
    );
}
