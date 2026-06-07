//! Queued chain tests: jobs run in order, each only after the prior ack's.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use std::time::Duration;
use suprnova::error::FrameworkError;
use suprnova::queue::{
    Job, MemoryQueueDriver, Queue, QueueDriver,
    worker::{WorkerConfig, register_job, run_worker},
};
use tokio_util::sync::CancellationToken;

static ORDER: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

#[derive(Serialize, Deserialize, Clone)]
struct ChainStep {
    label: u32,
}

#[async_trait]
impl Job for ChainStep {
    fn job_name() -> &'static str {
        "queue_chains::ChainStep"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        ORDER.lock().unwrap().push(self.label);
        Ok(())
    }
}

#[tokio::test]
#[serial]
async fn chain_runs_in_order() {
    ORDER.lock().unwrap().clear();
    register_job::<ChainStep>();
    let driver = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());

    Queue::chain()
        .add(ChainStep { label: 1 })
        .unwrap()
        .add(ChainStep { label: 2 })
        .unwrap()
        .add(ChainStep { label: 3 })
        .unwrap()
        .dispatch()
        .await
        .unwrap();

    // The chain dispatches one envelope; the worker pops it, runs step 1,
    // then pushes step 2 on success — and so on. So we need three loop
    // iterations to drain.
    let cfg = WorkerConfig {
        visibility_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(5),
        max_jobs: Some(3),
    };
    let cancel = CancellationToken::new();
    run_worker(driver.clone(), cfg, cancel).await;

    let seen = ORDER.lock().unwrap().clone();
    assert_eq!(seen, vec![1, 2, 3], "chain must execute in order");
}

static STOP_AT: std::sync::Mutex<u32> = std::sync::Mutex::new(0);

#[derive(Serialize, Deserialize, Clone)]
struct StopAt {
    label: u32,
}

#[async_trait]
impl Job for StopAt {
    fn job_name() -> &'static str {
        "queue_chains::StopAt"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        *STOP_AT.lock().unwrap() = self.label;
        if self.label == 2 {
            Err(FrameworkError::internal("step 2 fails permanently"))
        } else {
            Ok(())
        }
    }
    fn max_tries() -> u32 {
        1
    }
}

#[tokio::test]
#[serial]
async fn chain_stops_after_a_failing_link() {
    register_job::<StopAt>();
    let driver = Arc::new(MemoryQueueDriver::new());
    Queue::set_driver(driver.clone());

    Queue::chain()
        .add(StopAt { label: 1 })
        .unwrap()
        .add(StopAt { label: 2 })
        .unwrap()
        .add(StopAt { label: 3 })
        .unwrap()
        .dispatch()
        .await
        .unwrap();

    let cfg = WorkerConfig {
        visibility_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(5),
        max_jobs: Some(2),
    };
    let cancel = CancellationToken::new();
    run_worker(driver.clone(), cfg, cancel).await;

    // Step 2 dead-letters; step 3 never gets enqueued (the worker doesn't
    // propagate the tail on failure).
    assert_eq!(
        *STOP_AT.lock().unwrap(),
        2,
        "chain must stop at the failing link"
    );
    let pending = driver.pending_size().await.unwrap();
    let reserved = driver.reserved_size().await.unwrap();
    let delayed = driver.delayed_size().await.unwrap();
    assert_eq!(
        pending + reserved + delayed,
        0,
        "no further chain envelopes after the failure"
    );
}

// Lights up the M39 fix: a job overriding `Job::backoff()` must propagate
// that schedule through the chain — `ChainLink::to_envelope` previously
// hardcoded `BackoffSchedule::default()` and dropped the override on
// rehydration.
#[derive(Serialize, Deserialize, Clone)]
struct FixedBackoffJob;

#[async_trait]
impl Job for FixedBackoffJob {
    fn job_name() -> &'static str {
        "queue_chains::FixedBackoffJob"
    }
    async fn handle(self) -> Result<(), FrameworkError> {
        Ok(())
    }
    fn backoff() -> suprnova::queue::BackoffSchedule {
        suprnova::queue::BackoffSchedule::Fixed { secs: 7 }
    }
}

#[test]
fn chain_link_propagates_job_backoff_into_envelope() {
    let link =
        suprnova::queue::chain::ChainLink::from_job(FixedBackoffJob).expect("chain link encode");
    let env = link.to_envelope();
    assert_eq!(
        env.backoff,
        suprnova::queue::BackoffSchedule::Fixed { secs: 7 },
        "chain envelope must carry the job's custom backoff schedule"
    );
}

// Forward-compat: a chain payload serialized BEFORE the M39 fix did not
// include `backoff` on the link. The `#[serde(default)]` annotation makes
// such payloads decode to the framework-default schedule, preserving
// pre-fix behaviour for in-flight messages.
#[test]
fn v2_chain_link_without_backoff_decodes_to_default() {
    let v2_payload = serde_json::json!({
        "job_name": "queue_chains::FixedBackoffJob",
        "payload": {},
        "max_tries": 3,
        "timeout_secs": null,
        "fail_on_timeout": false,
    });
    let link: suprnova::queue::chain::ChainLink =
        serde_json::from_value(v2_payload).expect("v2 chain link must decode under serde(default)");
    assert_eq!(
        link.backoff,
        suprnova::queue::BackoffSchedule::default(),
        "missing backoff must fall back to framework default"
    );
}
