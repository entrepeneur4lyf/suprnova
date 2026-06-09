//! Integration tests for [`EnsureEmailVerifiedMiddleware`].
//!
//! The middleware now reads the authenticated user's verification state
//! through the application's configured
//! [`UserProvider`](suprnova::UserProvider) — the same provider
//! [`Auth::user`](suprnova::Auth::user) resolves against — rather than
//! reaching into a specific auth store. These tests drive that path with an
//! [`EloquentUserProvider`]`<TestUser>` registered as the active "users"
//! provider, mirroring `framework/tests/email_verify.rs`'s provider setup.
//!
//! # Why GLOBAL bindings here (not `TestContainer`)
//!
//! The middleware executes on a tokio worker thread: `spawn` →
//! `tokio::spawn` → `handle_request`. Container lookup is
//! task-local → thread-local → **global**, and the thread-local layer that
//! `TestDatabase`/`TestContainer` install does NOT cross into the worker
//! thread. So `active_user_provider()` (which resolves the `AuthManager`)
//! and `DB::connection()` (which the Eloquent provider's query uses) must be
//! bound **globally** for the worker thread to see them — the same reason
//! the prior torii-backed version relied on the global `init_torii`. This is
//! the one place where global beats `TestContainer`; it is safe because this
//! integration binary is its own process and the four tests address distinct
//! user rows.
//!
//! The HTTP plumbing is the loopback-socket pattern from
//! `framework/tests/auth_http_middleware.rs`: a `LoginAs` global middleware
//! installs a fixed user id into request state (what `Auth::id()` reads),
//! then the middleware under test checks that user's verification flag.

use std::any::Any;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use once_cell::sync::Lazy;
use tokio::runtime::Runtime;

use suprnova::auth::AuthConfig;
use suprnova::auth_flows::token_store::create_auth_flow_tokens_table;
use suprnova::http::text;
use suprnova::{
    App, Auth, Authenticatable, AuthManager, CanResetPassword, DB, DatabaseConfig, DbConnection,
    EloquentUserProvider, EnsureEmailVerifiedMiddleware, Middleware, MiddlewareRegistry,
    MustVerifyEmail, Next, Request, Response, Router, handle_request, model,
};

/// One tokio runtime shared across every test — the in-memory SQLite pool is
/// bound to the runtime it was created on (the SQLx-bound-to-runtime reasoning
/// from `framework/tests/email_verify.rs`).
static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("tokio runtime"));

/// The app's `User` shape: a typed model that is also `Authenticatable` +
/// `MustVerifyEmail`. `email_verified_at` is a nullable datetime; the model
/// macro auto-injects `AsOptionalDateTime` on `Option<DateTime<Utc>>` fields.
#[model(table = "users", fillable = ["email", "password"])]
pub struct TestUser {
    pub id: i64,
    pub email: String,
    pub password: String,
    pub email_verified_at: Option<DateTime<Utc>>,
}

impl Authenticatable for TestUser {
    fn get_auth_identifier(&self) -> String {
        self.id.to_string()
    }
    fn get_auth_password(&self) -> Option<&str> {
        Some(&self.password)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_arc_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl MustVerifyEmail for TestUser {
    fn email(&self) -> &str {
        &self.email
    }
    fn email_verified_at(&self) -> Option<DateTime<Utc>> {
        self.email_verified_at
    }
    fn set_email_verified_at(&mut self, v: Option<DateTime<Utc>>) {
        self.email_verified_at = v;
    }
    fn name(&self) -> Option<&str> {
        None
    }
}

impl CanResetPassword for TestUser {
    fn email_for_reset(&self) -> &str {
        &self.email
    }
    fn set_password_hash(&mut self, hash: &str) {
        self.password = hash.to_string();
    }
}

/// One-time GLOBAL setup: an in-memory SQLite DB with the `users` and
/// `auth_flow_tokens` tables, the connection bound via `App::singleton` so
/// `DB::connection()` resolves on the worker thread, and an
/// `EloquentUserProvider::<TestUser>` registered as the active "users"
/// provider through the globally-bound `AuthManager`.
static SETUP: Lazy<()> = Lazy::new(|| {
    RT.block_on(async {
        use sea_orm::ConnectionTrait;

        // `max_connections(1)` keeps the single pooled connection alive, so the
        // in-memory database persists for the life of the process.
        let config = DatabaseConfig::builder()
            .url("sqlite::memory:")
            .max_connections(1)
            .min_connections(1)
            .logging(false)
            .build();
        let conn = DbConnection::connect(&config)
            .await
            .expect("sqlite in-memory connection");

        conn.inner()
            .execute_unprepared(
                "CREATE TABLE users (\
                    id INTEGER PRIMARY KEY AUTOINCREMENT, \
                    email TEXT NOT NULL, \
                    password TEXT NOT NULL, \
                    email_verified_at TEXT\
                 )",
            )
            .await
            .expect("create users table");

        let create = create_auth_flow_tokens_table();
        conn.inner()
            .execute(conn.inner().get_database_backend().build(&create))
            .await
            .expect("create auth_flow_tokens table");

        // GLOBAL bindings (see module docs): the worker thread can only see
        // global container state.
        App::singleton(conn);
        App::singleton(AuthManager::new(AuthConfig::default()));
        Auth::register_provider("users", Arc::new(EloquentUserProvider::<TestUser>::new()))
            .expect("register provider");
    });
});

/// Monotonic id seed so each test addresses a distinct user row — the rows
/// share one global DB, and a per-test email avoids any cross-test coupling.
static NEXT_ID: AtomicI64 = AtomicI64::new(1);

/// Insert a fresh user row and return its (id, email). `verified` controls
/// whether `email_verified_at` is stamped — the provider's
/// `is_email_verified(id)` reads exactly this column.
async fn make_user(verified: bool) -> (i64, String) {
    use sea_orm::ConnectionTrait;

    let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let email = format!("user{id}@example.com");
    let conn = DB::connection().expect("global DB connection");

    let verified_sql = if verified {
        format!("'{}'", Utc::now().to_rfc3339())
    } else {
        "NULL".to_string()
    };
    conn.inner()
        .execute_unprepared(&format!(
            "INSERT INTO users (id, email, password, email_verified_at) \
             VALUES ({id}, '{email}', 'x', {verified_sql})"
        ))
        .await
        .expect("seed user");

    (id, email)
}

/// `Authenticatable` whose `get_auth_identifier()` returns the seeded user's
/// id string. `Auth::set_user(Arc::new(this))` installs the id into request
/// state, which is what the middleware reads via `Auth::id()`.
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

/// Global middleware: installs a fixed user-id into request state so
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
        // A user whose `email_verified_at` is stamped — the provider's
        // `is_email_verified(id)` returns true, so the request passes through.
        let (id, _email) = make_user(true).await;

        let registry = MiddlewareRegistry::new()
            .append(LoginAs(id.to_string()))
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
        // `email_verified_at` stays NULL → provider reports unverified.
        let (id, _email) = make_user(false).await;

        let registry = MiddlewareRegistry::new()
            .append(LoginAs(id.to_string()))
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
        let (id, _email) = make_user(false).await;

        let registry = MiddlewareRegistry::new()
            .append(LoginAs(id.to_string()))
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
        // No LoginAs in the chain — `Auth::id()` returns None. The middleware
        // mirrors Laravel's `! $request->user() || ! verified` by responding
        // with the same 403 in this branch — and crucially does so WITHOUT
        // consulting the provider (the id-None check comes first).
        let registry = MiddlewareRegistry::new().append(EnsureEmailVerifiedMiddleware::new());
        let addr = spawn(registry, 1).await;
        let (status, _headers, body) = get(addr).await;

        assert_eq!(status, 403);
        assert!(body.contains("not verified"));
    });
}
