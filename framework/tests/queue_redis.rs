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
        batch_id: None,
        chain_remaining: Vec::new(),
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

/// `Queue::later` / `push` with a future `available_at` MUST not be visible
/// to pop until the delay elapses. Without the ZSET fix, the envelope went
/// straight onto the stream and was popped immediately.
#[ignore = "requires a real Redis"]
#[tokio::test]
async fn redis_driver_push_with_future_available_at_defers_until_due() {
    let stream = format!("test-{}", uuid::Uuid::new_v4());
    let d = RedisQueueDriver::connect(
        "redis://127.0.0.1:6379",
        &stream,
        "g3",
        "c3",
        Duration::from_secs(60),
    )
    .await
    .unwrap();

    let mut e = env("delayed");
    // Schedule ~1.5s into the future.
    e.available_at = Utc::now() + chrono::Duration::milliseconds(1500);
    d.push(e).await.unwrap();

    // Immediate pop must NOT see the envelope.
    let now_view = d.pop(Duration::from_millis(150)).await.unwrap();
    assert!(
        now_view.is_none(),
        "delayed envelope leaked into the stream before its available_at"
    );

    // Wait past the deadline; pop must promote and deliver.
    tokio::time::sleep(Duration::from_millis(2_000)).await;
    let later_view = d.pop(Duration::from_secs(5)).await.unwrap();
    let r = later_view.expect("delayed envelope must be visible after the deadline");
    assert_eq!(r.envelope.job_name, "delayed");
    d.ack(&r.token).await.unwrap();
}

/// `nack` with a non-zero `requeue_delay` MUST also route via the ZSET; an
/// immediately-following pop must not see the redelivered envelope until the
/// delay elapses.
#[ignore = "requires a real Redis"]
#[tokio::test]
async fn redis_driver_nack_with_delay_defers_redelivery() {
    let stream = format!("test-{}", uuid::Uuid::new_v4());
    let d = RedisQueueDriver::connect(
        "redis://127.0.0.1:6379",
        &stream,
        "g4",
        "c4",
        Duration::from_secs(60),
    )
    .await
    .unwrap();

    d.push(env("retry")).await.unwrap();
    let r1 = d.pop(Duration::from_secs(60)).await.unwrap().unwrap();

    d.nack(&r1.token, Duration::from_millis(1_500))
        .await
        .unwrap();

    // Immediate pop sees nothing (envelope is parked in the ZSET).
    let now_view = d.pop(Duration::from_millis(150)).await.unwrap();
    assert!(
        now_view.is_none(),
        "nack(delay=1.5s) re-delivered immediately"
    );

    tokio::time::sleep(Duration::from_millis(2_000)).await;
    let r2 = d
        .pop(Duration::from_secs(5))
        .await
        .unwrap()
        .expect("retry must surface after its delay");
    assert_eq!(r2.envelope.job_name, "retry");
    assert_eq!(r2.envelope.attempts, 1);
}

/// Lights up the M40 fix. With no overrides, the trait defaults for
/// `size`/`pending_size`/`reserved_size`/`delayed_size`/`clear` returned
/// `Err("does not implement")` — admin dashboards inspecting a Redis
/// queue got no number back. The overrides round-trip:
///   - push 2 immediate + 1 delayed → size = 3, delayed = 1
///   - pop one → reserved = 1, pending shrinks accordingly
///   - clear → everything = 0
#[ignore = "requires a real Redis"]
#[tokio::test]
async fn redis_driver_size_introspection_round_trip() {
    let stream = format!("test-{}", uuid::Uuid::new_v4());
    let d = RedisQueueDriver::connect(
        "redis://127.0.0.1:6379",
        &stream,
        "g-size",
        "c-size",
        Duration::from_secs(60),
    )
    .await
    .unwrap();

    // Pre-pop the empty stream so the consumer group exists for XPENDING.
    let _ = d.pop(Duration::from_millis(50)).await.unwrap();

    // Empty state.
    assert_eq!(d.size().await.unwrap(), 0);
    assert_eq!(d.pending_size().await.unwrap(), 0);
    assert_eq!(d.delayed_size().await.unwrap(), 0);
    assert_eq!(d.reserved_size().await.unwrap(), 0);

    // Push 2 immediate + 1 delayed.
    d.push(env("s1")).await.unwrap();
    d.push(env("s2")).await.unwrap();
    let mut delayed = env("s3-late");
    delayed.available_at = Utc::now() + chrono::Duration::milliseconds(3_000);
    d.push(delayed).await.unwrap();

    assert_eq!(
        d.size().await.unwrap(),
        3,
        "size = XLEN(stream) + ZCARD(delayed) = 2 + 1"
    );
    assert_eq!(
        d.delayed_size().await.unwrap(),
        1,
        "one envelope parked on the delayed ZSET"
    );
    assert_eq!(d.reserved_size().await.unwrap(), 0, "nothing reserved yet");

    // Pop one; reservation count climbs.
    let r1 = d.pop(Duration::from_secs(5)).await.unwrap().unwrap();
    assert_eq!(
        d.reserved_size().await.unwrap(),
        1,
        "one entry should be in the consumer group's PEL"
    );

    // Ack it; reservation count drops.
    d.ack(&r1.token).await.unwrap();
    assert_eq!(d.reserved_size().await.unwrap(), 0);

    // clear() returns an approximate count and drains everything.
    let cleared = d.clear().await.unwrap();
    assert!(cleared >= 1, "clear must report dropped envelopes");
    assert_eq!(
        d.delayed_size().await.unwrap(),
        0,
        "delayed key must be empty after clear"
    );
}
