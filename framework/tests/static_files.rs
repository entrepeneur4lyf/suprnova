use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

use suprnova::{MiddlewareRegistry, Router, fallback, handle_request};

#[tokio::test]
async fn static_files_serves_public_asset_and_blocks_traversal() {
    let dir = tempfile::tempdir().unwrap();
    tokio::fs::write(dir.path().join("app.css"), "body{color:red}")
        .await
        .unwrap();
    let router = Arc::new(
        fallback!(suprnova::StaticFiles::from_dir(dir.path()).handler()).register(Router::new()),
    );

    let ok = drive_get(router.clone(), "/app.css").await;
    assert_eq!(ok.status(), 200);
    assert_eq!(header(&ok, "content-type"), "text/css; charset=utf-8");
    assert_eq!(body(ok).await, "body{color:red}");

    let blocked = drive_get(router, "/%2e%2e/Cargo.toml").await;
    assert_eq!(blocked.status(), 404);
}

#[tokio::test]
async fn static_files_rejects_literal_and_decoded_dot_components() {
    let dir = tempfile::tempdir().unwrap();
    tokio::fs::create_dir(dir.path().join("assets"))
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("assets/app.css"), "body{color:red}")
        .await
        .unwrap();
    let router = Arc::new(
        fallback!(suprnova::StaticFiles::from_dir(dir.path()).handler()).register(Router::new()),
    );

    let ok = drive_get(router.clone(), "/assets/app.css").await;
    assert_eq!(ok.status(), 200);

    let literal_dot = drive_get(router.clone(), "/assets/./app.css").await;
    assert_eq!(literal_dot.status(), 404);

    let decoded_dot = drive_get(router, "/assets/%2e/app.css").await;
    assert_eq!(decoded_dot.status(), 404);
}

#[tokio::test]
async fn static_files_head_returns_headers_without_body() {
    let dir = tempfile::tempdir().unwrap();
    let source = "console.log('ok')";
    tokio::fs::write(dir.path().join("app.js"), source)
        .await
        .unwrap();
    let router = Arc::new(
        fallback!(suprnova::StaticFiles::from_dir(dir.path()).handler()).register(Router::new()),
    );

    let response = drive_request(router, "HEAD", "/app.js").await;

    assert_eq!(response.status(), 200);
    assert_eq!(
        header(&response, "content-type"),
        "text/javascript; charset=utf-8"
    );
    assert_eq!(
        header(&response, "content-length"),
        source.len().to_string()
    );
    assert_eq!(body(response).await, "");
}

#[tokio::test]
async fn static_files_streams_large_get_with_content_length() {
    let dir = tempfile::tempdir().unwrap();
    let data: Vec<u8> = (0..200_000).map(|i| (i % 251) as u8).collect();
    tokio::fs::write(dir.path().join("large.bin"), &data)
        .await
        .unwrap();
    let router = Arc::new(
        fallback!(suprnova::StaticFiles::from_dir(dir.path()).handler()).register(Router::new()),
    );

    let response = drive_get(router, "/large.bin").await;

    assert_eq!(response.status(), 200);
    assert_eq!(
        header(&response, "content-type"),
        "application/octet-stream"
    );
    assert_eq!(header(&response, "content-length"), data.len().to_string());
    assert_eq!(body_bytes(response).await, data);
}

#[cfg(unix)]
#[tokio::test]
async fn static_files_get_and_head_match_for_unreadable_file() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("secret.txt");
    tokio::fs::write(&file_path, "secret").await.unwrap();

    let mut permissions = tokio::fs::metadata(&file_path).await.unwrap().permissions();
    permissions.set_mode(0o0);
    tokio::fs::set_permissions(&file_path, permissions)
        .await
        .unwrap();

    if tokio::fs::File::open(&file_path).await.is_ok() {
        let mut restore = tokio::fs::metadata(&file_path).await.unwrap().permissions();
        restore.set_mode(0o600);
        let _ = tokio::fs::set_permissions(&file_path, restore).await;
        eprintln!("skipping unreadable-file regression: current user can read mode-000 files");
        return;
    }

    let router = Arc::new(
        fallback!(suprnova::StaticFiles::from_dir(dir.path()).handler()).register(Router::new()),
    );

    let get = drive_get(router.clone(), "/secret.txt").await;
    let head = drive_request(router, "HEAD", "/secret.txt").await;

    let get_status = get.status().as_u16();
    let head_status = head.status().as_u16();
    let head_body = body(head).await;

    let mut restore = tokio::fs::metadata(&file_path).await.unwrap().permissions();
    restore.set_mode(0o600);
    let _ = tokio::fs::set_permissions(&file_path, restore).await;

    assert_eq!(get_status, 404);
    assert_eq!(head_status, get_status);
    assert_eq!(head_body, "");
}

#[tokio::test]
async fn static_files_rejects_post_and_directories() {
    let dir = tempfile::tempdir().unwrap();
    tokio::fs::create_dir(dir.path().join("assets"))
        .await
        .unwrap();
    let router = Arc::new(
        fallback!(suprnova::StaticFiles::from_dir(dir.path()).handler()).register(Router::new()),
    );

    let post = drive_request(router.clone(), "POST", "/assets").await;
    assert_eq!(post.status(), 404);

    let directory = drive_get(router, "/assets").await;
    assert_eq!(directory.status(), 404);
}

#[tokio::test]
async fn static_files_sets_cache_control_when_configured() {
    let dir = tempfile::tempdir().unwrap();
    tokio::fs::write(dir.path().join("logo.svg"), "<svg></svg>")
        .await
        .unwrap();
    let router = Arc::new(
        fallback!(
            suprnova::StaticFiles::from_dir(dir.path())
                .cache_control("public, max-age=31536000")
                .handler()
        )
        .register(Router::new()),
    );

    let response = drive_get(router, "/logo.svg").await;

    assert_eq!(response.status(), 200);
    assert_eq!(
        header(&response, "content-type"),
        "image/svg+xml; charset=utf-8"
    );
    assert_eq!(
        header(&response, "cache-control"),
        "public, max-age=31536000"
    );
}

#[tokio::test]
async fn static_files_json_and_manifest_mime_types_have_expected_charset() {
    let dir = tempfile::tempdir().unwrap();
    tokio::fs::write(dir.path().join("data.json"), r#"{"ok":true}"#)
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("site.webmanifest"), r#"{"name":"App"}"#)
        .await
        .unwrap();
    let router = Arc::new(
        fallback!(suprnova::StaticFiles::from_dir(dir.path()).handler()).register(Router::new()),
    );

    let json = drive_get(router.clone(), "/data.json").await;
    assert_eq!(json.status(), 200);
    assert_eq!(
        header(&json, "content-type"),
        "application/json; charset=utf-8"
    );

    let manifest = drive_get(router, "/site.webmanifest").await;
    assert_eq!(manifest.status(), 200);
    assert_eq!(
        header(&manifest, "content-type"),
        "application/manifest+json; charset=utf-8"
    );
}

async fn drive_get(router: Arc<Router>, path: &str) -> hyper::Response<Incoming> {
    drive_request(router, "GET", path).await
}

async fn drive_request(router: Arc<Router>, method: &str, path: &str) -> hyper::Response<Incoming> {
    let middleware = Arc::new(MiddlewareRegistry::new());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral listener");
    let addr = listener.local_addr().expect("local_addr");

    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let io = TokioIo::new(stream);
        let svc = service_fn(move |req: hyper::Request<Incoming>| {
            let router = router.clone();
            let middleware = middleware.clone();
            async move { Ok::<_, Infallible>(handle_request(router, middleware, req).await) }
        });
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(io, svc)
            .await;
    });

    let stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect to test server");
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .expect("client handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header("Host", "localhost")
        .header("Content-Length", "0")
        .body(Full::new(Bytes::new()))
        .expect("build request");

    tokio::time::timeout(Duration::from_secs(5), sender.send_request(req))
        .await
        .expect("send_request timeout")
        .expect("hyper send_request")
}

fn header(response: &hyper::Response<Incoming>, name: &str) -> String {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string()
}

async fn body(response: hyper::Response<Incoming>) -> String {
    String::from_utf8_lossy(&body_bytes(response).await).to_string()
}

async fn body_bytes(response: hyper::Response<Incoming>) -> Bytes {
    response
        .into_body()
        .collect()
        .await
        .expect("collect body bytes")
        .to_bytes()
}
