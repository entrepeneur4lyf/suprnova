//! Integration tests for [`TwoFactorChallengeMiddleware`].
//!
//! Two moving parts compose here: (a) a `WithSession` upstream
//! middleware that installs a session task-local for the request
//! (the production install lives in `SessionMiddleware`, but we
//! don't want a real session driver in the test); (b) the actual
//! `TwoFactorChallengeMiddleware`. The chain is:
//!
//! ```text
//! WithSession{maybe-pending} → TwoFactorChallengeMiddleware → handler
//! ```
//!
//! `WithSession` pre-populates the session's pending slot before
//! installing the task-local scope, so when
//! `TwoFactorChallengeMiddleware` reads
//! `TwoFactor::pending_user_id()` it sees the slot the test set up.

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
use suprnova::{
    Middleware, MiddlewareRegistry, Next, Request, Response, Router, TwoFactorChallengeMiddleware,
    handle_request,
};

/// Test-only upstream middleware that installs a session task-local
/// for the request. Optionally pre-populates the pending slot.
struct WithSession {
    pending_user_id: Option<String>,
}

#[async_trait::async_trait]
impl Middleware for WithSession {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let slot = suprnova::session::new_session_slot_for_test();
        if let Some(uid) = &self.pending_user_id {
            // Pre-populate the slot BEFORE installing the scope —
            // that's how `TwoFactorChallengeMiddleware` will observe
            // a pending session.
            let mut guard = slot.lock().unwrap();
            if let Some(ref mut session) = *guard {
                session.data.insert(
                    "_two_factor_pending_user_id".to_string(),
                    serde_json::Value::String(uid.clone()),
                );
            }
            drop(guard);
        }
        suprnova::session::session_scope_for_test(slot, async move { next(request).await }).await
    }
}

fn router() -> Router {
    Router::new()
        .get("/protected", |_req| async { text("reached") })
        .into()
}

async fn spawn(registry: MiddlewareRegistry, accepts: usize) -> SocketAddr {
    let router = Arc::new(router());
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

async fn get(addr: SocketAddr) -> (u16, HashMap<String, String>, String) {
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
        .uri("/protected")
        .header("Host", "localhost")
        .header("Content-Length", "0")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = tokio::time::timeout(Duration::from_secs(5), sender.send_request(req))
        .await
        .expect("send_request timeout")
        .expect("hyper send_request");

    let (parts, body) = resp.into_parts();
    let status = parts.status.as_u16();
    let headers = parts
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
    (status, headers, String::from_utf8_lossy(&bytes).to_string())
}

#[tokio::test]
async fn pending_session_gets_403_in_api_form() {
    let registry = MiddlewareRegistry::new()
        .append(WithSession {
            pending_user_id: Some("user-pending".into()),
        })
        .append(TwoFactorChallengeMiddleware::new());
    let addr = spawn(registry, 1).await;

    let (status, _headers, body) = get(addr).await;

    assert_eq!(status, 403);
    assert!(
        body.contains("Two-factor authentication challenge pending"),
        "403 body must mention pending challenge; got: {body}"
    );
}

#[tokio::test]
async fn pending_session_redirects_in_web_form() {
    let registry = MiddlewareRegistry::new()
        .append(WithSession {
            pending_user_id: Some("user-pending".into()),
        })
        .append(TwoFactorChallengeMiddleware::redirect_to(
            "/two-factor-challenge",
        ));
    let addr = spawn(registry, 1).await;

    let (status, headers, _body) = get(addr).await;

    assert_eq!(status, 302);
    assert_eq!(
        headers.get("location").map(String::as_str),
        Some("/two-factor-challenge"),
        "redirect target must match the configured path"
    );
}

#[tokio::test]
async fn non_pending_session_passes_through() {
    let registry = MiddlewareRegistry::new()
        .append(WithSession {
            pending_user_id: None, // no pending state
        })
        .append(TwoFactorChallengeMiddleware::new());
    let addr = spawn(registry, 1).await;

    let (status, _headers, body) = get(addr).await;

    assert_eq!(
        status, 200,
        "no-pending session must pass through the middleware unchanged"
    );
    assert_eq!(body, "reached");
}
