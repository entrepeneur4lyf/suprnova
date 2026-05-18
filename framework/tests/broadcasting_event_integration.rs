//! Broadcastable + EventDispatcher integration.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use suprnova::broadcasting::{BroadcastHub, Broadcastable, InMemoryBroadcastHub};
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
