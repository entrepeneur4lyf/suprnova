use chrono::Utc;
use std::time::Duration;
use suprnova::queue::driver::{QueueDriver, Reservation};
use suprnova::queue::memory::MemoryQueueDriver;
use suprnova::queue::{BackoffSchedule, CURRENT_SCHEMA_VERSION, Envelope};
use uuid::Uuid;

fn env(name: &str, payload: serde_json::Value) -> Envelope {
    Envelope {
        schema_version: CURRENT_SCHEMA_VERSION,
        id: Uuid::new_v4(),
        job_name: name.into(),
        payload,
        dispatched_at: Utc::now(),
        available_at: Utc::now(),
        attempts: 0,
        max_tries: 3,
        backoff: BackoffSchedule::default(),
        timeout_secs: None,
        fail_on_timeout: false,
        idempotency_key: None,
    }
}

#[tokio::test]
async fn pushed_jobs_pop_in_fifo_order_and_ack_removes_them() {
    let d = MemoryQueueDriver::new();
    d.push(env("J", serde_json::json!({ "i": 1 })))
        .await
        .unwrap();
    d.push(env("J", serde_json::json!({ "i": 2 })))
        .await
        .unwrap();

    let r1 = d.pop(Duration::from_secs(60)).await.unwrap().unwrap();
    let r2 = d.pop(Duration::from_secs(60)).await.unwrap().unwrap();
    assert_eq!(r1.envelope.payload["i"], 1);
    assert_eq!(r2.envelope.payload["i"], 2);

    d.ack(&r1.token).await.unwrap();
    d.ack(&r2.token).await.unwrap();

    let r3 = d.pop(Duration::from_millis(10)).await.unwrap();
    assert!(
        r3.is_none(),
        "queue should be drained after acking everything"
    );
}

#[tokio::test(start_paused = true)]
async fn unacked_messages_reappear_after_visibility_timeout() {
    let d = MemoryQueueDriver::new();
    d.push(env("J", serde_json::json!({ "x": 1 })))
        .await
        .unwrap();

    let r1: Reservation = d.pop(Duration::from_secs(5)).await.unwrap().unwrap();
    drop(r1);

    tokio::time::advance(Duration::from_secs(4)).await;
    let still_empty = d.pop(Duration::from_millis(1)).await.unwrap();
    assert!(still_empty.is_none(), "message should still be reserved");

    tokio::time::advance(Duration::from_secs(2)).await;
    let reclaimed = d.pop(Duration::from_secs(1)).await.unwrap();
    assert!(
        reclaimed.is_some(),
        "message must reappear after visibility timeout"
    );
}

#[tokio::test(start_paused = true)]
async fn nack_returns_message_to_the_head_of_the_queue() {
    let d = MemoryQueueDriver::new();
    d.push(env("J", serde_json::json!({ "x": 1 })))
        .await
        .unwrap();
    d.push(env("J", serde_json::json!({ "x": 2 })))
        .await
        .unwrap();

    let r1 = d.pop(Duration::from_secs(60)).await.unwrap().unwrap();
    d.nack(&r1.token, Duration::from_millis(0)).await.unwrap();

    let r1_again = d.pop(Duration::from_secs(60)).await.unwrap().unwrap();
    assert_eq!(
        r1_again.envelope.payload["x"], 1,
        "nack with zero delay returns the message immediately"
    );

    let r2 = d.pop(Duration::from_secs(60)).await.unwrap().unwrap();
    assert_eq!(r2.envelope.payload["x"], 2);
}

#[tokio::test(start_paused = true)]
async fn delayed_jobs_become_visible_after_available_at() {
    let d = MemoryQueueDriver::new();
    let now = chrono::Utc::now();
    let env = Envelope {
        schema_version: CURRENT_SCHEMA_VERSION,
        id: Uuid::new_v4(),
        job_name: "DelayedJob".into(),
        payload: serde_json::json!({}),
        dispatched_at: now,
        available_at: now + chrono::Duration::seconds(60),
        attempts: 0,
        max_tries: 3,
        backoff: BackoffSchedule::default(),
        timeout_secs: None,
        fail_on_timeout: false,
        idempotency_key: None,
    };
    d.push(env).await.unwrap();

    // Before 60s, queue appears empty:
    let nothing = d.pop(std::time::Duration::from_millis(10)).await.unwrap();
    assert!(nothing.is_none(), "delayed job must not be visible yet");

    // After advancing past available_at, it pops:
    tokio::time::advance(std::time::Duration::from_secs(61)).await;
    let visible = d.pop(std::time::Duration::from_millis(10)).await.unwrap();
    assert!(
        visible.is_some(),
        "delayed job must be visible after available_at"
    );
}
