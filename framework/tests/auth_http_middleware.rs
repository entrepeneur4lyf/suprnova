//! Integration tests for the HTTP auth middlewares — `BasicAuthMiddleware`
//! and the per-guard `AuthMiddleware::for_guard` — driven end-to-end through
//! `handle_request` over a real loopback socket.
//!
//! This is the established middleware test pattern (`hyper::body::Incoming`
//! cannot be built synthetically, so we go over the wire). `handle_request`
//! installs the per-request auth `request_state` scope, so the guard-backed
//! paths resolve exactly as they do in a running server. No database or
//! `SessionMiddleware` is needed: the `FakeProvider` validates credentials in
//! memory, and the stateful login's session write simply no-ops without a
//! session scope (the authentication decision still stands).

use std::any::Any;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use once_cell::sync::Lazy;

use suprnova::http::text;
use suprnova::{
    Auth, AuthConfig, AuthManager, AuthMiddleware, Authenticatable, BasicAuthMiddleware,
    FrameworkError, Middleware, MiddlewareRegistry, Next, Request, Response, Router, UserProvider,
    handle_request,
};

/// Register the default-config `AuthManager` (web → session → "users") and the
/// in-memory provider behind it, process-wide, so every `Auth::*` facade call
/// resolves the default guard. Config + provider are identical for all tests.
static SETUP: Lazy<()> = Lazy::new(|| {
    suprnova::App::singleton(AuthManager::new(AuthConfig::default()));
    Auth::register_provider("users", Arc::new(FakeProvider)).expect("register users provider");
});

#[derive(Clone)]
struct TestUser;

impl Authenticatable for TestUser {
    fn get_auth_identifier(&self) -> String {
        "7".to_string()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_arc_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

fn the_user() -> Arc<dyn Authenticatable> {
    Arc::new(TestUser)
}

/// Knows one user: id `"7"`, email `"a@b.com"`, password `"secret"`.
struct FakeProvider;

#[async_trait::async_trait]
impl UserProvider for FakeProvider {
    async fn retrieve_by_id(
        &self,
        id: &str,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        Ok((id == "7").then(the_user))
    }

    async fn retrieve_by_credentials(
        &self,
        credentials: &serde_json::Value,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        let email = credentials.get("email").and_then(|v| v.as_str());
        Ok((email == Some("a@b.com")).then(the_user))
    }

    async fn validate_credentials(
        &self,
        _user: &dyn Authenticatable,
        credentials: &serde_json::Value,
    ) -> Result<bool, FrameworkError> {
        Ok(credentials.get("password").and_then(|v| v.as_str()) == Some("secret"))
    }
}

/// A global middleware that authenticates the request as the test user for the
/// rest of the chain (the public hook is `Auth::set_user`). Used to prove that
/// `AuthMiddleware::for_guard` sees an upstream-resolved user.
struct LoginAsUser;

#[async_trait::async_trait]
impl Middleware for LoginAsUser {
    async fn handle(&self, request: Request, next: Next) -> Response {
        Auth::set_user(the_user());
        next(request).await
    }
}

/// Test-only stand-in for `SessionMiddleware`: installs the session and
/// pending-cookies task-locals so anything inside `next` can call
/// `Auth::login_id` / `Auth::login_remember` without persisting to a real
/// session store. Used by the `BasicAuthMiddleware::new()` (stateful)
/// integration tests: that flow calls `Auth::login_id`, which now returns
/// `Err` when called outside a session scope. The fake scope makes the
/// session writes succeed; the test only cares about the HTTP status the
/// middleware returns, not what landed in the store.
struct FakeSessionScope;

#[async_trait::async_trait]
impl Middleware for FakeSessionScope {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let session = suprnova::session::new_session_slot_for_test();
        let pending = suprnova::session::new_pending_cookies_slot_for_test();
        suprnova::session::session_scope_for_test(
            session,
            suprnova::session::pending_cookies_scope_for_test(pending, next(request)),
        )
        .await
    }
}

fn router() -> Router {
    Router::new()
        .get("/protected", |_req| async { text("reached") })
        .into()
}

fn basic_header(user: &str, password: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
    format!("Basic {encoded}")
}

/// Spawn a test server with `registry` as the global middleware set, accepting
/// `accepts` connections.
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

/// Send a request and return `(status, lowercased response headers, body)`.
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

// ── BasicAuthMiddleware ──────────────────────────────────────────────────────

#[tokio::test]
async fn basic_once_valid_credentials_reach_handler() {
    Lazy::force(&SETUP);
    let registry = MiddlewareRegistry::new().append(BasicAuthMiddleware::once());
    let addr = spawn_server(router(), registry, 1).await;

    let (status, _headers, body) = request(
        addr,
        "GET",
        "/protected",
        &[("Authorization", &basic_header("a@b.com", "secret"))],
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body, "reached");
}

#[tokio::test]
async fn basic_missing_header_challenges_401() {
    Lazy::force(&SETUP);
    let registry = MiddlewareRegistry::new().append(BasicAuthMiddleware::once());
    let addr = spawn_server(router(), registry, 1).await;

    let (status, headers, _body) = request(addr, "GET", "/protected", &[]).await;

    assert_eq!(status, 401);
    assert!(
        headers
            .get("www-authenticate")
            .map(|v| v.starts_with("Basic realm="))
            .unwrap_or(false),
        "missing-header 401 must carry a Basic challenge, got: {:?}",
        headers.get("www-authenticate")
    );
}

#[tokio::test]
async fn basic_malformed_header_challenges_401() {
    Lazy::force(&SETUP);
    let registry = MiddlewareRegistry::new().append(BasicAuthMiddleware::once());
    let addr = spawn_server(router(), registry, 1).await;

    let (status, headers, _body) = request(
        addr,
        "GET",
        "/protected",
        &[("Authorization", "Basic !!!not-valid-base64!!!")],
    )
    .await;

    assert_eq!(status, 401);
    assert!(headers.contains_key("www-authenticate"));
}

#[tokio::test]
async fn basic_wrong_password_challenges_401() {
    Lazy::force(&SETUP);
    let registry = MiddlewareRegistry::new().append(BasicAuthMiddleware::once());
    let addr = spawn_server(router(), registry, 1).await;

    let (status, _headers, _body) = request(
        addr,
        "GET",
        "/protected",
        &[("Authorization", &basic_header("a@b.com", "wrong"))],
    )
    .await;

    assert_eq!(status, 401);
}

#[tokio::test]
async fn basic_stateful_valid_credentials_reach_handler() {
    Lazy::force(&SETUP);
    // BasicAuthMiddleware::new() is the stateful variant — on a credential
    // match it persists the user into the session via `Auth::login_id`.
    // That now requires a SessionMiddleware-equivalent task-local scope
    // upstream; FakeSessionScope provides the slot without binding a real
    // store driver to keep the test hermetic.
    let registry = MiddlewareRegistry::new()
        .append(FakeSessionScope)
        .append(BasicAuthMiddleware::new());
    let addr = spawn_server(router(), registry, 1).await;

    let (status, _headers, body) = request(
        addr,
        "GET",
        "/protected",
        &[("Authorization", &basic_header("a@b.com", "secret"))],
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body, "reached");
}

// ── AuthMiddleware::for_guard ────────────────────────────────────────────────

#[tokio::test]
async fn auth_for_guard_unauthenticated_returns_401() {
    Lazy::force(&SETUP);
    let registry = MiddlewareRegistry::new().append(AuthMiddleware::new().for_guard("web"));
    let addr = spawn_server(router(), registry, 1).await;

    let (status, _headers, _body) = request(addr, "GET", "/protected", &[]).await;

    assert_eq!(status, 401);
}

#[tokio::test]
async fn auth_for_guard_authenticated_reaches_handler() {
    Lazy::force(&SETUP);
    // LoginAsUser runs first (sets the request user via `Auth::set_user`), then
    // the named-guard check sees it through the shared request state.
    let registry = MiddlewareRegistry::new()
        .append(LoginAsUser)
        .append(AuthMiddleware::new().for_guard("web"));
    let addr = spawn_server(router(), registry, 1).await;

    let (status, _headers, body) = request(addr, "GET", "/protected", &[]).await;

    assert_eq!(status, 200);
    assert_eq!(body, "reached");
}
