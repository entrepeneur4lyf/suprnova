//! End-to-end tests for the SSE delivery primitive.
//!
//! Pattern matches `framework/tests/precognition.rs`: bind a one-shot
//! TCP listener, run a hyper service that returns
//! `HttpResponse::sse(...)`, and connect a real hyper client over the
//! socket so the body collection exercises the full streaming path
//! (not just an in-memory `BoxBody`).
//!
//! Hyper's `body::Incoming` can't be constructed outside its
//! connection machinery, which is why these tests go through real
//! sockets rather than calling `into_hyper()` directly.

use bytes::Bytes;
use futures::stream;
use http_body_util::{BodyExt, Empty};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use suprnova::{HttpResponse, sse::SseEvent};

/// Spawn a one-shot server that emits three SSE events and shuts
/// down after the first connection. Returns the bound address.
async fn spawn_sse_server() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((stream_tcp, _)) = listener.accept().await {
            let io = TokioIo::new(stream_tcp);
            let svc = service_fn(|_req: hyper::Request<hyper::body::Incoming>| async move {
                // Three events: data-only, named, and named+id with
                // multi-line data. This covers every framing branch
                // in SseEvent::to_wire.
                let events = vec![
                    SseEvent::data("hello"),
                    SseEvent::data("world").with_event("greet"),
                    SseEvent::data("first line\nsecond line")
                        .with_event("multi")
                        .with_id("7"),
                ];
                let resp = HttpResponse::sse(stream::iter(events));
                Ok::<_, Infallible>(resp.into_hyper())
            });
            let _ = http1::Builder::new().serve_connection(io, svc).await;
        }
    });
    addr
}

/// Open a client connection, GET `/`, and return the response with a
/// fully collected body. Streaming bodies are valid input to
/// `BodyExt::collect` — the test just blocks until the producing
/// stream ends.
async fn fetch(addr: SocketAddr) -> hyper::Response<Bytes> {
    let stream_tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream_tcp);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method("GET")
        .uri("/")
        .header("Host", "localhost")
        .body(Empty::<Bytes>::new())
        .unwrap();

    let resp = sender.send_request(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    let collected = body.collect().await.unwrap();
    hyper::Response::from_parts(parts, collected.to_bytes())
}

#[tokio::test]
async fn sse_response_sets_event_stream_headers() {
    let addr = spawn_sse_server().await;
    let resp = fetch(addr).await;

    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("Content-Type").unwrap(),
        "text/event-stream"
    );
    assert_eq!(resp.headers().get("Cache-Control").unwrap(), "no-cache");
    assert_eq!(resp.headers().get("Connection").unwrap(), "keep-alive");
    assert_eq!(
        resp.headers().get("X-Accel-Buffering").unwrap(),
        "no",
        "X-Accel-Buffering must be 'no' so nginx doesn't swallow events"
    );
}

#[tokio::test]
async fn sse_response_streams_events_with_correct_framing() {
    let addr = spawn_sse_server().await;
    let resp = fetch(addr).await;

    let body = resp.body().clone();
    let s = std::str::from_utf8(&body).unwrap();

    // Each event is terminated by a blank line. The producer stream
    // ordering is preserved, so concatenated wire output should be
    // exactly the three frames in sequence.
    let expected = "\
data: hello\n\
\n\
event: greet\n\
data: world\n\
\n\
event: multi\n\
id: 7\n\
data: first line\n\
data: second line\n\
\n";
    assert_eq!(s, expected);
}

#[tokio::test]
async fn sse_response_emits_each_event_on_its_own_frame() {
    // Stricter check: split the body on blank lines (the SSE event
    // separator) and verify each non-empty frame has the expected
    // shape — guards against a regression where frame boundaries
    // collapse (which browsers tolerate for `data:`-only frames but
    // breaks `event:` and `id:` dispatch).
    let addr = spawn_sse_server().await;
    let resp = fetch(addr).await;

    let s = std::str::from_utf8(resp.body()).unwrap();
    // Trailing terminator on the last event means split yields an
    // empty final segment we must drop.
    let frames: Vec<&str> = s.split("\n\n").filter(|f| !f.is_empty()).collect();
    assert_eq!(frames.len(), 3, "expected 3 SSE frames, got: {:?}", frames);

    assert_eq!(frames[0], "data: hello");
    assert_eq!(frames[1], "event: greet\ndata: world");
    assert_eq!(
        frames[2],
        "event: multi\nid: 7\ndata: first line\ndata: second line"
    );
}
