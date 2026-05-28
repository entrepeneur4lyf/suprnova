//! End-to-end tests for `FormRequest::after_validation_async` — the
//! async cross-field hook where DB-backed rules like `Unique` participate
//! in automatic request validation.
//!
//! `hyper::body::Incoming` isn't constructible outside hyper, so these
//! tests bind a one-shot TCP listener, send a real HTTP request through a
//! hyper client, and assert on the response shape — exactly like
//! `precognition.rs`. The async hook here uses a deterministic in-memory
//! check (no database) so the tests stay fast and pin the *wiring* in
//! `extract()`; the real `Unique`-through-the-hook path is exercised in
//! `validation_rules.rs`.

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use std::convert::Infallible;
use std::net::SocketAddr;
use suprnova::{FormRequest, Request, ValidationErrors};
use validator::Validate;

/// `email` is validated synchronously (format) by the derive. `username`
/// is checked by the async hook against a fixed "already taken" value,
/// standing in for a DB `Unique` query without needing a database.
///
/// Crucially the async hook reports its failure on `username` — a
/// *different* field from the synchronously-validated `email`. That lets
/// the bail test prove the hook never ran by the *absence* of a
/// `username` error.
#[derive(Deserialize, Validate)]
struct UniqueishForm {
    #[validate(email)]
    pub email: String,
    pub username: String,
}

#[suprnova::async_trait]
impl FormRequest for UniqueishForm {
    async fn after_validation_async(&self) -> Result<(), ValidationErrors> {
        let mut errs = ValidationErrors::new();
        if self.username == "taken" {
            errs.add("username", "already taken");
        }
        errs.into_result()
    }
}

async fn spawn() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let io = TokioIo::new(stream);
            let svc = service_fn(
                |hyper_req: hyper::Request<hyper::body::Incoming>| async move {
                    let req = Request::new(hyper_req);
                    let resp = match UniqueishForm::extract(req).await {
                        Ok(_form) => suprnova::HttpResponse::json(serde_json::json!({"ok": true}))
                            .status(200),
                        Err(e) => e.into(),
                    };
                    Ok::<_, Infallible>(resp.into_hyper())
                },
            );
            let _ = http1::Builder::new().serve_connection(io, svc).await;
        }
    });
    addr
}

async fn post_json(
    addr: SocketAddr,
    body: serde_json::Value,
    headers: &[(&str, &str)],
) -> hyper::Response<Bytes> {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let body_bytes = serde_json::to_vec(&body).unwrap();
    let mut req = hyper::Request::builder()
        .method("POST")
        .uri("http://localhost/signup")
        .header("content-type", "application/json")
        .header("content-length", body_bytes.len());
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let req = req.body(Full::new(Bytes::from(body_bytes))).unwrap();

    let resp = sender.send_request(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    let collected = body.collect().await.unwrap();
    hyper::Response::from_parts(parts, collected.to_bytes())
}

#[tokio::test]
async fn async_hook_passing_lets_the_request_succeed() {
    // Valid email + a free username — sync rules pass, the async hook
    // runs and passes, the handler executes (200).
    let addr = spawn().await;
    let resp = post_json(
        addr,
        serde_json::json!({"email": "a@b.com", "username": "free"}),
        &[],
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = serde_json::from_slice(resp.body()).unwrap();
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn async_hook_failure_surfaces_as_422_in_standard_flow() {
    // Valid email but a taken username — sync rules pass, the async hook
    // runs and fails. The request is rejected 422 with the hook's error,
    // proving `extract()` actually awaits the async hook.
    let addr = spawn().await;
    let resp = post_json(
        addr,
        serde_json::json!({"email": "a@b.com", "username": "taken"}),
        &[],
    )
    .await;
    assert_eq!(resp.status(), 422);
    let body: serde_json::Value = serde_json::from_slice(resp.body()).unwrap();
    let errors = body["errors"].as_object().unwrap();
    assert!(
        errors.contains_key("username"),
        "async hook error must reach the response: {errors:?}"
    );
}

#[tokio::test]
async fn async_hook_runs_in_precognition_flow() {
    // Precognition asks about `username`; the async hook fails on it →
    // 422. This proves the async hook runs in the Precognition Ok-branch,
    // not just the standard flow.
    let addr = spawn().await;
    let resp = post_json(
        addr,
        serde_json::json!({"email": "a@b.com", "username": "taken"}),
        &[
            ("Precognition", "true"),
            ("Precognition-Validate-Only", "username"),
        ],
    )
    .await;
    assert_eq!(resp.status(), 422);
    let body: serde_json::Value = serde_json::from_slice(resp.body()).unwrap();
    assert!(body["errors"].as_object().unwrap().contains_key("username"));
}

#[tokio::test]
async fn async_hook_errors_are_precognition_filtered() {
    // Same failing async hook, but the client only asks about `email`
    // (which is valid). The hook's `username` error is filtered out, so
    // from the client's perspective the asked field is fine → 204.
    let addr = spawn().await;
    let resp = post_json(
        addr,
        serde_json::json!({"email": "a@b.com", "username": "taken"}),
        &[
            ("Precognition", "true"),
            ("Precognition-Validate-Only", "email"),
        ],
    )
    .await;
    assert_eq!(resp.status(), 204);
}

#[tokio::test]
async fn async_hook_is_skipped_when_a_sync_field_is_malformed() {
    // The documented bail behavior: `extract()` runs stages in order and
    // bails at the first failure, so a malformed `email` (sync) stops the
    // pipeline before the async hook. The client asks about `username` —
    // whose async check WOULD fail — yet the response is 204, because the
    // async hook never ran. If it had run, `username` would be in the bag
    // and survive the `username` filter → 422. 204 is the proof it bailed.
    let addr = spawn().await;
    let resp = post_json(
        addr,
        serde_json::json!({"email": "not-an-email", "username": "taken"}),
        &[
            ("Precognition", "true"),
            ("Precognition-Validate-Only", "username"),
        ],
    )
    .await;
    assert_eq!(
        resp.status(),
        204,
        "async hook must not run when an earlier sync stage failed"
    );
}
