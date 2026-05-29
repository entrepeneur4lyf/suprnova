//! End-to-end WebSocket integration test.
//!
//! Spawns the framework's `handle_request` on a free port behind a
//! hyper http1 service loop with `.with_upgrades()`, connects a real
//! `tokio-tungstenite` client, exchanges text frames, asserts the
//! echoes round-trip. Also pins the bad-handshake path and the
//! unmatched-route fall-through.

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use suprnova::FrameworkError;
use suprnova::http::Request;
use suprnova::middleware::MiddlewareRegistry;
use suprnova::routing::Router;
use suprnova::ws::{OriginPolicy, WebSocketHandler, WsConfig, WsSocket};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// The tokio-tungstenite test client doesn't send an `Origin` header by
/// default, so the production-default `OriginPolicy::SameOrigin` would
/// 403 every test. Opt into `AllowAny` for these tests — they're not
/// exercising browser CSRF semantics. See `ws_origin_policy.rs` for the
/// dedicated coverage of `SameOrigin` / `AllowList`.
fn open_ws_config() -> WsConfig {
    WsConfig {
        origin_policy: OriginPolicy::AllowAny,
        ..Default::default()
    }
}

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
    // Router::ws returns Router directly — no .build() / .into() needed.
    let router = Arc::new(Router::new().ws_with_config("/ws/echo", EchoHandler, open_ws_config()));
    let middleware = Arc::new(MiddlewareRegistry::new());

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind free port");
    let port = listener.local_addr().expect("local_addr").port();

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
                // .with_upgrades() is essential — without it the
                // OnUpgrade future never resolves and the handler
                // task hangs.
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .with_upgrades()
                    .await;
            });
        }
    });

    // Give the listener a beat to bind.
    tokio::time::sleep(Duration::from_millis(50)).await;
    port
}

#[tokio::test]
async fn echo_handler_round_trips_messages() {
    let port = spawn_test_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/echo");

    let (mut ws, response) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect to ws echo endpoint");
    assert_eq!(
        response.status(),
        101,
        "expected 101 Switching Protocols, got {}",
        response.status()
    );

    ws.send(Message::text("hello")).await.expect("send hello");
    let reply = ws
        .next()
        .await
        .expect("recv reply to hello")
        .expect("no error on hello reply");
    assert_eq!(reply.to_text().expect("reply is text"), "echo: hello");

    ws.send(Message::text("world")).await.expect("send world");
    let reply = ws
        .next()
        .await
        .expect("recv reply to world")
        .expect("no error on world reply");
    assert_eq!(reply.to_text().expect("reply is text"), "echo: world");

    ws.close(None).await.expect("clean close");
}

#[tokio::test]
async fn upgrade_returns_400_on_bad_handshake() {
    let port = spawn_test_server().await;

    // Raw HTTP with `Upgrade: websocket` but missing `Sec-WebSocket-Version`.
    // hyper-tungstenite's upgrade() rejects this as a malformed handshake.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("tcp connect to test server");
    let req = "GET /ws/echo HTTP/1.1\r\n\
               Host: 127.0.0.1\r\n\
               Connection: Upgrade\r\n\
               Upgrade: websocket\r\n\
               Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
               \r\n";
    stream
        .write_all(req.as_bytes())
        .await
        .expect("write raw request");

    let mut buf = [0u8; 512];
    let n = stream.read(&mut buf).await.expect("read response");
    let s = std::str::from_utf8(&buf[..n]).expect("response is utf8");
    assert!(
        s.starts_with("HTTP/1.1 400"),
        "expected 400 Bad Request; got:\n{s}"
    );
}

#[tokio::test]
async fn missing_ws_route_returns_normal_404() {
    let port = spawn_test_server().await;

    // tokio-tungstenite's connect_async will fail because the path
    // falls through to normal HTTP routing which 404s. We assert the
    // connection is rejected — the exact error shape varies across
    // tungstenite versions.
    let url = format!("ws://127.0.0.1:{port}/ws/nope");
    let result = tokio_tungstenite::connect_async(&url).await;
    assert!(
        result.is_err(),
        "unregistered ws path should reject the upgrade, not return 101"
    );

    // Additionally confirm via plain HTTP that the path returns 404,
    // ruling out the possibility the server crashed (which would also
    // fail connect_async).
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("tcp connect to test server after ws rejection");
    let req = "GET /ws/nope HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
    stream
        .write_all(req.as_bytes())
        .await
        .expect("write plain GET");
    let mut buf = [0u8; 512];
    let n = stream.read(&mut buf).await.expect("read plain response");
    let s = std::str::from_utf8(&buf[..n]).expect("response is utf8");
    assert!(
        s.starts_with("HTTP/1.1 404"),
        "plain GET to unregistered path should 404; got:\n{s}"
    );
}

/// Proves [`WsConfig::generous`] actually raises the per-route limit
/// past the public-safe default of 1 MiB. The default config would
/// reject a 2 MiB message; the generous config accepts it and
/// round-trips it through the echo handler.
#[tokio::test]
async fn generous_config_round_trips_a_2_mib_message() {
    // Trusted-feed limits + AllowAny so tokio-tungstenite (no Origin
    // header) can connect. `..WsConfig::generous()` carries the 64 MiB
    // message + 16 MiB frame caps from the trusted-feed factory.
    let cfg = WsConfig {
        origin_policy: OriginPolicy::AllowAny,
        ..WsConfig::generous()
    };

    let router = Arc::new(Router::new().ws_with_config("/ws/echo", EchoHandler, cfg));
    let middleware = Arc::new(MiddlewareRegistry::new());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("local_addr").port();

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

    let url = format!("ws://127.0.0.1:{port}/ws/echo");
    // tokio-tungstenite enforces its own per-message cap on the
    // client side; raise it past the 2 MiB body + "echo: " prefix
    // so the reply isn't capped by the client.
    let client_cfg = {
        let mut c = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
        c.max_message_size = Some(8 * 1024 * 1024);
        c.max_frame_size = Some(8 * 1024 * 1024);
        c
    };
    let request = tokio_tungstenite::tungstenite::handshake::client::Request::builder()
        .uri(&url)
        .header("Host", format!("127.0.0.1:{port}"))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .expect("build handshake request");
    let (mut ws, _) =
        tokio_tungstenite::connect_async_with_config(request, Some(client_cfg), false)
            .await
            .expect("connect to generous endpoint");

    // 2 MiB payload — comfortably over the 1 MiB public default,
    // comfortably under the 64 MiB generous cap.
    let payload: String = "x".repeat(2 * 1024 * 1024);
    ws.send(Message::text(payload.clone())).await.expect("send");
    let reply = ws.next().await.expect("recv").expect("no error");
    let reply_text = reply.to_text().expect("text reply").to_string();
    assert_eq!(reply_text.len(), "echo: ".len() + payload.len());
    assert!(reply_text.starts_with("echo: "));
    assert!(reply_text.ends_with(&payload[payload.len() - 16..]));
    ws.close(None).await.expect("clean close");
}

#[tokio::test]
async fn idle_connection_survives_quiet_period_and_can_still_send() {
    // Verify the heartbeat machinery's presence doesn't BREAK an
    // otherwise-idle connection. The default ping interval is 30s
    // (we don't wait that long); we just confirm the connection
    // stays usable across a short quiet period — proving the
    // forwarder task and the heartbeat coexistence are correct.
    let port = spawn_test_server().await;
    let url = format!("ws://127.0.0.1:{port}/ws/echo");

    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");

    // Idle for >150ms — significantly longer than the spawn delay
    // and any plausible network blip but well under the 30s ping.
    tokio::time::sleep(Duration::from_millis(150)).await;

    ws.send(Message::text("still here")).await.unwrap();
    let reply = ws.next().await.unwrap().unwrap();
    assert_eq!(reply.to_text().unwrap(), "echo: still here");

    ws.close(None).await.unwrap();
}
