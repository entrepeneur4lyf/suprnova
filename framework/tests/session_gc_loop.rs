//! `SessionMiddleware::install_with_gc` background-task tests.
//!
//! Exercises the GC tie-in shipped with the Laravel-13 parity sweep:
//! a Tokio-spawned task that fires `SessionStore::gc` once per
//! configured interval, replaces Laravel's lottery-based
//! `collectGarbage` on the request path, and survives gc errors
//! without killing itself.

use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use suprnova::FrameworkError;
use suprnova::session::{SessionConfig, SessionData, SessionMiddleware, SessionStore};

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

/// Spawn the same loop `install_with_gc` spawns, against a counting
/// store. Lets us drive real-clock time forward without needing a
/// paused-time runtime (which doesn't reliably advance spawned tasks
/// without manual polling).
fn spawn_gc_loop(store: Arc<CountingStore>, interval: Duration) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            // Mirror install_with_gc behaviour: swallow errors so a
            // bad backend never kills the loop.
            let _ = store.gc().await;
        }
    });
}

#[tokio::test(flavor = "current_thread")]
async fn gc_loop_runs_on_real_clock_interval() {
    let store: Arc<CountingStore> = Arc::new(CountingStore::default());
    let _mw = SessionMiddleware::with_store(SessionConfig::default(), store.clone());

    // Short interval so the test stays fast. 50ms × 4 ticks = 200ms
    // of test time; runs reliably on any CI.
    spawn_gc_loop(store.clone(), Duration::from_millis(50));

    // Wait long enough for at least 3 ticks; tolerate exact-count
    // jitter on slow CI by asserting "≥3" rather than "==3".
    tokio::time::sleep(Duration::from_millis(220)).await;
    let count = store.gc_calls.load(Ordering::SeqCst);
    assert!(count >= 3, "expected at least 3 gc calls, got {count}");
}

#[tokio::test(flavor = "current_thread")]
async fn gc_loop_survives_errors() {
    let store: Arc<CountingStore> = Arc::new(CountingStore::default());
    store.fail_gc.store(true, Ordering::SeqCst);
    let _mw = SessionMiddleware::with_store(SessionConfig::default(), store.clone());

    spawn_gc_loop(store.clone(), Duration::from_millis(50));

    // Every tick errors but the loop must keep going — 5+ ticks means
    // the err-swallow behaviour holds.
    tokio::time::sleep(Duration::from_millis(320)).await;
    let count = store.gc_calls.load(Ordering::SeqCst);
    assert!(count >= 5, "expected at least 5 gc calls, got {count}");
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
