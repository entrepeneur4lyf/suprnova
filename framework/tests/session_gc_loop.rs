//! `SessionMiddleware::install_with_gc` background-task tests.
//!
//! Exercises the GC supervisor that backs `install_with_gc` /
//! `install`: a framework-supervised loop that fires `SessionStore::gc`
//! once per configured interval, replaces Laravel's lottery-based
//! `collectGarbage` on the request path, survives gc errors without
//! killing itself, and exits cleanly when the supervisor cancellation
//! token fires (so the shutdown drain doesn't have to force-abort it).

use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use suprnova::FrameworkError;
use suprnova::session::{
    SessionConfig, SessionData, SessionGcSupervisor, SessionMiddleware, SessionStore,
};
use suprnova::supervisor::{Supervisor, run_with_restart_for_testing_with_cancel};
use tokio_util::sync::CancellationToken;

/// A SessionStore that counts how many times each method is called.
/// Returns `Ok(0)` from `gc` by default; flip `fail_gc` to make it
/// error.
#[derive(Default)]
struct CountingStore {
    gc_calls: AtomicU64,
    fail_gc: AtomicBool,
}

#[async_trait]
impl SessionStore for CountingStore {
    async fn read(&self, _id: &str) -> Result<Option<SessionData>, FrameworkError> {
        Ok(None)
    }
    async fn write(&self, _s: &SessionData) -> Result<(), FrameworkError> {
        Ok(())
    }
    async fn destroy(&self, _id: &str) -> Result<(), FrameworkError> {
        Ok(())
    }
    async fn destroy_for_user(&self, _uid: &str) -> Result<u64, FrameworkError> {
        Ok(0)
    }
    async fn gc(&self) -> Result<u64, FrameworkError> {
        self.gc_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_gc.load(Ordering::SeqCst) {
            Err(FrameworkError::internal("synthetic gc failure"))
        } else {
            Ok(0)
        }
    }
}

/// Spawn the real [`SessionGcSupervisor`] under the test-only restart
/// loop so we can drive it with a `CancellationToken` we own. This
/// exercises the same body that production runs through
/// `SupervisorRegistry::spawn`, without touching the per-process
/// `SUPERVISOR_TASKS` static (which other parallel tests share).
fn spawn_session_gc_supervisor(
    store: Arc<CountingStore>,
    interval: Duration,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let supervisor: Arc<dyn Supervisor> = Arc::new(SessionGcSupervisor {
        store: store as Arc<dyn SessionStore>,
        interval,
    });
    tokio::spawn(async move {
        run_with_restart_for_testing_with_cancel(supervisor, cancel).await;
    })
}

#[tokio::test(flavor = "current_thread")]
async fn gc_loop_runs_on_real_clock_interval() {
    let store: Arc<CountingStore> = Arc::new(CountingStore::default());
    let _mw = SessionMiddleware::with_store(SessionConfig::default(), store.clone());

    // Short interval so the test stays fast. 50ms × 4 ticks = 200ms
    // of test time; runs reliably on any CI.
    let cancel = CancellationToken::new();
    let handle =
        spawn_session_gc_supervisor(store.clone(), Duration::from_millis(50), cancel.clone());

    // Wait long enough for at least 3 ticks; tolerate exact-count
    // jitter on slow CI by asserting "≥3" rather than "==3".
    tokio::time::sleep(Duration::from_millis(220)).await;
    let count = store.gc_calls.load(Ordering::SeqCst);
    assert!(count >= 3, "expected at least 3 gc calls, got {count}");

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
}

#[tokio::test(flavor = "current_thread")]
async fn gc_loop_survives_errors() {
    let store: Arc<CountingStore> = Arc::new(CountingStore::default());
    store.fail_gc.store(true, Ordering::SeqCst);
    let _mw = SessionMiddleware::with_store(SessionConfig::default(), store.clone());

    let cancel = CancellationToken::new();
    let handle =
        spawn_session_gc_supervisor(store.clone(), Duration::from_millis(50), cancel.clone());

    // Every tick errors but the loop must keep going — 5+ ticks means
    // the err-swallow behaviour holds.
    tokio::time::sleep(Duration::from_millis(320)).await;
    let count = store.gc_calls.load(Ordering::SeqCst);
    assert!(count >= 5, "expected at least 5 gc calls, got {count}");

    cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(1), handle).await;
}

#[tokio::test]
async fn middleware_store_accessor_returns_the_bound_store() {
    let store: Arc<CountingStore> = Arc::new(CountingStore::default());
    let mw = SessionMiddleware::with_store(SessionConfig::default(), store.clone());
    let got = mw.store();
    // Same store handle (Arc::ptr_eq via the dyn trait pointer).
    assert!(Arc::strong_count(&got) >= 2);
    got.gc().await.unwrap();
    assert_eq!(store.gc_calls.load(Ordering::SeqCst), 1);
}

/// Regression for the L1 finding: the gc supervisor MUST exit cleanly
/// when the supervisor cancellation token fires. Without `select!` on
/// `cancel.cancelled()` the loop would keep sleeping forever and the
/// 5-second shutdown drain would have to force-abort it — defeating
/// the point of bringing the gc loop under the supervisor.
#[tokio::test(flavor = "current_thread")]
async fn gc_supervisor_exits_promptly_on_cancellation() {
    let store: Arc<CountingStore> = Arc::new(CountingStore::default());

    // 60-second interval so the loop is "stuck" in the sleep arm at the
    // moment we cancel — if select! wasn't honoured we'd time out.
    let cancel = CancellationToken::new();
    let handle =
        spawn_session_gc_supervisor(store.clone(), Duration::from_secs(60), cancel.clone());

    // Let the loop reach its sleep arm.
    tokio::time::sleep(Duration::from_millis(20)).await;
    cancel.cancel();

    // Must return well within the 5-second supervisor drain window.
    tokio::time::timeout(Duration::from_secs(1), handle)
        .await
        .expect("gc supervisor did not exit within 1s of cancellation")
        .expect("gc supervisor task panicked");
}
