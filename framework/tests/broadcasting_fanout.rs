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

/// Poll until `hub.list_members(channel).await.len() == expected`.
///
/// Returns when the count matches. Panics with a clear message if `timeout`
/// elapses before the count reaches the expected value.
/// Use this instead of fixed `tokio::time::sleep` calls to avoid CI flake.
async fn wait_for_member_count(
    hub: &SeaStreamerBroadcastHub,
    channel: &str,
    expected: usize,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let count = hub.list_members(channel).await.len();
        if count == expected {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timeout waiting for member count to reach {expected} on '{channel}'; \
                 last seen: {count}"
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Convenience: build a test envelope.
fn envelope(channel: &str, event: &str, data: serde_json::Value) -> BroadcastEnvelope {
    BroadcastEnvelope::new(channel, event, data)
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

    hub.publish(envelope(
        "chat.1",
        "MessagePosted",
        json!({ "text": "hello" }),
    ))
    .await
    .unwrap();

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
        .await
        .unwrap();
}

/// Publishing to the reserved `__presence__` meta-channel (or any
/// `__`-prefixed name) must be rejected at the publish boundary. The
/// vulnerability the guard closes: a TaggedEnvelope serialised to the
/// stream on `__presence__` is routed by every peer's consumer pump
/// straight into `apply_presence_event`, injecting phantom presence
/// records into every process's `cross_process_view`. The hub itself
/// produces legitimate presence traffic via a dedicated path that
/// never crosses `publish`, so the guard is total.
#[tokio::test]
async fn publish_rejects_reserved_presence_channel() {
    let hub = SeaStreamerBroadcastHub::new_loopback("stdio://", "suprnova-test-reserved")
        .await
        .expect("hub connect");

    let err = hub
        .publish(envelope(
            "__presence__",
            "member_added",
            json!({ "spoof": true }),
        ))
        .await
        .expect_err("publish to __presence__ must be rejected at the boundary");
    assert!(
        err.to_string().contains("reserved prefix '__'"),
        "error must name the reserved prefix; got: {err}"
    );

    // Any other __-prefixed name is equally reserved — the registry
    // forbids registration of the family, not just the one literal.
    let err2 = hub
        .publish(envelope("__rpc__", "ping", json!({})))
        .await
        .expect_err("any __-prefixed channel must be rejected");
    assert!(err2.to_string().contains("reserved prefix '__'"));

    // A regular channel still publishes successfully through the same hub.
    hub.publish(envelope("chat.42", "Tick", json!({})))
        .await
        .expect("non-reserved channel still publishes");
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
        .await
        .unwrap();

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

// ── cross-hub delivery ───────────────────────────────────────────────────────

/// Two hub instances sharing the same stream_key (loopback): a message
/// published on hub1 reaches hub2's subscriber.
///
/// This exercises the deliver branch of the consumer pump — the path that
/// calls `local.publish(tagged.envelope)` when the inbound message's
/// `instance_id` does NOT match the receiving hub's own ID.
///
/// Both hubs share the process-global stdio consumer table keyed by stream
/// name. With loopback enabled, hub1's producer write is dispatched to all
/// registered consumers on the same stream_key, including hub2's consumer.
#[tokio::test]
async fn cross_hub_delivery() {
    let hub1 = SeaStreamerBroadcastHub::new_loopback("stdio://", "suprnova-test-cross")
        .await
        .expect("hub1 connect");
    let hub2 = SeaStreamerBroadcastHub::new_loopback("stdio://", "suprnova-test-cross")
        .await
        .expect("hub2 connect");

    // Subscribe on hub2 before hub1 publishes.
    let mut rx = hub2.subscribe("chat.shared");

    hub1.publish(envelope("chat.shared", "Posted", json!({ "from": "hub1" })))
        .await
        .unwrap();

    // hub2's subscriber must receive the message (via the consumer pump's
    // deliver branch, NOT local fanout — hub2 didn't call publish locally).
    let msg = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("hub2 received hub1's message within 1s")
        .expect("recv ok");

    assert_eq!(msg.channel, "chat.shared");
    assert_eq!(msg.event, "Posted");
    assert_eq!(msg.data["from"], "hub1");
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

    hub.publish(envelope("ch.a", "EventA", json!({ "src": "a" })))
        .await
        .unwrap();
    hub.publish(envelope("ch.b", "EventB", json!({ "src": "b" })))
        .await
        .unwrap();

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

// ── cross-process presence ───────────────────────────────────────────────────

/// Two hub instances sharing the same stream in loopback mode simulate two
/// different processes. Each hub tracks a different member; after a brief
/// propagation window both hubs must be able to list both members via
/// `list_members`.
///
/// This exercises the presence-replication path:
///   track_member → publish PresenceEvent to __presence__ meta-channel →
///   consumer pump → apply_presence_event → cross_process_view
///
/// Note: `track_member` updates `cross_process_view` directly for the local
/// instance (immediate consistency), so hub_a sees its own member right away.
/// The remote member arrives via the stream round-trip within the 500ms window.
#[tokio::test]
async fn cross_process_member_visibility() {
    let hub_a = SeaStreamerBroadcastHub::new_loopback("stdio://", "suprnova-test-presence-vis")
        .await
        .expect("hub_a connect");
    let hub_b = SeaStreamerBroadcastHub::new_loopback("stdio://", "suprnova-test-presence-vis")
        .await
        .expect("hub_b connect");

    // Allow consumer pumps to start.
    tokio::time::sleep(Duration::from_millis(100)).await;

    hub_a
        .track_member("presence.lobby", "alice", json!({ "user_id": 1 }))
        .await;
    hub_b
        .track_member("presence.lobby", "bob", json!({ "user_id": 2 }))
        .await;

    // Poll until both hubs see both members (stream round-trip complete).
    wait_for_member_count(&hub_a, "presence.lobby", 2, Duration::from_secs(2)).await;
    wait_for_member_count(&hub_b, "presence.lobby", 2, Duration::from_secs(2)).await;

    let members_a = hub_a.list_members("presence.lobby").await;
    let members_b = hub_b.list_members("presence.lobby").await;

    assert_eq!(
        members_a.len(),
        2,
        "hub_a should see both alice and bob; got {members_a:?}"
    );
    assert_eq!(
        members_b.len(),
        2,
        "hub_b should see both alice and bob; got {members_b:?}"
    );

    // Verify the actual member data is present, not just the count.
    let user_ids_a: Vec<i64> = members_a
        .iter()
        .filter_map(|v| v["user_id"].as_i64())
        .collect();
    assert!(
        user_ids_a.contains(&1),
        "hub_a missing alice; {members_a:?}"
    );
    assert!(user_ids_a.contains(&2), "hub_a missing bob; {members_a:?}");
}

/// `untrack_member` removes a member from the cross-process view. After the
/// removal propagates, the other hub must no longer include that member in
/// `list_members`.
#[tokio::test]
async fn untrack_propagates_across_processes() {
    let hub_a = SeaStreamerBroadcastHub::new_loopback("stdio://", "suprnova-test-presence-untrack")
        .await
        .expect("hub_a connect");
    let hub_b = SeaStreamerBroadcastHub::new_loopback("stdio://", "suprnova-test-presence-untrack")
        .await
        .expect("hub_b connect");

    tokio::time::sleep(Duration::from_millis(100)).await;

    hub_a
        .track_member("presence.lobby", "alice", json!({ "user_id": 1 }))
        .await;
    hub_b
        .track_member("presence.lobby", "bob", json!({ "user_id": 2 }))
        .await;

    // Poll until hub_b sees both members before removing one.
    wait_for_member_count(&hub_b, "presence.lobby", 2, Duration::from_secs(2)).await;
    assert_eq!(
        hub_b.list_members("presence.lobby").await.len(),
        2,
        "setup: hub_b should see both members before untrack"
    );

    // Remove alice from hub_a.
    hub_a.untrack_member("presence.lobby", "alice").await;

    // Poll until hub_b sees alice has been removed.
    wait_for_member_count(&hub_b, "presence.lobby", 1, Duration::from_secs(2)).await;

    let members = hub_b.list_members("presence.lobby").await;
    assert_eq!(
        members.len(),
        1,
        "hub_b should only see bob after alice is untracked; got {members:?}"
    );
    assert_eq!(
        members[0]["user_id"], 2,
        "remaining member should be bob; got {members:?}"
    );
}

// ── TTL / crash-recovery ─────────────────────────────────────────────────────

/// Simulate a hub crash: hub_a is dropped (all its background tasks abort),
/// which stops its heartbeat. hub_b should prune hub_a's member (alice) once
/// her `last_seen` passes the configured TTL.
///
/// Uses `new_loopback_with_presence_ttl` with a 600 ms TTL (100 ms heartbeat,
/// 300 ms prune scan) so the full crash-recovery path executes within seconds
/// rather than the production 60 s default.
#[tokio::test]
async fn crashed_hub_members_pruned_via_ttl() {
    let ttl = Duration::from_millis(600); // heartbeat = 100ms, prune = 300ms

    let hub_b = SeaStreamerBroadcastHub::new_loopback_with_presence_ttl(
        "stdio://",
        "suprnova-test-ttl-prune",
        ttl,
    )
    .await
    .expect("hub_b connect");

    // Allow hub_b's consumer pump to start.
    tokio::time::sleep(Duration::from_millis(100)).await;

    {
        // hub_a is scoped so its Drop aborts the heartbeat task, simulating a crash.
        let hub_a = SeaStreamerBroadcastHub::new_loopback_with_presence_ttl(
            "stdio://",
            "suprnova-test-ttl-prune",
            ttl,
        )
        .await
        .expect("hub_a connect");

        tokio::time::sleep(Duration::from_millis(100)).await;

        hub_a
            .track_member("presence.lobby", "alice", json!({ "user_id": 1 }))
            .await;

        // Wait until hub_b sees alice (presence event propagated).
        wait_for_member_count(&hub_b, "presence.lobby", 1, Duration::from_secs(2)).await;
    } // hub_a dropped here — heartbeat task aborted, alice's last_seen goes stale

    // Wait until hub_b prunes alice (TTL expired + prune scan fired).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        let count = hub_b.list_members("presence.lobby").await.len();
        if count == 0 {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("alice not pruned after TTL expired; still {count} members visible on hub_b");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ── env-gated Redis backend integration ──────────────────────────────────────

/// Two independent `SeaStreamerBroadcastHub` instances pointed at the same
/// Redis stream MUST exchange envelopes cross-process — this is the whole
/// point of HIGH #208 (the socket-adapter refactor). Subscribers on hub_b
/// receive events published on hub_a.
///
/// Skipped unless `REDIS_BROADCAST_URL` is set, e.g.
///
/// ```sh
/// REDIS_BROADCAST_URL=redis://127.0.0.1:6379 \
///     cargo test -p suprnova --features broadcasting-fanout \
///     --test broadcasting_fanout -- --ignored
/// ```
///
/// Uses a per-run UUID stream key so concurrent test runs and prior failed
/// runs do not see each other's events.
#[tokio::test]
#[ignore = "requires REDIS_BROADCAST_URL pointing at a running Redis"]
async fn redis_backend_cross_hub_fanout() {
    let url = match std::env::var("REDIS_BROADCAST_URL") {
        Ok(u) if !u.is_empty() => u,
        _ => return, // explicit ignore covers this, but belt+braces
    };

    let stream_key = format!("suprnova-test-{}", uuid::Uuid::new_v4());

    let hub_a = SeaStreamerBroadcastHub::new(&url, &stream_key)
        .await
        .expect("hub_a Redis connect");
    let hub_b = SeaStreamerBroadcastHub::new(&url, &stream_key)
        .await
        .expect("hub_b Redis connect");

    // Subscribe on hub_b so the stream consumer is live before hub_a publishes.
    let mut rx = hub_b.subscribe("chat.cross");

    // Give the Redis consumer pump a beat to come up; without this, Redis
    // Streams' XADD before XREAD-from-tail can be lost.
    tokio::time::sleep(Duration::from_millis(200)).await;

    hub_a
        .publish(envelope(
            "chat.cross",
            "MessagePosted",
            json!({ "from": "hub_a" }),
        ))
        .await
        .unwrap();

    let envelope = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("cross-hub event arrives within 5 s")
        .expect("recv");
    assert_eq!(envelope.channel, "chat.cross");
    assert_eq!(envelope.event, "MessagePosted");
    assert_eq!(envelope.data["from"], "hub_a");
}
