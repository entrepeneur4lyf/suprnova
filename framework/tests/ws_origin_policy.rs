//! WebSocket Origin-policy and handler-error close-code tests.
//!
//! Covers the two ws-module HIGH findings:
//!
//! 1. `OriginPolicy::SameOrigin` (the production default) rejects upgrades
//!    whose `Origin` doesn't match the request's `Host`, including the
//!    no-Origin case. `AllowList` accepts only exact matches.
//!    `AllowAny` skips the check entirely (test / non-browser opt-in).
//!
//! 2. A handler returning `Err(_)` causes the framework to send an explicit
//!    Close frame with code 1011 (Error) before tearing the connection
//!    down — matching the documented `WebSocketHandler` trait contract.

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
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

// ---------------------------------------------------------------------------
// Echo handler — used by Origin-policy tests where the handler itself isn't
// the subject under test.
// ---------------------------------------------------------------------------

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

/// Handler that always returns `Err`. Used to verify the framework sends
/// Close 1011 on the Err path.
struct ErroringHandler;

#[async_trait]
impl WebSocketHandler for ErroringHandler {
    async fn handle(&self, _socket: WsSocket, _req: Request) -> Result<(), FrameworkError> {
        Err(FrameworkError::internal("simulated handler failure"))
    }
}

// ---------------------------------------------------------------------------
// Loopback test server. Each test spawns its own server with the route /
// config it needs and gets back the port to dial.
// ---------------------------------------------------------------------------

async fn spawn_server(router: Router) -> u16 {
    let router = Arc::new(router);
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

fn ws_config(policy: OriginPolicy) -> WsConfig {
    WsConfig {
        origin_policy: policy,
        ..Default::default()
    }
}

/// Build a tungstenite client request with a custom `Origin` header.
fn ws_request_with_origin(
    url: &str,
    origin: &str,
) -> tokio_tungstenite::tungstenite::handshake::client::Request {
    let mut req = url.into_client_request().expect("valid ws url");
    req.headers_mut()
        .insert("Origin", origin.parse().expect("valid Origin header"));
    req
}

// ---------------------------------------------------------------------------
// SameOrigin policy (the production default)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn same_origin_default_rejects_missing_origin_header() {
    // Bare `connect_async(url)` does NOT send an `Origin` header. Under the
    // production-default `SameOrigin` policy this must fail the upgrade with
    // a non-2xx response — proving SameOrigin closes the no-Origin bypass.
    let port = spawn_server(Router::new().ws_with_config(
        "/ws/echo",
        EchoHandler,
        ws_config(OriginPolicy::SameOrigin),
    ))
    .await;
    let url = format!("ws://127.0.0.1:{port}/ws/echo");
    let res = tokio_tungstenite::connect_async(&url).await;
    assert!(
        res.is_err(),
        "SameOrigin must reject an upgrade with no Origin header; got Ok"
    );
}

#[tokio::test]
async fn same_origin_default_rejects_cross_origin() {
    // A browser pointing at `evil.example.com` would send
    // `Origin: https://evil.example.com` while connecting to our Host
    // (127.0.0.1:port). SameOrigin compares hosts — different → reject.
    let port = spawn_server(Router::new().ws_with_config(
        "/ws/echo",
        EchoHandler,
        ws_config(OriginPolicy::SameOrigin),
    ))
    .await;
    let url = format!("ws://127.0.0.1:{port}/ws/echo");
    let req = ws_request_with_origin(&url, "https://evil.example.com");
    let res = tokio_tungstenite::connect_async(req).await;
    assert!(
        res.is_err(),
        "SameOrigin must reject cross-origin upgrade; got Ok"
    );
}

#[tokio::test]
async fn same_origin_default_allows_matching_origin() {
    // The browser's same-origin Origin would be the page URL's
    // scheme://host[:port]. For our 127.0.0.1:port host that is
    // http://127.0.0.1:port. SameOrigin must accept it.
    let port = spawn_server(Router::new().ws_with_config(
        "/ws/echo",
        EchoHandler,
        ws_config(OriginPolicy::SameOrigin),
    ))
    .await;
    let url = format!("ws://127.0.0.1:{port}/ws/echo");
    let origin = format!("http://127.0.0.1:{port}");
    let req = ws_request_with_origin(&url, &origin);
    let (mut ws, response) = tokio_tungstenite::connect_async(req)
        .await
        .expect("same-origin upgrade must succeed");
    assert_eq!(response.status(), 101);
    ws.send(Message::text("ping")).await.unwrap();
    let reply = ws.next().await.unwrap().unwrap();
    assert_eq!(reply.to_text().unwrap(), "echo: ping");
    ws.close(None).await.unwrap();
}

// ---------------------------------------------------------------------------
// AllowList policy
// ---------------------------------------------------------------------------

#[tokio::test]
async fn allow_list_accepts_listed_origin() {
    let port = spawn_server(Router::new().ws_with_config(
        "/ws/echo",
        EchoHandler,
        ws_config(OriginPolicy::AllowList(vec![
            "https://app.example.com".into(),
            "https://staging.example.com".into(),
        ])),
    ))
    .await;
    let url = format!("ws://127.0.0.1:{port}/ws/echo");
    let req = ws_request_with_origin(&url, "https://app.example.com");
    let (mut ws, response) = tokio_tungstenite::connect_async(req)
        .await
        .expect("AllowList must accept a listed Origin");
    assert_eq!(response.status(), 101);
    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn allow_list_rejects_unlisted_origin() {
    let port = spawn_server(Router::new().ws_with_config(
        "/ws/echo",
        EchoHandler,
        ws_config(OriginPolicy::AllowList(vec![
            "https://app.example.com".into(),
        ])),
    ))
    .await;
    let url = format!("ws://127.0.0.1:{port}/ws/echo");
    let req = ws_request_with_origin(&url, "https://other.example.com");
    let res = tokio_tungstenite::connect_async(req).await;
    assert!(
        res.is_err(),
        "AllowList must reject an Origin not in the list"
    );
}

#[tokio::test]
async fn allow_list_rejects_missing_origin() {
    // No Origin header — AllowList has nothing to match against, must reject.
    let port = spawn_server(Router::new().ws_with_config(
        "/ws/echo",
        EchoHandler,
        ws_config(OriginPolicy::AllowList(vec![
            "https://app.example.com".into(),
        ])),
    ))
    .await;
    let url = format!("ws://127.0.0.1:{port}/ws/echo");
    let res = tokio_tungstenite::connect_async(&url).await;
    assert!(
        res.is_err(),
        "AllowList must reject an upgrade with no Origin header"
    );
}

// ---------------------------------------------------------------------------
// Handler Err → Close 1011
// ---------------------------------------------------------------------------

#[tokio::test]
async fn handler_err_sends_close_1011() {
    // Connect with AllowAny so the upgrade succeeds, then verify the
    // server sends a Close frame with code 1011 (Error) when the handler
    // returns Err — matching the documented `WebSocketHandler` contract.
    let port = spawn_server(Router::new().ws_with_config(
        "/ws/err",
        ErroringHandler,
        ws_config(OriginPolicy::AllowAny),
    ))
    .await;
    let url = format!("ws://127.0.0.1:{port}/ws/err");
    let (mut ws, response) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("upgrade should succeed under AllowAny");
    assert_eq!(response.status(), 101);

    // The handler returned Err immediately. Read until we see the Close
    // frame; the explicit Close should arrive before connection teardown.
    let mut close_frame = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), ws.next()).await {
            Ok(Some(Ok(Message::Close(frame)))) => {
                close_frame = frame;
                break;
            }
            Ok(Some(Ok(_other))) => continue,
            Ok(Some(Err(_))) | Ok(None) => break,
            Err(_) => continue,
        }
    }
    let frame = close_frame.expect("handler Err must trigger a Close frame within the deadline");
    assert_eq!(
        frame.code,
        CloseCode::Error,
        "expected Close 1011 (Error) on handler Err; got {:?}",
        frame.code
    );
}
