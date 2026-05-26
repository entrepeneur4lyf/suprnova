//! Regression tests for Domain 2 audit finding M1: panicking middleware
//! and handlers MUST translate to a 500 response, not a dropped
//! connection. Without this guarantee the OSS framework would punish
//! user-authored code with TCP resets that are very hard to debug.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use suprnova::http::text;
use suprnova::{Middleware, MiddlewareRegistry, Next, Request, Response, Router, handle_request};

// ── test fixtures ──────────────────────────────────────────────────────────

/// Middleware that panics with a literal string message.
struct PanickingMiddleware;

#[async_trait]
impl Middleware for PanickingMiddleware {
    async fn handle(&self, _request: Request, _next: Next) -> Response {
        panic!("intentional test panic — string literal payload");
    }
}

/// Middleware that panics with a String (different downcast path).
struct PanickingMiddlewareString;

#[async_trait]
impl Middleware for PanickingMiddlewareString {
    async fn handle(&self, _request: Request, _next: Next) -> Response {
        panic!(
            "{}",
            String::from("intentional test panic — String payload")
        );
    }
}

// ── test server helpers ────────────────────────────────────────────────────

async fn spawn_server(router: impl Into<Router>, accepts: usize) -> SocketAddr {
    let router = Arc::new(router.into());
    let middleware = Arc::new(MiddlewareRegistry::new());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral listener");
    let addr = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        for _ in 0..accepts {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let io = TokioIo::new(stream);
            let router = router.clone();
            let middleware = middleware.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req: hyper::Request<Incoming>| {
                    let router = router.clone();
                    let middleware = middleware.clone();
                    async move { Ok::<_, Infallible>(handle_request(router, middleware, req).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await;
            });
        }
    });

    addr
}

async fn send_get(addr: SocketAddr, path: &str) -> (hyper::http::StatusCode, Bytes) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method("GET")
        .uri(path)
        .header("Host", "localhost")
        .header("Content-Length", "0")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = tokio::time::timeout(Duration::from_secs(5), sender.send_request(req))
        .await
        .expect("send_get timeout")
        .expect("hyper send_request");
    let (parts, body) = resp.into_parts();
    let collected = body.collect().await.unwrap().to_bytes();
    (parts.status, collected)
}

// ── tests ──────────────────────────────────────────────────────────────────

/// A panicking middleware must translate to a 500 response, not a
/// dropped connection. After audit HIGH `error` #1 the body uses the
/// same standardised JSON shape the `FrameworkError -> HttpResponse`
/// path emits — generic 5xx-sanitised `message`, optional
/// `request_id`, optional `debug_message` when `APP_DEBUG=true`.
/// The panic payload still appears in the structured tracing log,
/// just not in the wire response.
#[tokio::test]
async fn panicking_middleware_translates_to_500() {
    let router = Router::new()
        .get("/panic-mw", |_req: Request| async {
            text("should never reach handler")
        })
        .middleware(PanickingMiddleware);

    let addr = spawn_server(router, 2).await;
    let (status, body) = send_get(addr, "/panic-mw").await;
    assert_eq!(
        status.as_u16(),
        500,
        "panicking middleware must yield 500, got body: {}",
        String::from_utf8_lossy(&body),
    );
    let parsed: serde_json::Value = serde_json::from_slice(&body)
        .expect("panic response must be valid JSON via the FrameworkError -> HttpResponse path");
    assert_eq!(
        parsed["message"],
        "Internal Server Error",
        "panic body must carry the sanitised 5xx message; got: {}",
        String::from_utf8_lossy(&body),
    );
    assert!(
        parsed.get("request_id").is_some(),
        "panic body must include `request_id` key (null outside an active scope)"
    );
}

/// String-payload panics use a different `Box<dyn Any>` downcast path;
/// pin that they also translate cleanly to 500.
#[tokio::test]
async fn panicking_middleware_string_payload_translates_to_500() {
    let router = Router::new()
        .get("/panic-mw-str", |_req: Request| async {
            text("should never reach handler")
        })
        .middleware(PanickingMiddlewareString);

    let addr = spawn_server(router, 2).await;
    let (status, _body) = send_get(addr, "/panic-mw-str").await;
    assert_eq!(status.as_u16(), 500);
}

/// A panicking route handler (no middleware involved) must also yield
/// 500. This is the inner-most catch_unwind boundary in
/// `execute_chain_safely`.
#[tokio::test]
async fn panicking_handler_translates_to_500() {
    let router = Router::new().get("/panic-handler", |_req: Request| async {
        panic!("intentional test panic — handler");
        #[allow(unreachable_code)]
        text("unreachable")
    });

    let addr = spawn_server(router, 2).await;
    let (status, body) = send_get(addr, "/panic-handler").await;
    assert_eq!(
        status.as_u16(),
        500,
        "panicking handler must yield 500, got body: {}",
        String::from_utf8_lossy(&body),
    );
}

/// A 500 from a panic must not poison subsequent requests on the same
/// listener. The accept loop survives the panicked task and the next
/// request gets its normal response.
#[tokio::test]
async fn server_survives_panic_and_serves_next_request() {
    let router = Router::new()
        .get("/panic", |_req: Request| async {
            panic!("intentional");
            #[allow(unreachable_code)]
            text("unreachable")
        })
        .get("/ok", |_req: Request| async { text("ok") });

    let addr = spawn_server(router, 4).await;

    // First request panics — gets 500.
    let (s1, _) = send_get(addr, "/panic").await;
    assert_eq!(s1.as_u16(), 500);

    // Second request on the same listener succeeds normally.
    let (s2, b2) = send_get(addr, "/ok").await;
    assert_eq!(s2.as_u16(), 200);
    assert_eq!(b2.as_ref(), b"ok");
}
