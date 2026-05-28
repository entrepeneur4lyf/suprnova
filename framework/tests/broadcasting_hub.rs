//! In-process BroadcastHub tests — publish + subscribe round-trip,
//! multiple subscribers, channel isolation, unsubscribe.

use serde_json::json;
use suprnova::broadcasting::{BroadcastEnvelope, BroadcastHub, InMemoryBroadcastHub};

#[tokio::test]
async fn publish_to_subscribed_channel_delivers_envelope() {
    let hub = InMemoryBroadcastHub::new();
    let mut rx = hub.subscribe("chat.42");

    hub.publish(BroadcastEnvelope::new(
        "chat.42",
        "MessagePosted",
        json!({ "text": "hello" }),
    ))
    .await;

    let received = rx.recv().await.expect("envelope arrives");
    assert_eq!(received.channel, "chat.42");
    assert_eq!(received.event, "MessagePosted");
    assert_eq!(received.data["text"], "hello");
}

#[tokio::test]
async fn publish_to_unsubscribed_channel_is_ignored_by_other_subscribers() {
    let hub = InMemoryBroadcastHub::new();
    let mut chat_rx = hub.subscribe("chat.42");
    let mut presence_rx = hub.subscribe("presence.lobby");

    hub.publish(BroadcastEnvelope::new(
        "presence.lobby",
        "MemberJoined",
        json!({ "user_id": 7 }),
    ))
    .await;

    let presence_msg = presence_rx
        .recv()
        .await
        .expect("presence subscriber receives");
    assert_eq!(presence_msg.event, "MemberJoined");

    use tokio::time::{Duration, timeout};
    assert!(
        timeout(Duration::from_millis(50), chat_rx.recv())
            .await
            .is_err(),
        "chat subscriber should not receive presence-channel events"
    );
}

#[tokio::test]
async fn unsubscribe_via_drop_releases_slot() {
    let hub = InMemoryBroadcastHub::new();
    let rx1 = hub.subscribe("chat.42");
    let rx2 = hub.subscribe("chat.42");
    assert_eq!(hub.subscriber_count("chat.42"), 2);

    drop(rx1);
    // tokio::sync::broadcast detects drops on the next publish (or
    // explicit poll). Yield + publish to push the count refresh.
    tokio::task::yield_now().await;
    hub.publish(BroadcastEnvelope::new("chat.42", "Tick", json!({})))
        .await;
    tokio::task::yield_now().await;

    assert_eq!(hub.subscriber_count("chat.42"), 1);
    drop(rx2);
}

#[tokio::test]
async fn subscriber_count_returns_zero_for_unknown_channel() {
    let hub = InMemoryBroadcastHub::new();
    assert_eq!(hub.subscriber_count("nobody.here"), 0);
}
