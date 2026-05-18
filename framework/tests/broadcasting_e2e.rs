//! End-to-end broadcasting tests: WS client subscribes via JSON
//! envelope, server publishes via hub, client receives Event frame.

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use suprnova::broadcasting::{
    BroadcastEnvelope, BroadcastHub, BroadcastingWsHandler, Channel, ChannelRegistry,
    InMemoryBroadcastHub,
};
use suprnova::http::Request;
use suprnova::middleware::MiddlewareRegistry;
use suprnova::routing::Router;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

struct PublicChat;

#[async_trait]
impl Channel for PublicChat {
    fn name(&self) -> &'static str {
        "chat.public"
    }
}

struct PrivateChat;

#[async_trait]
impl Channel for PrivateChat {
    fn name(&self) -> &'static str {
        "chat.private"
    }
    async fn authorize(&self, _req: &Request, data: &Value) -> bool {
        // Accept only if data carries `{"token":"valid"}`
        data["token"] == "valid"
    }
}

async fn spawn_broadcasting_server() -> (u16, Arc<InMemoryBroadcastHub>) {
    let hub: Arc<InMemoryBroadcastHub> = Arc::new(InMemoryBroadcastHub::new());

    let mut registry = ChannelRegistry::new();
    registry.register(PublicChat);
    registry.register(PrivateChat);
    let registry = Arc::new(registry);

    let handler = BroadcastingWsHandler::new(hub.clone(), registry.clone());

    let router = Arc::new(Router::new().ws("/ws/broadcast", handler));
    let middleware = Arc::new(MiddlewareRegistry::new());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            let io = hyper_util::rt::TokioIo::new(stream);
            let router = router.clone();
            let middleware = middleware.clone();
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |req| {
                    let router = router.clone();
                    let middleware = middleware.clone();
                    async move {
                        Ok::<_, std::convert::Infallible>(
                            suprnova::server::handle_request(router, middleware, req).await,
                        )
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .with_upgrades()
                    .await;
            });
        }
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    (port, hub)
}

async fn read_server_frame(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    let msg = ws.next().await.unwrap().unwrap();
    let text = msg.to_text().unwrap();
    serde_json::from_str(text).unwrap()
}

#[tokio::test]
async fn subscribe_and_receive_event_round_trip() {
    let (port, hub) = spawn_broadcasting_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/broadcast");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.expect("connect");

    // Subscribe to public channel
    let sub = serde_json::to_string(&json!({
        "action": "subscribe",
        "channel": "chat.public"
    }))
    .unwrap();
    ws.send(Message::text(sub)).await.unwrap();

    let frame = read_server_frame(&mut ws).await;
    assert_eq!(frame["action"], "subscribed");
    assert_eq!(frame["channel"], "chat.public");

    // Publish via hub
    hub.publish(BroadcastEnvelope {
        channel: "chat.public".into(),
        event: "MessagePosted".into(),
        data: json!({ "text": "hello" }),
    })
    .await;

    let frame = read_server_frame(&mut ws).await;
    assert_eq!(frame["action"], "event");
    assert_eq!(frame["channel"], "chat.public");
    assert_eq!(frame["event"], "MessagePosted");
    assert_eq!(frame["data"]["text"], "hello");

    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn unauthorized_subscribe_returns_error_frame() {
    let (port, _hub) = spawn_broadcasting_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/broadcast");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.expect("connect");

    // Subscribe to private channel with no token
    let sub = serde_json::to_string(&json!({
        "action": "subscribe",
        "channel": "chat.private"
    }))
    .unwrap();
    ws.send(Message::text(sub)).await.unwrap();

    let frame = read_server_frame(&mut ws).await;
    assert_eq!(frame["action"], "error");
    assert_eq!(frame["channel"], "chat.private");

    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn unsubscribe_stops_event_delivery() {
    let (port, hub) = spawn_broadcasting_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/broadcast");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.expect("connect");

    // Subscribe
    ws.send(
        Message::text(
            serde_json::to_string(&json!({
                "action": "subscribe",
                "channel": "chat.public"
            }))
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    let _ = read_server_frame(&mut ws).await; // subscribed

    // Unsubscribe
    ws.send(
        Message::text(
            serde_json::to_string(&json!({
                "action": "unsubscribe",
                "channel": "chat.public"
            }))
            .unwrap(),
        ),
    )
    .await
    .unwrap();
    let frame = read_server_frame(&mut ws).await;
    assert_eq!(frame["action"], "unsubscribed");

    // Give abort a moment to propagate before publishing
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Publish after unsubscribe — should NOT be delivered
    hub.publish(BroadcastEnvelope {
        channel: "chat.public".into(),
        event: "MessagePosted".into(),
        data: json!({ "text": "lost" }),
    })
    .await;

    // Wait a beat then confirm no event arrives.
    // 150ms is conservative — abort is near-instant.
    let result =
        tokio::time::timeout(Duration::from_millis(150), ws.next()).await;
    assert!(
        result.is_err(),
        "should not receive an event after unsubscribe"
    );

    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn unknown_channel_returns_error_frame() {
    let (port, _hub) = spawn_broadcasting_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/broadcast");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.expect("connect");

    ws.send(
        Message::text(
            serde_json::to_string(&json!({
                "action": "subscribe",
                "channel": "nonexistent"
            }))
            .unwrap(),
        ),
    )
    .await
    .unwrap();

    let frame = read_server_frame(&mut ws).await;
    assert_eq!(frame["action"], "error");
    assert_eq!(frame["channel"], "nonexistent");

    ws.close(None).await.unwrap();
}
