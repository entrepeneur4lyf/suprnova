//! M46 — `LoginThrottleMiddleware` backend-error policy. This file
//! intentionally does NOT initialize torii: with no torii bound the
//! `BruteForce::get_lockout_status` call errors out via `instance()?`,
//! letting us exercise the FailClosed / FailOpen branches end-to-end.
//!
//! The matching torii-bound integration tests live in
//! `framework/tests/brute_force.rs` — those cover the locked /
//! unlocked paths where the backend returns a real `LockoutStatus`.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use serial_test::serial;

use suprnova::auth_flows::{BackendErrorPolicy, LoginThrottleMiddleware};
use suprnova::http::text;
use suprnova::{MiddlewareRegistry, Router, handle_request};

async fn spawn_server(router: impl Into<Router>, accepts: usize) -> SocketAddr {
    let router = Arc::new(router.into());
    let middleware = Arc::new(MiddlewareRegistry::new());

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

async fn post_login(addr: SocketAddr) -> (u16, Option<String>) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method("POST")
        .uri("/login")
        .header("Host", "localhost")
        .header("Content-Length", "0")
        .header("X-Login-Email", "alice@example.com")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = tokio::time::timeout(Duration::from_secs(5), sender.send_request(req))
        .await
        .expect("send_request timeout")
        .expect("hyper send_request");

    let (parts, body) = resp.into_parts();
    let _ = body.collect().await.unwrap();
    let retry_after = parts
        .headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok().map(String::from));
    (parts.status.as_u16(), retry_after)
}

fn email_extractor() -> impl Fn(&suprnova::Request) -> Option<String> + Send + Sync + 'static {
    |req: &suprnova::Request| req.header("X-Login-Email").map(|s| s.to_string())
}

/// `LoginThrottleMiddleware` default policy is `FailClosed` — when the
/// backend errors (no torii bound here), the middleware MUST refuse
/// the request with 503 + `Retry-After: 1` rather than letting it
/// through to the login handler.
#[test]
#[serial]
fn default_policy_fail_closed_returns_503() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let throttle = LoginThrottleMiddleware::new(email_extractor());
        let router = Router::new()
            .post("/login", |_req| async { text("login-ok") })
            .middleware(throttle);
        let addr = spawn_server(router, 5).await;

        let (status, retry_after) = post_login(addr).await;
        assert_eq!(
            status, 503,
            "fail-closed must short-circuit on backend error"
        );
        assert_eq!(
            retry_after.as_deref(),
            Some("1"),
            "fail-closed must advertise Retry-After: 1"
        );
    });
}

/// Operators who pick `BackendErrorPolicy::FailOpen` get the previous
/// behaviour: the request passes through to the login handler when
/// the backend errors.
#[test]
#[serial]
fn fail_open_policy_passes_through_to_handler() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
        let throttle = LoginThrottleMiddleware::new(email_extractor())
            .on_backend_error(BackendErrorPolicy::FailOpen);
        let router = Router::new()
            .post("/login", |_req| async { text("login-ok") })
            .middleware(throttle);
        let addr = spawn_server(router, 5).await;

        let (status, _) = post_login(addr).await;
        assert_eq!(
            status, 200,
            "fail-open must let the request reach the login handler"
        );
    });
}
