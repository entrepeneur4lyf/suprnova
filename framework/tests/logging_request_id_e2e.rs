//! End-to-end request-id tests that drive the real `handle_request`
//! path (router + middleware registry + hyper request), as opposed to
//! the `logging.rs` tests which drive `chain.execute()` directly.
//!
//! Driving `handle_request` is what makes the panic-boundary and
//! built-in-endpoint behaviours testable: a handler panic is caught in
//! `execute_chain_safely`, and the health endpoint short-circuits
//! before the normal routing path — neither is reachable through a
//! bare `chain.execute()`.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

use suprnova::http::text;
use suprnova::{MiddlewareRegistry, Router, handle_request};
use tracing_test::traced_test;

/// Spawn a test server that routes through the real `handle_request`.
async fn spawn_server(
    router: impl Into<Router>,
    registry: MiddlewareRegistry,
    accepts: usize,
) -> SocketAddr {
    let router = Arc::new(router.into());
    let middleware = Arc::new(registry);

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

/// Send a request and return `(status, lowercased response headers, body)`.
async fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> (u16, HashMap<String, String>, String) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut builder = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header("Host", "localhost")
        .header("Content-Length", "0");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let req = builder.body(Full::new(Bytes::new())).unwrap();

    let resp = tokio::time::timeout(Duration::from_secs(5), sender.send_request(req))
        .await
        .expect("send_request timeout")
        .expect("hyper send_request");

    let (parts, body) = resp.into_parts();
    let status = parts.status.as_u16();
    let header_map = parts
        .headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_lowercase(),
                v.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    let bytes = body.collect().await.unwrap().to_bytes();
    (
        status,
        header_map,
        String::from_utf8_lossy(&bytes).to_string(),
    )
}

/// A router whose `/boom` handler panics, `/logs` handler emits a
/// `tracing` event, plus a healthy `/ok` route.
fn router() -> Router {
    Router::new()
        .get("/ok", |_req| async { text("ok") })
        .get("/boom", |_req| async move {
            panic!("intentional handler panic for the request-id echo test");
            #[allow(unreachable_code)]
            text("unreachable")
        })
        .get("/logs", |_req| async {
            // Deliberately does NOT mention any request id — if the id
            // shows up in this event's captured output, it can only have
            // come from the surrounding `request` span context.
            tracing::info!(target: "span_probe", "handler executed");
            text("logged")
        })
        .into()
}

/// A panicking handler is caught by `execute_chain_safely` and converted
/// to a 500 OUTSIDE the `RequestIdMiddleware` scope (the unwind tore the
/// scope down). The synthesized 500 must still echo the inbound
/// `X-Request-Id` so an operator can correlate the client-visible error
/// with the structured panic log. Before the single-source-id fix the
/// header was absent on this path.
#[tokio::test]
async fn panic_response_still_echoes_inbound_request_id() {
    let addr = spawn_server(router(), MiddlewareRegistry::new(), 1).await;

    let (status, headers, _body) = request(
        addr,
        "GET",
        "/boom",
        &[("X-Request-Id", "panic-echo-correlation-id-0001")],
    )
    .await;

    assert_eq!(status, 500, "a panicking handler must surface as a 500");
    assert_eq!(
        headers.get("x-request-id").map(String::as_str),
        Some("panic-echo-correlation-id-0001"),
        "the synthesized panic 500 must echo the inbound X-Request-Id"
    );
}

/// A panic without an inbound id must still carry SOME echoed
/// `X-Request-Id` (a fresh one), so every panic 500 is correlatable.
#[tokio::test]
async fn panic_response_echoes_a_fresh_request_id_when_none_supplied() {
    let addr = spawn_server(router(), MiddlewareRegistry::new(), 1).await;

    let (status, headers, _body) = request(addr, "GET", "/boom", &[]).await;

    assert_eq!(status, 500);
    let echoed = headers
        .get("x-request-id")
        .expect("panic 500 must carry a fresh X-Request-Id even with no inbound id");
    // Fresh ids are lowercase hyphenated UUID v4 (36 chars, 4 dashes).
    assert_eq!(echoed.len(), 36, "fresh id should be a UUID v4");
    assert_eq!(echoed.chars().filter(|c| *c == '-').count(), 4);
}

/// The HIGH fix: `RequestIdMiddleware` enters a `request` span carrying
/// `request_id`, so a downstream handler's `tracing` event inherits the
/// id as span context even though the event itself never mentions it.
/// `logs_contain` matches against the formatted output, which includes
/// the span-field prefix — so a hit proves the id propagated via the
/// span, not via the event. Without `.instrument(span)` the id would be
/// absent from the handler's log line.
#[tokio::test]
#[traced_test]
async fn downstream_events_inherit_request_id_via_request_span() {
    let addr = spawn_server(router(), MiddlewareRegistry::new(), 1).await;

    let (status, _headers, _body) = request(
        addr,
        "GET",
        "/logs",
        &[("X-Request-Id", "span-context-probe-id-4242")],
    )
    .await;
    assert_eq!(status, 200);

    assert!(
        logs_contain("span-context-probe-id-4242"),
        "the /logs handler event must carry request_id from the request span context"
    );
}
