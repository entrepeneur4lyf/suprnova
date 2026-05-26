//! End-to-end tests for the Tier 5 Precognition flow.
//!
//! `hyper::body::Incoming` isn't constructible outside hyper, so these
//! tests bind a one-shot TCP listener, send a real HTTP request through
//! a hyper client, and assert on the response shape.

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use std::convert::Infallible;
use std::net::SocketAddr;
use suprnova::{FormRequest, Request};
use validator::Validate;

#[derive(Deserialize, Validate)]
struct SignupRequest {
    #[validate(email)]
    pub email: String,
    #[validate(length(min = 8))]
    pub password: String,
}

impl FormRequest for SignupRequest {}

/// Spawn a one-shot server that routes through `SignupRequest::extract`
/// and returns whatever the conversion produces. Returns the address.
async fn spawn() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let io = TokioIo::new(stream);
            let svc = service_fn(
                |hyper_req: hyper::Request<hyper::body::Incoming>| async move {
                    let req = Request::new(hyper_req);
                    let resp = match SignupRequest::extract(req).await {
                        Ok(_form) => {
                            // In a real handler this is where the body runs.
                            suprnova::HttpResponse::json(serde_json::json!({
                                "ok": true
                            }))
                            .status(200)
                        }
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
async fn precognition_success_returns_204() {
    let addr = spawn().await;
    let resp = post_json(
        addr,
        serde_json::json!({"email": "a@b.com", "password": "longenough"}),
        &[("Precognition", "true")],
    )
    .await;
    assert_eq!(resp.status(), 204);
    assert_eq!(resp.headers().get("Precognition").unwrap(), "true");
    assert_eq!(resp.headers().get("Precognition-Success").unwrap(), "true");
    assert_eq!(resp.headers().get("Vary").unwrap(), "Precognition");
}

#[tokio::test]
async fn precognition_failure_returns_422_with_filtered_errors() {
    let addr = spawn().await;
    // Both email and password invalid; ask only about email.
    let resp = post_json(
        addr,
        serde_json::json!({"email": "not-an-email", "password": "short"}),
        &[
            ("Precognition", "true"),
            ("Precognition-Validate-Only", "email"),
        ],
    )
    .await;
    assert_eq!(resp.status(), 422);
    assert_eq!(resp.headers().get("Precognition").unwrap(), "true");
    assert_eq!(resp.headers().get("Vary").unwrap(), "Precognition");
    let body: serde_json::Value = serde_json::from_slice(resp.body()).unwrap();
    let errors = body["errors"].as_object().unwrap();
    assert!(errors.contains_key("email"));
    assert!(
        !errors.contains_key("password"),
        "password error should be filtered out: {:?}",
        errors
    );
}

#[tokio::test]
async fn precognition_only_unrequested_fields_failing_returns_204() {
    // password is invalid but client only asked about email which IS
    // valid. From the client's perspective, the answer they asked for
    // is "OK" — return 204, not 422.
    let addr = spawn().await;
    let resp = post_json(
        addr,
        serde_json::json!({"email": "a@b.com", "password": "short"}),
        &[
            ("Precognition", "true"),
            ("Precognition-Validate-Only", "email"),
        ],
    )
    .await;
    assert_eq!(resp.status(), 204);
}

#[tokio::test]
async fn non_precognition_request_runs_handler_on_valid_input() {
    let addr = spawn().await;
    let resp = post_json(
        addr,
        serde_json::json!({"email": "a@b.com", "password": "longenough"}),
        &[], // no Precognition header
    )
    .await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = serde_json::from_slice(resp.body()).unwrap();
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn non_precognition_invalid_returns_422_without_precognition_headers() {
    let addr = spawn().await;
    let resp = post_json(
        addr,
        serde_json::json!({"email": "bad", "password": "short"}),
        &[],
    )
    .await;
    assert_eq!(resp.status(), 422);
    assert!(resp.headers().get("Precognition").is_none());
    assert!(resp.headers().get("Vary").is_none());
}

#[tokio::test]
async fn precognition_case_insensitive() {
    let addr = spawn().await;
    let resp = post_json(
        addr,
        serde_json::json!({"email": "a@b.com", "password": "longenough"}),
        &[("Precognition", "TRUE")],
    )
    .await;
    assert_eq!(resp.status(), 204);
}
