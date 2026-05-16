//! End-to-end tests for the dogfood Inertia pagination route.
//!
//! Spins up a one-shot hyper server that mounts the app's full route
//! tree via the framework's `handle_request` adapter, then drives real
//! HTTP requests with a hyper client. Covers both branches of the
//! controller:
//! - default Inertia path → `Inertia::paginate("Users/Index", "users", ...)`
//!   → JSON page object with `props.users` (data + scroll metadata).
//! - `?format=json` path → raw paginator JSON.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

use suprnova::{handle_request, MiddlewareRegistry};

/// Spawn a one-shot hyper server that serves the app's router for a
/// configurable number of inbound connections. Returns the bound
/// address. The accept loop terminates once the per-test budget is
/// drained.
async fn spawn_app_server(max_connections: usize) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = Arc::new(app::routes::register());
    let middleware = Arc::new(MiddlewareRegistry::new());

    tokio::spawn(async move {
        for _ in 0..max_connections {
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
                    async move {
                        Ok::<_, Infallible>(handle_request(router, middleware, req).await)
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await;
            });
        }
    });

    addr
}

/// Send a GET to `path` against `addr`. `inertia_headers=true` sets
/// `X-Inertia: true` + `Accept: text/html, application/xhtml+xml`,
/// matching what the Inertia client sends after the initial visit.
async fn get(
    addr: SocketAddr,
    path: &str,
    inertia_headers: bool,
) -> (hyper::http::StatusCode, hyper::HeaderMap, Bytes) {
    let stream_tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream_tcp);
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(io)
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut builder = hyper::Request::builder()
        .method("GET")
        .uri(path)
        .header("Host", "localhost");
    if inertia_headers {
        builder = builder
            .header("X-Inertia", "true")
            .header("X-Inertia-Version", "test-version")
            .header("Accept", "text/html, application/xhtml+xml");
    }
    let req = builder.body(Empty::<Bytes>::new()).unwrap();

    let resp = sender.send_request(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    let collected = body.collect().await.unwrap();
    (parts.status, parts.headers, collected.to_bytes())
}

#[tokio::test]
async fn inertia_path_emits_users_prop_and_scroll_metadata() {
    let addr = spawn_app_server(2).await;

    // Request 1: as an Inertia XHR (X-Inertia: true) — expect JSON page object.
    let (status, headers, body) = get(addr, "/api/users?per_page=20", true).await;
    assert_eq!(status.as_u16(), 200, "Inertia route should 200");
    // X-Inertia echo confirms the response came from the Inertia builder.
    let ct = headers
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.contains("application/json"),
        "Inertia XHR response should be JSON, got: {ct}"
    );

    let v: serde_json::Value =
        serde_json::from_slice(&body).expect("response should be JSON-parseable");
    // Page object shape: `component`, `props`, `url`, `version`.
    assert_eq!(
        v.get("component").and_then(|c| c.as_str()),
        Some("Users/Index"),
        "expected component 'Users/Index' in page object: {v}"
    );
    let users = v
        .get("props")
        .and_then(|p| p.get("users"))
        .expect("props.users must be present");
    let arr = users.as_array().expect("props.users is the rows array");
    assert_eq!(arr.len(), 20, "first page returns 20 rows by default");
    // First and last row IDs sanity.
    assert_eq!(arr.first().and_then(|r| r.get("id")), Some(&serde_json::json!(1)));
    assert_eq!(arr.last().and_then(|r| r.get("id")), Some(&serde_json::json!(20)));

    // Scroll metadata: the Inertia v3 protocol attaches scroll info
    // under `scrollProps.<key>`. Confirm `next` cursor is set (we have
    // more rows) and `previous` is None (first page).
    let scroll = v
        .get("scrollProps")
        .expect("scrollProps must be present (paginator was wired via Inertia::paginate)");
    let users_scroll = scroll
        .get("users")
        .expect("scrollProps.users must be present");
    assert_eq!(
        users_scroll.get("pageName").and_then(|p| p.as_str()),
        Some("cursor"),
        "page_name should be 'cursor' for CursorPaginator"
    );
    let next = users_scroll
        .get("next")
        .or_else(|| users_scroll.get("nextPage"));
    assert!(
        next.is_some() && !next.unwrap().is_null(),
        "next cursor must be set (more rows remain): {users_scroll:?}"
    );
    let prev = users_scroll
        .get("previous")
        .or_else(|| users_scroll.get("previousPage"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    assert!(prev.is_null(), "first page must have no prev_cursor: {prev:?}");
}

#[tokio::test]
async fn json_fallback_returns_raw_paginator() {
    let addr = spawn_app_server(1).await;
    let (status, headers, body) = get(addr, "/api/users?per_page=5&format=json", false).await;
    assert_eq!(status.as_u16(), 200);
    let ct = headers
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("application/json"), "expected JSON, got {ct}");

    let v: serde_json::Value =
        serde_json::from_slice(&body).expect("response should be JSON-parseable");
    let arr = v["data"].as_array().expect("data must be an array");
    assert_eq!(arr.len(), 5);
    assert_eq!(arr[0]["id"], 1);
    assert_eq!(arr[4]["id"], 5);
    assert_eq!(v["meta"]["page_name"], "cursor");
    assert!(v["meta"]["next"].is_string(), "next cursor must be set");
}
