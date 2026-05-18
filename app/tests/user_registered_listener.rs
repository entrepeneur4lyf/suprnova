//! Tests the application's `UserRegistered` event listeners in
//! isolation — the welcome-email logger and the hub-based broadcast
//! path — without touching the process-global dispatcher or the
//! bootstrap state. Listeners are invoked directly so the tests stay
//! parallel-safe and don't depend on registration order.
//!
//! The legacy `UserRegisteredBroadcaster` (Phase 1 bespoke sender)
//! was removed in Phase 7B Task 9; broadcast fanout now goes through
//! `EventFacade::broadcast::<UserRegistered>(hub)` / `BroadcastListener`.
//! These tests verify the same behavioral contracts through the new surface.

use app::events::UserRegistered;
use app::listeners::SendWelcomeEmailListener;
use std::sync::Arc;
use std::time::Duration;
use suprnova::broadcasting::{BroadcastHub, BroadcastListener, InMemoryBroadcastHub};
use suprnova::events::Listener;

#[tokio::test]
async fn welcome_email_listener_returns_ok() {
    let listener = SendWelcomeEmailListener;
    let event = UserRegistered {
        user_id: 1,
        email: "alice@example.com".into(),
    };
    // Listener is side-effect only (logs); the contract is that it
    // doesn't error on valid input.
    listener.handle(&event).await.expect("listener should succeed");
}

#[tokio::test]
async fn broadcast_listener_forwards_event_to_subscriber() {
    let hub: Arc<dyn BroadcastHub> = Arc::new(InMemoryBroadcastHub::new());
    let mut rx = hub.subscribe("user_registered");

    let listener = BroadcastListener::<UserRegistered>::new(Arc::clone(&hub));
    let event = UserRegistered {
        user_id: 42,
        email: "bob@example.com".into(),
    };
    listener.handle(&event).await.expect("listener should succeed");

    let envelope = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .expect("envelope arrives within 500ms")
        .expect("recv ok");

    assert_eq!(envelope.channel, "user_registered");
    assert_eq!(envelope.event, "UserRegistered");
    assert_eq!(envelope.data["user_id"], 42);
    assert_eq!(envelope.data["email"], "bob@example.com");
}

#[tokio::test]
async fn broadcast_listener_is_ok_with_no_subscribers() {
    // No subscriber — hub publishes silently. The listener must not error.
    let hub: Arc<dyn BroadcastHub> = Arc::new(InMemoryBroadcastHub::new());
    let listener = BroadcastListener::<UserRegistered>::new(Arc::clone(&hub));

    let event = UserRegistered {
        user_id: 99,
        email: "carol@example.com".into(),
    };

    // Must not error even though publish goes to an empty channel.
    listener
        .handle(&event)
        .await
        .expect("listener must tolerate no-subscriber publish");
}

#[tokio::test]
async fn broadcast_listener_fans_out_to_multiple_subscribers() {
    let hub: Arc<dyn BroadcastHub> = Arc::new(InMemoryBroadcastHub::new());
    let mut rx1 = hub.subscribe("user_registered");
    let mut rx2 = hub.subscribe("user_registered");

    let listener = BroadcastListener::<UserRegistered>::new(Arc::clone(&hub));
    let event = UserRegistered {
        user_id: 7,
        email: "dave@example.com".into(),
    };
    listener.handle(&event).await.unwrap();

    let a = tokio::time::timeout(Duration::from_millis(500), rx1.recv())
        .await
        .expect("rx1 receives")
        .unwrap();
    let b = tokio::time::timeout(Duration::from_millis(500), rx2.recv())
        .await
        .expect("rx2 receives")
        .unwrap();

    assert_eq!(a.data["user_id"], 7);
    assert_eq!(b.data["user_id"], 7);
}
