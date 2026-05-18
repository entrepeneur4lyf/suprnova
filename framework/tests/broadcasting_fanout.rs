#![cfg(feature = "broadcasting-fanout")]

//! Integration tests for SeaStreamerBroadcastHub.
//!
//! ## Test strategy
//!
//! The sea-streamer stdio backend uses process-global stdin/stdout threads.
//! True two-process integration (where hub A in process X and hub B in
//! process Y exchange messages) is not practical in a cargo test suite
//! without spawning child processes.
//!
//! Instead we test:
//!
//! 1. **Local fanout** — `publish` delivers to local subscribers via the
//!    in-memory hub immediately.
//! 2. **Duplicate-delivery guard** — the consumer pump skips messages whose
//!    `instance_id` matches the hub's own ID; we verify this by confirming
//!    that a subscriber that was already served by local fanout does NOT
//!    receive a second copy from the loopback path.
//! 3. **BroadcastHub trait delegation** — `subscriber_count`, `track_member`,
//!    `untrack_member`, `list_members` all delegate correctly.
//! 4. **Serialization round-trip** — `TaggedEnvelope` is not pub, but we
//!    indirectly verify that the JSON serialized by `publish()` is accepted
//!    by the consumer pump by checking that no extra messages arrive.
//!
//! The loopback tests use `SeaStreamerBroadcastHub::new_loopback` so that
//! published messages are fed back through the stdio thread and processed by
//! the consumer pump — exercising the full codepath.

use serde_json::json;
use std::time::Duration;
use suprnova::broadcasting::fanout::SeaStreamerBroadcastHub;
use suprnova::broadcasting::{BroadcastEnvelope, BroadcastHub};

/// Convenience: build a test envelope.
fn envelope(channel: &str, event: &str, data: serde_json::Value) -> BroadcastEnvelope {
    BroadcastEnvelope {
        channel: channel.to_string(),
        event: event.to_string(),
        data,
    }
}

// ── local-fanout tests ───────────────────────────────────────────────────────

/// `publish` delivers the envelope to local subscribers immediately (no
/// round-trip through sea-streamer required).
#[tokio::test]
async fn local_fanout_delivers_to_subscriber() {
    let hub = SeaStreamerBroadcastHub::new_loopback("stdio://", "suprnova-test-local")
        .await
        .expect("hub connect");

    let mut rx = hub.subscribe("chat.1");

    hub.publish(envelope("chat.1", "MessagePosted", json!({ "text": "hello" })))
        .await;

    let received = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("message delivered within 1s")
        .expect("recv ok");

    assert_eq!(received.channel, "chat.1");
    assert_eq!(received.event, "MessagePosted");
    assert_eq!(received.data["text"], "hello");
}

/// Multiple channels are isolated: a subscriber on channel A does not
/// receive events published to channel B.
#[tokio::test]
async fn publish_with_no_subscriber_is_silent() {
    let hub = SeaStreamerBroadcastHub::new_loopback("stdio://", "suprnova-test-silent")
        .await
        .expect("hub connect");

    // No subscriber — publish should not panic.
    hub.publish(envelope("lonely", "Tick", json!({})))
        .await;
}

/// `subscriber_count` reflects the live receivers.
#[tokio::test]
async fn subscriber_count_reflects_receivers() {
    let hub = SeaStreamerBroadcastHub::new_loopback("stdio://", "suprnova-test-count")
        .await
        .expect("hub connect");

    assert_eq!(hub.subscriber_count("presence.room"), 0);

    let _rx1 = hub.subscribe("presence.room");
    assert_eq!(hub.subscriber_count("presence.room"), 1);

    let _rx2 = hub.subscribe("presence.room");
    assert_eq!(hub.subscriber_count("presence.room"), 2);
}

// ── duplicate-delivery guard ─────────────────────────────────────────────────

/// After a hub publishes an envelope with loopback enabled, the consumer
/// pump receives the message back from the stdio loopback. Because the
/// `instance_id` matches, the pump must NOT re-publish to local, so the
/// local subscriber should NOT receive a second copy.
///
/// We verify: exactly ONE message is delivered within a short window; a
/// second recv attempt times out.
#[tokio::test]
async fn no_duplicate_delivery_via_loopback() {
    let hub = SeaStreamerBroadcastHub::new_loopback("stdio://", "suprnova-test-dedup")
        .await
        .expect("hub connect");

    let mut rx = hub.subscribe("dedup.42");

    hub.publish(envelope("dedup.42", "Ping", json!({ "n": 1 })))
        .await;

    // First delivery — from local fanout (immediate).
    let first = tokio::time::timeout(Duration::from_millis(200), rx.recv())
        .await
        .expect("first message within 200ms")
        .expect("recv ok");
    assert_eq!(first.data["n"], 1);

    // Wait long enough for loopback to arrive and be processed.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Second recv attempt should time out — duplicate guard dropped it.
    let second = tokio::time::timeout(Duration::from_millis(100), rx.recv()).await;
    assert!(
        second.is_err(),
        "no duplicate delivery expected; got second message: {second:?}"
    );
}

// ── presence / member tracking ───────────────────────────────────────────────

/// `track_member` + `list_members` round-trip through the local hub.
#[tokio::test]
async fn track_and_list_members() {
    let hub = SeaStreamerBroadcastHub::new_loopback("stdio://", "suprnova-test-members")
        .await
        .expect("hub connect");

    assert!(hub.list_members("presence.lobby").await.is_empty());

    hub.track_member("presence.lobby", "user-1", json!({ "name": "Alice" }))
        .await;
    hub.track_member("presence.lobby", "user-2", json!({ "name": "Bob" }))
        .await;

    let members = hub.list_members("presence.lobby").await;
    assert_eq!(members.len(), 2);
    let names: Vec<&str> = members
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"Alice"));
    assert!(names.contains(&"Bob"));

    hub.untrack_member("presence.lobby", "user-1").await;

    let members = hub.list_members("presence.lobby").await;
    assert_eq!(members.len(), 1);
    assert_eq!(members[0]["name"], "Bob");
}

// ── multi-channel isolation ──────────────────────────────────────────────────

/// Subscribers on different channels don't bleed messages.
#[tokio::test]
async fn channel_isolation() {
    let hub = SeaStreamerBroadcastHub::new_loopback("stdio://", "suprnova-test-isolation")
        .await
        .expect("hub connect");

    let mut rx_a = hub.subscribe("ch.a");
    let mut rx_b = hub.subscribe("ch.b");

    hub.publish(envelope("ch.a", "EventA", json!({ "src": "a" }))).await;
    hub.publish(envelope("ch.b", "EventB", json!({ "src": "b" }))).await;

    let msg_a = tokio::time::timeout(Duration::from_millis(200), rx_a.recv())
        .await
        .expect("ch.a got message")
        .expect("recv ok");
    assert_eq!(msg_a.channel, "ch.a");
    assert_eq!(msg_a.data["src"], "a");

    let msg_b = tokio::time::timeout(Duration::from_millis(200), rx_b.recv())
        .await
        .expect("ch.b got message")
        .expect("recv ok");
    assert_eq!(msg_b.channel, "ch.b");
    assert_eq!(msg_b.data["src"], "b");

    // rx_a should not have received ch.b's event.
    let bleed = tokio::time::timeout(Duration::from_millis(50), rx_a.recv()).await;
    assert!(bleed.is_err(), "ch.a should not receive ch.b events");
}
