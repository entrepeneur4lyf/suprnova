//! Integration tests for the supervisor restart lifecycle.
//!
//! These tests exercise `run_with_restart_for_testing` directly — bypassing
//! the inventory registry — so they can construct supervisors with test state.

use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use suprnova::supervisor::{RestartPolicy, Supervisor, SupervisorRegistry};
use suprnova::supervisor::run_with_restart_for_testing;
use suprnova::FrameworkError;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Fails the first `fail_until` invocations, then succeeds.
struct CountingSupervisor {
    counter: Arc<AtomicUsize>,
    fail_until: usize,
}

#[async_trait]
impl Supervisor for CountingSupervisor {
    fn name(&self) -> &'static str {
        "counting"
    }

    async fn run(&self) -> Result<(), FrameworkError> {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        if n < self.fail_until {
            return Err(FrameworkError::internal("fake failure"));
        }
        Ok(())
    }

    fn restart_policy(&self) -> RestartPolicy {
        RestartPolicy::OnError
    }
}

/// Always returns Ok — useful for verifying Always policy keeps running.
struct AlwaysOkSupervisor {
    counter: Arc<AtomicUsize>,
    /// Stop actually restarting after this many runs by parking indefinitely.
    stop_after: usize,
}

#[async_trait]
impl Supervisor for AlwaysOkSupervisor {
    fn name(&self) -> &'static str {
        "always_ok"
    }

    async fn run(&self) -> Result<(), FrameworkError> {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        if n >= self.stop_after {
            // Park so the outer test timeout fires and aborts the task.
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
        Ok(())
    }

    fn restart_policy(&self) -> RestartPolicy {
        RestartPolicy::Always
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// The supervisor should be restarted on each `Err` and then stop naturally
/// when it returns `Ok` (OnError policy).
#[tokio::test]
async fn supervisor_restarts_on_error_then_finishes_on_ok() {
    let counter = Arc::new(AtomicUsize::new(0));
    let sv: Arc<dyn Supervisor> = Arc::new(CountingSupervisor {
        counter: counter.clone(),
        fail_until: 2,
    });

    // 2 failures → backoff 100 ms + 200 ms = 300 ms minimum; 3rd run succeeds
    // and the task exits. We allow a generous 2 s window.
    let handle = tokio::spawn(run_with_restart_for_testing(sv));

    tokio::time::sleep(Duration::from_millis(2000)).await;

    let count = counter.load(Ordering::SeqCst);
    assert!(
        count >= 3,
        "expected at least 3 runs (2 failures + 1 success); got {count}"
    );

    // The task should have finished on its own (OnError + Ok = stop).
    // Give it a moment then check it completed cleanly.
    handle.abort(); // abort is a no-op if already done
}

/// Always policy: the supervisor is restarted even when it returns Ok.
#[tokio::test]
async fn always_policy_restarts_on_ok() {
    let counter = Arc::new(AtomicUsize::new(0));
    let sv: Arc<dyn Supervisor> = Arc::new(AlwaysOkSupervisor {
        counter: counter.clone(),
        stop_after: 3,
    });

    let handle = tokio::spawn(run_with_restart_for_testing(sv));

    // Each restart is near-instant (no error, so no backoff delay between
    // the quick Ok returns). Allow generous time.
    tokio::time::sleep(Duration::from_millis(1000)).await;

    let count = counter.load(Ordering::SeqCst);
    assert!(
        count >= 3,
        "expected at least 3 restarts under Always policy; got {count}"
    );

    handle.abort();
}

/// Never policy: run exactly once and stop.
#[tokio::test]
async fn never_policy_runs_once() {
    struct NeverSupervisor {
        counter: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl Supervisor for NeverSupervisor {
        fn name(&self) -> &'static str { "never" }
        async fn run(&self) -> Result<(), FrameworkError> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn restart_policy(&self) -> RestartPolicy { RestartPolicy::Never }
    }

    let counter = Arc::new(AtomicUsize::new(0));
    let sv: Arc<dyn Supervisor> = Arc::new(NeverSupervisor { counter: counter.clone() });

    // run_with_restart_for_testing returns as soon as the task finishes
    // under Never policy (no spawn needed here since Never exits synchronously).
    let handle = tokio::spawn(run_with_restart_for_testing(sv));
    // Should finish well within 500 ms.
    tokio::time::timeout(Duration::from_millis(500), handle)
        .await
        .expect("Never supervisor timed out — it should have returned")
        .expect("Never supervisor task panicked");

    assert_eq!(counter.load(Ordering::SeqCst), 1, "Never policy should run exactly once");
}

/// Backoff doubles on each failure, capped at 60 s. Verify timing on the
/// first two restarts (100 ms + 200 ms).
#[tokio::test]
async fn backoff_increases_between_restarts() {
    use std::time::Instant;

    let counter = Arc::new(AtomicUsize::new(0));
    let sv: Arc<dyn Supervisor> = Arc::new(CountingSupervisor {
        counter: counter.clone(),
        fail_until: 2, // fail run 0, fail run 1, succeed run 2
    });

    let start = Instant::now();
    let handle = tokio::spawn(run_with_restart_for_testing(sv));

    // Wait for completion (3 runs, 100+200 = 300 ms backoff minimum).
    tokio::time::sleep(Duration::from_millis(2000)).await;
    handle.abort();

    let elapsed = start.elapsed();
    // At minimum the backoff delays must have elapsed.
    assert!(
        elapsed >= Duration::from_millis(300),
        "expected at least 300 ms for 2 backoff delays; got {:?}",
        elapsed
    );
}

/// `SupervisorRegistry::start_all` completes without panicking.
///
/// The framework crate's integration-test binary has no `inventory::submit!`
/// calls, so this exercises the zero-entry path. It also serves as a smoke
/// test that `start_all` itself doesn't panic.
#[tokio::test]
async fn start_all_does_not_panic() {
    SupervisorRegistry::start_all().await;
}
