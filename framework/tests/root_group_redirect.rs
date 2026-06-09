//! Regression tests for two HTTP-layer defects surfaced by smoke-testing
//! a scaffolded app over a real connection:
//!
//! 1. `group!("/", { ... })` registered `//login`-style patterns that
//!    matchit can never match — every route in a root-prefix group
//!    404'd over the wire even though facade-level tests passed.
//! 2. `redirect!("/path")` resolved its argument as a route *name*,
//!    500ing at runtime in any app that names no routes.
//!
//! Both defects are pinned end-to-end through a real hyper connection
//! in the exact shape the scaffolder emits (`routes.rs.tpl` /
//! `auth.rs.tpl`): root-prefix groups with middleware must serve, and a
//! handler returning `redirect!("/literal")` must answer 302 with that
//! literal in `Location`.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::sync::Arc;
use suprnova::http::text;
use suprnova::{
    Middleware, MiddlewareRegistry, Next, Request, Response, Router, handle_request, redirect,
};

/// Stateless pass-through middleware, standing in for the scaffold's
/// `guest()` / `auth()` group middleware. Being a unit struct it can be
/// instantiated inside the `routes!`-generated `register()` fn (which
/// cannot capture environment).
#[derive(Clone)]
struct NoopMw;

#[async_trait]
impl Middleware for NoopMw {
    async fn handle(&self, request: Request, next: Next) -> Response {
        next(request).await
    }
}

/// Spawn an ephemeral hyper server that serves `accepts` connections
/// through `handle_request` against the supplied router. Returns the
/// bound socket address. Mirrors the harness in `routing_verbs.rs`.
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

/// The scaffold's `routes.rs.tpl` shape — two root-prefix groups, each
/// with group middleware — serves every route over real HTTP. Before
/// the `join_paths` fix these all registered as `//login` etc. and
/// 404'd on the wire.
#[tokio::test]
async fn root_prefix_groups_serve_over_http() {
    use suprnova::{get, group, post, routes};

    routes! {
        group!("/", {
            get!("/login", |_req| async { text("login page") }),
            post!("/login", |_req| async { text("logged in") }),
        })
        .middleware(NoopMw),

        group!("/", {
            get!("/dashboard", |_req| async { text("dashboard") }),
        })
        .middleware(NoopMw),
    }

    let router = register();
    let addr = spawn_server(router, 4).await;

    let (status, _, body) = send_request(addr, "GET", "/login").await;
    assert_eq!(
        status.as_u16(),
        200,
        "GET /login through a root-prefix group must match, not 404",
    );
    assert_eq!(body, "login page");

    let (status, _, body) = send_request(addr, "POST", "/login").await;
    assert_eq!(status.as_u16(), 200);
    assert_eq!(body, "logged in");

    let (status, _, body) = send_request(addr, "GET", "/dashboard").await;
    assert_eq!(status.as_u16(), 200);
    assert_eq!(body, "dashboard");

    // The broken pattern must NOT have been registered: a literal
    // `//login` request stays 404 rather than matching an empty segment.
    let (status, _, _) = send_request(addr, "GET", "//login").await;
    assert_eq!(status.as_u16(), 404);
}

/// `redirect!("/literal")` in a handler — the scaffold's `auth.rs.tpl`
/// logout/login flow — answers 302 with the literal `Location`. Before
/// the macro dispatch fix this expanded to `Redirect::route("/login")`
/// and 500'd because no route is *named* "/login".
#[tokio::test]
async fn redirect_macro_literal_path_redirects_over_http() {
    use suprnova::{post, routes};

    async fn logout(_req: Request) -> Response {
        redirect!("/login").into()
    }

    routes! {
        post!("/logout", logout),
    }

    let router = register();
    let addr = spawn_server(router, 1).await;

    let (status, headers, _) = send_request(addr, "POST", "/logout").await;
    assert_eq!(
        status.as_u16(),
        302,
        "redirect!(\"/login\") must produce a redirect, not a named-route 500",
    );
    assert_eq!(
        headers.get("Location").expect("Location header present"),
        "/login",
    );
}

/// The literal-dispatch arm covers the root path, query chaining, and
/// absolute URLs; the named-route arm must still expand to the params
/// builder — the `_named` binding type-checks only if the macro emitted
/// `Redirect::route` (whose `.with(...)` binds a route parameter and
/// returns `RedirectRouteBuilder`).
#[tokio::test]
async fn redirect_macro_dispatches_on_literal_shape() {
    let _named: suprnova::RedirectRouteBuilder = redirect!("users.show").with("id", "42");

    // Root path — the scaffold's `redirect!("/")` logout target.
    let resp: Response = redirect!("/").into();
    let hyper_resp = resp.unwrap_or_else(|e| e).into_hyper();
    assert_eq!(hyper_resp.status().as_u16(), 302);
    assert_eq!(hyper_resp.headers().get("Location").unwrap(), "/");

    // Literal path with a query parameter.
    let resp: Response = redirect!("/search").query("q", "rust").into();
    let hyper_resp = resp.unwrap_or_else(|e| e).into_hyper();
    assert_eq!(
        hyper_resp.headers().get("Location").unwrap(),
        "/search?q=rust",
    );

    // Absolute URL with a scheme — off-site redirect.
    let resp: Response = redirect!("https://example.com/oauth").into();
    let hyper_resp = resp.unwrap_or_else(|e| e).into_hyper();
    assert_eq!(hyper_resp.status().as_u16(), 302);
    assert_eq!(
        hyper_resp.headers().get("Location").unwrap(),
        "https://example.com/oauth",
    );
}
