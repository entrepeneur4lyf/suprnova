//! Regression: HIGH audit finding `data` #336 — completeness pass.
//!
//! The original fix called `Self::max_body_bytes()` from the inlined-
//! lifecycle `FormRequest::extract` arm, so per-DTO body-cap overrides
//! were nominally honored. BUT: there was no way for a user to actually
//! override `max_body_bytes` — the derive emitted the FormRequest impl
//! itself, which made a manual `impl FormRequest for Dto` conflict.
//!
//! `#[data(max_body_bytes = N)]` closes that gap: it tells the derive
//! to emit `fn max_body_bytes() -> usize { N }` as part of the
//! FormRequest impl, so the override propagates everywhere the trait
//! method is consulted — both the route-param-aware extract arm AND
//! the default no-route-param arm.
//!
//! These tests prove:
//!   1. A DTO with `#[data(max_body_bytes = N)]` and no route-param
//!      fields rejects a body larger than N (default-extract arm).
//!   2. A DTO with `#[data(max_body_bytes = N)]` AND a route-param
//!      field rejects a body larger than N (inlined-lifecycle arm).
//!   3. A body at the cap is still accepted in the no-route-param arm
//!      (off-by-one guard).
//!
//! 413 PayloadTooLarge is the cap-exceeded signal — see
//! `framework/src/http/body.rs::over_limit`.

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

// Override is 256 bytes — small enough that a 1 KiB body trips it but
// well clear of the framework's default 64 MiB cap (so the override is
// the load-bearing signal, not the global default).
const TINY_CAP: usize = 256;

// Arm 1: no route-param field — emits the simple FormRequest impl
// arm in `build_form_request`.
#[derive(Debug, suprnova::Data, validator::Validate)]
#[data(max_body_bytes = 256)]
struct SmallBodyDto {
    #[validate(length(min = 1))]
    pub name: String,
}

// Arm 2: has a route-param field — emits the inlined-lifecycle arm,
// which calls `Self::max_body_bytes()` inside `body_bytes_with_cap`.
#[derive(Debug, suprnova::Data, validator::Validate)]
#[data(max_body_bytes = 256)]
struct SmallBodyRouteParamDto {
    #[data(from_route_param("id"))]
    pub id: i64,

    #[validate(length(min = 1))]
    pub name: String,
}

async fn spawn_extracting<T>(
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

async fn send_json(addr: SocketAddr, body: Vec<u8>) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method("PATCH")
        .uri("http://localhost/items/1")
        .header("content-type", "application/json")
        .header("content-length", body.len())
        .body(Full::new(Bytes::from(body)))
        .unwrap();

    let _ = sender.send_request(req).await;
}

async fn wait_capture<T>(
    captured: Arc<Mutex<Option<Result<T, FrameworkError>>>>,
) -> Result<T, FrameworkError> {
    for _ in 0..50 {
        if let Some(r) = captured.lock().unwrap().take() {
            return r;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("server task never captured a result");
}

fn body_with_padding(min_size: usize) -> Vec<u8> {
    // Build a valid JSON object whose serialized size exceeds `min_size`.
    // Padding is a single big field; the JSON parser still has a chance
    // to deserialize it if the cap doesn't bite first.
    let padding = "x".repeat(min_size);
    format!(r#"{{"name": "{padding}"}}"#).into_bytes()
}

#[tokio::test]
async fn override_rejects_oversized_body_in_simple_arm() {
    // No route-param field → `build_form_request` emits the simple
    // FormRequest impl arm. The override `fn max_body_bytes() -> usize { 256 }`
    // shadows the trait default, so the default `extract` path
    // (which calls `Self::max_body_bytes()`) rejects bodies > 256.
    let (addr, captured) = spawn_extracting::<SmallBodyDto>(HashMap::new()).await;

    let body = body_with_padding(1024); // ≫ 256 byte cap
    assert!(
        body.len() > TINY_CAP,
        "test body must exceed cap: len={}, cap={}",
        body.len(),
        TINY_CAP
    );
    send_json(addr, body).await;

    let err = wait_capture(captured)
        .await
        .expect_err("oversized body must be rejected under the override");
    assert_eq!(
        err.status_code(),
        413,
        "max_body_bytes override must produce 413 PayloadTooLarge, got: {err}"
    );
    let msg = format!("{err}");
    assert!(
        msg.contains(&TINY_CAP.to_string()),
        "error must cite the override cap ({TINY_CAP}); got: {msg}"
    );
}

#[tokio::test]
async fn override_rejects_oversized_body_in_inlined_lifecycle_arm() {
    // Has a route-param field → `build_form_request` emits the
    // inlined-lifecycle arm. The emitted code calls
    // `body_bytes_with_cap(Self::max_body_bytes())`, so the override
    // applies here too. This closes the audit gap that originally
    // motivated `#[data(max_body_bytes = N)]`.
    let mut params = HashMap::new();
    params.insert("id".to_string(), "1".to_string());
    let (addr, captured) = spawn_extracting::<SmallBodyRouteParamDto>(params).await;

    let body = body_with_padding(1024);
    send_json(addr, body).await;

    let err = wait_capture(captured)
        .await
        .expect_err("oversized body must be rejected under the override (inlined arm)");
    assert_eq!(
        err.status_code(),
        413,
        "inlined-lifecycle arm must honor max_body_bytes override; got: {err}"
    );
    let msg = format!("{err}");
    assert!(
        msg.contains(&TINY_CAP.to_string()),
        "error must cite the override cap ({TINY_CAP}); got: {msg}"
    );
}

#[tokio::test]
async fn body_under_override_cap_is_accepted_in_simple_arm() {
    // Off-by-one guard: a body well under the cap must still succeed.
    // If the override was emitted as `usize::MAX` or `0` due to a
    // macro-side casting bug, this would either always pass (no cap
    // ever bites) or always fail (cap bites immediately).
    let (addr, captured) = spawn_extracting::<SmallBodyDto>(HashMap::new()).await;

    let body = br#"{"name":"Alice"}"#.to_vec();
    assert!(body.len() < TINY_CAP);
    send_json(addr, body).await;

    let dto = wait_capture(captured)
        .await
        .expect("body well under the cap must succeed");
    assert_eq!(dto.name, "Alice");
}
