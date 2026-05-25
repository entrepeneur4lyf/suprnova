//! Regression: HIGH audit finding `error` #1 — request panics used to
//! short-circuit the framework's standard error-response pipeline.
//! `execute_chain_safely` would catch the panic, log it via `tracing`,
//! and return `HttpResponse::text("Internal Server Error").status(500)`
//! — a plain-text body with no `request_id`, no JSON shape, and no
//! `ErrorOccurred` event dispatch. That meant a panic in production
//! produced a different response contract from any returned 5xx error,
//! and observability listeners that watched 5xx error events (Sentry
//! shippers, custom dashboards) never saw the panic.
//!
//! Fix: route the panic through the same `FrameworkError ->
//! HttpResponse` conversion that handles returned 5xx errors. Panics
//! now produce:
//!   - The sanitised `{"message":"Internal Server Error", ...}` JSON
//!     body (panic payload stays in tracing logs, NEVER in the wire).
//!   - `request_id` injection so clients and operators can correlate.
//!   - `ErrorOccurred` event dispatch — listeners that fire on 5xx
//!     errors now also fire on panics.
//!
//! These tests assert the full contract via a real hyper TCP server.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use suprnova::events::{ErrorOccurred, EventFacade, Listener};
use suprnova::http::text;
use suprnova::{
    handle_request, FrameworkError, MiddlewareRegistry, Request, Router,
};

static ERROR_EVENT_FIRED: AtomicUsize = AtomicUsize::new(0);

struct CountErrorOccurred;

#[async_trait]
impl Listener<ErrorOccurred> for CountErrorOccurred {
    async fn handle(&self, _event: &ErrorOccurred) -> Result<(), FrameworkError> {
        ERROR_EVENT_FIRED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

async fn spawn_server(router: impl Into<Router>, accepts: usize) -> SocketAddr {
    let router = Arc::new(router.into());
    let middleware = Arc::new(MiddlewareRegistry::new());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

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
                    async move {
                        Ok::<_, Infallible>(
                            handle_request(router, middleware, req).await,
                        )
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
        .expect("timeout")
        .expect("send_request");
    let (parts, body) = resp.into_parts();
    let collected = body.collect().await.unwrap().to_bytes();
    (parts.status, collected)
}

#[tokio::test]
async fn panicking_handler_emits_json_body_with_sanitised_message() {
    let router = Router::new().get("/panic-json", |_req: Request| async {
        panic!("DB connection refused at 127.0.0.1:5432 — should NOT leak to wire");
        #[allow(unreachable_code)]
        text("unreachable")
    });

    let addr = spawn_server(router, 2).await;
    let (status, body) = send_get(addr, "/panic-json").await;
    assert_eq!(status.as_u16(), 500);

    let parsed: serde_json::Value = serde_json::from_slice(&body).expect(
        "panic response must be JSON via the FrameworkError -> HttpResponse path",
    );
    assert_eq!(
        parsed["message"], "Internal Server Error",
        "wire body MUST use the sanitised `message` field; the raw \
         panic payload must never appear in `message`"
    );
    // The `debug_message` field is only populated when APP_DEBUG=true
    // and is allowed to carry the panic detail for development
    // visibility (frontends MUST NOT key on `debug_message`). The
    // `message` field stays generic in both modes — that's the
    // contract this test enforces.
    let msg_value = parsed["message"].as_str().unwrap_or("");
    assert!(
        !msg_value.contains("connection refused"),
        "`message` field must never carry the raw panic detail; got: {msg_value}"
    );
    assert!(
        parsed.get("request_id").is_some(),
        "panic body must carry a request_id key (null when no active scope)"
    );
}

#[tokio::test]
async fn panicking_handler_dispatches_error_occurred_event() {
    // Audit's load-bearing concern: observability listeners (Sentry,
    // Pagerduty, custom log shippers) registered for `ErrorOccurred`
    // events must see panics, not just returned 5xx errors.
    ERROR_EVENT_FIRED.store(0, Ordering::SeqCst);
    EventFacade::listen::<ErrorOccurred, _>(Arc::new(CountErrorOccurred)).await;

    let router = Router::new().get("/panic-event", |_req: Request| async {
        panic!("intentional test panic — must fire ErrorOccurred");
        #[allow(unreachable_code)]
        text("unreachable")
    });

    let addr = spawn_server(router, 2).await;
    let (status, _body) = send_get(addr, "/panic-event").await;
    assert_eq!(status.as_u16(), 500);

    // The From<FrameworkError> for HttpResponse path spawns the
    // dispatch via Handle::try_current — give it a tick to land
    // before we assert.
    for _ in 0..50 {
        if ERROR_EVENT_FIRED.load(Ordering::SeqCst) >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        ERROR_EVENT_FIRED.load(Ordering::SeqCst) >= 1,
        "panic must dispatch ErrorOccurred — listeners that observe 5xx \
         errors otherwise wouldn't see panics in production"
    );
}
