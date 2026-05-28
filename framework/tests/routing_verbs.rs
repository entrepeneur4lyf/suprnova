//! Integration tests for the verb-gap fix.
//!
//! These tests drive `handle_request` end-to-end through a real hyper
//! connection so they cover the parts the inline router tests can't:
//! HEAD body strip on the wire (RFC 9110 §9.3.2), HEAD→GET middleware
//! inheritance when no explicit HEAD route is registered, PATCH and
//! OPTIONS dispatch through the full middleware chain, and explicit
//! HEAD handlers winning over the GET fallback. Each test pins one
//! contract; the assertions document the invariant inline.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use suprnova::http::text;
use suprnova::{Middleware, Next};
use suprnova::{MiddlewareRegistry, Request, Response, Router, handle_request};

/// Middleware that records each call under a tag, mirroring the
/// pattern used by `router_middleware_keying.rs`.
#[derive(Clone)]
struct TaggingMiddleware {
    tag: &'static str,
    tracker: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl Middleware for TaggingMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        self.tracker
            .lock()
            .expect("tracker lock poisoned")
            .push(self.tag);
        next(request).await
    }
}

/// Spawn an ephemeral hyper server that serves `accepts` connections
/// through `handle_request` against the supplied router. Returns the
/// bound socket address.
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

/// Send an HTTP/1.1 request and capture status + headers + body.
async fn send_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
) -> (hyper::http::StatusCode, hyper::HeaderMap, Bytes) {
    let stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect to test server");
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .expect("client handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header("Host", "localhost")
        .header("Content-Length", "0")
        .body(Full::new(Bytes::new()))
        .expect("build request");

    let resp = tokio::time::timeout(Duration::from_secs(5), sender.send_request(req))
        .await
        .expect("send_request timeout")
        .expect("hyper send_request");
    let (parts, body) = resp.into_parts();
    let collected = body.collect().await.expect("collect body bytes").to_bytes();
    (parts.status, parts.headers, collected)
}

/// HEAD against a GET-only route succeeds (RFC 9110 §9.3.2 fallback)
/// and returns the GET status with the body stripped to zero bytes.
#[tokio::test]
async fn head_falls_back_to_get_and_strips_body() {
    let router = Router::new().get("/articles", |_req| async { text("a long article body") });

    let addr = spawn_server(router, 2).await;
    let (status, _headers, body) = send_request(addr, "HEAD", "/articles").await;

    assert_eq!(status.as_u16(), 200, "HEAD must inherit GET's 200");
    assert!(
        body.is_empty(),
        "HEAD body must be empty after strip; got {body:?}",
    );

    // Sanity: the same GET still returns the body.
    let (get_status, _, get_body) = send_request(addr, "GET", "/articles").await;
    assert_eq!(get_status.as_u16(), 200);
    assert_eq!(get_body, "a long article body");
}

/// Middleware attached to the GET route runs for HEAD requests that
/// fall back to it. Without `has_explicit_head` driving the effective
/// method, auth / CSRF / rate-limit middleware would silently skip on
/// HEAD probes.
#[tokio::test]
async fn head_fallback_inherits_get_route_middleware() {
    let calls = Arc::new(Mutex::new(Vec::<&'static str>::new()));

    let router = Router::new()
        .get("/secured", |_req| async { text("ok") })
        .middleware(TaggingMiddleware {
            tag: "auth",
            tracker: calls.clone(),
        });

    let addr = spawn_server(router, 2).await;
    let (head_status, _, head_body) = send_request(addr, "HEAD", "/secured").await;
    assert_eq!(head_status.as_u16(), 200);
    assert!(head_body.is_empty(), "HEAD body must be empty");

    assert_eq!(
        *calls.lock().expect("tracker lock"),
        vec!["auth"],
        "GET middleware MUST run for the HEAD fallback request",
    );
}

/// An explicit HEAD handler wins over the GET fallback. Use case:
/// returning bespoke headers without running the GET body
/// computation. The HEAD handler's body is still stripped on the wire.
#[tokio::test]
async fn explicit_head_handler_wins_over_get_fallback() {
    use suprnova::http::HttpResponse;

    let router = Router::new()
        .get("/cached", |_req| async { text("expensive payload") })
        .head("/cached", |_req| async {
            Ok::<HttpResponse, HttpResponse>(
                HttpResponse::new()
                    .status(200)
                    .header("X-Cache-Status", "HIT"),
            )
        });

    let addr = spawn_server(router, 1).await;
    let (status, headers, body) = send_request(addr, "HEAD", "/cached").await;
    assert_eq!(status.as_u16(), 200);
    assert!(body.is_empty(), "HEAD body always empty on the wire");
    assert_eq!(
        headers
            .get("X-Cache-Status")
            .map(|v| v.to_str().expect("header utf8")),
        Some("HIT"),
        "explicit HEAD handler must have run (its header is on the response)",
    );
}

/// PATCH route registers + dispatches end-to-end through the full
/// middleware chain.
#[tokio::test]
async fn patch_route_dispatches_end_to_end() {
    let router = Router::new().patch("/posts/:id", |_req| async { text("patched") });
    let addr = spawn_server(router, 1).await;
    let (status, _, body) = send_request(addr, "PATCH", "/posts/42").await;
    assert_eq!(status.as_u16(), 200);
    assert_eq!(body, "patched");
}

/// OPTIONS route registers + dispatches end-to-end. (CORS preflight
/// short-circuits in middleware before this layer; this exercises the
/// non-preflight discovery path.)
#[tokio::test]
async fn options_route_dispatches_end_to_end() {
    let router = Router::new().options("/api/posts", |_req| async { text("GET, POST, PATCH") });
    let addr = spawn_server(router, 1).await;
    let (status, _, body) = send_request(addr, "OPTIONS", "/api/posts").await;
    assert_eq!(status.as_u16(), 200);
    assert_eq!(body, "GET, POST, PATCH");
}

/// HEAD against a path that has neither HEAD nor GET registered falls
/// through to the 404 chain (RequestId + global middleware still runs
/// per the no-route policy, terminating in a fixed 404).
#[tokio::test]
async fn head_against_unrouted_path_returns_404() {
    let router = Router::new().post("/submit", |_req| async { text("created") });
    let addr = spawn_server(router, 1).await;
    let (status, _, body) = send_request(addr, "HEAD", "/submit").await;
    assert_eq!(status.as_u16(), 404);
    assert!(body.is_empty(), "HEAD bodies are always empty");
}
