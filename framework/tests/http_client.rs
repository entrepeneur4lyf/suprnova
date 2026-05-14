//! Integration tests for `Http` and `Http::fake()`.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use suprnova::{assert_not_sent, assert_sent, fake_response, Http};

// Http uses a process-wide fake-state OnceLock that intercepts every
// outbound request while a fake guard is alive. Any test in this file
// that sends a real or fake request must hold this lock for its
// duration — otherwise tests running in parallel would either trip the
// other test's fake or pollute its capture.
static HTTP_LOCK: Mutex<()> = Mutex::new(());

/// One-shot echo server. Accepts a single connection, captures the
/// inbound request, replies with a JSON body that includes the
/// request method + URI + selected headers + body, and exits.
async fn spawn_echo() -> (SocketAddr, Arc<Mutex<Option<EchoCapture>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let captured: Arc<Mutex<Option<EchoCapture>>> = Arc::new(Mutex::new(None));
    let cap_for_task = captured.clone();
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let io = TokioIo::new(stream);
            let captured = cap_for_task.clone();
            let svc = service_fn(move |req: hyper::Request<Incoming>| {
                let captured = captured.clone();
                async move {
                    let method = req.method().to_string();
                    let uri = req.uri().to_string();
                    let auth = req
                        .headers()
                        .get("authorization")
                        .and_then(|h| h.to_str().ok())
                        .map(|s| s.to_string());
                    let ct = req
                        .headers()
                        .get("content-type")
                        .and_then(|h| h.to_str().ok())
                        .map(|s| s.to_string());
                    let body_bytes = req.into_body().collect().await.unwrap().to_bytes();
                    let body_str = String::from_utf8_lossy(&body_bytes).to_string();

                    *captured.lock().unwrap() = Some(EchoCapture {
                        method: method.clone(),
                        uri: uri.clone(),
                        authorization: auth.clone(),
                        content_type: ct.clone(),
                        body: body_str.clone(),
                    });

                    let payload = serde_json::json!({
                        "method": method,
                        "uri": uri,
                        "authorization": auth,
                        "content_type": ct,
                        "body": body_str,
                    });
                    let bytes = serde_json::to_vec(&payload).unwrap();
                    Ok::<_, Infallible>(
                        hyper::Response::builder()
                            .status(200)
                            .header("content-type", "application/json")
                            .body(Full::new(Bytes::from(bytes)))
                            .unwrap(),
                    )
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, svc)
                .await;
        }
    });
    (addr, captured)
}

#[derive(Debug, Clone)]
struct EchoCapture {
    method: String,
    uri: String,
    authorization: Option<String>,
    content_type: Option<String>,
    body: String,
}

#[tokio::test]
async fn get_returns_200() {
    let _g = HTTP_LOCK.lock().unwrap();
    let (addr, _cap) = spawn_echo().await;
    let url = format!("http://{}/ping", addr);
    let resp = Http::get(&url).send().await.expect("send");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["method"], "GET");
    assert!(body["uri"].as_str().unwrap().contains("/ping"));
}

#[tokio::test]
async fn post_json_echoes() {
    let _g = HTTP_LOCK.lock().unwrap();
    let (addr, cap) = spawn_echo().await;
    let url = format!("http://{}/echo", addr);
    let payload = serde_json::json!({"hello": "world"});
    let resp = Http::post(&url).json(&payload).send().await.expect("send");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["method"], "POST");
    // The echoed body string equals the JSON we sent
    let echoed = body["body"].as_str().unwrap();
    let echoed_json: serde_json::Value = serde_json::from_str(echoed).unwrap();
    assert_eq!(echoed_json, payload);
    // The server saw content-type: application/json
    // Give the echo server task a moment to publish its capture
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let captured = cap.lock().unwrap().clone().unwrap();
    assert!(captured.content_type.as_deref().unwrap().contains("json"));
}

#[tokio::test]
async fn bearer_token_sets_auth_header() {
    let _g = HTTP_LOCK.lock().unwrap();
    let (addr, cap) = spawn_echo().await;
    let url = format!("http://{}/secure", addr);
    let resp = Http::get(&url)
        .bearer_token("my-token-123")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 200);
    // Give the echo server task a moment to publish its capture
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let captured = cap.lock().unwrap().clone().unwrap();
    assert_eq!(
        captured.authorization.as_deref(),
        Some("Bearer my-token-123")
    );
}

#[tokio::test]
async fn basic_auth_sets_auth_header() {
    let _g = HTTP_LOCK.lock().unwrap();
    let (addr, cap) = spawn_echo().await;
    let url = format!("http://{}/secure", addr);
    let resp = Http::get(&url)
        .basic_auth("alice", Some("s3cret"))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 200);
    // Give the echo server task a moment to publish its capture
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let captured = cap.lock().unwrap().clone().unwrap();
    let auth = captured.authorization.as_deref().unwrap();
    assert!(auth.starts_with("Basic "));
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let encoded = auth.strip_prefix("Basic ").unwrap();
    let decoded = String::from_utf8(STANDARD.decode(encoded).unwrap()).unwrap();
    assert_eq!(decoded, "alice:s3cret");
}

#[tokio::test]
async fn fake_intercepts_and_records() {
    let _g = HTTP_LOCK.lock().unwrap();
    let _guard = Http::fake();
    fake_response(
        "POST",
        "/api/users",
        201,
        serde_json::json!({"id": 42, "name": "Ada"}),
    );

    let resp = Http::post("https://example.com/api/users")
        .json(&serde_json::json!({"name": "Ada"}))
        .send()
        .await
        .expect("send");

    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["id"], 42);

    assert_sent(|r| r.method == "POST" && r.url.contains("/api/users"));
}

#[tokio::test]
async fn fake_assert_not_sent_passes_when_clean() {
    let _g = HTTP_LOCK.lock().unwrap();
    let _guard = Http::fake();
    // No requests sent — assert_not_sent must not panic.
    assert_not_sent(|r| r.url.contains("anything"));
}

#[tokio::test]
async fn fake_unmatched_request_returns_default_200() {
    let _g = HTTP_LOCK.lock().unwrap();
    let _guard = Http::fake();
    // No canned response queued — request still succeeds with 200 {}
    let resp = Http::get("https://example.com/anything")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.is_object());
}
