//! BroadcastChannel (production gate #374) — verifies the channel publishes
//! to the container-bound `BroadcastHub`, and fails closed when none is bound.
//!
//! Phase 7B shipped the `BroadcastHub`; the channel (formerly a logging stub
//! that silently returned `Ok(())`) now publishes each notification to the
//! hub as a `BroadcastEnvelope`. The route returned by the `Notifiable` is the
//! broadcast channel name, the notification's type name is the event, and its
//! `data()` is the payload.
//!
//! Both tests run inside their own `TestContainer::scope` so they are
//! order-independent and immune to any sibling test that binds a hub globally.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use suprnova::broadcasting::{BroadcastHub, InMemoryBroadcastHub};
use suprnova::notifications::channels::broadcast::BroadcastChannel;
use suprnova::notifications::{Channel, Notification};
use suprnova::testing::TestContainer;

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
async fn deliver_publishes_to_the_bound_hub() {
    TestContainer::scope(async {
        let hub = Arc::new(InMemoryBroadcastHub::new());
        // Subscribe BEFORE delivering so the publish is observable.
        let mut rx = hub.subscribe("room.lobby");
        TestContainer::bind::<dyn BroadcastHub>(hub.clone());

        let channel = BroadcastChannel::new();
        assert_eq!(channel.name(), "broadcast");
        channel
            .deliver("room.lobby", &PingNote)
            .await
            .expect("delivery must succeed when a BroadcastHub is bound");

        let env = rx
            .try_recv()
            .expect("the subscriber must receive the published envelope");
        assert_eq!(env.channel, "room.lobby", "route becomes the channel name");
        assert_eq!(env.event, "PingNote", "event is the notification type name");
        assert_eq!(
            env.data,
            serde_json::json!({ "title": "ping", "body": "hello" }),
            "payload is the notification's data()"
        );
    })
    .await;
}

#[tokio::test]
async fn deliver_fails_closed_when_no_hub_bound() {
    TestContainer::scope(async {
        // Empty scope — no BroadcastHub bound. The pre-fix stub returned
        // Ok(()) here, silently dropping the notification; that was the bug.
        let channel = BroadcastChannel::new();
        let err = channel
            .deliver("room.lobby", &PingNote)
            .await
            .expect_err("delivery must fail closed when no hub is bound, not silently succeed");
        assert!(
            err.to_string().contains("BroadcastHub"),
            "the error must name the missing dependency; got: {err}"
        );
    })
    .await;
}
