//! BroadcastChannelStub — verifies the stub emits a structured info log.
//!
//! Phase 5B ships the broadcast channel as a logging stub; real WebSocket
//! fan-out arrives in Phase 7B (on top of the Phase 7A WebSocket
//! transport). Until then, the test below pins the contract that lets the
//! stub stand in for the real implementation:
//!
//! 1. `Channel::name()` returns `"broadcast"` so notifications declaring
//!    `"broadcast"` resolve through the dispatcher today without warning.
//! 2. `Channel::deliver()` always returns `Ok(())` — the stub never
//!    short-circuits a multi-channel notification.
//! 3. Every delivery emits a `tracing::info` event carrying enough
//!    structured context (channel, route, notification name, data) for
//!    operators to audit what *would* have been broadcast.

use serde::{Deserialize, Serialize};
use suprnova::notifications::channels::broadcast::BroadcastChannelStub;
use suprnova::notifications::{Channel, Notification};
use tracing_test::traced_test;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct PingNote;

impl Notification for PingNote {
    fn notification_name() -> &'static str {
        "PingNote"
    }
    fn channels(&self) -> Vec<&'static str> {
        vec!["broadcast"]
    }
    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "title": "ping", "body": "hello" })
    }
}

#[tokio::test]
#[traced_test]
async fn broadcast_stub_emits_info_event_with_structured_fields() {
    let channel = BroadcastChannelStub::new();
    assert_eq!(channel.name(), "broadcast");

    // The "route" for broadcast is whatever the Notifiable returns — for a
    // future WebSocket transport this will typically be a channel name
    // (`"user.42"`, `"room.lobby"`, etc.). The stub just logs it.
    channel
        .deliver("room.lobby", &PingNote)
        .await
        .expect("stub delivery is infallible");

    assert!(
        logs_contain("broadcast channel stub"),
        "expected the stub's info message"
    );
    assert!(
        logs_contain("broadcast"),
        "expected channel=\"broadcast\" field in the log"
    );
    assert!(
        logs_contain("PingNote"),
        "expected notification name in the log"
    );
    assert!(
        logs_contain("room.lobby"),
        "expected route in the log"
    );
    assert!(
        logs_contain("ping"),
        "expected the notification data payload in the log"
    );
}
