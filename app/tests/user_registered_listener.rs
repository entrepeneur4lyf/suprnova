//! Tests the application's `UserRegistered` event listeners in
//! isolation — both the welcome-email logger and the SSE
//! broadcaster — without touching the process-global dispatcher or
//! the bootstrap `OnceLock`. We invoke listeners directly so the
//! tests stay parallel-safe and don't depend on registration order.

use app::events::UserRegistered;
use app::listeners::{SendWelcomeEmailListener, UserRegisteredBroadcaster};
use std::sync::Arc;
use suprnova::events::Listener;
use tokio::sync::broadcast;

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
async fn broadcaster_listener_forwards_event_to_subscriber() {
    let (tx, mut rx) = broadcast::channel::<UserRegistered>(8);
    let listener = UserRegisteredBroadcaster::new(Arc::new(tx));

    let event = UserRegistered {
        user_id: 42,
        email: "bob@example.com".into(),
    };
    listener.handle(&event).await.expect("listener should succeed");

    let received = rx.recv().await.expect("subscriber should receive event");
    assert_eq!(received.user_id, 42);
    assert_eq!(received.email, "bob@example.com");
}

#[tokio::test]
async fn broadcaster_listener_is_ok_with_no_subscribers() {
    // Drop the receiver before the listener fires — tokio's broadcast
    // channel reports SendError when there are no live receivers;
    // the listener must swallow it so dispatch keeps moving.
    let (tx, rx) = broadcast::channel::<UserRegistered>(8);
    drop(rx);

    let listener = UserRegisteredBroadcaster::new(Arc::new(tx));
    let event = UserRegistered {
        user_id: 99,
        email: "carol@example.com".into(),
    };

    // Must not error even though send() will fail internally.
    listener
        .handle(&event)
        .await
        .expect("listener must tolerate no-subscriber send failure");
}

#[tokio::test]
async fn broadcaster_listener_fans_out_to_multiple_subscribers() {
    let (tx, mut rx1) = broadcast::channel::<UserRegistered>(8);
    let mut rx2 = tx.subscribe();
    let listener = UserRegisteredBroadcaster::new(Arc::new(tx));

    let event = UserRegistered {
        user_id: 7,
        email: "dave@example.com".into(),
    };
    listener.handle(&event).await.unwrap();

    let a = rx1.recv().await.unwrap();
    let b = rx2.recv().await.unwrap();
    assert_eq!(a.user_id, 7);
    assert_eq!(b.user_id, 7);
}
