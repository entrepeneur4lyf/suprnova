//! Presence channel tests — join broadcasts `presence.joined` to
//! existing subscribers, `presence.here` snapshot to the new one,
//! and `presence.left` on disconnect.

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use suprnova::FrameworkError;
use suprnova::broadcasting::{
    BroadcastingWsHandler, Channel, ChannelParams, ChannelRegistry, InMemoryBroadcastHub,
    PresenceChannel,
};
use suprnova::http::Request;
use suprnova::routing::Router;
use suprnova::ws::{OriginPolicy, WsConfig};
use tokio::net::TcpListener;
use tokio::time::{Duration as TokioDuration, timeout};
use tokio_tungstenite::tungstenite::Message;

// ---------------------------------------------------------------------------
// Test channel
// ---------------------------------------------------------------------------

struct PresenceLobby;

#[async_trait]
impl Channel for PresenceLobby {
    fn name(&self) -> &'static str {
        "presence.lobby"
    }
    fn presence_info(&self) -> Option<&dyn PresenceChannel> {
        Some(self)
    }
}

#[async_trait]
impl PresenceChannel for PresenceLobby {
    async fn member_info(
        &self,
        _req: &Request,
        _params: &ChannelParams,
    ) -> Result<Value, FrameworkError> {
        // Use nanos as a cheap unique-ish user id within a test run.
        use std::time::{SystemTime, UNIX_EPOCH};
        let uid = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;
        Ok(json!({ "user_id": uid }))
    }
}

// ---------------------------------------------------------------------------
// Server factory
// ---------------------------------------------------------------------------

async fn spawn_presence_server() -> u16 {
    let hub: Arc<InMemoryBroadcastHub> = Arc::new(InMemoryBroadcastHub::new());
    let mut registry = ChannelRegistry::new();
    registry.register(PresenceLobby);
    let registry = Arc::new(registry);

    let handler = BroadcastingWsHandler::new(hub.clone(), registry.clone());
    // The tokio-tungstenite test client doesn't send `Origin`; opt out of the
    // production-default `OriginPolicy::SameOrigin` so the presence flow is
    // what's under test.
    let router = Arc::new(Router::new().ws_with_config(
        "/ws/presence",
        handler,
        WsConfig {
            origin_policy: OriginPolicy::AllowAny,
            ..Default::default()
        },
    ));
    let middleware = Arc::new(suprnova::middleware::MiddlewareRegistry::new());

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
    // Give the listener a moment to be ready.
    tokio::time::sleep(Duration::from_millis(50)).await;
    port
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(url: &str) -> WsStream {
    let mut ws = tokio_tungstenite::connect_async(url).await.unwrap().0;
    // The broadcasting handler sends a `connected` frame first; consume it so
    // presence assertions see `subscribed` / presence events as the first frame.
    let frame = next_frame(&mut ws).await;
    assert_eq!(frame["action"], "connected");
    ws
}

async fn send_json(ws: &mut WsStream, v: Value) {
    ws.send(Message::text(serde_json::to_string(&v).unwrap()))
        .await
        .unwrap();
}

async fn next_frame(ws: &mut WsStream) -> Value {
    loop {
        let msg = ws.next().await.unwrap().unwrap();
        match msg {
            Message::Text(t) => return serde_json::from_str(&t).unwrap(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected WS message: {other:?}"),
        }
    }
}

async fn subscribe(ws: &mut WsStream, channel: &str) {
    send_json(ws, json!({ "action": "subscribe", "channel": channel })).await;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// A single subscriber to a presence channel receives `presence.here`
/// immediately after `subscribed`. The initial member list is empty
/// because no other subscribers are connected yet.
/// After `presence.here`, the server publishes `presence.joined` for
/// the new subscriber via the hub (standard Pusher self-join); Alice
/// receives that too.
#[tokio::test]
async fn presence_channel_emits_here_to_new_subscriber() {
    let port = spawn_presence_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/presence");

    let mut alice = connect(&url).await;
    subscribe(&mut alice, "presence.lobby").await;

    // Frame 1: Subscribed ack.
    let frame = next_frame(&mut alice).await;
    assert_eq!(
        frame["action"], "subscribed",
        "expected subscribed, got {frame}"
    );
    assert_eq!(frame["channel"], "presence.lobby");

    // Frame 2: presence.here snapshot.
    let frame = next_frame(&mut alice).await;
    assert_eq!(frame["action"], "event", "expected event, got {frame}");
    assert_eq!(frame["event"], "presence.here");
    assert_eq!(frame["channel"], "presence.lobby");
    // Alice is the first subscriber so the member list should be empty.
    assert!(
        frame["data"]["members"].as_array().unwrap().is_empty(),
        "expected empty members list for first subscriber, got {frame}"
    );

    // Frame 3: presence.joined for Alice herself (hub echo — self-join).
    let frame = next_frame(&mut alice).await;
    assert_eq!(
        frame["event"], "presence.joined",
        "expected self-join, got {frame}"
    );

    alice.close(None).await.unwrap();
}

/// When Bob subscribes to a presence channel that Alice is already
/// on, Alice receives `presence.joined` with Bob's member info.
/// Bob receives `presence.here` with Alice already listed.
#[tokio::test]
async fn presence_channel_broadcasts_joined_to_existing_subscribers() {
    let port = spawn_presence_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/presence");

    // Alice subscribes first.
    let mut alice = connect(&url).await;
    subscribe(&mut alice, "presence.lobby").await;
    let f = next_frame(&mut alice).await; // subscribed
    assert_eq!(f["action"], "subscribed");
    let f = next_frame(&mut alice).await; // presence.here (empty)
    assert_eq!(f["event"], "presence.here");
    // Drain Alice's own presence.joined (hub echoes the join to all
    // subscribers including the new subscriber themselves).
    let f = next_frame(&mut alice).await;
    assert_eq!(
        f["event"], "presence.joined",
        "expected alice's own join, got {f}"
    );

    // Bob subscribes second.
    let mut bob = connect(&url).await;
    subscribe(&mut bob, "presence.lobby").await;
    let f = next_frame(&mut bob).await; // subscribed
    assert_eq!(f["action"], "subscribed");
    let f = next_frame(&mut bob).await; // presence.here — Alice should be listed
    assert_eq!(f["event"], "presence.here");
    assert_eq!(
        f["data"]["members"].as_array().unwrap().len(),
        1,
        "Bob should see Alice in presence.here, got {f}"
    );

    // Alice should receive presence.joined for Bob within 2 s.
    let frame = timeout(TokioDuration::from_secs(2), next_frame(&mut alice))
        .await
        .expect("alice receives presence.joined within 2s");
    assert_eq!(frame["action"], "event", "got {frame}");
    assert_eq!(frame["event"], "presence.joined", "got {frame}");
    assert_eq!(frame["channel"], "presence.lobby");
    assert!(
        frame["data"]["user_id"].is_number(),
        "expected user_id in presence.joined data, got {frame}"
    );

    alice.close(None).await.unwrap();
    bob.close(None).await.unwrap();
}

/// When Bob disconnects, Alice receives `presence.left` with Bob's
/// member info.
#[tokio::test]
async fn presence_channel_broadcasts_left_on_disconnect() {
    let port = spawn_presence_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/presence");

    // Alice subscribes first.
    let mut alice = connect(&url).await;
    subscribe(&mut alice, "presence.lobby").await;
    let _ = next_frame(&mut alice).await; // subscribed
    let _ = next_frame(&mut alice).await; // presence.here (empty)
    // Drain Alice's own presence.joined.
    let f = next_frame(&mut alice).await;
    assert_eq!(
        f["event"], "presence.joined",
        "expected alice's own join, got {f}"
    );

    // Bob subscribes.
    let mut bob = connect(&url).await;
    subscribe(&mut bob, "presence.lobby").await;
    let _ = next_frame(&mut bob).await; // subscribed
    let _ = next_frame(&mut bob).await; // presence.here (alice listed)
    // Drain Bob's own presence.joined (Bob receives his own join via forwarder).
    let f = timeout(TokioDuration::from_secs(2), next_frame(&mut bob))
        .await
        .expect("bob receives own presence.joined within 2s");
    assert_eq!(
        f["event"], "presence.joined",
        "expected bob's own join, got {f}"
    );

    // Consume Alice's presence.joined for Bob.
    let joined = timeout(TokioDuration::from_secs(2), next_frame(&mut alice))
        .await
        .expect("alice receives presence.joined for bob within 2s");
    assert_eq!(joined["event"], "presence.joined", "got {joined}");

    // Bob disconnects.
    bob.close(None).await.unwrap();

    // Alice should receive presence.left for Bob within 2 s.
    let left = timeout(TokioDuration::from_secs(2), next_frame(&mut alice))
        .await
        .expect("alice receives presence.left within 2s");
    assert_eq!(left["action"], "event", "got {left}");
    assert_eq!(left["event"], "presence.left", "got {left}");
    assert_eq!(left["channel"], "presence.lobby");
    assert!(
        left["data"]["user_id"].is_number(),
        "expected user_id in presence.left data, got {left}"
    );

    alice.close(None).await.unwrap();
}
