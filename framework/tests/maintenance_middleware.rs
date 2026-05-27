//! Integration tests for `MaintenanceMiddleware`, installed as GLOBAL middleware.
//!
//! Proves the end-to-end behavior over real HTTP: a `503` with `Retry-After` /
//! `Refresh` while down, `except` paths passing through to the handler, the
//! secret-URL bypass-cookie round trip, and a clean `200` once back up.

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

use suprnova::http::text;
use suprnova::{
    Crypt, EncryptionKey, FileMaintenanceMode, MaintenanceMiddleware, MaintenanceMode,
    MaintenancePayload, MiddlewareRegistry, Router, handle_request,
};

/// The bypass cookie is encrypted, so the crypto layer must be initialized.
/// Process-wide and idempotent — init once per test binary.
fn ensure_crypt() {
    static INIT: Once = Once::new();
    INIT.call_once(|| Crypt::init(EncryptionKey::generate()));
}

/// A unique, non-existent down-file path per test so parallel tests never
/// collide on shared maintenance state.
fn unique_down_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut p = std::env::temp_dir();
    p.push(format!("suprnova-maint-it-{}-{nanos}", std::process::id()));
    p.push("framework/down");
    p
}

/// A registry with `MaintenanceMiddleware` installed globally, backed by a
/// file driver at `path`, with `/api/health` always reachable.
fn registry_for(path: &Path) -> MiddlewareRegistry {
    let driver = Arc::new(FileMaintenanceMode::with_path(path.to_path_buf()));
    MiddlewareRegistry::new()
        .append(MaintenanceMiddleware::with_driver(driver).except(["api/health"]))
}

fn router() -> Router {
    Router::new()
        .get("/", |_req| async { text("home") })
        .get("/api/health", |_req| async { text("ok") })
        .into()
}

async fn spawn_server(
    router: impl Into<Router>,
    registry: MiddlewareRegistry,
    accepts: usize,
) -> SocketAddr {
    let router = Arc::new(router.into());
    let middleware = Arc::new(registry);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral listener");
    let addr = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        for _ in 0..accepts {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let io = TokioIo::new(stream);
            let router = router.clone();
            let middleware = middleware.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |req: hyper::Request<Incoming>| {
                    let router = router.clone();
                    let middleware = middleware.clone();
                    async move { Ok::<_, Infallible>(handle_request(router, middleware, req).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await;
            });
        }
    });

    addr
}

async fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> (u16, HashMap<String, String>, String) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut builder = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header("Host", "localhost")
        .header("Content-Length", "0");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let req = builder.body(Full::new(Bytes::new())).unwrap();

    let resp = tokio::time::timeout(Duration::from_secs(5), sender.send_request(req))
        .await
        .expect("send_request timeout")
        .expect("hyper send_request");

    let (parts, body) = resp.into_parts();
    let status = parts.status.as_u16();
    let header_map = parts
        .headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_lowercase(),
                v.to_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    let bytes = body.collect().await.unwrap().to_bytes();
    (
        status,
        header_map,
        String::from_utf8_lossy(&bytes).to_string(),
    )
}

#[tokio::test]
async fn down_serves_503_with_retry_and_refresh_headers() {
    let path = unique_down_path();
    FileMaintenanceMode::with_path(path.clone())
        .activate(&MaintenancePayload {
            retry: Some(120),
            refresh: Some(5),
            ..Default::default()
        })
        .await
        .unwrap();

    let addr = spawn_server(router(), registry_for(&path), 2).await;
    let (status, headers, _body) = request(addr, "GET", "/", &[]).await;

    assert_eq!(status, 503, "a down app must answer 503");
    assert_eq!(headers.get("retry-after").map(String::as_str), Some("120"));
    assert_eq!(headers.get("refresh").map(String::as_str), Some("5"));
}

#[tokio::test]
async fn except_path_passes_through_while_down() {
    let path = unique_down_path();
    FileMaintenanceMode::with_path(path.clone())
        .activate(&MaintenancePayload::new())
        .await
        .unwrap();

    let addr = spawn_server(router(), registry_for(&path), 2).await;
    let (status, _headers, body) = request(addr, "GET", "/api/health", &[]).await;

    assert_eq!(
        status, 200,
        "an excepted path must reach its handler while down"
    );
    assert_eq!(body, "ok");
}

#[tokio::test]
async fn secret_url_sets_bypass_cookie_which_then_grants_access() {
    ensure_crypt();
    let path = unique_down_path();
    FileMaintenanceMode::with_path(path.clone())
        .activate(&MaintenancePayload {
            secret: Some("opensesame".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    let addr = spawn_server(router(), registry_for(&path), 3).await;

    // Visiting the secret URL redirects home and sets the bypass cookie.
    let (status, headers, _body) = request(addr, "GET", "/opensesame", &[]).await;
    assert_eq!(status, 302, "the secret URL must redirect");
    let set_cookie = headers
        .get("set-cookie")
        .expect("the secret URL must set a bypass cookie");
    assert!(set_cookie.starts_with("suprnova_maintenance="));

    // Re-request a normal path carrying that cookie — it must now pass through.
    let cookie_pair = set_cookie.split(';').next().unwrap();
    let (status, _headers, body) = request(addr, "GET", "/", &[("Cookie", cookie_pair)]).await;
    assert_eq!(status, 200, "a valid bypass cookie must grant access");
    assert_eq!(body, "home");
}

#[tokio::test]
async fn a_request_without_the_bypass_cookie_is_still_blocked() {
    let path = unique_down_path();
    FileMaintenanceMode::with_path(path.clone())
        .activate(&MaintenancePayload {
            secret: Some("opensesame".into()),
            ..Default::default()
        })
        .await
        .unwrap();

    let addr = spawn_server(router(), registry_for(&path), 2).await;
    let (status, _headers, _body) = request(addr, "GET", "/", &[]).await;
    assert_eq!(status, 503, "no bypass cookie means the 503 still applies");
}

#[tokio::test]
async fn up_serves_normally() {
    // A path that was never activated: the app is up.
    let path = unique_down_path();
    let addr = spawn_server(router(), registry_for(&path), 2).await;
    let (status, _headers, body) = request(addr, "GET", "/", &[]).await;
    assert_eq!(status, 200);
    assert_eq!(body, "home");
}
