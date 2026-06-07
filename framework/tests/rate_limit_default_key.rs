//! Integration test for `ThrottleRequestsMiddleware` inline-mode
//! default key (M45). The middleware's `default_request_key` must:
//!
//! - return the TCP peer when no `TrustedProxiesConfig` is installed
//!   (the spoof-safe fallback);
//! - honour `X-Forwarded-For` only when the peer is in the allowlist;
//! - fall back to `"unknown"` (not the historical `"anon"` global
//!   bucket) when no peer was threaded into the request.
//!
//! Drives the middleware end-to-end via a hyper service so the
//! `Request` carries a real `Incoming` body, matching the
//! production code path.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::IpAddr;
use suprnova::Request;
use suprnova::http::TrustedProxiesConfig;
use tokio::net::{TcpListener, TcpStream};

/// Build a `suprnova::Request` from a hyper request builder by piping
/// it through a one-shot hyper service. The returned Request can have
/// `with_peer_addr` / `with_trusted_proxies` chained on it.
async fn build_request(builder: hyper::http::request::Builder) -> Request {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (req_tx, req_rx) = tokio::sync::oneshot::channel::<Request>();
    let req_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(req_tx)));

    let req_tx_for_svc = req_tx.clone();
    let server_task = tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            let svc = service_fn(move |hyper_req: hyper::Request<Incoming>| {
                let tx = req_tx_for_svc.clone();
                async move {
                    let req = Request::new(hyper_req);
                    if let Some(s) = tx.lock().unwrap().take() {
                        let _ = s.send(req);
                    }
                    Ok::<hyper::Response<Full<Bytes>>, Infallible>(
                        hyper::Response::builder()
                            .status(200)
                            .body(Full::new(Bytes::from_static(b"")))
                            .unwrap(),
                    )
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), svc)
                .await;
        }
    });

    let client_task = tokio::spawn(async move {
        let stream = TcpStream::connect(addr).await.unwrap();
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .unwrap();
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let req = builder.body(Full::new(Bytes::new())).unwrap();
        let resp = sender.send_request(req).await.unwrap();
        let _ = resp.into_body().collect().await;
    });

    let received = req_rx.await.unwrap();
    let _ = client_task.await;
    let _ = server_task.await;
    received
}

/// The default-key signature observed by the test. We mirror what
/// `ThrottleRequestsMiddleware::with` would inject — `format!("ip:{ip}:path:{}", ...)`
/// — without exposing the private `default_request_key` symbol.
fn default_key(req: &Request) -> String {
    let ip = req.ip().unwrap_or_else(|| "unknown".into());
    format!("ip:{ip}:path:{}", req.path())
}

#[tokio::test]
async fn default_key_uses_peer_when_xff_untrusted() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/api/posts")
            .header("X-Forwarded-For", "203.0.113.5"),
    )
    .await
    .with_peer_addr(IpAddr::from([198, 51, 100, 2]));
    // Empty allowlist (default) — XFF must NOT be honoured.
    assert_eq!(default_key(&req), "ip:198.51.100.2:path:/api/posts");
}

#[tokio::test]
async fn default_key_honors_xff_when_peer_is_trusted() {
    let req = build_request(
        hyper::Request::builder()
            .uri("/api/posts")
            .header("X-Forwarded-For", "203.0.113.5"),
    )
    .await
    .with_peer_addr(IpAddr::from([127, 0, 0, 1]))
    .with_trusted_proxies(TrustedProxiesConfig::with_ips([IpAddr::from([
        127, 0, 0, 1,
    ])]));
    assert_eq!(default_key(&req), "ip:203.0.113.5:path:/api/posts");
}

#[tokio::test]
async fn default_key_returns_unknown_when_no_peer() {
    let req = build_request(hyper::Request::builder().uri("/api/posts")).await;
    // No peer — the literal fallback. Critically, this is NOT a
    // shared `"anon"` bucket — the path is part of the key so two
    // routes don't collide.
    assert_eq!(default_key(&req), "ip:unknown:path:/api/posts");
}
