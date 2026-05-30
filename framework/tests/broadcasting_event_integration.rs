//! Broadcastable + EventDispatcher integration.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use suprnova::FrameworkError;
use suprnova::broadcasting::{
    BroadcastEnvelope, BroadcastHub, Broadcastable, InMemoryBroadcastHub,
};
use suprnova::{Event, EventFacade};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OrderPlaced {
    order_id: i64,
    user_id: i64,
}

impl Event for OrderPlaced {
    fn event_name() -> &'static str {
        "OrderPlaced"
    }
}

impl Broadcastable for OrderPlaced {
    fn broadcast_on(&self) -> Vec<String> {
        vec![
            format!("user.{}.orders", self.user_id),
            "orders.global".into(),
        ]
    }
}

#[tokio::test]
async fn dispatching_broadcastable_event_publishes_to_hub_channels() {
    let hub: Arc<dyn BroadcastHub> = Arc::new(InMemoryBroadcastHub::new());

    // Subscribe to one of the channels named by broadcast_on
    let mut rx = hub.subscribe("user.42.orders");

    EventFacade::broadcast::<OrderPlaced>(Arc::clone(&hub)).await;

    EventFacade::dispatch(OrderPlaced {
        order_id: 100,
        user_id: 42,
    })
    .await
    .unwrap();

    // The dispatcher runs sync listeners inline; the broadcast listener
    // calls hub.publish which is async — wait briefly for delivery.
    let envelope = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .expect("envelope delivered within 500ms")
        .expect("recv ok");

    assert_eq!(envelope.channel, "user.42.orders");
    assert_eq!(envelope.event, "OrderPlaced");
    assert_eq!(envelope.data["order_id"], 100);
    assert_eq!(envelope.data["user_id"], 42);
}

#[tokio::test]
async fn dispatching_broadcastable_event_publishes_to_all_channels() {
    let hub: Arc<dyn BroadcastHub> = Arc::new(InMemoryBroadcastHub::new());

    // Subscribe to the global channel too
    let mut global_rx = hub.subscribe("orders.global");

    EventFacade::broadcast::<OrderPlaced>(Arc::clone(&hub)).await;

    EventFacade::dispatch(OrderPlaced {
        order_id: 200,
        user_id: 99,
    })
    .await
    .unwrap();

    let envelope = tokio::time::timeout(Duration::from_millis(500), global_rx.recv())
        .await
        .expect("global channel receives")
        .expect("recv ok");
    assert_eq!(envelope.channel, "orders.global");
    assert_eq!(envelope.event, "OrderPlaced");
}

// ── broadcast_with: curate the wire payload (Laravel's broadcastWith) ─────────

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AccountFunded {
    account_id: i64,
    secret_balance: i64,
}

impl Event for AccountFunded {
    fn event_name() -> &'static str {
        "AccountFunded"
    }
}

impl Broadcastable for AccountFunded {
    fn broadcast_on(&self) -> Vec<String> {
        vec![format!("account.{}", self.account_id)]
    }
    // Broadcast only the public id — never the balance.
    fn broadcast_with(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "account_id": self.account_id }))
    }
}

#[tokio::test]
async fn broadcast_with_replaces_the_serialized_payload() {
    let hub: Arc<dyn BroadcastHub> = Arc::new(InMemoryBroadcastHub::new());
    let mut rx = hub.subscribe("account.7");
    EventFacade::broadcast::<AccountFunded>(Arc::clone(&hub)).await;

    EventFacade::dispatch(AccountFunded {
        account_id: 7,
        secret_balance: 999,
    })
    .await
    .unwrap();

    let envelope = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .expect("delivered")
        .expect("recv ok");

    assert_eq!(envelope.data["account_id"], 7);
    // The curated payload REPLACES the full serialization — the secret field
    // the event carries must not reach the wire.
    assert!(
        envelope.data.get("secret_balance").is_none(),
        "broadcast_with() must replace the serialized payload, not merge with it"
    );
}

// ── broadcast_when: gate the push per instance (Laravel's broadcastWhen) ──────

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DraftSaved {
    doc_id: i64,
    publish: bool,
}

impl Event for DraftSaved {
    fn event_name() -> &'static str {
        "DraftSaved"
    }
}

impl Broadcastable for DraftSaved {
    fn broadcast_on(&self) -> Vec<String> {
        vec![format!("doc.{}", self.doc_id)]
    }
    // Only broadcast a draft when it is being published.
    fn broadcast_when(&self) -> bool {
        self.publish
    }
}

#[tokio::test]
async fn broadcast_when_gates_the_push() {
    let hub: Arc<dyn BroadcastHub> = Arc::new(InMemoryBroadcastHub::new());
    let mut rx = hub.subscribe("doc.3");
    EventFacade::broadcast::<DraftSaved>(Arc::clone(&hub)).await;

    // Dispatch the suppressed instance (publish=false) first, then the
    // broadcastable one (publish=true). Sync listeners run inline within
    // `dispatch().await`, so both publish attempts finish in order before we
    // receive — the suppressed one must simply be absent from the channel.
    EventFacade::dispatch(DraftSaved {
        doc_id: 3,
        publish: false,
    })
    .await
    .unwrap();
    EventFacade::dispatch(DraftSaved {
        doc_id: 3,
        publish: true,
    })
    .await
    .unwrap();

    let envelope = tokio::time::timeout(Duration::from_millis(500), rx.recv())
        .await
        .expect("publish=true delivers")
        .expect("recv ok");
    assert_eq!(
        envelope.data["publish"], true,
        "broadcast_when()==false must suppress the push: the first envelope on \
         the channel must be the publish=true event, not the suppressed one"
    );
    // Exactly one envelope was broadcast — the suppressed instance left nothing.
    assert!(
        rx.try_recv().is_err(),
        "only the publish=true event should have been broadcast"
    );
}

// ── publish failures propagate to the dispatcher caller ──────────────────────

/// A hub that fails every `publish` — used to verify that fanout errors
/// reach `EventFacade::dispatch` through `BroadcastListener`.
struct FailingHub;

#[async_trait]
impl BroadcastHub for FailingHub {
    fn subscribe(&self, _channel: &str) -> tokio::sync::broadcast::Receiver<BroadcastEnvelope> {
        tokio::sync::broadcast::channel(1).0.subscribe()
    }

    async fn publish(&self, _envelope: BroadcastEnvelope) -> Result<(), FrameworkError> {
        Err(FrameworkError::internal("broker disconnected"))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PageViewed {
    page_id: i64,
}

impl Event for PageViewed {
    fn event_name() -> &'static str {
        "PageViewed"
    }
}

impl Broadcastable for PageViewed {
    fn broadcast_on(&self) -> Vec<String> {
        vec![format!("page.{}", self.page_id)]
    }
}

#[tokio::test]
async fn hub_publish_failure_surfaces_to_event_dispatch() {
    let hub: Arc<dyn BroadcastHub> = Arc::new(FailingHub);
    EventFacade::broadcast::<PageViewed>(Arc::clone(&hub)).await;

    // The dispatch must Err: a hub failure used to be silently swallowed
    // by BroadcastListener (returning Ok), letting cross-process
    // broadcasts vanish from underneath EventFacade::dispatch callers.
    let err = EventFacade::dispatch(PageViewed { page_id: 7 })
        .await
        .expect_err("publish failure must propagate through BroadcastListener");
    assert!(
        err.to_string().contains("broker disconnected"),
        "expected broker error message in {err}"
    );
}
