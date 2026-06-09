//! Fail-closed regression test for [`EnsureEmailVerifiedMiddleware`].
//!
//! # The security property this binary pins
//!
//! [`EnsureEmailVerifiedMiddleware::handle`] asks the application's configured
//! [`UserProvider`](suprnova::UserProvider) whether the authenticated user has
//! verified their email, via
//! `active_user_provider()?.is_email_verified(&id).await?`. When the provider
//! **cannot answer** that question — the storage layer is down, OR the active
//! provider is *token-only* and doesn't support email verification (the
//! default [`UserProvider::is_email_verified`] returns
//! `FrameworkError::internal(...)`) — the `?` propagates that error and the
//! request collapses to a **500**. The unverified/unknowable user MUST NOT
//! pass through to the protected handler.
//!
//! This is fail-closed: verification gating must not silently open under outage
//! or misconfiguration. Today that property is only asserted by a code-trace
//! plus an inline comment in `email_verified_middleware.rs`; nothing in CI pins
//! it. A future refactor that turned the `?` into a swallowed error or an
//! `unwrap_or(true)` would silently open a hole. This test fails CI if the
//! fail-closed branch ever breaks: it registers a token-only provider (one that
//! inherits the erroring `is_email_verified` default), authenticates a user,
//! and asserts the response is 500 — **not** 200 — and that the protected
//! handler was never reached.
//!
//! # Why this is a SEPARATE integration binary
//!
//! The middleware executes on a tokio worker thread (`spawn` → `tokio::spawn`
//! → `handle_request`). Container lookup is task-local → thread-local →
//! **global**, and the thread-local layer that `TestContainer` installs does
//! NOT cross into the worker thread. So `active_user_provider()` (which
//! resolves the `AuthManager`) must be bound **globally** for the worker to see
//! it — and the provider is registered globally via `Auth::register_provider`.
//! That global binding would race/bleed into the four parallel tests in
//! `framework/tests/email_verified_middleware.rs`, which register a *different*
//! (Eloquent) provider under the same `"users"` name. Keeping this test in its
//! own integration binary gives it its own process → no global-state bleed.
//!
//! The HTTP plumbing mirrors `email_verified_middleware.rs`: a `LoginAs` global
//! middleware installs a fixed user id into request state (what `Auth::id()`
//! reads), then the middleware under test consults the provider — which here
//! errors, driving the 500.

use std::any::Any;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use once_cell::sync::Lazy;
use tokio::runtime::Runtime;

use suprnova::FrameworkError;
use suprnova::auth::AuthConfig;
use suprnova::http::text;
use suprnova::{
    App, Auth, AuthManager, Authenticatable, EnsureEmailVerifiedMiddleware, Middleware,
    MiddlewareRegistry, Next, Request, Response, Router, UserProvider, handle_request,
};

/// One tokio runtime shared across the test, mirroring the sibling binary.
static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("tokio runtime"));

/// Set true iff the protected handler runs. The fail-closed property requires
/// this to stay false: an unverifiable user must never reach the handler.
static HANDLER_REACHED: AtomicBool = AtomicBool::new(false);

/// Set true iff the token-only provider's `is_email_verified` is actually
/// invoked. This proves the 500 comes from the PROVIDER ERROR — not from no
/// provider being registered or some unrelated cause. `is_email_verified` is
/// the only method overridden here purely so we can flip this flag; the
/// override still returns the same unsupported error the default would, so the
/// fail-closed path under test is unchanged.
static PROVIDER_CONSULTED: AtomicBool = AtomicBool::new(false);

/// A minimal **token-only** user provider: it satisfies the only required
/// `UserProvider` method (`retrieve_by_id`) and otherwise inherits the trait
/// defaults — including the `is_email_verified` default that returns
/// `FrameworkError::internal(...)` ("not supported"). The only override below
/// is a thin wrapper that records that the provider was consulted, then returns
/// that same unsupported error — so the middleware's fail-closed `?` fires for
/// exactly the production reason.
struct TokenOnlyProvider;

#[async_trait]
impl UserProvider for TokenOnlyProvider {
    async fn retrieve_by_id(
        &self,
        _id: &str,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        // The fail-closed path never calls this (it errors on the
        // `is_email_verified` check first), but a real provider must satisfy
        // the required method. `Ok(None)` is the simplest valid answer.
        Ok(None)
    }

    async fn is_email_verified(&self, _id: &str) -> Result<bool, FrameworkError> {
        // Record that the provider WAS the thing consulted, then reproduce the
        // exact unsupported error a token-only provider returns by default.
        PROVIDER_CONSULTED.store(true, Ordering::SeqCst);
        Err(FrameworkError::internal(
            "this user provider does not support email verification",
        ))
    }
}

/// One-time GLOBAL setup: bind an `AuthManager` and register the token-only
/// provider as the active `"users"` provider — globally, so the worker thread
/// resolving `active_user_provider()` can see it. No DB is bound: this path
/// errors before any storage read.
static SETUP: Lazy<()> = Lazy::new(|| {
    App::singleton(AuthManager::new(AuthConfig::default()));
    Auth::register_provider("users", Arc::new(TokenOnlyProvider)).expect("register provider");
});

/// `Authenticatable` whose `get_auth_identifier()` returns a fixed id string —
/// installed into request state by `LoginAs` so `Auth::id()` is `Some(id)`.
struct UserById(String);

impl Authenticatable for UserById {
    fn get_auth_identifier(&self) -> String {
        self.0.clone()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_arc_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

/// Global middleware that authenticates the request by installing a fixed user
/// id into request state. Mirrors the `LoginAs` pattern from the sibling binary.
struct LoginAs(String);

#[async_trait]
impl Middleware for LoginAs {
    async fn handle(&self, request: Request, next: Next) -> Response {
        Auth::set_user(Arc::new(UserById(self.0.clone())));
        next(request).await
    }
}

fn router() -> Router {
    Router::new()
        .get("/protected", |_req| async {
            HANDLER_REACHED.store(true, Ordering::SeqCst);
            text("reached")
        })
        .into()
}

async fn spawn(registry: MiddlewareRegistry, accepts: usize) -> SocketAddr {
    let router = Arc::new(router());
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

async fn get(addr: SocketAddr) -> (u16, HashMap<String, String>, String) {
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
        .uri("/protected")
        .header("Host", "localhost")
        .header("Content-Length", "0")
        .body(Full::new(Bytes::new()))
        .unwrap();

    let resp = tokio::time::timeout(Duration::from_secs(5), sender.send_request(req))
        .await
        .expect("send_request timeout")
        .expect("hyper send_request");

    let (parts, body) = resp.into_parts();
    let status = parts.status.as_u16();
    let headers = parts
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
    (status, headers, String::from_utf8_lossy(&bytes).to_string())
}

/// When the active provider cannot answer `is_email_verified` (here: a
/// token-only provider that returns the unsupported error), the middleware
/// fails CLOSED — the request collapses to a 500 and the protected handler is
/// never reached. This is the security property: an unverified/unknowable user
/// must NOT pass through.
#[test]
fn provider_error_fails_closed_with_500() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        HANDLER_REACHED.store(false, Ordering::SeqCst);
        PROVIDER_CONSULTED.store(false, Ordering::SeqCst);

        // Authenticate a user so `Auth::id()` is `Some(...)` — this gets the
        // middleware PAST its `id`-None short-circuit and into the provider
        // call that errors. Without this, a 500 could not be attributed to the
        // provider error (the None branch returns a 403 instead).
        let registry = MiddlewareRegistry::new()
            .append(LoginAs("token-user-1".to_string()))
            .append(EnsureEmailVerifiedMiddleware::new());
        let addr = spawn(registry, 1).await;
        let (status, _headers, _body) = get(addr).await;

        // KEY assertion: 500, never 200. The provider error must not let the
        // unverifiable user through.
        assert_eq!(
            status, 500,
            "provider error must fail CLOSED with 500, never let the user through (200)"
        );

        // The provider WAS the thing consulted — proves the 500 is from the
        // provider error, not from no-provider-registered or another cause.
        assert!(
            PROVIDER_CONSULTED.load(Ordering::SeqCst),
            "the token-only provider's is_email_verified must have been called — \
             otherwise the 500 is not attributable to the provider error"
        );

        // And the protected handler never ran — the user did NOT pass through.
        assert!(
            !HANDLER_REACHED.load(Ordering::SeqCst),
            "protected handler must NOT be reached when the provider errors"
        );
    });
}
