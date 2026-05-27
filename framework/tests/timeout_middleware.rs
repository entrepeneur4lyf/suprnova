//! Integration tests for `TimeoutMiddleware`.
//!
//! These drive real requests through `handle_request` so the middleware
//! runs in the actual chain. They prove the deadline bounds *time-to-
//! response* (a slow handler becomes 503 and is cancelled), while streaming
//! responses and WebSocket upgrades are left unbounded.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use futures::stream;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

use suprnova::http::text;
use suprnova::sse::SseEvent;
use suprnova::{HttpResponse, MiddlewareRegistry, Router, TimeoutMiddleware, handle_request};

/// Spawn a test HTTP/1.1 server bound to an ephemeral port, dispatch up to
/// `accepts` connections via `handle_request`, then exit. Mirrors the harness
/// in `rate_limit_middleware.rs`.
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

/// Issue a single GET (with optional extra request headers) and return
/// `(status, content_type, body)`.
async fn get(
    addr: SocketAddr,
    path: &str,
    extra_headers: &[(&str, &str)],
) -> (u16, Option<String>, String) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut builder = hyper::Request::builder()
        .method("GET")
        .uri(path)
        .header("Host", "localhost")
        .header("Content-Length", "0");
    for (name, value) in extra_headers {
        builder = builder.header(*name, *value);
    }
    let req = builder.body(Full::new(Bytes::new())).unwrap();

    // Generous client-side cap so a hung handler can't wedge the test
    // process; the server-side deadline under test is far shorter.
    let resp = tokio::time::timeout(Duration::from_secs(5), sender.send_request(req))
        .await
        .expect("send_request timeout")
        .expect("hyper send_request");

    let (parts, body) = resp.into_parts();
    let status = parts.status.as_u16();
    let content_type = parts
        .headers
        .get("content-type")
        .and_then(|v| v.to_str().ok().map(String::from));
    let bytes = body.collect().await.unwrap().to_bytes();
    let body = String::from_utf8_lossy(&bytes).to_string();
    (status, content_type, body)
}

/// A handler that returns well within the deadline passes through untouched,
/// real body and all.
#[tokio::test]
async fn fast_handler_passes_through_with_its_real_response() {
    let router = Router::new()
        .get("/ok", |_req| async { text("served") })
        .middleware(TimeoutMiddleware::seconds(30));

    let addr = spawn_server(router, 3).await;
    let (status, _ct, body) = get(addr, "/ok", &[]).await;

    assert_eq!(status, 200, "a fast handler must not be timed out");
    assert_eq!(body, "served", "the handler's real body must be returned");
}

/// A handler that exceeds the deadline yields 503 AND is cancelled: the
/// post-await side effect never runs. The `AtomicBool` is what gives this
/// teeth — a 503 alone can't prove the handler future was actually dropped
/// rather than left running to completion in the background.
#[tokio::test]
async fn slow_handler_times_out_with_503_and_is_cancelled() {
    let completed = Arc::new(AtomicBool::new(false));
    let flag = completed.clone();

    let router = Router::new()
        .get("/slow", move |_req| {
            let flag = flag.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                // Only reached if the handler is NOT cancelled by the timeout.
                flag.store(true, Ordering::SeqCst);
                text("late")
            }
        })
        .middleware(TimeoutMiddleware::new(Duration::from_millis(50)));

    let addr = spawn_server(router, 3).await;
    let (status, _ct, _body) = get(addr, "/slow", &[]).await;

    assert_eq!(
        status, 503,
        "a handler exceeding the deadline must yield 503"
    );

    // Wait past the handler's own sleep. If the timeout had merely returned
    // 503 while letting the handler run on, the flag would flip to true.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        !completed.load(Ordering::SeqCst),
        "the timed-out handler must be cancelled, not left running to completion"
    );
}

/// SSE responses return immediately with a lazy body that hyper drains AFTER
/// the middleware chain completes, so the deadline (which bounds time-to-
/// response) never fires. Under a 50ms deadline the stream still produces a
/// 200 `text/event-stream` with its events intact. If the deadline wrongly
/// bounded stream lifetime this would be a 503.
#[tokio::test]
async fn sse_response_is_not_killed_by_a_short_deadline() {
    let router = Router::new()
        .get("/events", |_req| async {
            let events = vec![SseEvent::data("one"), SseEvent::data("two")];
            Ok(HttpResponse::sse(stream::iter(events)))
        })
        .middleware(TimeoutMiddleware::new(Duration::from_millis(50)));

    let addr = spawn_server(router, 3).await;
    let (status, content_type, body) = get(addr, "/events", &[]).await;

    assert_eq!(status, 200, "an SSE response must pass through, not 503");
    assert_eq!(
        content_type.as_deref(),
        Some("text/event-stream"),
        "the streaming response's own content type must survive"
    );
    assert!(
        body.contains("data: one") && body.contains("data: two"),
        "every event must be delivered despite the short deadline; got: {body:?}"
    );
}

/// A request carrying `Upgrade: websocket` skips the deadline entirely. The
/// contrast is the proof: the SAME slow handler under the SAME deadline is
/// 503 without the header and 200 with it.
#[tokio::test]
async fn websocket_upgrade_request_skips_the_deadline() {
    let router = Router::new()
        .get("/maybe-ws", |_req| async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            text("handler ran")
        })
        .middleware(TimeoutMiddleware::new(Duration::from_millis(50)));

    let addr = spawn_server(router, 4).await;

    // Baseline: no upgrade header -> the slow handler is bounded.
    let (plain_status, _ct, _body) = get(addr, "/maybe-ws", &[]).await;
    assert_eq!(
        plain_status, 503,
        "without Upgrade, the slow handler must be bounded"
    );

    // With the websocket upgrade header the deadline is skipped and the
    // handler runs to completion.
    let (ws_status, _ct, ws_body) = get(
        addr,
        "/maybe-ws",
        &[("Upgrade", "websocket"), ("Connection", "upgrade")],
    )
    .await;
    assert_eq!(
        ws_status, 200,
        "an Upgrade: websocket request must skip the deadline"
    );
    assert_eq!(ws_body, "handler ran");
}
