//! Regression: HIGH audit finding `data` #336 — `#[data(from_route_param)]`
//! DTOs did not run the full `FormRequest` lifecycle.
//!
//! Before the fix, the macro emitted a custom `extract` for any DTO
//! with a `from_route_param` field that bypassed:
//!   - Form-urlencoded body parsing (everything went through
//!     `serde_json::from_slice`, so a form POST became "malformed JSON")
//!   - Non-object JSON rejection (arrays/strings/null silently became
//!     `{}` via `.unwrap_or_default()` and either passed via defaults
//!     or failed with confusing serde messages)
//!   - Precognition envelope handling
//!   - `body_bytes_with_cap` (used the uncapped `body_bytes` instead)
//!   - `after_validation()` cross-field hook
//!
//! The fix inlines the full default `FormRequest::extract` lifecycle in
//! the macro's custom path, with one extra step: route params are
//! injected into the parsed body map before deserialization (path wins).
//!
//! These tests demonstrate the lifecycle is now wired:
//!   - Form-urlencoded body extracts cleanly.
//!   - Non-object JSON body is rejected with a clear 400.
//!   - A `Precognition: true` header short-circuits to 204
//!     (PrecognitionSuccess) rather than completing normally.
//!
//! The macro emits the `FormRequest` impl unconditionally for DTOs with
//! route-param fields, so per-struct `max_body_bytes` /
//! `after_validation` overrides can't be tested via a manual impl
//! without colliding. The lifecycle-call-through-to-trait-defaults
//! shown here is the load-bearing demonstration; the overrides
//! themselves are exercised by the default-extract path's existing
//! tests, and the route-param path's emitted code uses the same trait
//! method calls (`Self::max_body_bytes()` / `dto.after_validation()`).

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

// DTO with one route-param field and a form-shaped body field. Both
// content-type branches must work.
#[derive(Debug, suprnova::Data, validator::Validate)]
struct UpdateProfileDto {
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

async fn send(
    addr: SocketAddr,
    content_type: &'static str,
    body: Vec<u8>,
    extra_header: Option<(&'static str, &'static str)>,
) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut b = hyper::Request::builder()
        .method("PATCH")
        .uri("http://localhost/profiles/42")
        .header("content-type", content_type)
        .header("content-length", body.len());
    if let Some((k, v)) = extra_header {
        b = b.header(k, v);
    }
    let req = b.body(Full::new(Bytes::from(body))).unwrap();

    let _ = sender.send_request(req).await;
}

fn route_params() -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("id".to_string(), "42".to_string());
    m
}

async fn wait_capture<T>(
    captured: Arc<Mutex<Option<Result<T, FrameworkError>>>>,
) -> Result<T, FrameworkError> {
    // Poll briefly for the server task to capture; avoids hard-coded sleeps.
    for _ in 0..50 {
        if let Some(r) = captured.lock().unwrap().take() {
            return r;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("server task never captured a result");
}

#[tokio::test]
async fn form_urlencoded_body_parses_when_route_param_present() {
    // Before the fix: form-urlencoded bodies were fed to
    // `serde_json::from_slice`, which choked on the `&`-delimited
    // shape. Post-fix: form-urlencoded is parsed into a map and
    // merged with the route param.
    let (addr, captured) = spawn_extracting::<UpdateProfileDto>(route_params()).await;

    let body = b"name=Alice".to_vec();
    send(addr, "application/x-www-form-urlencoded", body, None).await;

    let dto = wait_capture(captured)
        .await
        .expect("form-urlencoded body with route param must extract cleanly");
    assert_eq!(dto.id, 42);
    assert_eq!(dto.name, "Alice");
}

#[tokio::test]
async fn non_object_json_body_is_rejected_not_silently_emptied() {
    // Before the fix: `body.as_object().cloned().unwrap_or_default()`
    // silently turned `[1,2,3]` / `"string"` / `null` into `{}`, then
    // deserialize either passed (if route params + defaults filled
    // all required) or failed with confusing serde messages.
    // Post-fix: non-object JSON is rejected explicitly with 400 and a
    // clear message.
    let (addr, captured) = spawn_extracting::<UpdateProfileDto>(route_params()).await;

    let body = b"[1, 2, 3]".to_vec();
    send(addr, "application/json", body, None).await;

    let err = wait_capture(captured)
        .await
        .expect_err("non-object JSON body must be rejected");
    let msg = format!("{err}");
    assert_eq!(
        err.status_code(),
        400,
        "non-object body must produce 400, got {}: {msg}",
        err.status_code()
    );
    assert!(
        msg.contains("must be a JSON object"),
        "error must explain non-object rejection; got: {msg}"
    );
}

#[tokio::test]
async fn precognition_header_short_circuits_with_route_param_dto() {
    // Before the fix: `Precognition: true` requests went through the
    // custom extractor that ignored the header — they ran normal
    // extraction and either succeeded (`Ok(dto)`) or failed with
    // ordinary validation errors. Post-fix: the header triggers the
    // PrecognitionSuccess short-circuit (204 No Content) when all
    // validators pass.
    let (addr, captured) = spawn_extracting::<UpdateProfileDto>(route_params()).await;

    let body = b"name=Alice".to_vec();
    send(
        addr,
        "application/x-www-form-urlencoded",
        body,
        Some(("Precognition", "true")),
    )
    .await;

    let err = wait_capture(captured).await.expect_err(
        "Precognition request must short-circuit (PrecognitionSuccess), \
         not return Ok(dto)",
    );
    assert_eq!(
        err.status_code(),
        204,
        "PrecognitionSuccess must surface as 204 No Content; got {}: {err}",
        err.status_code()
    );
}
