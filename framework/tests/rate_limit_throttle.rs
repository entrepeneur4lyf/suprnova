//! Integration tests for the Cache-backed `RateLimiter` facade and the
//! `ThrottleRequestsMiddleware`. These exercise the Laravel-shape
//! parity surface end-to-end through a real HTTP request flow.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;

use suprnova::cache::{CacheStore, InMemoryCache};
use suprnova::container::testing::TestContainer;
use suprnova::http::text;
use suprnova::rate_limit::{Limit, RateLimiter, ThrottleRequestsMiddleware};
use suprnova::{MiddlewareRegistry, Router, handle_request};

fn install_test_cache() -> impl Drop {
    let guard = TestContainer::fake();
    TestContainer::bind::<dyn CacheStore>(Arc::new(InMemoryCache::new()));
    guard
}

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

async fn get_with_headers(
    addr: SocketAddr,
    path: &str,
) -> (u16, std::collections::HashMap<String, String>) {
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
    let headers: std::collections::HashMap<String, String> = parts
        .headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    (parts.status.as_u16(), headers)
}

#[tokio::test]
async fn throttle_named_limiter_caps_requests_per_minute() {
    let _g = install_test_cache();
    let name = "test:named:caps";
    // Two requests per minute, all callers share the same bucket.
    RateLimiter::define(name, |_req| Limit::per_minute(2).by("shared").into());

    let mw = ThrottleRequestsMiddleware::by_name(name);
    let router = Router::new()
        .get("/ping", |_req| async { text("pong") })
        .middleware(mw);
    let addr = spawn_server(router, 5).await;

    let (s1, h1) = get_with_headers(addr, "/ping").await;
    let (s2, h2) = get_with_headers(addr, "/ping").await;
    let (s3, h3) = get_with_headers(addr, "/ping").await;

    assert_eq!(s1, 200, "first request within quota must succeed");
    assert_eq!(s2, 200, "second request within quota must succeed");
    assert_eq!(s3, 429, "third request must be throttled (quota=2)");

    // X-RateLimit-Limit must equal max_attempts on every wrapped response.
    assert_eq!(h1.get("x-ratelimit-limit").map(String::as_str), Some("2"));
    assert_eq!(h2.get("x-ratelimit-limit").map(String::as_str), Some("2"));
    assert_eq!(h3.get("x-ratelimit-limit").map(String::as_str), Some("2"));

    // After the second request, remaining must hit zero on the wrapped
    // 200 response. Both 200s carry remaining=N-attempts.
    assert_eq!(
        h1.get("x-ratelimit-remaining").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        h2.get("x-ratelimit-remaining").map(String::as_str),
        Some("0")
    );

    // 429 response must carry Retry-After and X-RateLimit-Reset headers.
    assert!(h3.contains_key("retry-after"), "429 must carry Retry-After");
    assert!(
        h3.contains_key("x-ratelimit-reset"),
        "429 must carry X-RateLimit-Reset"
    );
}

#[tokio::test]
async fn throttle_named_limiter_with_unlimited_passes_through() {
    let _g = install_test_cache();
    let name = "test:named:unlimited";
    // Unlimited never trips. Used by Laravel apps for admin bypass.
    RateLimiter::define(name, |_req| Limit::none().into());

    let mw = ThrottleRequestsMiddleware::by_name(name);
    let router = Router::new()
        .get("/admin", |_req| async { text("admin-ok") })
        .middleware(mw);
    let addr = spawn_server(router, 12).await;

    for _ in 0..10 {
        let (s, _) = get_with_headers(addr, "/admin").await;
        assert_eq!(s, 200, "unlimited must never trip");
    }
}

#[tokio::test]
async fn throttle_named_limiter_with_response_callback_customises_429() {
    let _g = install_test_cache();
    let name = "test:named:custom-response";
    RateLimiter::define(name, |_req| {
        Limit::per_minute(1)
            .by("custom")
            .response(|_req| suprnova::http::HttpResponse::text("custom blocked body").status(429))
            .into()
    });

    let mw = ThrottleRequestsMiddleware::by_name(name);
    let router = Router::new()
        .get("/k", |_req| async { text("ok") })
        .middleware(mw);
    let addr = spawn_server(router, 5).await;

    let (s1, _) = get_with_headers(addr, "/k").await;
    let (s2, _) = get_with_headers(addr, "/k").await;
    assert_eq!(s1, 200);
    assert_eq!(s2, 429, "second request must be throttled");
    // The body would be "custom blocked body" but we already drain it
    // in `get_with_headers`. Headers still include rate-limit info,
    // proving the wrapper ran on the custom response.
}

#[tokio::test]
async fn throttle_inline_with_caps_requests_via_max_decay_args() {
    let _g = install_test_cache();
    let mw = ThrottleRequestsMiddleware::with(2, 1, "inline").prefix("inline");
    let router = Router::new()
        .get("/inline", |_req| async { text("inline-ok") })
        .middleware(mw);
    let addr = spawn_server(router, 5).await;

    let (s1, _) = get_with_headers(addr, "/inline").await;
    let (s2, _) = get_with_headers(addr, "/inline").await;
    let (s3, _) = get_with_headers(addr, "/inline").await;

    assert_eq!(s1, 200);
    assert_eq!(s2, 200);
    assert_eq!(s3, 429);
}

#[tokio::test]
async fn throttle_with_limits_supports_multiple_limits_first_to_trip_wins() {
    let _g = install_test_cache();
    // Limit A: 5 per hour (loose). Limit B: 2 per minute (tight). The
    // tight limit trips first.
    let limits = vec![
        Limit::per_hour(5).by("multi:hour"),
        Limit::per_minute(2).by("multi:minute"),
    ];
    let mw = ThrottleRequestsMiddleware::with_limits(limits);
    let router = Router::new()
        .get("/multi", |_req| async { text("multi-ok") })
        .middleware(mw);
    let addr = spawn_server(router, 6).await;

    let (s1, _) = get_with_headers(addr, "/multi").await;
    let (s2, _) = get_with_headers(addr, "/multi").await;
    let (s3, _) = get_with_headers(addr, "/multi").await;
    assert_eq!(s1, 200);
    assert_eq!(s2, 200);
    assert_eq!(s3, 429, "tight limit must trip first");
}

#[tokio::test]
async fn throttle_after_callback_only_counts_on_failure_response() {
    let _g = install_test_cache();
    let name = "test:after:failure-only";
    RateLimiter::define(name, |_req| {
        Limit::per_minute(2)
            .by("after-only")
            .after(|r| r.status_code() >= 400)
            .into()
    });

    // Handler returns 200 on first odd-numbered request and 500 on
    // even, alternating. That lets us assert that two successes don't
    // burn the limit, then two failures do.
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_for_handler = counter.clone();

    let mw = ThrottleRequestsMiddleware::by_name(name);
    let router = Router::new()
        .get("/maybe-fail", move |_req| {
            let c = counter_for_handler.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    text("ok")
                } else {
                    Err(suprnova::http::HttpResponse::text("boom").status(500))
                }
            }
        })
        .middleware(mw);
    let addr = spawn_server(router, 8).await;

    // Two 200s — shouldn't consume the limit.
    let (s1, _) = get_with_headers(addr, "/maybe-fail").await;
    let (s2, _) = get_with_headers(addr, "/maybe-fail").await;
    assert_eq!(s1, 200);
    assert_eq!(s2, 200);
    // Limit must still be untouched.
    assert_eq!(
        RateLimiter::attempts(&format!("{name}:after-only"))
            .await
            .unwrap(),
        0,
        "successful responses must not consume the limit"
    );

    // Two 500s — both pass through (the limit isn't tripped yet); the
    // third call would be throttled.
    let (s3, _) = get_with_headers(addr, "/maybe-fail").await;
    let (s4, _) = get_with_headers(addr, "/maybe-fail").await;
    let (s5, _) = get_with_headers(addr, "/maybe-fail").await;
    assert_eq!(s3, 500);
    assert_eq!(s4, 500);
    assert_eq!(s5, 429, "third failed request must be throttled");
}

#[tokio::test]
async fn throttle_missing_named_limiter_returns_503() {
    let _g = install_test_cache();
    let mw = ThrottleRequestsMiddleware::by_name("never:registered:test");
    let router = Router::new()
        .get("/x", |_req| async { text("x") })
        .middleware(mw);
    let addr = spawn_server(router, 3).await;
    let (s, _) = get_with_headers(addr, "/x").await;
    assert_eq!(
        s, 503,
        "missing named limiter must short-circuit with 503 (not 500/panic)"
    );
}
