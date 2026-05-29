//! Queue::restart signal causes a running worker to exit cleanly without
//! claiming additional work.

use serial_test::serial;
use std::sync::Arc;
use std::time::Duration;
use suprnova::App;
use suprnova::cache::{Cache, CacheStore, InMemoryCache};
use suprnova::queue::{
    MemoryQueueDriver, Queue,
    worker::{WorkerConfig, run_worker},
};
use tokio_util::sync::CancellationToken;

fn cache_init() {
    if !Cache::is_initialized() {
        App::bind::<dyn CacheStore>(Arc::new(InMemoryCache::new()));
    }
}

#[tokio::test]
#[serial]
async fn restart_signal_breaks_worker_loop() {
    cache_init();
    let driver = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());

    let cfg = WorkerConfig {
        visibility_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(5),
        max_jobs: Some(100),
    };
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let handle = tokio::spawn(async move {
        run_worker(driver, cfg, token).await;
    });

    // Issue the restart signal AFTER the worker boots (so worker_started_at
    // < signal_ts) — the worker observes the signal on its next loop pass
    // and exits cleanly.
    tokio::time::sleep(Duration::from_millis(50)).await;
    Queue::restart().await.unwrap();

    let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
    assert!(
        result.is_ok(),
        "worker should exit after observing restart signal"
    );
}

#[tokio::test]
#[serial]
async fn restart_signal_value_is_readable() {
    cache_init();
    Queue::restart().await.unwrap();
    let ts = Queue::restart_signal().await.unwrap();
    assert!(ts.is_some());
}
