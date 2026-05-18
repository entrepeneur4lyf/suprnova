//! Close-on-no-pong heartbeat enforcement test.
//!
//! Phase 7B Task 8. The structural test below verifies that the heartbeat
//! wiring doesn't break healthy connections (tokio-tungstenite auto-responds
//! to pings by default, so a healthy peer is never mistakenly closed). The
//! default ping_interval is 30s so the heartbeat never fires in a 250ms
//! window; the test exercises the echo path to confirm the connection
//! remains functional.
//!
//! # Close-on-no-pong integration test (future)
//!
//! A full close-on-no-pong test requires:
//! 1. Per-route WsConfig override (e.g. `Router::ws_with_config(path, handler, config)`
//!    or `ws!()` macro support for overriding `WsConfig::max_missed_pings` and
//!    `WsConfig::ping_interval` per route). This is a 7B+ concern; v1 applies
//!    `WsConfig::default()` globally.
//! 2. A raw TCP client that suppresses the auto-pong behavior (not possible
//!    with the standard tokio-tungstenite client — the library responds to
//!    pings automatically). A raw `tokio::net::TcpStream` + manual WebSocket
//!    framing or a custom `tungstenite` config with `auto_pong: false` would
//!    work when that API surface is exposed.
//!
//! Once per-route WsConfig override ships, add a test here that:
//! - Constructs a route with `ping_interval = Duration::from_millis(50)`,
//!   `max_missed_pings = 1`
//! - Connects a raw client that never sends pong frames
//! - Asserts a Close frame with code 1011 arrives within ~200ms

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use suprnova::http::Request;
use suprnova::routing::Router;
use suprnova::ws::{WebSocketHandler, WsSocket};
use suprnova::FrameworkError;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

struct EchoHandler;

#[async_trait]
impl WebSocketHandler for EchoHandler {
    async fn handle(&self, mut socket: WsSocket, _req: Request) -> Result<(), FrameworkError> {
        while let Some(text) = socket.recv_text().await? {
            socket.send_text(format!("echo: {text}")).await?;
        }
        Ok(())
    }
}

async fn spawn_test_server() -> u16 {
    let router = Arc::new(Router::new().ws("/ws/echo", EchoHandler));
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
    tokio::time::sleep(Duration::from_millis(50)).await;
    port
}

#[tokio::test]
async fn pong_responsive_client_stays_connected() {
    // tokio-tungstenite auto-responds to pings by default — this
    // test verifies the heartbeat doesn't mistakenly close a healthy
    // connection. Run for 250ms (much longer than the default
    // heartbeat would tick if it were tight, but the default is 30s
    // so it shouldn't fire at all in this window).
    let port = spawn_test_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/echo");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");

    tokio::time::sleep(Duration::from_millis(250)).await;
    ws.send(Message::text("still here")).await.unwrap();
    let reply = ws.next().await.unwrap().unwrap();
    assert_eq!(reply.to_text().unwrap(), "echo: still here");
    ws.close(None).await.unwrap();
}
