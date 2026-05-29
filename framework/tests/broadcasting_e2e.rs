//! End-to-end broadcasting tests: WS client subscribes via JSON
//! envelope, server publishes via hub, client receives Event frame.

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use suprnova::broadcasting::{
    BroadcastEnvelope, BroadcastHub, Broadcastable, BroadcastingWsHandler, Channel, ChannelParams,
    ChannelRegistry, InMemoryBroadcastHub,
};
use suprnova::http::Request;
use suprnova::middleware::MiddlewareRegistry;
use suprnova::routing::Router;
use suprnova::ws::{OriginPolicy, WsConfig};
use suprnova::{Event, EventFacade, text};

/// `tokio-tungstenite::connect_async` doesn't send `Origin`, so the
/// production-default `OriginPolicy::SameOrigin` would 403 every test.
/// Opt into `AllowAny` for these tests — they exercise broadcasting
/// semantics, not browser CSRF defense.
fn open_ws_config() -> WsConfig {
    WsConfig {
        origin_policy: OriginPolicy::AllowAny,
        ..Default::default()
    }
}
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

struct PublicChat;

#[async_trait]
impl Channel for PublicChat {
    fn name(&self) -> &'static str {
        "chat.public"
    }

    /// Allow client-initiated publishes for standard chat events only.
    async fn authorize_publish(
        &self,
        _req: &Request,
        _params: &ChannelParams,
        event: &str,
        _data: &Value,
    ) -> bool {
        event == "MessagePosted"
    }
}

struct PrivateChat;

#[async_trait]
impl Channel for PrivateChat {
    fn name(&self) -> &'static str {
        "chat.private"
    }
    async fn authorize(&self, _req: &Request, _params: &ChannelParams, data: &Value) -> bool {
        // Accept only if data carries `{"token":"valid"}`
        data["token"] == "valid"
    }
    // No authorize_publish override → inherits default false (deny).
}

/// A channel that inherits the default `authorize_publish` (false) to
/// verify the deny path in publish-authorization tests.
struct NoPublishChat;

#[async_trait]
impl Channel for NoPublishChat {
    fn name(&self) -> &'static str {
        "chat.no_publish"
    }
    // No authorize_publish override → default deny.
}

/// A parameterized channel `room.{id}` whose `authorize` reads the captured
/// `{id}` and admits only room 42 — proving the handler resolves the pattern
/// and threads the params to the hook over the live WS path.
struct RoomChannel;

#[async_trait]
impl Channel for RoomChannel {
    fn name(&self) -> &'static str {
        "room.{id}"
    }
    async fn authorize(&self, _req: &Request, params: &ChannelParams, _data: &Value) -> bool {
        params.get("id") == Some("42")
    }
}

async fn spawn_broadcasting_server() -> (u16, Arc<InMemoryBroadcastHub>) {
    let hub: Arc<InMemoryBroadcastHub> = Arc::new(InMemoryBroadcastHub::new());

    let mut registry = ChannelRegistry::new();
    registry.register(PublicChat);
    registry.register(PrivateChat);
    registry.register(NoPublishChat);
    registry.register(RoomChannel);
    let registry = Arc::new(registry);

    let handler = BroadcastingWsHandler::new(hub.clone(), registry.clone());

    let router = Arc::new(Router::new().ws_with_config("/ws/broadcast", handler, open_ws_config()));
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

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn read_server_frame(ws: &mut WsStream) -> Value {
    let msg = ws.next().await.unwrap().unwrap();
    let text = msg.to_text().unwrap();
    serde_json::from_str(text).unwrap()
}

/// Consume the `connected` frame every broadcasting connection sends first,
/// returning the assigned socket id.
async fn expect_connected(ws: &mut WsStream) -> String {
    let frame = read_server_frame(ws).await;
    assert_eq!(
        frame["action"], "connected",
        "first frame on a broadcasting connection must be `connected`"
    );
    frame["socket_id"]
        .as_str()
        .expect("socket_id in connected frame")
        .to_string()
}

#[tokio::test]
async fn subscribe_and_receive_event_round_trip() {
    let (port, hub) = spawn_broadcasting_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/broadcast");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");
    expect_connected(&mut ws).await;

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
    hub.publish(BroadcastEnvelope::new(
        "chat.public",
        "MessagePosted",
        json!({ "text": "hello" }),
    ))
    .await;

    let frame = read_server_frame(&mut ws).await;
    assert_eq!(frame["action"], "event");
    assert_eq!(frame["channel"], "chat.public");
    assert_eq!(frame["event"], "MessagePosted");
    assert_eq!(frame["data"]["text"], "hello");

    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn parameterized_channel_threads_params_to_authorize() {
    let (port, hub) = spawn_broadcasting_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/broadcast");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");
    expect_connected(&mut ws).await;

    // room.42 → the pattern `room.{id}` matches, authorize sees id=="42" → allowed.
    let sub =
        serde_json::to_string(&json!({ "action": "subscribe", "channel": "room.42" })).unwrap();
    ws.send(Message::text(sub)).await.unwrap();
    let frame = read_server_frame(&mut ws).await;
    assert_eq!(frame["action"], "subscribed");
    assert_eq!(frame["channel"], "room.42");

    // A publish to the concrete room name reaches this subscriber.
    hub.publish(BroadcastEnvelope::new("room.42", "Ping", json!({ "n": 1 })))
        .await;
    let frame = read_server_frame(&mut ws).await;
    assert_eq!(frame["action"], "event");
    assert_eq!(frame["channel"], "room.42");

    // room.99 → same pattern, authorize sees id=="99" → denied.
    let sub =
        serde_json::to_string(&json!({ "action": "subscribe", "channel": "room.99" })).unwrap();
    ws.send(Message::text(sub)).await.unwrap();
    let frame = read_server_frame(&mut ws).await;
    assert_eq!(frame["action"], "error");
    assert_eq!(frame["channel"], "room.99");

    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn unauthorized_subscribe_returns_error_frame() {
    let (port, _hub) = spawn_broadcasting_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/broadcast");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");
    expect_connected(&mut ws).await;

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
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");
    expect_connected(&mut ws).await;

    // Subscribe
    ws.send(Message::text(
        serde_json::to_string(&json!({
            "action": "subscribe",
            "channel": "chat.public"
        }))
        .unwrap(),
    ))
    .await
    .unwrap();
    let _ = read_server_frame(&mut ws).await; // subscribed

    // Unsubscribe
    ws.send(Message::text(
        serde_json::to_string(&json!({
            "action": "unsubscribe",
            "channel": "chat.public"
        }))
        .unwrap(),
    ))
    .await
    .unwrap();
    let frame = read_server_frame(&mut ws).await;
    assert_eq!(frame["action"], "unsubscribed");

    // Give abort a moment to propagate before publishing
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Publish after unsubscribe — should NOT be delivered
    hub.publish(BroadcastEnvelope::new(
        "chat.public",
        "MessagePosted",
        json!({ "text": "lost" }),
    ))
    .await;

    // Wait a beat then confirm no event arrives.
    // 150ms is conservative — abort is near-instant.
    let result = tokio::time::timeout(Duration::from_millis(150), ws.next()).await;
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
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");
    expect_connected(&mut ws).await;

    ws.send(Message::text(
        serde_json::to_string(&json!({
            "action": "subscribe",
            "channel": "nonexistent"
        }))
        .unwrap(),
    ))
    .await
    .unwrap();

    let frame = read_server_frame(&mut ws).await;
    assert_eq!(frame["action"], "error");
    assert_eq!(frame["channel"], "nonexistent");

    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn client_publish_rejected_when_channel_denies() {
    let (port, _hub) = spawn_broadcasting_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/broadcast");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");
    expect_connected(&mut ws).await;

    // Subscribe to chat.no_publish (which has default authorize_publish = false).
    ws.send(Message::text(
        serde_json::to_string(&json!({
            "action": "subscribe",
            "channel": "chat.no_publish"
        }))
        .unwrap(),
    ))
    .await
    .unwrap();
    let _ = read_server_frame(&mut ws).await; // subscribed

    // Attempt to publish — should be rejected.
    ws.send(Message::text(
        serde_json::to_string(&json!({
            "action": "publish",
            "channel": "chat.no_publish",
            "event": "Spam",
            "data": {"text": "blocked"}
        }))
        .unwrap(),
    ))
    .await
    .unwrap();

    let frame = read_server_frame(&mut ws).await;
    assert_eq!(frame["action"], "error");
    assert_eq!(frame["channel"], "chat.no_publish");
    assert!(
        frame["reason"]
            .as_str()
            .unwrap_or("")
            .contains("unauthorized"),
        "expected 'unauthorized' in reason, got: {:?}",
        frame["reason"]
    );

    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn client_publish_allowed_when_channel_authorizes() {
    let (port, _hub) = spawn_broadcasting_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/broadcast");
    let (mut publisher, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("publisher connects");
    let (mut listener, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("listener connects");
    expect_connected(&mut publisher).await;
    expect_connected(&mut listener).await;

    // Both subscribe to chat.public (which authorizes "MessagePosted").
    for ws in [&mut publisher, &mut listener] {
        ws.send(Message::text(
            serde_json::to_string(&json!({
                "action": "subscribe",
                "channel": "chat.public"
            }))
            .unwrap(),
        ))
        .await
        .unwrap();
        let _ = read_server_frame(ws).await; // subscribed
    }

    // Publisher sends a client-initiated publish.
    publisher
        .send(Message::text(
            serde_json::to_string(&json!({
                "action": "publish",
                "channel": "chat.public",
                "event": "MessagePosted",
                "data": {"text": "hi"}
            }))
            .unwrap(),
        ))
        .await
        .unwrap();

    // Listener should receive the event.
    let frame = read_server_frame(&mut listener).await;
    assert_eq!(frame["action"], "event");
    assert_eq!(frame["event"], "MessagePosted");
    assert_eq!(frame["data"]["text"], "hi");

    publisher.close(None).await.unwrap();
    listener.close(None).await.unwrap();
}

/// Regression: HIGH #207 (ChatGPT audit `broadcasting`). A client
/// that never subscribed to a channel must not be able to publish
/// to it, even if `authorize_publish` would have returned `true`
/// for the event name. Pusher's client-event contract requires an
/// established subscription first; we mirror it.
#[tokio::test]
async fn client_publish_rejected_when_not_subscribed() {
    let (port, _hub) = spawn_broadcasting_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/broadcast");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");
    expect_connected(&mut ws).await;

    // Note: NO subscribe step. Jump straight to publish on chat.public,
    // which would have authorized "MessagePosted" if we had subscribed.
    ws.send(Message::text(
        serde_json::to_string(&json!({
            "action": "publish",
            "channel": "chat.public",
            "event": "MessagePosted",
            "data": {"text": "smuggled"}
        }))
        .unwrap(),
    ))
    .await
    .unwrap();

    let frame = read_server_frame(&mut ws).await;
    assert_eq!(frame["action"], "error");
    assert_eq!(frame["channel"], "chat.public");
    assert!(
        frame["reason"]
            .as_str()
            .unwrap_or("")
            .contains("unauthorized"),
        "expected 'unauthorized' in reason, got: {:?}",
        frame["reason"]
    );

    ws.close(None).await.unwrap();
}

/// Regression: HIGH #207 (ChatGPT audit `broadcasting`). A client
/// that has subscribed to one channel must not be able to publish
/// to a different channel just because it has an active connection.
/// Each channel's subscription is its own publish gate.
#[tokio::test]
async fn client_publish_rejected_when_subscribed_to_different_channel() {
    let (port, _hub) = spawn_broadcasting_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/broadcast");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");
    expect_connected(&mut ws).await;

    // Subscribe to chat.no_publish (authorize_publish defaults to false anyway,
    // but that's fine — what we're testing is publish-to-different-channel).
    ws.send(Message::text(
        serde_json::to_string(&json!({
            "action": "subscribe",
            "channel": "chat.no_publish"
        }))
        .unwrap(),
    ))
    .await
    .unwrap();
    let _ = read_server_frame(&mut ws).await; // subscribed

    // Try to publish to chat.public (which DOES authorize MessagePosted)
    // from a connection that subscribed to chat.no_publish — must fail.
    ws.send(Message::text(
        serde_json::to_string(&json!({
            "action": "publish",
            "channel": "chat.public",
            "event": "MessagePosted",
            "data": {"text": "cross-channel"}
        }))
        .unwrap(),
    ))
    .await
    .unwrap();

    let frame = read_server_frame(&mut ws).await;
    assert_eq!(frame["action"], "error");
    assert_eq!(frame["channel"], "chat.public");
    assert!(
        frame["reason"]
            .as_str()
            .unwrap_or("")
            .contains("unauthorized"),
        "expected 'unauthorized' in reason, got: {:?}",
        frame["reason"]
    );

    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn client_publish_rejected_when_event_name_disallowed() {
    let (port, _hub) = spawn_broadcasting_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/broadcast");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");
    expect_connected(&mut ws).await;

    ws.send(Message::text(
        serde_json::to_string(&json!({
            "action": "subscribe",
            "channel": "chat.public"
        }))
        .unwrap(),
    ))
    .await
    .unwrap();
    let _ = read_server_frame(&mut ws).await; // subscribed

    // chat.public only authorizes "MessagePosted" — "Spam" should be rejected.
    ws.send(Message::text(
        serde_json::to_string(&json!({
            "action": "publish",
            "channel": "chat.public",
            "event": "Spam",
            "data": {}
        }))
        .unwrap(),
    ))
    .await
    .unwrap();

    let frame = read_server_frame(&mut ws).await;
    assert_eq!(frame["action"], "error");
    assert_eq!(frame["channel"], "chat.public");
    assert!(
        frame["reason"]
            .as_str()
            .unwrap_or("")
            .contains("unauthorized"),
        "expected 'unauthorized' in reason, got: {:?}",
        frame["reason"]
    );

    ws.close(None).await.unwrap();
}

// ── toOthers: a request-triggered broadcast excludes the originating socket ───

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RoomPing {
    n: i64,
}

impl Event for RoomPing {
    fn event_name() -> &'static str {
        "RoomPing"
    }
}

impl Broadcastable for RoomPing {
    fn broadcast_on(&self) -> Vec<String> {
        vec!["chat.public".into()]
    }
    // Exclude the connection that triggered the broadcast (its X-Socket-ID).
    fn broadcast_to_others(&self) -> bool {
        true
    }
}

/// Server with the broadcasting WS route plus a `GET /ping` route whose handler
/// dispatches a `broadcast_to_others` `RoomPing`. The handler runs inside the
/// request scope, so the request's `X-Socket-ID` reaches `broadcast_to_others`.
async fn spawn_toothers_server() -> (u16, Arc<InMemoryBroadcastHub>) {
    let hub: Arc<InMemoryBroadcastHub> = Arc::new(InMemoryBroadcastHub::new());

    let mut registry = ChannelRegistry::new();
    registry.register(PublicChat); // public "chat.public"
    let registry = Arc::new(registry);

    let handler = BroadcastingWsHandler::new(hub.clone(), registry);
    let router: Arc<Router> = Arc::new(
        Router::new()
            .ws_with_config("/ws/broadcast", handler, open_ws_config())
            .get("/ping", |_req: Request| async {
                EventFacade::dispatch(RoomPing { n: 1 }).await.ok();
                text("ok")
            })
            .into(),
    );

    // Bridge RoomPing dispatches to this hub.
    EventFacade::broadcast::<RoomPing>(hub.clone()).await;

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

/// Fire `GET /ping` with an `X-Socket-ID` header over a raw loopback socket,
/// draining the response so the server-side dispatch has completed on return.
async fn http_get_ping_as(port: u16, socket_id: &str) {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect /ping");
    let req = format!(
        "GET /ping HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Socket-ID: {socket_id}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf).await; // returns once the handler finished
}

#[tokio::test]
async fn broadcast_to_others_excludes_the_originating_socket() {
    let (port, _hub) = spawn_toothers_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/broadcast");

    // Connection A — capture its socket id from the `connected` frame.
    let (mut ws_a, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("A connects");
    let socket_a = expect_connected(&mut ws_a).await;

    // Connection B — a second subscriber that should still receive the event.
    let (mut ws_b, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("B connects");
    let _socket_b = expect_connected(&mut ws_b).await;

    // Both subscribe to chat.public.
    for ws in [&mut ws_a, &mut ws_b] {
        let sub =
            serde_json::to_string(&json!({ "action": "subscribe", "channel": "chat.public" }))
                .unwrap();
        ws.send(Message::text(sub)).await.unwrap();
        let ack = read_server_frame(ws).await;
        assert_eq!(ack["action"], "subscribed");
    }

    // An HTTP request carrying A's socket id dispatches a `broadcast_to_others`
    // RoomPing — A must NOT receive it, B must.
    http_get_ping_as(port, &socket_a).await;

    // B receives the event.
    let frame = tokio::time::timeout(Duration::from_millis(500), read_server_frame(&mut ws_b))
        .await
        .expect("B receives within 500ms");
    assert_eq!(frame["action"], "event");
    assert_eq!(frame["event"], "RoomPing");

    // A is silent — it triggered the broadcast and asked to exclude itself.
    let a_silent =
        tokio::time::timeout(Duration::from_millis(200), read_server_frame(&mut ws_a)).await;
    assert!(
        a_silent.is_err(),
        "originating socket A must be excluded by broadcast_to_others"
    );
}
