//! Integration tests for `Http` and `Http::fake`.
//!
//! `Http::fake` uses `tokio::task_local!` for fake-state isolation, so
//! tests in this file run in parallel without any explicit locking.

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
                    let traceparent = req
                        .headers()
                        .get("traceparent")
                        .and_then(|h| h.to_str().ok())
                        .map(|s| s.to_string());
                    let tracestate = req
                        .headers()
                        .get("tracestate")
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
                        traceparent,
                        tracestate,
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
#[allow(dead_code)]
struct EchoCapture {
    method: String,
    uri: String,
    authorization: Option<String>,
    content_type: Option<String>,
    body: String,
    traceparent: Option<String>,
    tracestate: Option<String>,
}

#[tokio::test]
async fn get_returns_200() {
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
    Http::fake(|| async {
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
    })
    .await;
}

#[tokio::test]
async fn fake_assert_not_sent_passes_when_clean() {
    Http::fake(|| async {
        // No requests sent — assert_not_sent must not panic.
        assert_not_sent(|r| r.url.contains("anything"));
    })
    .await;
}

#[tokio::test]
async fn fake_unmatched_request_returns_default_200() {
    Http::fake(|| async {
        // No canned response queued — request still succeeds with 200 {}
        let resp = Http::get("https://example.com/anything")
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body.is_object());
    })
    .await;
}

#[tokio::test]
async fn parallel_fakes_are_isolated() {
    // Two concurrent fake scopes on independent tasks must not see
    // each other's recorded requests or canned responses. This is the
    // whole point of moving FAKE_STATE to task_local.
    let h1 = tokio::spawn(async {
        Http::fake(|| async {
            fake_response("GET", "/one", 200, serde_json::json!({"who": "one"}));
            let r = Http::get("https://x.test/one").send().await.unwrap();
            let body: serde_json::Value = r.json().await.unwrap();
            assert_eq!(body["who"], "one");
            // Only our own send is visible.
            assert_sent(|r| r.url.contains("/one"));
            assert_not_sent(|r| r.url.contains("/two"));
        })
        .await;
    });
    let h2 = tokio::spawn(async {
        Http::fake(|| async {
            fake_response("GET", "/two", 200, serde_json::json!({"who": "two"}));
            let r = Http::get("https://x.test/two").send().await.unwrap();
            let body: serde_json::Value = r.json().await.unwrap();
            assert_eq!(body["who"], "two");
            assert_sent(|r| r.url.contains("/two"));
            assert_not_sent(|r| r.url.contains("/one"));
        })
        .await;
    });
    h1.await.unwrap();
    h2.await.unwrap();
}

#[tokio::test]
#[should_panic(expected = "Http fake helpers called outside an Http::fake")]
async fn fake_response_outside_scope_panics() {
    // Without an active Http::fake scope, fake helpers must panic
    // loudly instead of touching uninitialized state.
    fake_response("GET", "/", 200, serde_json::json!({}));
}

#[tokio::test]
async fn into_inner_returns_ok_for_real_response() {
    // Real reqwest::Response should round-trip out via into_inner.
    let (addr, _cap) = spawn_echo().await;
    let url = format!("http://{}/", addr);
    let resp = Http::get(&url).send().await.expect("send");
    let inner = resp.into_inner().expect("real response should unwrap");
    // Sanity: it really is a reqwest::Response with the same status.
    assert_eq!(inner.status().as_u16(), 200);
}

/// Spawn a server that replies based on a queue of canned responses
/// (status, optional `Retry-After` seconds, body). Each accepted
/// connection is served by popping one element; once the queue is
/// empty, every later connection gets `200 {}`.
async fn spawn_canned(
    canned: Vec<(u16, Option<u64>, &'static str)>,
) -> (SocketAddr, Arc<Mutex<usize>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let count_for_task = count.clone();
    let queue = Arc::new(Mutex::new(canned));
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let io = TokioIo::new(stream);
            let queue = queue.clone();
            let count = count_for_task.clone();
            let svc = service_fn(move |_req: hyper::Request<Incoming>| {
                let queue = queue.clone();
                let count = count.clone();
                async move {
                    *count.lock().unwrap() += 1;
                    let next = queue.lock().unwrap().pop();
                    // pop from end; reverse the input ordering when populating
                    let (status, retry_after, body) =
                        next.unwrap_or((200, None, "{}"));
                    let mut builder = hyper::Response::builder()
                        .status(status)
                        .header("content-type", "application/json");
                    if let Some(secs) = retry_after {
                        builder = builder.header("retry-after", secs.to_string());
                    }
                    Ok::<_, Infallible>(
                        builder.body(Full::new(Bytes::from_static(body.as_bytes()))).unwrap(),
                    )
                }
            });
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await;
            });
        }
    });
    (addr, count)
}

#[tokio::test]
async fn retry_5xx_then_succeeds() {
    // Populate queue so the FIRST pop is the first response served.
    // pop() reads from the END, so reverse the intended order.
    let mut canned: Vec<(u16, Option<u64>, &'static str)> = vec![
        (503, None, "{\"err\":1}"),
        (503, None, "{\"err\":2}"),
        (200, None, "{\"ok\":true}"),
    ];
    canned.reverse();
    let (addr, count) = spawn_canned(canned).await;
    let url = format!("http://{}/x", addr);
    let resp = Http::get(&url)
        .retry(4, std::time::Duration::from_millis(5))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(*count.lock().unwrap(), 3, "expected 3 attempts");
}

#[tokio::test]
async fn retry_exhausted_returns_last_5xx() {
    // Three 500s; max_attempts=3 → all three consumed.
    let mut canned: Vec<(u16, Option<u64>, &'static str)> = vec![
        (500, None, "{\"err\":1}"),
        (500, None, "{\"err\":2}"),
        (500, None, "{\"err\":3}"),
    ];
    canned.reverse();
    let (addr, count) = spawn_canned(canned).await;
    let url = format!("http://{}/x", addr);
    let resp = Http::get(&url)
        .retry(3, std::time::Duration::from_millis(5))
        .send()
        .await
        .expect("send returns the last response, not an error");
    assert_eq!(resp.status(), 500);
    assert_eq!(*count.lock().unwrap(), 3, "expected 3 attempts");
}

#[tokio::test]
async fn retry_not_attempted_on_4xx() {
    let (addr, count) = spawn_canned(vec![(404, None, "{\"err\":\"nf\"}")]).await;
    let url = format!("http://{}/x", addr);
    let resp = Http::get(&url)
        .retry(5, std::time::Duration::from_millis(5))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 404);
    assert_eq!(*count.lock().unwrap(), 1, "4xx must not retry");
}

#[tokio::test]
async fn retry_honors_retry_after_header_on_503() {
    // First response: 503 with Retry-After: 1 (1 second).
    // Second response: 200.
    // Base backoff is 1ms — far below the 1s Retry-After. We assert
    // the wait between attempts honors the larger of the two by
    // checking the elapsed wall clock.
    let mut canned: Vec<(u16, Option<u64>, &'static str)> =
        vec![(503, Some(1), "{}"), (200, None, "{\"ok\":true}")];
    canned.reverse();
    let (addr, _count) = spawn_canned(canned).await;
    let url = format!("http://{}/x", addr);
    let start = std::time::Instant::now();
    let resp = Http::get(&url)
        .retry(3, std::time::Duration::from_millis(1))
        .send()
        .await
        .expect("send");
    let elapsed = start.elapsed();
    assert_eq!(resp.status(), 200);
    // The 1s Retry-After must dominate the 1ms base backoff. Allow a
    // little slack but require at least 900ms.
    assert!(
        elapsed >= std::time::Duration::from_millis(900),
        "Retry-After=1s not honored; elapsed={:?}",
        elapsed
    );
}

#[tokio::test]
async fn into_inner_returns_err_for_fake_response() {
    Http::fake(|| async {
        fake_response("GET", "/x", 200, serde_json::json!({"ok": true}));
        let resp = Http::get("https://x.test/x").send().await.expect("send");
        let err = resp
            .into_inner()
            .expect_err("fake response should not unwrap");
        let msg = err.to_string();
        assert!(
            msg.contains("not available on fake responses"),
            "unexpected error message: {msg}"
        );
    })
    .await;
}

// ── Codex review finding 8 — W3C trace context injection ─────────────────
//
// `Http::send` must inject `traceparent` (and `tracestate` when
// non-empty) into outbound requests when an OTel context is active.
// This is gated behind the `otel` feature because the propagator and
// context types only exist there.

#[cfg(feature = "otel")]
#[tokio::test]
async fn outbound_request_includes_traceparent_when_otel_context_active() {
    use opentelemetry::trace::{
        FutureExt as _, SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState,
    };
    use opentelemetry::Context;

    // Install the W3C TraceContext propagator. Idempotent — safe even
    // if other tests in this run also called it.
    suprnova::telemetry::propagation::install_trace_context_propagator();

    // Build a fully-specified OTel span context. We use deterministic
    // IDs so the test can assert that the wire `traceparent` carries
    // exactly the trace id we attached.
    let trace_id = TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736")
        .expect("hex trace id parses");
    let span_id = SpanId::from_hex("00f067aa0ba902b7").expect("hex span id parses");
    let span_ctx = SpanContext::new(
        trace_id,
        span_id,
        TraceFlags::SAMPLED,
        /* is_remote: */ true,
        TraceState::default(),
    );
    let cx = Context::current().with_remote_span_context(span_ctx);

    let (addr, cap) = spawn_echo().await;
    let url = format!("http://{}/traced", addr);

    // Wrap the send in `with_context(cx)` so the OTel context is the
    // current one across the await boundary, which is what the
    // propagator reads from inside `inject_w3c_trace_context`.
    async {
        let resp = Http::get(&url).send().await.expect("send");
        assert_eq!(resp.status(), 200);
    }
    .with_context(cx)
    .await;

    let captured = cap.lock().unwrap().clone().expect("server saw no request");
    let traceparent = captured
        .traceparent
        .expect("traceparent header must be injected when an OTel context is active");

    // W3C format: `version-trace_id-parent_id-flags`, all hex.
    let parts: Vec<&str> = traceparent.split('-').collect();
    assert_eq!(parts.len(), 4, "traceparent has 4 dash-separated parts, got {traceparent:?}");
    assert_eq!(parts[0], "00", "version must be 00, got {traceparent:?}");
    assert_eq!(parts[1].len(), 32, "trace_id is 32 hex chars, got {traceparent:?}");
    assert_eq!(parts[2].len(), 16, "span_id is 16 hex chars, got {traceparent:?}");
    assert_eq!(parts[3].len(), 2, "flags is 2 hex chars, got {traceparent:?}");

    // The trace id we attached must show up on the wire.
    assert_eq!(
        parts[1], "4bf92f3577b34da6a3ce929d0e0e4736",
        "trace id on wire does not match attached context, got {traceparent:?}"
    );
}

#[cfg(feature = "otel")]
#[tokio::test]
async fn outbound_request_omits_traceparent_without_active_context() {
    // No `with_context` wrapper here — `Context::current()` is empty,
    // so the propagator should inject nothing and the echo server
    // sees no `traceparent` header.
    suprnova::telemetry::propagation::install_trace_context_propagator();

    let (addr, cap) = spawn_echo().await;
    let url = format!("http://{}/untraced", addr);
    let resp = Http::get(&url).send().await.expect("send");
    assert_eq!(resp.status(), 200);

    let captured = cap.lock().unwrap().clone().expect("server saw no request");
    assert!(
        captured.traceparent.is_none(),
        "empty OTel context must NOT inject traceparent, got {:?}",
        captured.traceparent
    );
}
