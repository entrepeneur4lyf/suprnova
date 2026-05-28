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

#[derive(Serialize, Deserialize, Debug, Clone)]
struct FlakyJob {
    fail_until: u32,
    id: u32,
}

static ATTEMPTS: AtomicU32 = AtomicU32::new(0);
static SUCCESSES: AtomicU32 = AtomicU32::new(0);

#[async_trait]
impl Job for FlakyJob {
    fn job_name() -> &'static str {
        "FlakyJob"
    }
    fn max_tries() -> u32 {
        5
    }
    fn backoff() -> BackoffSchedule {
        // Tiny fixed delay so tests don't run for real time.
        BackoffSchedule::Fixed { secs: 0 }
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        let n = ATTEMPTS.fetch_add(1, Ordering::SeqCst) + 1;
        if n < self.fail_until {
            Err(FrameworkError::internal(format!("synthetic fail #{n}")))
        } else {
            SUCCESSES.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
}

#[tokio::test]
#[serial]
async fn worker_retries_failing_job_until_success() {
    ATTEMPTS.store(0, Ordering::SeqCst);
    SUCCESSES.store(0, Ordering::SeqCst);

    let d = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(d.clone());
    register_job::<FlakyJob>();

    Queue::push(FlakyJob {
        fail_until: 3,
        id: 1,
    })
    .await
    .unwrap();

    let cfg = WorkerConfig {
        visibility_timeout: Duration::from_secs(60),
        poll_interval: Duration::from_millis(5),
        max_jobs: None,
    };
    let handle = tokio::spawn(run_worker(d.clone(), cfg, CancellationToken::new()));

    for _ in 0..200 {
        if SUCCESSES.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    handle.abort();

    assert_eq!(
        SUCCESSES.load(Ordering::SeqCst),
        1,
        "job must eventually succeed"
    );
    assert_eq!(
        ATTEMPTS.load(Ordering::SeqCst),
        3,
        "should attempt exactly 3 times"
    );
}

#[tokio::test]
#[serial]
async fn worker_dead_letters_after_max_tries() {
    ATTEMPTS.store(0, Ordering::SeqCst);
    SUCCESSES.store(0, Ordering::SeqCst);

    let d = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(d.clone());
    register_job::<FlakyJob>();

    // fail_until=999 means never succeed within 5 tries.
    Queue::push(FlakyJob {
        fail_until: 999,
        id: 2,
    })
    .await
    .unwrap();

    let cfg = WorkerConfig {
        visibility_timeout: Duration::from_secs(60),
        poll_interval: Duration::from_millis(5),
        max_jobs: None,
    };
    let handle = tokio::spawn(run_worker(d.clone(), cfg, CancellationToken::new()));

    for _ in 0..400 {
        if ATTEMPTS.load(Ordering::SeqCst) >= 5 {
            break;
        }
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    handle.abort();

    assert_eq!(
        ATTEMPTS.load(Ordering::SeqCst),
        5,
        "should stop after max_tries"
    );
    assert_eq!(SUCCESSES.load(Ordering::SeqCst), 0, "must not succeed");
}

/// Pins the cancel-during-empty-pop path: a worker idling on
/// `sleep(poll_interval)` between pops must wake within milliseconds of
/// cancel, not wait out the full interval. The deliberately long
/// `poll_interval = 5s` is the test's teeth — without the `tokio::select!`
/// wrapping both pop and the empty-queue sleep, this would time out.
#[tokio::test]
#[serial]
async fn run_worker_exits_promptly_after_cancel() {
    let driver: Arc<dyn QueueDriver> = Arc::new(MemoryQueueDriver::new());
    let cancel = CancellationToken::new();
    let cfg = WorkerConfig {
        visibility_timeout: Duration::from_secs(60),
        poll_interval: Duration::from_secs(5),
        max_jobs: None,
    };
    let handle = tokio::spawn(run_worker(driver, cfg, cancel.clone()));

    // Let the worker tick once and settle into the empty-queue sleep.
    tokio::time::sleep(Duration::from_millis(20)).await;
    cancel.cancel();

    let r = tokio::time::timeout(Duration::from_millis(500), handle).await;
    assert!(
        r.is_ok(),
        "worker did not exit within 500ms of CancellationToken::cancel \
         (the poll-interval select! arm must wake on cancel)"
    );
}
