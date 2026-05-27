//! Global middleware must run on WebSocket upgrade requests, not just
//! plain HTTP. A WS upgrade is an HTTP GET, so global auth / session /
//! rate-limit / logging middleware protects `/ws/*` routes exactly as it
//! protects any other route. Before this was wired, `handle_ws_upgrade`
//! ignored the global registry entirely and only ran per-route WS
//! middleware — a globally installed auth gate did nothing for upgrades.
//!
//! These drive the real `handle_request` over a loopback socket with
//! `.with_upgrades()` (modeled on `ws_e2e.rs`) and a real
//! `tokio-tungstenite` client, because the upgrade/handshake outcome
//! cannot be observed through a bare in-process call.
//!
//! The "global" middleware is installed through an explicit
//! `MiddlewareRegistry` passed to the server rather than the process-wide
//! `register_global_middleware`. That keeps these parallel tests isolated
//! from the `OnceLock` global state while exercising the identical code
//! path: `handle_ws_upgrade` applies whatever globals the registry holds.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use suprnova::http::{HttpResponse, Request};
use suprnova::ws::{WebSocketHandler, WsSocket};
use suprnova::{FrameworkError, Middleware, MiddlewareRegistry, Next, Response, Router};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

/// Minimal echo handler so a successful upgrade has a working session.
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

/// Global middleware that rejects every request with 401 before it can
/// reach the handler — stands in for an auth / session gate.
struct RejectingGlobalMiddleware;

#[async_trait]
impl Middleware for RejectingGlobalMiddleware {
    async fn handle(&self, _request: Request, _next: Next) -> Response {
        Err(HttpResponse::text("blocked by global middleware").status(401))
    }
}

/// Global middleware that records it ran, then continues the chain.
struct CapturingGlobalMiddleware {
    ran: Arc<AtomicBool>,
}

#[async_trait]
impl Middleware for CapturingGlobalMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        self.ran.store(true, Ordering::SeqCst);
        next(request).await
    }
}

/// Spawn the framework server with an explicit middleware registry on a
/// free port behind a hyper http1 loop with `.with_upgrades()`.
async fn spawn_test_server(registry: MiddlewareRegistry) -> u16 {
    let router = Arc::new(Router::new().ws("/ws/echo", EchoHandler));
    let middleware = Arc::new(registry);

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

/// A global middleware returning 401 must abort the upgrade — proving
/// globals are applied to the WS path. The 401 must also echo
/// `X-Request-Id`, proving the RequestId + global chain (not just
/// per-route middleware) ran ahead of the handler.
#[tokio::test]
async fn global_middleware_can_reject_a_ws_upgrade() {
    let registry = MiddlewareRegistry::new().append(RejectingGlobalMiddleware);
    let port = spawn_test_server(registry).await;
    let url = format!("ws://127.0.0.1:{port}/ws/echo");

    match tokio_tungstenite::connect_async(&url).await {
        Err(WsError::Http(resp)) => {
            assert_eq!(
                resp.status(),
                401,
                "global middleware must reject the upgrade with its own status"
            );
            assert!(
                resp.headers().get("x-request-id").is_some(),
                "a rejected upgrade must echo X-Request-Id (proves the RequestId \
                 + global chain ran on the WS path)"
            );
        }
        Ok(_) => panic!(
            "upgrade succeeded despite a global 401 middleware — global middleware \
             is not being applied to WS upgrades"
        ),
        Err(other) => panic!("expected an HTTP 401 rejection, got: {other:?}"),
    }
}

/// A global middleware that allows the request through must actually run
/// during the upgrade chain, the handshake must complete with 101, the
/// 101 must echo `X-Request-Id`, and the established session must work.
#[tokio::test]
async fn global_middleware_runs_on_a_successful_ws_upgrade() {
    let ran = Arc::new(AtomicBool::new(false));
    let registry = MiddlewareRegistry::new().append(CapturingGlobalMiddleware { ran: ran.clone() });
    let port = spawn_test_server(registry).await;
    let url = format!("ws://127.0.0.1:{port}/ws/echo");

    let (mut ws, response) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("a global allow-through middleware must let the upgrade complete");

    assert_eq!(response.status(), 101, "expected 101 Switching Protocols");
    assert!(
        ran.load(Ordering::SeqCst),
        "the global middleware must have run during the upgrade chain"
    );
    assert!(
        response.headers().get("x-request-id").is_some(),
        "the 101 handshake response must echo X-Request-Id"
    );

    // The session works end-to-end through the global chain.
    ws.send(Message::text("hi")).await.expect("send");
    let reply = ws.next().await.expect("recv").expect("no error on reply");
    assert_eq!(reply.to_text().expect("text reply"), "echo: hi");
    ws.close(None).await.expect("clean close");
}
