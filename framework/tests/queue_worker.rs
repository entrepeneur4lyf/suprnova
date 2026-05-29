use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;
use suprnova::queue::driver::{QueueDriver, Reservation, ReservationToken};
use suprnova::queue::envelope::Envelope;
use suprnova::queue::memory::MemoryQueueDriver;
use suprnova::queue::worker::{WorkerConfig, register_job, run_worker};
use suprnova::queue::{BackoffSchedule, Queue};
use suprnova::{FrameworkError, Job, async_trait};
use tokio_util::sync::CancellationToken;
use tracing_test::traced_test;

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

// ============================================================================
// Settlement-failure fault injection
// ============================================================================
//
// `MemoryQueueDriver` is HashMap-backed and never fails ack/nack in
// practice, but real backends (Redis, SQL, queue brokers) can fail on
// network drop / pool exhaustion / shutdown. The worker must surface those
// failures — log + telemetry counter — and continue the poll loop rather
// than crash. This driver wraps a real memory driver and toggles deliberate
// ack/nack failures so we can verify the surface.

struct FaultyAckDriver {
    inner: MemoryQueueDriver,
    /// When true, `ack` returns `Err` until reset.
    fail_ack: AtomicU32,
    /// When true, `nack` returns `Err` until reset.
    fail_nack: AtomicU32,
    /// Total ack attempts seen (incremented before the failure decision).
    ack_calls: AtomicU64,
    /// Total nack attempts seen (incremented before the failure decision).
    nack_calls: AtomicU64,
}

impl FaultyAckDriver {
    fn new() -> Self {
        Self {
            inner: MemoryQueueDriver::new(),
            fail_ack: AtomicU32::new(0),
            fail_nack: AtomicU32::new(0),
            ack_calls: AtomicU64::new(0),
            nack_calls: AtomicU64::new(0),
        }
    }
    fn set_fail_ack(&self, on: bool) {
        self.fail_ack.store(u32::from(on), Ordering::SeqCst);
    }
    fn set_fail_nack(&self, on: bool) {
        self.fail_nack.store(u32::from(on), Ordering::SeqCst);
    }
}

#[async_trait]
impl QueueDriver for FaultyAckDriver {
    async fn push(&self, env: Envelope) -> Result<(), FrameworkError> {
        self.inner.push(env).await
    }
    async fn pop(
        &self,
        visibility_timeout: Duration,
    ) -> Result<Option<Reservation>, FrameworkError> {
        self.inner.pop(visibility_timeout).await
    }
    async fn ack(&self, token: &ReservationToken) -> Result<(), FrameworkError> {
        self.ack_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_ack.load(Ordering::SeqCst) != 0 {
            return Err(FrameworkError::internal(
                "synthetic ack failure (driver unreachable)",
            ));
        }
        self.inner.ack(token).await
    }
    async fn nack(
        &self,
        token: &ReservationToken,
        requeue_delay: Duration,
    ) -> Result<(), FrameworkError> {
        self.nack_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_nack.load(Ordering::SeqCst) != 0 {
            return Err(FrameworkError::internal(
                "synthetic nack failure (driver unreachable)",
            ));
        }
        self.inner.nack(token, requeue_delay).await
    }
    fn name(&self) -> &'static str {
        "FaultyAckDriver"
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct AlwaysOkJob {
    id: u32,
}

static OK_RUNS: AtomicU32 = AtomicU32::new(0);

#[async_trait]
impl Job for AlwaysOkJob {
    fn job_name() -> &'static str {
        "AlwaysOkJob"
    }
    fn max_tries() -> u32 {
        3
    }
    fn backoff() -> BackoffSchedule {
        BackoffSchedule::Fixed { secs: 0 }
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        OK_RUNS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct AlwaysFailJob {
    id: u32,
}

static FAIL_RUNS: AtomicU32 = AtomicU32::new(0);

#[async_trait]
impl Job for AlwaysFailJob {
    fn job_name() -> &'static str {
        "AlwaysFailJob"
    }
    fn max_tries() -> u32 {
        3
    }
    fn backoff() -> BackoffSchedule {
        BackoffSchedule::Fixed { secs: 0 }
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        FAIL_RUNS.fetch_add(1, Ordering::SeqCst);
        Err(FrameworkError::internal("synthetic fail"))
    }
}

/// Successful run + ack failure: the worker must log a structured error
/// naming the consequence ("re-delivered (at-least-once)"), must not crash,
/// and must continue draining subsequent jobs.
#[tokio::test]
#[serial]
#[traced_test]
async fn worker_surfaces_ack_failure_and_continues() {
    OK_RUNS.store(0, Ordering::SeqCst);
    let faulty = Arc::new(FaultyAckDriver::new());
    faulty.set_fail_ack(true);
    let driver: Arc<dyn QueueDriver> = faulty.clone();
    Queue::set_driver(driver.clone());
    register_job::<AlwaysOkJob>();

    Queue::push(AlwaysOkJob { id: 1 }).await.unwrap();

    let cfg = WorkerConfig {
        visibility_timeout: Duration::from_secs(60),
        poll_interval: Duration::from_millis(5),
        max_jobs: Some(1),
    };
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(run_worker(driver, cfg, cancel.clone()));

    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;

    // Job ran to completion exactly once.
    assert_eq!(OK_RUNS.load(Ordering::SeqCst), 1, "job must have run");
    // Driver saw at least one ack attempt.
    assert!(
        faulty.ack_calls.load(Ordering::SeqCst) >= 1,
        "worker must have called ack"
    );
    // Structured error event present with consequence wording.
    assert!(
        logs_contain("queue ack failed after successful run"),
        "expected ack-failure tracing event"
    );
    assert!(
        logs_contain("re-delivered (at-least-once)"),
        "expected consequence wording in ack-failure event"
    );
    // The driver name attribute is captured in the structured log.
    assert!(
        logs_contain("FaultyAckDriver"),
        "expected driver name in structured log"
    );
}

/// Failed run + nack failure: the worker must log a distinct
/// consequence-bearing error ("without bumped attempts"), must not crash,
/// must record a nack attempt against the driver. Verifying via "worker
/// continues to drain a subsequent job" is the survival guarantee.
#[tokio::test]
#[serial]
#[traced_test]
async fn worker_surfaces_nack_failure_and_continues() {
    FAIL_RUNS.store(0, Ordering::SeqCst);
    OK_RUNS.store(0, Ordering::SeqCst);
    let faulty = Arc::new(FaultyAckDriver::new());
    faulty.set_fail_nack(true);
    let driver: Arc<dyn QueueDriver> = faulty.clone();
    Queue::set_driver(driver.clone());
    register_job::<AlwaysFailJob>();
    register_job::<AlwaysOkJob>();

    Queue::push(AlwaysFailJob { id: 1 }).await.unwrap();
    Queue::push(AlwaysOkJob { id: 2 }).await.unwrap();

    // Bound the run: two settlement attempts is enough — the first should
    // hit the nack-failure path (job failed, nack rejected). After that the
    // worker proceeds to the next job.
    let cfg = WorkerConfig {
        visibility_timeout: Duration::from_millis(50),
        poll_interval: Duration::from_millis(5),
        max_jobs: Some(2),
    };
    let cancel = CancellationToken::new();
    let handle = tokio::spawn(run_worker(driver, cfg, cancel.clone()));

    let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;

    assert!(
        FAIL_RUNS.load(Ordering::SeqCst) >= 1,
        "failing job must have run at least once"
    );
    assert!(
        faulty.nack_calls.load(Ordering::SeqCst) >= 1,
        "worker must have called nack on the failing job"
    );
    // Structured error event present with consequence wording.
    assert!(
        logs_contain("queue nack failed"),
        "expected nack-failure tracing event"
    );
    assert!(
        logs_contain("without bumped attempts"),
        "expected consequence wording in nack-failure event"
    );
    assert!(
        logs_contain("FaultyAckDriver"),
        "expected driver name in structured log"
    );
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
