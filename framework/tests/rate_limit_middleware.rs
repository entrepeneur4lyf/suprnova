//! Integration tests for `RateLimitMiddleware`.
//!
//! Verifies that the sliding-window middleware enforces per-key quotas and
//! returns HTTP 429 with a `Retry-After` header when the quota is exhausted.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

use suprnova::http::text;
use suprnova::rate_limit::memory::InMemoryRateLimiter;
use suprnova::rate_limit::{
    BackendErrorPolicy, RateLimitMiddleware, RateLimiterDriver, SlidingWindowConfig,
};
use suprnova::{FrameworkError, MiddlewareRegistry, Router, async_trait, handle_request};

/// Spawn a test HTTP/1.1 server bound to an ephemeral port, dispatch
/// up to `accepts` connections via `handle_request`, then exit.
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

/// Issue a single GET request and return `(status_code, retry_after_header)`.
async fn get(addr: SocketAddr, path: &str) -> (u16, Option<String>) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method("GET")
        .uri(path)
        .header("Host", "localhost")
        .header("Content-Length", "0")
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

/// The middleware must allow up to `max_requests` and then return 429.
/// The 429 response must include a `Retry-After` header.
#[tokio::test]
async fn middleware_enforces_per_route_quota_and_returns_429_with_retry_after() {
    let limiter: Arc<dyn RateLimiterDriver> = Arc::new(InMemoryRateLimiter::new());
    let cfg = SlidingWindowConfig {
        max_requests: 2,
        window: Duration::from_secs(60),
    };
    let mw = RateLimitMiddleware::new(limiter, cfg, |req| format!("route:{}", req.path()));

    let router = Router::new()
        .get("/ping", |_req| async { text("pong") })
        .middleware(mw);

    // 5 accepts: 3 for the three test requests + slack for TCP overhead.
    let addr = spawn_server(router, 5).await;

    let (s1, _) = get(addr, "/ping").await;
    let (s2, _) = get(addr, "/ping").await;
    let (s3, retry) = get(addr, "/ping").await;

    assert_eq!(s1, 200, "first request within quota must succeed");
    assert_eq!(s2, 200, "second request within quota must succeed");
    assert_eq!(s3, 429, "third request must be rejected (quota = 2)");
    assert!(
        retry.is_some(),
        "429 response must include a Retry-After header; got: {:?}",
        retry
    );
}

/// A static key function ("global") puts all routes in the same bucket.
/// After the quota is exhausted any path returns 429 — verifying that
/// the key closure drives bucket selection independently of routing.
#[tokio::test]
async fn middleware_key_fn_drives_bucket_selection() {
    let limiter: Arc<dyn RateLimiterDriver> = Arc::new(InMemoryRateLimiter::new());
    let cfg = SlidingWindowConfig {
        max_requests: 2,
        window: Duration::from_secs(60),
    };
    // All requests share one global bucket regardless of path.
    let mw = RateLimitMiddleware::new(limiter, cfg, |_req| "global".to_string());

    let router = Router::new()
        .get("/ping", |_req| async { text("pong") })
        .middleware(mw);

    let addr = spawn_server(router, 5).await;

    let (s1, _) = get(addr, "/ping").await;
    let (s2, _) = get(addr, "/ping").await;
    let (s3, retry) = get(addr, "/ping").await;

    assert_eq!(s1, 200, "first request within global quota");
    assert_eq!(s2, 200, "second request within global quota");
    assert_eq!(s3, 429, "third request must be rejected by global bucket");
    assert!(retry.is_some(), "429 must carry Retry-After");
}

/// A `RateLimiter` whose backend always errors — models Redis being
/// unreachable. Used to exercise the `BackendErrorPolicy` branches, which are
/// distinct from the over-quota (429) path.
struct FailingLimiter;

#[async_trait]
impl RateLimiterDriver for FailingLimiter {
    async fn try_acquire(
        &self,
        _key: &str,
        _config: &SlidingWindowConfig,
    ) -> Result<bool, FrameworkError> {
        Err(FrameworkError::internal(
            "rate limiter backend unreachable (test)",
        ))
    }

    async fn retry_after(
        &self,
        _key: &str,
        _config: &SlidingWindowConfig,
    ) -> Result<Option<Duration>, FrameworkError> {
        Err(FrameworkError::internal(
            "rate limiter backend unreachable (test)",
        ))
    }
}

/// Build a router whose `/ping` handler flips `flag` to `true` when reached,
/// guarded by `mw`. The flag is what gives the backend-error tests teeth: a
/// status code alone cannot distinguish "handler ran" from "handler skipped".
fn router_with_flag<F>(mw: RateLimitMiddleware<F>, flag: Arc<AtomicBool>) -> Router
where
    F: Fn(&suprnova::Request) -> String + Send + Sync + 'static,
{
    Router::new()
        .get("/ping", move |_req| {
            let flag = flag.clone();
            async move {
                flag.store(true, Ordering::SeqCst);
                text("pong")
            }
        })
        .middleware(mw)
        .into()
}

/// Default policy is fail-open: when the backend errors the request is passed
/// through to the handler. The handler flips a shared flag, so the test proves
/// the request actually reached it — a 200 alone could come from anywhere.
#[tokio::test]
async fn backend_error_fails_open_by_default_and_runs_handler() {
    let limiter: Arc<dyn RateLimiterDriver> = Arc::new(FailingLimiter);
    let cfg = SlidingWindowConfig {
        max_requests: 2,
        window: Duration::from_secs(60),
    };
    // No .on_backend_error(...) — exercise the default.
    let mw = RateLimitMiddleware::new(limiter, cfg, |_req| "global".to_string());

    let handler_ran = Arc::new(AtomicBool::new(false));
    let router = router_with_flag(mw, handler_ran.clone());

    let addr = spawn_server(router, 3).await;
    let (status, _retry) = get(addr, "/ping").await;

    assert_eq!(
        status, 200,
        "fail-open default must pass the request through on a backend error"
    );
    assert!(
        handler_ran.load(Ordering::SeqCst),
        "fail-open must actually reach the handler, not merely return 200"
    );
}

/// With `BackendErrorPolicy::FailClosed`, a backend error rejects the request
/// with HTTP 503 + `Retry-After` and the handler is NEVER reached. Asserting
/// the flag stays false discriminates a real fail-closed from infrastructure
/// returning 503 for an unrelated reason.
#[tokio::test]
async fn backend_error_fails_closed_returns_503_and_skips_handler() {
    let limiter: Arc<dyn RateLimiterDriver> = Arc::new(FailingLimiter);
    let cfg = SlidingWindowConfig {
        max_requests: 2,
        window: Duration::from_secs(60),
    };
    let mw = RateLimitMiddleware::new(limiter, cfg, |_req| "global".to_string())
        .on_backend_error(BackendErrorPolicy::FailClosed);

    let handler_ran = Arc::new(AtomicBool::new(false));
    let router = router_with_flag(mw, handler_ran.clone());

    let addr = spawn_server(router, 3).await;
    let (status, retry) = get(addr, "/ping").await;

    assert_eq!(
        status, 503,
        "fail-closed must reject with 503 when the backend errors"
    );
    assert_eq!(
        retry.as_deref(),
        Some("1"),
        "fail-closed 503 must carry a Retry-After header"
    );
    assert!(
        !handler_ran.load(Ordering::SeqCst),
        "fail-closed must NOT reach the handler"
    );
}
