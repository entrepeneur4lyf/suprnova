//! Live Redis integration test for the queue driver. Requires a Redis
//! daemon on `redis://127.0.0.1:6379`.
//!
//! Run with `cargo test -p suprnova --test queue_redis -- --ignored`.

use chrono::Utc;
use std::time::Duration;
use suprnova::queue::driver::QueueDriver;
use suprnova::queue::redis::RedisQueueDriver;
use suprnova::queue::{BackoffSchedule, CURRENT_SCHEMA_VERSION, Envelope};
use uuid::Uuid;

fn env(name: &str) -> Envelope {
    let now = Utc::now();
    Envelope {
        schema_version: CURRENT_SCHEMA_VERSION,
        id: Uuid::new_v4(),
        job_name: name.into(),
        payload: serde_json::json!({}),
        dispatched_at: now,
        available_at: now,
        attempts: 0,
        max_tries: 3,
        backoff: BackoffSchedule::default(),
        timeout_secs: None,
        fail_on_timeout: false,
        idempotency_key: None,
    }
}

#[ignore = "requires a real Redis"]
#[tokio::test]
async fn redis_driver_push_pop_ack_round_trip() {
    let stream = format!("test-{}", uuid::Uuid::new_v4());
    let d = RedisQueueDriver::connect(
        "redis://127.0.0.1:6379",
        &stream,
        "g1",
        "c1",
        Duration::from_secs(60),
    )
    .await
    .unwrap();

    d.push(env("R")).await.unwrap();

    let r1 = d.pop(Duration::from_secs(60)).await.unwrap().unwrap();
    assert_eq!(r1.envelope.job_name, "R");
    d.ack(&r1.token).await.unwrap();

    let none = d.pop(Duration::from_millis(50)).await.unwrap();
    assert!(none.is_none());
}

#[ignore = "requires a real Redis"]
#[tokio::test]
async fn redis_driver_nack_with_delay_redelivers_with_bumped_attempts() {
    let stream = format!("test-{}", uuid::Uuid::new_v4());
    let d = RedisQueueDriver::connect(
        "redis://127.0.0.1:6379",
        &stream,
        "g2",
        "c2",
        Duration::from_secs(60),
    )
    .await
    .unwrap();

    d.push(env("R")).await.unwrap();

    let r1 = d.pop(Duration::from_secs(60)).await.unwrap().unwrap();
    assert_eq!(r1.envelope.attempts, 0);

    d.nack(&r1.token, Duration::from_millis(0)).await.unwrap();

    let r2 = d.pop(Duration::from_secs(60)).await.unwrap().unwrap();
    assert_eq!(
        r2.envelope.attempts, 1,
        "nack must bump attempts per trait contract"
    );
}
