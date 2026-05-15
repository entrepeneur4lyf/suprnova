//! Integration tests for `IncludeMiddleware`.
//!
//! `hyper::body::Incoming` cannot be constructed outside hyper, so we
//! follow the same pattern used in `framework/tests/inertia.rs`: boot a
//! minimal one-shot TCP server, send a real HTTP request through a hyper
//! client, and capture the observed `current_include_set()` value via a
//! shared `Arc<Mutex<Option<RequestIncludeSet>>>` so we can assert on it
//! after the round-trip completes.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

use suprnova::data::{current_include_set, IncludeMiddleware, RequestIncludeSet};
use suprnova::middleware::Middleware;
use suprnova::{HttpResponse, Next, Request};

/// Boot a one-shot HTTP server that wraps `IncludeMiddleware`, captures the
/// `current_include_set()` value seen inside the handler into `captured`, and
/// sends back a plain 200. Returns the bound address.
async fn drive(
    uri: &'static str,
    captured: Arc<Mutex<Option<RequestIncludeSet>>>,
) -> hyper::Response<Bytes> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();

    let mw = Arc::new(IncludeMiddleware);

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let io = TokioIo::new(stream);
        let mw = mw.clone();
        let service = service_fn(move |hyper_req: hyper::Request<hyper::body::Incoming>| {
            let mw = mw.clone();
            let captured = captured.clone();
            async move {
                let req = Request::new(hyper_req);

                // Build a Next that captures the current_include_set() value
                // then returns 200. Do NOT assert inside the closure —
                // a panic here terminates the connection and produces an
                // unclean test failure rather than a proper assertion error.
                let next: Next = Arc::new(move |_req: Request| {
                    let captured = captured.clone();
                    Box::pin(async move {
                        let set = current_include_set();
                        *captured.lock().unwrap() = Some((*set).clone());
                        Ok(HttpResponse::text("ok"))
                    })
                });

                let response = mw.handle(req, next).await;
                let http = response.unwrap_or_else(|e| e);
                Ok::<_, Infallible>(http.into_hyper())
            }
        });
        http1::Builder::new()
            .serve_connection(io, service)
            .await
            .ok();
    });

    // Send a real HTTP GET to the server.
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method("GET")
        .uri(uri)
        .body(Empty::<Bytes>::new())
        .unwrap();

    let resp = sender.send_request(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    let body_bytes = body.collect().await.unwrap().to_bytes();
    hyper::Response::from_parts(parts, body_bytes)
}

#[tokio::test]
async fn middleware_binds_include_set_for_handler() {
    let captured: Arc<Mutex<Option<RequestIncludeSet>>> = Arc::new(Mutex::new(None));
    let resp = drive("http://localhost/items?include=author,tags&only=id", captured.clone()).await;

    assert_eq!(resp.status(), 200);

    let set = captured.lock().unwrap().take().expect("next was not called");
    assert_eq!(set.include, vec!["author", "tags"]);
    assert_eq!(set.only.as_deref(), Some(["id".to_string()].as_slice()));
    assert!(set.exclude.is_empty());
    assert!(set.except.is_empty());
}

#[tokio::test]
async fn middleware_passes_through_empty_set() {
    let captured: Arc<Mutex<Option<RequestIncludeSet>>> = Arc::new(Mutex::new(None));
    let resp = drive("http://localhost/items", captured.clone()).await;

    assert_eq!(resp.status(), 200);

    let set = captured.lock().unwrap().take().expect("next was not called");
    assert!(set.is_empty(), "expected empty set for request with no query string, got {set:?}");
}

#[tokio::test]
async fn middleware_parses_array_form_via_hyper() {
    // Mirrors the integration path: hyper request → Request::query() →
    // RequestIncludeSet::from_query — confirms `?include[]=a&include[]=b`
    // accumulates correctly when the value flows through the actual
    // server stack.
    let captured: Arc<Mutex<Option<RequestIncludeSet>>> = Arc::new(Mutex::new(None));
    let resp = drive(
        "http://localhost/items?include[]=author&include[]=tags",
        captured.clone(),
    )
    .await;

    assert_eq!(resp.status(), 200);

    let set = captured.lock().unwrap().take().expect("next was not called");
    assert_eq!(set.include, vec!["author", "tags"]);
    assert!(set.exclude.is_empty());
    assert!(set.only.is_none());
    assert!(set.except.is_empty());
}
