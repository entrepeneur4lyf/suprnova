//! Integration tests for [`EnsureEmailVerifiedMiddleware`].
//!
//! Combines the torii-backed user setup pattern from
//! `framework/tests/email_verify.rs` (shared tokio runtime + one-time
//! `init_torii`) with the loopback-socket HTTP pattern from
//! `framework/tests/auth_http_middleware.rs`. The middleware reads
//! the auth id from `request_state` (set via a `LoginAs` global
//! middleware that calls `Auth::set_user`), then queries torii for
//! the user's `email_verified_at`.

use std::any::Any;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use once_cell::sync::Lazy;
use tokio::runtime::Runtime;

use suprnova::http::text;
use suprnova::torii_integration::{ToriiConfig, init_torii};
use suprnova::{
    Auth, Authenticatable, EnsureEmailVerifiedMiddleware, Middleware, MiddlewareRegistry, Next,
    Request, Response, Router, handle_request,
};

/// One tokio runtime shared across every test — see
/// `framework/tests/email_verify.rs` for the SQLx-bound-to-runtime
/// reasoning.
static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("tokio runtime"));

/// One-time Torii initialisation + `MAIL_FROM` so any incidental
/// auth-flow path that needs it doesn't fail closed.
static SETUP: Lazy<()> = Lazy::new(|| {
    // SAFETY: tests in this file all force `SETUP` before reading
    // these env vars; no parallel writer ever mutates them.
    unsafe {
        std::env::set_var("MAIL_FROM", "test-mailer@example.com");
    }
    RT.block_on(async {
        let config = ToriiConfig::sqlite_in_memory()
            .await
            .expect("sqlite in-memory connection")
            .apply_migrations(true);
        init_torii(config).await.expect("init_torii");
    });
});

/// `Authenticatable` whose `get_auth_identifier()` returns a torii
/// `UserId` string. `Auth::set_user(Arc::new(this))` installs the id
/// into request_state, which is what the middleware reads via
/// `Auth::id()`.
struct UserById(String);

impl Authenticatable for UserById {
    fn get_auth_identifier(&self) -> String {
        self.0.clone()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Global middleware: installs a fixed user-id into request_state so
/// downstream middleware sees an authenticated request. Mirrors the
/// `LoginAsUser` pattern from `auth_http_middleware.rs`.
struct LoginAs(String);

#[async_trait::async_trait]
impl Middleware for LoginAs {
    async fn handle(&self, request: Request, next: Next) -> Response {
        Auth::set_user(Arc::new(UserById(self.0.clone())));
        next(request).await
    }
}

fn router() -> Router {
    Router::new()
        .get("/protected", |_req| async { text("reached") })
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

#[test]
fn verified_user_reaches_handler() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        // Register + verify the user via the real EmailVerification
        // facade, so torii's `email_verified_at` actually gets stamped.
        let user = Auth::password()
            .register("verified@example.com", "longpassword123")
            .await
            .expect("register");
        let token = suprnova::auth_flows::EmailVerification::generate_token(&user.id)
            .await
            .expect("generate_token");
        let plaintext = token
            .token()
            .expect("plaintext available immediately after creation")
            .to_string();
        suprnova::auth_flows::EmailVerification::verify(&plaintext)
            .await
            .expect("verify");

        let registry = MiddlewareRegistry::new()
            .append(LoginAs(user.id.to_string()))
            .append(EnsureEmailVerifiedMiddleware::new());
        let addr = spawn(registry, 1).await;
        let (status, _headers, body) = get(addr).await;

        assert_eq!(status, 200, "verified user must reach the handler");
        assert_eq!(body, "reached");
    });
}

#[test]
fn unverified_user_gets_403_in_api_form() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        // Register, do NOT verify — `email_verified_at` stays None.
        let user = Auth::password()
            .register("unverified-api@example.com", "longpassword123")
            .await
            .expect("register");

        let registry = MiddlewareRegistry::new()
            .append(LoginAs(user.id.to_string()))
            .append(EnsureEmailVerifiedMiddleware::new());
        let addr = spawn(registry, 1).await;
        let (status, _headers, body) = get(addr).await;

        assert_eq!(status, 403);
        assert!(
            body.contains("not verified"),
            "403 body must mention verification status; got: {body}"
        );
    });
}

#[test]
fn unverified_user_redirects_in_web_form() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let user = Auth::password()
            .register("unverified-web@example.com", "longpassword123")
            .await
            .expect("register");

        let registry = MiddlewareRegistry::new()
            .append(LoginAs(user.id.to_string()))
            .append(EnsureEmailVerifiedMiddleware::redirect_to("/email/verify"));
        let addr = spawn(registry, 1).await;
        let (status, headers, _body) = get(addr).await;

        assert_eq!(status, 302);
        assert_eq!(
            headers.get("location").map(String::as_str),
            Some("/email/verify"),
            "redirect target must match the configured path"
        );
    });
}

#[test]
fn no_auth_user_falls_into_same_branch() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        // No LoginAs in the chain — Auth::id() returns None. The
        // middleware mirrors Laravel's `! $request->user() || ! verified`
        // by responding with the same 403 in this branch.
        let registry = MiddlewareRegistry::new().append(EnsureEmailVerifiedMiddleware::new());
        let addr = spawn(registry, 1).await;
        let (status, _headers, body) = get(addr).await;

        assert_eq!(status, 403);
        assert!(body.contains("not verified"));
    });
}
