//! Integration tests for the verb-gap fix.
//!
//! These tests drive `handle_request` end-to-end through a real hyper
//! connection so they cover the parts the inline router tests can't:
//! HEAD body strip on the wire (RFC 9110 §9.3.2), HEAD→GET middleware
//! inheritance when no explicit HEAD route is registered, PATCH and
//! OPTIONS dispatch through the full middleware chain, and explicit
//! HEAD handlers winning over the GET fallback. Each test pins one
//! contract; the assertions document the invariant inline.

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
use suprnova::{Middleware, Next};
use suprnova::{MiddlewareRegistry, Request, Response, Router, handle_request};

/// Middleware that records each call under a tag, mirroring the
/// pattern used by `router_middleware_keying.rs`.
#[derive(Clone)]
struct TaggingMiddleware {
    tag: &'static str,
    tracker: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl Middleware for TaggingMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        self.tracker
            .lock()
            .expect("tracker lock poisoned")
            .push(self.tag);
        next(request).await
    }
}

/// Spawn an ephemeral hyper server that serves `accepts` connections
/// through `handle_request` against the supplied router. Returns the
/// bound socket address.
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

/// Send an HTTP/1.1 request and capture status + headers + body.
async fn send_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
) -> (hyper::http::StatusCode, hyper::HeaderMap, Bytes) {
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

    let resp = tokio::time::timeout(Duration::from_secs(5), sender.send_request(req))
        .await
        .expect("send_request timeout")
        .expect("hyper send_request");
    let (parts, body) = resp.into_parts();
    let collected = body.collect().await.expect("collect body bytes").to_bytes();
    (parts.status, parts.headers, collected)
}

/// HEAD against a GET-only route succeeds (RFC 9110 §9.3.2 fallback)
/// and returns the GET status with the body stripped to zero bytes.
#[tokio::test]
async fn head_falls_back_to_get_and_strips_body() {
    let router = Router::new().get("/articles", |_req| async { text("a long article body") });

    let addr = spawn_server(router, 2).await;
    let (status, _headers, body) = send_request(addr, "HEAD", "/articles").await;

    assert_eq!(status.as_u16(), 200, "HEAD must inherit GET's 200");
    assert!(
        body.is_empty(),
        "HEAD body must be empty after strip; got {body:?}",
    );

    // Sanity: the same GET still returns the body.
    let (get_status, _, get_body) = send_request(addr, "GET", "/articles").await;
    assert_eq!(get_status.as_u16(), 200);
    assert_eq!(get_body, "a long article body");
}

/// Middleware attached to the GET route runs for HEAD requests that
/// fall back to it. Without `has_explicit_head` driving the effective
/// method, auth / CSRF / rate-limit middleware would silently skip on
/// HEAD probes.
#[tokio::test]
async fn head_fallback_inherits_get_route_middleware() {
    let calls = Arc::new(Mutex::new(Vec::<&'static str>::new()));

    let router = Router::new()
        .get("/secured", |_req| async { text("ok") })
        .middleware(TaggingMiddleware {
            tag: "auth",
            tracker: calls.clone(),
        });

    let addr = spawn_server(router, 2).await;
    let (head_status, _, head_body) = send_request(addr, "HEAD", "/secured").await;
    assert_eq!(head_status.as_u16(), 200);
    assert!(head_body.is_empty(), "HEAD body must be empty");

    assert_eq!(
        *calls.lock().expect("tracker lock"),
        vec!["auth"],
        "GET middleware MUST run for the HEAD fallback request",
    );
}

/// An explicit HEAD handler wins over the GET fallback. Use case:
/// returning bespoke headers without running the GET body
/// computation. The HEAD handler's body is still stripped on the wire.
#[tokio::test]
async fn explicit_head_handler_wins_over_get_fallback() {
    use suprnova::http::HttpResponse;

    let router = Router::new()
        .get("/cached", |_req| async { text("expensive payload") })
        .head("/cached", |_req| async {
            Ok::<HttpResponse, HttpResponse>(
                HttpResponse::new()
                    .status(200)
                    .header("X-Cache-Status", "HIT"),
            )
        });

    let addr = spawn_server(router, 1).await;
    let (status, headers, body) = send_request(addr, "HEAD", "/cached").await;
    assert_eq!(status.as_u16(), 200);
    assert!(body.is_empty(), "HEAD body always empty on the wire");
    assert_eq!(
        headers
            .get("X-Cache-Status")
            .map(|v| v.to_str().expect("header utf8")),
        Some("HIT"),
        "explicit HEAD handler must have run (its header is on the response)",
    );
}

/// PATCH route registers + dispatches end-to-end through the full
/// middleware chain.
#[tokio::test]
async fn patch_route_dispatches_end_to_end() {
    let router = Router::new().patch("/posts/:id", |_req| async { text("patched") });
    let addr = spawn_server(router, 1).await;
    let (status, _, body) = send_request(addr, "PATCH", "/posts/42").await;
    assert_eq!(status.as_u16(), 200);
    assert_eq!(body, "patched");
}

/// OPTIONS route registers + dispatches end-to-end. (CORS preflight
/// short-circuits in middleware before this layer; this exercises the
/// non-preflight discovery path.)
#[tokio::test]
async fn options_route_dispatches_end_to_end() {
    let router = Router::new().options("/api/posts", |_req| async { text("GET, POST, PATCH") });
    let addr = spawn_server(router, 1).await;
    let (status, _, body) = send_request(addr, "OPTIONS", "/api/posts").await;
    assert_eq!(status.as_u16(), 200);
    assert_eq!(body, "GET, POST, PATCH");
}

/// HEAD against a path that has neither HEAD nor GET registered falls
/// through to the 404 chain (RequestId + global middleware still runs
/// per the no-route policy, terminating in a fixed 404).
#[tokio::test]
async fn head_against_unrouted_path_returns_404() {
    let router = Router::new().post("/submit", |_req| async { text("created") });
    let addr = spawn_server(router, 1).await;
    let (status, _, body) = send_request(addr, "HEAD", "/submit").await;
    assert_eq!(status.as_u16(), 404);
    assert!(body.is_empty(), "HEAD bodies are always empty");
}

/// User-call-site smoke test for the `patch!` / `head!` / `options!`
/// macros — exercises the full chain: `#[macro_export]` macro →
/// `__verb_impl` → `RouteDefBuilder` → `RouteDefBuilder::register` →
/// `Router::{patch, head, options}`. If any of those links break, this
/// test fails before the integration tests above can.
///
/// Uses the macros at top-level (the way user code does), nested
/// `.name()` chaining, and inside a `group!{}` so the GroupDef arm
/// for the new variants is exercised too.
#[tokio::test]
async fn macro_form_patch_head_options_register_and_dispatch() {
    use hyper::Method;
    use suprnova::{group, head, options, patch, routes};

    routes! {
        patch!("/things/:id", |_req| async { text("patched-thing") }).name("things.patch"),
        head!("/probe", |_req| async { text("ignored body") }),
        options!("/discover", |_req| async { text("GET, PATCH") }),
        group!("/api", {
            patch!("/users/:id", |_req| async { text("patched-api-user") }),
            head!("/users", |_req| async { text("ignored") }),
            options!("/users", |_req| async { text("GET, POST, PATCH") }),
        }),
    }

    let router = register();

    // Top-level macro registrations match.
    assert!(router.match_route(&Method::PATCH, "/things/42").is_some());
    assert!(router.match_route(&Method::HEAD, "/probe").is_some());
    assert!(router.match_route(&Method::OPTIONS, "/discover").is_some());

    // Grouped macro registrations match, prefix is applied.
    assert!(router.match_route(&Method::PATCH, "/api/users/7").is_some());
    assert!(router.match_route(&Method::HEAD, "/api/users").is_some());
    assert!(router.match_route(&Method::OPTIONS, "/api/users").is_some());

    // Drive a PATCH end-to-end to prove the dispatch path runs the
    // handler, not just registers the route.
    let addr = spawn_server(router, 1).await;
    let (status, _, body) = send_request(addr, "PATCH", "/things/42").await;
    assert_eq!(status.as_u16(), 200);
    assert_eq!(body, "patched-thing");
}

// `routes! {}` expands to a `pub fn register() -> Router` whose body
// can't capture local environment (fn items have no closures over
// outer state). The two tests below need to compare middleware
// invocation counts against a shared `Vec`, so we use a process-wide
// `OnceLock<Mutex<Vec<&str>>>` plus `#[serial]` to keep them from
// stepping on each other.
use serial_test::serial;
use std::sync::OnceLock;

fn any_macro_tracker() -> &'static std::sync::Mutex<Vec<&'static str>> {
    static T: OnceLock<std::sync::Mutex<Vec<&'static str>>> = OnceLock::new();
    T.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// `any!()` at the routes! top level registers the handler against
/// every common HTTP method, fans middleware across all seven, and
/// registers the name once. Drives the full chain via `handle_request`
/// against GET, POST, PUT, PATCH, DELETE, OPTIONS and confirms the
/// shared handler responds + the shared middleware ran for each.
#[tokio::test]
#[serial]
async fn any_macro_dispatches_all_methods_and_fans_middleware() {
    use suprnova::{any, routes};

    any_macro_tracker().lock().expect("tracker lock").clear();

    routes! {
        any!("/webhook", |_req| async { text("any-response") })
            .name("webhook.any.macro")
            .middleware(TaggingMiddleware {
                tag: "shared-auth",
                tracker: Arc::new(std::sync::Mutex::new(Vec::new())),
            })
            .middleware(StaticTracker { tag: "any-counter" }),
    }

    let router = register();
    let addr = spawn_server(router, 6).await;

    for method in ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
        let (status, _, body) = send_request(addr, method, "/webhook").await;
        assert_eq!(status.as_u16(), 200, "any! must respond 200 for {method}",);
        assert_eq!(body, "any-response");
    }

    let log = any_macro_tracker().lock().expect("tracker lock").clone();
    assert_eq!(
        log,
        vec![
            "any-counter",
            "any-counter",
            "any-counter",
            "any-counter",
            "any-counter",
            "any-counter",
        ],
        "middleware must fan out across every method (6 hits, one per verb)",
    );
}

/// `any!()` inside `group!{}` inherits the group prefix AND group
/// middleware fans across every verb of the multi-method route.
/// Exercises the `GroupItem::AnyRoute` arm in
/// `GroupDef::register_with_inherited`.
#[tokio::test]
#[serial]
async fn any_macro_inside_group_inherits_prefix_and_middleware() {
    use hyper::Method;
    use suprnova::{any, group, routes};

    any_macro_tracker().lock().expect("tracker lock").clear();

    routes! {
        group!("/api", {
            any!("/anything", |_req| async { text("g-any") }),
        }).middleware(StaticTracker { tag: "group-auth" }),
    }

    let router = register();
    for m in [Method::GET, Method::POST, Method::PATCH] {
        assert!(
            router.match_route(&m, "/api/anything").is_some(),
            "any! inside group must register {m} at /api/anything",
        );
    }

    let addr = spawn_server(router, 1).await;
    let (status, _, body) = send_request(addr, "POST", "/api/anything").await;
    assert_eq!(status.as_u16(), 200);
    assert_eq!(body, "g-any");
    assert_eq!(
        *any_macro_tracker().lock().expect("tracker lock"),
        vec!["group-auth"],
        "group middleware must fire on the any-route POST",
    );
}

/// `routes!{}`-friendly middleware: captures its tag and the global
/// `any_macro_tracker()` static, no per-instance state. The earlier
/// `TaggingMiddleware` carries an `Arc<Mutex<...>>` field which means
/// a fresh instance is needed per test — `StaticTracker` shares one
/// process-global Vec so the routes! body (a `fn` item) can construct
/// it inline.
#[derive(Clone)]
struct StaticTracker {
    tag: &'static str,
}

#[async_trait]
impl Middleware for StaticTracker {
    async fn handle(&self, request: Request, next: Next) -> Response {
        any_macro_tracker()
            .lock()
            .expect("tracker lock")
            .push(self.tag);
        next(request).await
    }
}
