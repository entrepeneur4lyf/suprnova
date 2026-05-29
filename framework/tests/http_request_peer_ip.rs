//! Integration test: `Request::ip()` returns the TCP peer address when
//! the server-side `handle_request_with_peer` is in use, even with no
//! proxy headers on the wire. Locks in that the accept-loop wiring
//! actually reaches `Request::with_peer_addr`.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::IpAddr;
use std::sync::Arc;
use suprnova::{MiddlewareRegistry, Router, handle_request_with_peer, http::text};
use tokio::net::{TcpListener, TcpStream};

#[tokio::test]
async fn ip_resolves_from_peer_addr_when_no_proxy_header() {
    let router: Router = Router::new()
        .get("/whoami", |req: suprnova::Request| async move {
            let ip = req.ip().unwrap_or_else(|| "<none>".to_string());
            text(ip)
        })
        .into();
    let router = Arc::new(router);
    let middleware = Arc::new(MiddlewareRegistry::new());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server_router = router.clone();
    let server_mw = middleware.clone();
    let server = tokio::spawn(async move {
        let (stream, peer) = listener.accept().await.unwrap();
        let peer_ip: Option<IpAddr> = Some(peer.ip());
        let r = server_router.clone();
        let m = server_mw.clone();
        let svc = service_fn(move |req: hyper::Request<Incoming>| {
            let r = r.clone();
            let m = m.clone();
            async move { Ok::<_, Infallible>(handle_request_with_peer(r, m, req, peer_ip).await) }
        });
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(stream), svc)
            .await;
    });

    let stream = TcpStream::connect(addr).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    let conn_task = tokio::spawn(async move {
        let _ = conn.await;
    });
    let req = hyper::Request::builder()
        .method("GET")
        .uri("/whoami")
        .header("Host", "test.local")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let resp = sender.send_request(req).await.unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = std::str::from_utf8(&bytes).unwrap().to_string();

    // Body is the client IP — for a 127.0.0.1 connect, that's "127.0.0.1".
    assert_eq!(body, "127.0.0.1", "got body: {body}");

    // Drop the sender to let the connection task drain; bound everything
    // so a stuck server doesn't hang the test runner.
    drop(sender);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), conn_task).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), server).await;
}
