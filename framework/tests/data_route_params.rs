//! Integration tests for `#[data(from_route_param)]` field injection.
//!
//! Route params are injected into the body map before deserialization, with
//! path params winning over body keys (IDOR protection). The TCP listener
//! pattern is used because `hyper::body::Incoming` cannot be constructed
//! outside hyper — the server's `service_fn` closure sets route params via
//! `Request::with_params(...)` to simulate the routing layer.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

use suprnova::error::FrameworkError;
use suprnova::{FormRequest, HttpResponse, Request};

// ── DTOs ─────────────────────────────────────────────────────────────────────

#[derive(Debug, suprnova::Data, validator::Validate)]
struct UpdateUserDto {
    #[data(from_route_param("id"))]
    pub id: i64,

    #[validate(length(min = 1))]
    pub name: String,
}

#[derive(Debug, suprnova::Data, validator::Validate)]
struct ShortDto {
    #[data(from_route_param)]
    pub slug: String,
}

// ── Generic test infrastructure ───────────────────────────────────────────────

/// Spawns a one-shot server that:
/// 1. Applies `route_params` to the Request before extraction.
/// 2. Calls `T::extract(req)`.
/// 3. Stores the result in the returned `Arc<Mutex<...>>`.
async fn spawn_server<T>(
    route_params: HashMap<String, String>,
) -> (SocketAddr, Arc<Mutex<Option<Result<T, FrameworkError>>>>)
where
    T: FormRequest + Send + 'static,
{
    let captured: Arc<Mutex<Option<Result<T, FrameworkError>>>> = Arc::new(Mutex::new(None));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let captured_server = captured.clone();
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let io = TokioIo::new(stream);
            let captured_svc = captured_server.clone();
            let params = route_params.clone();
            let svc = service_fn(move |hyper_req: hyper::Request<hyper::body::Incoming>| {
                let captured_inner = captured_svc.clone();
                let params_inner = params.clone();
                async move {
                    let req = Request::new(hyper_req).with_params(params_inner);
                    let result = T::extract(req).await;
                    *captured_inner.lock().unwrap() = Some(result);
                    Ok::<_, Infallible>(HttpResponse::text("ok").into_hyper())
                }
            });
            let _ = http1::Builder::new().serve_connection(io, svc).await;
        }
    });

    (addr, captured)
}

async fn patch_json(addr: SocketAddr, body: serde_json::Value) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let body_bytes = serde_json::to_vec(&body).unwrap();
    let req = hyper::Request::builder()
        .method("PATCH")
        .uri("http://localhost/users/42")
        .header("content-type", "application/json")
        .header("content-length", body_bytes.len())
        .body(Full::new(Bytes::from(body_bytes)))
        .unwrap();

    let _ = sender.send_request(req).await;
}

async fn get_request(addr: SocketAddr) {
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
        .uri("http://localhost/posts/hello-world")
        .header("content-type", "application/json")
        .header("content-length", "2")
        .body(Full::new(Bytes::from("{}")))
        .unwrap();

    let _ = sender.send_request(req).await;
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn body_and_route_param_merge() {
    let params = HashMap::from([("id".to_string(), "42".to_string())]);
    let (addr, captured) = spawn_server::<UpdateUserDto>(params).await;
    patch_json(addr, serde_json::json!({"name": "Ada"})).await;

    tokio::task::yield_now().await;

    let result = captured
        .lock()
        .unwrap()
        .take()
        .expect("server did not process request");
    let dto = result.expect("expected Ok, got Err");
    assert_eq!(dto.id, 42);
    assert_eq!(dto.name, "Ada");
}

#[tokio::test]
async fn missing_route_param_returns_400() {
    // No route params at all — "id" is missing.
    let params = HashMap::new();
    let (addr, captured) = spawn_server::<UpdateUserDto>(params).await;
    patch_json(addr, serde_json::json!({"name": "Ada"})).await;

    tokio::task::yield_now().await;

    let result = captured
        .lock()
        .unwrap()
        .take()
        .expect("server did not process request");
    let err = result.expect_err("expected Err for missing route param");
    assert_eq!(
        err.status_code(),
        400,
        "expected 400 Bad Request for missing route param, got {}",
        err.status_code()
    );
}

#[tokio::test]
async fn route_param_overrides_body() {
    // Security property: body sends id=999, route param sends id=42. Route wins.
    let params = HashMap::from([("id".to_string(), "42".to_string())]);
    let (addr, captured) = spawn_server::<UpdateUserDto>(params).await;
    // Body contains id=999 — an IDOR attempt.
    patch_json(addr, serde_json::json!({"id": 999, "name": "Ada"})).await;

    tokio::task::yield_now().await;

    let result = captured
        .lock()
        .unwrap()
        .take()
        .expect("server did not process request");
    let dto = result.expect("expected Ok, got Err");
    assert_eq!(dto.id, 42, "route param must win over body to prevent IDOR");
    assert_eq!(dto.name, "Ada");
}

#[tokio::test]
async fn defaults_to_field_name_when_attribute_arg_omitted() {
    let params = HashMap::from([("slug".to_string(), "hello-world".to_string())]);
    let (addr, captured) = spawn_server::<ShortDto>(params).await;
    get_request(addr).await;

    tokio::task::yield_now().await;

    let result = captured
        .lock()
        .unwrap()
        .take()
        .expect("server did not process request");
    let dto = result.expect("expected Ok, got Err");
    assert_eq!(dto.slug, "hello-world");
}
