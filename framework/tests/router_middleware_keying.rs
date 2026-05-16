//! Regression tests for route middleware keying.
//!
//! `Router::route_middleware` was previously keyed by path string
//! alone. That meant registering `POST /api/posts` under an
//! auth-bearing route group, then registering `GET /api/posts`
//! without middleware, caused the GET to silently inherit the POST's
//! auth middleware. Symmetrically, registering an auth-only
//! `POST /api/posts` after a public `GET /api/posts` made the
//! public GET start running auth.
//!
//! That bug was security-shaped:
//!   1. A route meant to be public could be auth-gated (DoS /
//!      availability bug).
//!   2. A route meant to be authenticated could leak through the
//!      public sibling on the same path (information disclosure when
//!      the auth-gated route was registered first and a clean public
//!      alternate landed on the same path).
//!
//! These tests pin the fix: middleware is keyed by `(Method, Path)`
//! and never bleeds across HTTP methods on the same path.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use suprnova::http::text;
use suprnova::{
    handle_request, MiddlewareRegistry, Request, Response, Router,
};
use suprnova::{Middleware, Next};

/// Test middleware that records every request that passes through
/// it under a stable tag. The handlers in these tests do NOT push to
/// the tracker — only this middleware does. That keeps the assertion
/// crisp: "did the middleware run" is exactly "did the tag land in
/// the tracker".
#[derive(Clone)]
struct TaggingMiddleware {
    tag: &'static str,
    tracker: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl Middleware for TaggingMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        self.tracker.lock().unwrap().push(self.tag);
        next(request).await
    }
}

/// Stand up a fresh listener bound to a system-assigned port, accept
/// up to `accepts` connections, and dispatch each via `handle_request`
/// against the supplied router. Returns the bound `SocketAddr`. The
/// spawned task exits once `accepts` connections have been served.
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
                    async move {
                        Ok::<_, Infallible>(handle_request(router, middleware, req).await)
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, svc)
                    .await;
            });
        }
    });

    addr
}

/// Issue a single HTTP/1.1 request against the test server and return
/// the status + body bytes. Empty body, no cookies — these are
/// routing-layer assertions, not auth assertions.
async fn send_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
) -> (hyper::http::StatusCode, Bytes) {
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let io = TokioIo::new(stream);
    let (mut sender, conn) =
        hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .method(method)
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
    let collected = body.collect().await.unwrap().to_bytes();
    (parts.status, collected)
}

/// 1. Middleware on `POST /x` MUST NOT run for `GET /x`, and vice
///    versa. This is the canonical leak: register one method with
///    middleware, register the other without — the second must stay
///    clean.
#[tokio::test]
async fn middleware_does_not_leak_across_methods_same_path() {
    let auth_calls = Arc::new(Mutex::new(Vec::<&'static str>::new()));

    let router = Router::new()
        .post("/api/posts", |_req| async { text("created") })
        .middleware(TaggingMiddleware {
            tag: "auth",
            tracker: auth_calls.clone(),
        })
        // Public GET on the SAME path string. Must NOT inherit the
        // POST's auth middleware.
        .get("/api/posts", |_req| async { text("listed") });

    let addr = spawn_server(router, 4).await;

    // Drive a GET — auth middleware must not run, response is 200.
    let (get_status, _) = send_request(addr, "GET", "/api/posts").await;
    assert_eq!(get_status.as_u16(), 200);
    assert!(
        auth_calls.lock().unwrap().is_empty(),
        "auth middleware leaked onto GET /api/posts: {:?}",
        auth_calls.lock().unwrap()
    );

    // Drive a POST — auth middleware MUST run exactly once.
    let (post_status, _) = send_request(addr, "POST", "/api/posts").await;
    assert_eq!(post_status.as_u16(), 200);
    assert_eq!(
        *auth_calls.lock().unwrap(),
        vec!["auth"],
        "auth middleware must run for POST /api/posts"
    );
}

/// 2. Middleware attached via the chained `.middleware()` after a
///    `.get(...)` call binds ONLY to that GET, not to a sibling POST
///    on the same path that was registered before it. Mirrors the
///    real-world dogfood pattern in `app/src/routes.rs` where a
///    public listing GET coexists with an auth-gated POST.
#[tokio::test]
async fn middleware_on_get_does_not_leak_to_existing_post_same_path() {
    let auth_calls = Arc::new(Mutex::new(Vec::<&'static str>::new()));

    // Register POST first (clean), then GET (with middleware). Before
    // the fix the POST would have inherited the GET's middleware
    // because the entry under the shared path key would accumulate.
    let router = Router::new()
        .post("/api/posts", |_req| async { text("created") })
        .get("/api/posts", |_req| async { text("listed") })
        .middleware(TaggingMiddleware {
            tag: "auth",
            tracker: auth_calls.clone(),
        });

    let addr = spawn_server(router, 4).await;

    // POST should be clean.
    let (post_status, _) = send_request(addr, "POST", "/api/posts").await;
    assert_eq!(post_status.as_u16(), 200);
    assert!(
        auth_calls.lock().unwrap().is_empty(),
        "auth middleware leaked onto POST: {:?}",
        auth_calls.lock().unwrap()
    );

    // GET should run the middleware.
    let (get_status, _) = send_request(addr, "GET", "/api/posts").await;
    assert_eq!(get_status.as_u16(), 200);
    assert_eq!(*auth_calls.lock().unwrap(), vec!["auth"]);
}

/// 3. Group middleware applied to `POST /api/posts` (via `group!`)
///    MUST NOT extend to a sibling `GET /api/posts` that was
///    registered outside the group on the same final path. This is
///    the exact dogfood layout in `app/src/routes.rs`.
#[tokio::test]
async fn group_middleware_does_not_leak_to_sibling_routes_outside_group() {
    use suprnova::{get, group, post};

    let group_calls = Arc::new(Mutex::new(Vec::<&'static str>::new()));
    let tracker = group_calls.clone();

    // Public GET registered first, outside any group.
    let router = get!("/api/posts", |_req: Request| async { text("listed") })
        .register(Router::new());

    // Auth-gated POST registered inside a group whose middleware
    // applies only to routes within it.
    let router = group!("/api/posts", {
        post!("/", |_req: Request| async { text("created") }),
    })
    .middleware(TaggingMiddleware {
        tag: "group",
        tracker,
    })
    .register(router);

    let addr = spawn_server(router, 4).await;

    // GET must not see group middleware.
    let (get_status, _) = send_request(addr, "GET", "/api/posts").await;
    assert_eq!(get_status.as_u16(), 200);
    assert!(
        group_calls.lock().unwrap().is_empty(),
        "group middleware leaked onto sibling GET: {:?}",
        group_calls.lock().unwrap()
    );

    // POST must run group middleware exactly once.
    let (post_status, _) = send_request(addr, "POST", "/api/posts").await;
    assert_eq!(post_status.as_u16(), 200);
    assert_eq!(*group_calls.lock().unwrap(), vec!["group"]);
}

/// 4. Middleware on one path MUST NOT apply to a different path.
///    Sanity check that path-component of the key still works — a
///    naive fix that drops the path entirely would pass tests 1-3
///    but fail this one.
#[tokio::test]
async fn middleware_on_one_path_does_not_apply_to_another_path() {
    let calls = Arc::new(Mutex::new(Vec::<&'static str>::new()));

    let router = Router::new()
        .post("/a", |_req| async { text("a") })
        .middleware(TaggingMiddleware {
            tag: "a-mw",
            tracker: calls.clone(),
        })
        .post("/b", |_req| async { text("b") });

    let addr = spawn_server(router, 4).await;

    // Hitting /b must not run /a's middleware.
    let (b_status, _) = send_request(addr, "POST", "/b").await;
    assert_eq!(b_status.as_u16(), 200);
    assert!(
        calls.lock().unwrap().is_empty(),
        "middleware on /a leaked to /b: {:?}",
        calls.lock().unwrap()
    );

    // Hitting /a runs the middleware exactly once.
    let (a_status, _) = send_request(addr, "POST", "/a").await;
    assert_eq!(a_status.as_u16(), 200);
    assert_eq!(*calls.lock().unwrap(), vec!["a-mw"]);
}
