//! Regression: HIGH audit finding `context` #333 — request query
//! parameters are never populated into the context on real HTTP
//! requests.
//!
//! Before the fix, `RequestIdMiddleware` (the only production
//! `CONTEXT.scope` installer) created the scope with
//! `ContextStore::default()` — so the in-scope `query` bag was always
//! empty for real HTTP requests, and `Context::query_param()` returned
//! `None` regardless of the URL's `?key=value` pairs. Downstream
//! consumers (Eloquent pagination, cursor pagination, anything reading
//! `Context::query_param("page")`) silently defaulted as if no query
//! string had been sent.
//!
//! The fix parses `request.query()` via `url::form_urlencoded::parse`
//! and seeds the `ContextStore` via `with_query(...)` so
//! `Context::query_param` returns the real URL values.
//!
//! This test exercises the full real-HTTP path: bind a tokio TCP
//! listener, drive a hyper service that wraps `RequestIdMiddleware`
//! around a handler that captures `Context::query_param`, send a real
//! GET with a query string, and assert the captured values match what
//! we sent.

use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use suprnova::{Context, HttpResponse, Middleware, Next, RequestIdMiddleware};

/// Drive a single request through `RequestIdMiddleware` and a handler
/// that snapshots `Context::query_param` for the listed keys, returning
/// the captured values to the test.
async fn drive_capturing_query_params(
    uri: &str,
    keys: &'static [&'static str],
) -> std::collections::HashMap<&'static str, Option<String>> {
    let captured: Arc<Mutex<std::collections::HashMap<&'static str, Option<String>>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();

    let mw = Arc::new(RequestIdMiddleware);
    let server_captured = captured.clone();

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let io = TokioIo::new(stream);
        let mw = mw.clone();
        let server_captured = server_captured.clone();
        let service = service_fn(move |hyper_req: hyper::Request<hyper::body::Incoming>| {
            let mw = mw.clone();
            let server_captured = server_captured.clone();
            async move {
                let req = suprnova::Request::new(hyper_req);
                let next: Next = Arc::new(move |_inner| {
                    let server_captured = server_captured.clone();
                    Box::pin(async move {
                        // Inside the handler — the middleware should
                        // have populated the context with the URL's
                        // query parameters.
                        let mut map = server_captured.lock().unwrap();
                        for key in keys {
                            map.insert(*key, Context::query_param(key));
                        }
                        Ok(HttpResponse::text("ok"))
                    })
                });
                let response = mw.handle(req, next).await;
                let http = response.unwrap_or_else(|e| e);
                Ok::<_, Infallible>(http.into_hyper())
            }
        });
        let _ = http1::Builder::new().serve_connection(io, service).await;
    });

    // Send a real GET to the in-process server.
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
    let (_parts, body) = resp.into_parts();
    let _ = body.collect().await.unwrap();

    // The spawned server task may still hold an Arc clone — we can't
    // unwrap it. Clone the inner map under the lock instead.
    captured.lock().unwrap().clone()
}

#[tokio::test]
async fn request_id_middleware_populates_context_query_params() {
    let captured =
        drive_capturing_query_params("/users?page=3&cursor=abc", &["page", "cursor", "missing"])
            .await;

    assert_eq!(
        captured.get("page").cloned().flatten(),
        Some("3".to_string()),
        "Context::query_param('page') must reflect the URL's ?page=3"
    );
    assert_eq!(
        captured.get("cursor").cloned().flatten(),
        Some("abc".to_string()),
        "Context::query_param('cursor') must reflect the URL's ?cursor=abc"
    );
    assert_eq!(
        captured.get("missing").cloned().flatten(),
        None,
        "Context::query_param for keys not in the URL must remain None"
    );
}

#[tokio::test]
async fn request_id_middleware_handles_no_query_string() {
    // A URI without `?...` must still scope CONTEXT cleanly; query_param
    // returns None for everything because there are no pairs.
    let captured = drive_capturing_query_params("/users", &["page"]).await;
    assert_eq!(
        captured.get("page").cloned().flatten(),
        None,
        "no query string in URI → query_param('page') must be None"
    );
}

#[tokio::test]
async fn request_id_middleware_url_decodes_query_values() {
    // `?name=hello+world&tag=a%26b` exercises url-decoding: `+` → space,
    // `%26` → `&`. This is what `url::form_urlencoded::parse` should
    // handle for us; the test pins the behavior so downstream callers
    // can rely on receiving decoded values.
    let captured =
        drive_capturing_query_params("/q?name=hello+world&tag=a%26b", &["name", "tag"]).await;

    assert_eq!(
        captured.get("name").cloned().flatten(),
        Some("hello world".to_string()),
        "+ in query value must decode to space"
    );
    assert_eq!(
        captured.get("tag").cloned().flatten(),
        Some("a&b".to_string()),
        "%26 in query value must decode to &"
    );
}
