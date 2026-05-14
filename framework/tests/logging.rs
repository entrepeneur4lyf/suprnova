//! End-to-end tests for the logging subsystem (RequestIdMiddleware,
//! request-id propagation, etc.).
//!
//! `hyper::body::Incoming` isn't constructible outside hyper, so these
//! tests bind a one-shot TCP listener, register the middleware around a
//! handler, and send a real HTTP request through a hyper client.

use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use suprnova::async_trait;
use suprnova::http::{HttpResponse, Request, Response};
use suprnova::logging::{current_request_id, RequestIdMiddleware};
use suprnova::middleware::{into_boxed, MiddlewareChain, Middleware, Next};

/// A capture middleware that records the request id observed inside
/// the chain (after RequestIdMiddleware has scoped it).
struct CaptureRequestId(Arc<Mutex<Option<String>>>);

#[async_trait]
impl Middleware for CaptureRequestId {
    async fn handle(&self, request: Request, next: Next) -> Response {
        if let Some(id) = current_request_id() {
            *self.0.lock().unwrap() = Some(id.as_str().to_string());
        }
        next(request).await
    }
}

/// A capture middleware that records `Context::get("_request_id")` —
/// confirms `RequestIdMiddleware` seeds the Context bag with the
/// per-request id under the conventional key.
struct CaptureContextRequestId(Arc<Mutex<Option<String>>>);

#[async_trait]
impl Middleware for CaptureContextRequestId {
    async fn handle(&self, request: Request, next: Next) -> Response {
        *self.0.lock().unwrap() = suprnova::Context::get::<String>("_request_id");
        next(request).await
    }
}

/// Spawn a one-shot server with `RequestIdMiddleware` installed
/// outermost, followed by a `CaptureRequestId` that records the
/// observed id, and an inner handler that returns 200 "ok".
async fn spawn_with_request_id(captured: Arc<Mutex<Option<String>>>) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let io = TokioIo::new(stream);
            let svc = service_fn(move |hyper_req: hyper::Request<hyper::body::Incoming>| {
                let captured = captured.clone();
                async move {
                    let req = Request::new(hyper_req);

                    let mut chain = MiddlewareChain::new();
                    chain.push(into_boxed(RequestIdMiddleware));
                    chain.push(into_boxed(CaptureRequestId(captured)));

                    // Innermost handler: always returns 200 "ok".
                    let handler: Arc<suprnova::routing::BoxedHandler> = Arc::new(Box::new(
                        move |_req: Request| -> suprnova::middleware::MiddlewareFuture {
                            Box::pin(async move { Ok(HttpResponse::text("ok")) })
                        },
                    ));

                    let resp = chain.execute(req, handler).await;
                    let hyper_resp = match resp {
                        Ok(r) => r.into_hyper(),
                        Err(r) => r.into_hyper(),
                    };
                    Ok::<_, Infallible>(hyper_resp)
                }
            });
            let _ = http1::Builder::new().serve_connection(io, svc).await;
        }
    });
    addr
}

async fn get(addr: SocketAddr, headers: &[(&str, &str)]) -> hyper::Response<Bytes> {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(io)
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut builder = hyper::Request::builder()
        .method("GET")
        .uri("http://localhost/");
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let req = builder.body(Empty::<Bytes>::new()).unwrap();
    let resp = sender.send_request(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    let collected = body.collect().await.unwrap();
    hyper::Response::from_parts(parts, collected.to_bytes())
}

#[tokio::test]
async fn middleware_generates_and_echoes_a_fresh_request_id() {
    let captured = Arc::new(Mutex::new(None::<String>));
    let addr = spawn_with_request_id(captured.clone()).await;

    let resp = get(addr, &[]).await;

    assert_eq!(resp.status(), 200);

    let echoed = resp
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .expect("X-Request-Id header echoed on response")
        .to_string();

    // Lowercase hyphenated UUID v4 = 36 chars with 4 dashes.
    assert_eq!(echoed.len(), 36);
    assert_eq!(echoed.chars().filter(|c| *c == '-').count(), 4);

    // The downstream handler observed the same id during request execution.
    let observed = captured
        .lock()
        .unwrap()
        .clone()
        .expect("handler ran with scoped request id");
    assert_eq!(observed, echoed);
}

#[tokio::test]
async fn middleware_reuses_inbound_x_request_id_header() {
    let captured = Arc::new(Mutex::new(None::<String>));
    let addr = spawn_with_request_id(captured.clone()).await;

    let resp = get(
        addr,
        &[("X-Request-Id", "test-inbound-id-12345-67890-abcdef-001122")],
    )
    .await;

    assert_eq!(resp.status(), 200);

    let echoed = resp
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .expect("X-Request-Id header echoed on response")
        .to_string();
    assert_eq!(echoed, "test-inbound-id-12345-67890-abcdef-001122");

    let observed = captured.lock().unwrap().clone().unwrap();
    assert_eq!(observed, "test-inbound-id-12345-67890-abcdef-001122");
}

#[tokio::test]
async fn middleware_seeds_request_id_into_context_bag() {
    // Spawn a one-shot server with RequestIdMiddleware + a CaptureContextRequestId
    // middleware that reads Context::get("_request_id") inside the chain.
    let captured = Arc::new(Mutex::new(None::<String>));
    let captured_for_handler = captured.clone();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let io = TokioIo::new(stream);
            let svc = service_fn(move |hyper_req: hyper::Request<hyper::body::Incoming>| {
                let captured = captured_for_handler.clone();
                async move {
                    let req = Request::new(hyper_req);

                    let mut chain = MiddlewareChain::new();
                    chain.push(into_boxed(RequestIdMiddleware));
                    chain.push(into_boxed(CaptureContextRequestId(captured)));

                    let handler: Arc<suprnova::routing::BoxedHandler> = Arc::new(Box::new(
                        move |_req: Request| -> suprnova::middleware::MiddlewareFuture {
                            Box::pin(async move { Ok(HttpResponse::text("ok")) })
                        },
                    ));

                    let resp = chain.execute(req, handler).await;
                    let hyper_resp = match resp {
                        Ok(r) => r.into_hyper(),
                        Err(r) => r.into_hyper(),
                    };
                    Ok::<_, Infallible>(hyper_resp)
                }
            });
            let _ = http1::Builder::new().serve_connection(io, svc).await;
        }
    });

    let resp = get(addr, &[("X-Request-Id", "abc-context-seed-id-1234567890123456")]).await;
    assert_eq!(resp.status(), 200);

    let echoed = resp
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .expect("X-Request-Id header echoed on response");
    assert_eq!(echoed, "abc-context-seed-id-1234567890123456");

    let from_context = captured.lock().unwrap().clone();
    assert_eq!(
        from_context.as_deref(),
        Some("abc-context-seed-id-1234567890123456"),
        "Context::get(\"_request_id\") must return the middleware-seeded id"
    );
}
