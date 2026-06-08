//! Integration tests for [`suprnova::SessionGuard`]'s lifecycle events
//! and the login → logout round trip.
//!
//! Event assertions use the process-global [`EventFacade::fake`], so the
//! tests in this file serialize on `TEST_LOCK`. Each `tests/*.rs` is its
//! own binary, which isolates this file's fake store from the fakes used
//! by other test files.
//!
//! DB-touching tests run on a shared [`Runtime`] (sqlx pools die with the
//! runtime that created them), mirroring `tests/remember_me.rs`.

use once_cell::sync::Lazy;
use sea_orm_migration::MigratorTrait;
use sea_orm_migration::prelude::*;
use std::any::Any;
use std::sync::Arc;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

use suprnova::auth::events::{Attempting, Authenticated, Failed, Login, Logout};
use suprnova::auth::request_state;
use suprnova::events::testing::{assert_dispatched, assert_not_dispatched};
use suprnova::{
    Auth, AuthConfig, AuthManager, Authenticatable, Credentials, EventFacade, FrameworkError,
    SessionGuard, SessionMiddleware, StatefulGuard, UserProvider,
};

/// Shared runtime — SQLx pools die with their creating runtime, so every
/// DB-touching path runs here rather than on a per-test runtime.
static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("tokio runtime"));

/// Serializes the event-fake critical sections; the fake store is
/// process-global.
static TEST_LOCK: Mutex<()> = Mutex::const_new(());

/// One-shot: install Crypt, register a shared in-memory SQLite connection
/// in the global container, and migrate `sessions` + `remember_tokens`.
static SETUP: Lazy<()> = Lazy::new(|| {
    let key = suprnova::EncryptionKey::generate();
    let _ = suprnova::crypto::_test_install_key(key);

    RT.block_on(async {
        let config = suprnova::database::DatabaseConfig::builder()
            .url("sqlite::memory:")
            .max_connections(1)
            .min_connections(1)
            .logging(false)
            .build();
        let conn = suprnova::database::DbConnection::connect(&config)
            .await
            .expect("connect in-memory sqlite");
        LocalMigrator::up(conn.inner(), None)
            .await
            .expect("run local migrator");
        suprnova::App::singleton(conn);

        // Register the default-config AuthManager (web → session → "users")
        // and the provider behind it, so the static `Auth::*` facade methods
        // resolve the default guard. The config + provider are identical for
        // every test, so a single process-wide registration is correct.
        suprnova::App::singleton(AuthManager::new(AuthConfig::default()));
        Auth::register_provider("users", Arc::new(FakeProvider)).expect("register users provider");
    });
});

/// Local migrator: just the `sessions` and `remember_tokens` tables the
/// session + remember-me code reads.
struct LocalMigrator;

#[async_trait::async_trait]
impl MigratorTrait for LocalMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(CreateSessionsTable),
            Box::new(CreateRememberTokensTable),
        ]
    }
}

struct CreateSessionsTable;

impl MigrationName for CreateSessionsTable {
    fn name(&self) -> &str {
        "m20240101_000001_create_sessions_table"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateSessionsTable {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Sessions::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Sessions::Id)
                            .string()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Sessions::UserId).string().null())
                    .col(ColumnDef::new(Sessions::Payload).text().not_null())
                    .col(ColumnDef::new(Sessions::CsrfToken).string().not_null())
                    .col(
                        ColumnDef::new(Sessions::LastActivity)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Sessions::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Sessions {
    Table,
    Id,
    UserId,
    Payload,
    CsrfToken,
    LastActivity,
}

struct CreateRememberTokensTable;

impl MigrationName for CreateRememberTokensTable {
    fn name(&self) -> &str {
        "m20240101_000002_create_remember_tokens_table"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for CreateRememberTokensTable {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(RememberTokens::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(RememberTokens::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(RememberTokens::UserId).string().not_null())
                    .col(ColumnDef::new(RememberTokens::Selector).string().not_null())
                    .col(
                        ColumnDef::new(RememberTokens::TokenHash)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RememberTokens::ExpiresAt)
                            .timestamp()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RememberTokens::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(RememberTokens::LastUsedAt)
                            .timestamp()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_test_remember_tokens_selector")
                    .table(RememberTokens::Table)
                    .col(RememberTokens::Selector)
                    .unique()
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(RememberTokens::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum RememberTokens {
    Table,
    Id,
    UserId,
    Selector,
    TokenHash,
    ExpiresAt,
    CreatedAt,
    LastUsedAt,
}

#[derive(Clone)]
struct TestUser {
    id: String,
}

impl Authenticatable for TestUser {
    fn get_auth_identifier(&self) -> String {
        self.id.clone()
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_arc_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

/// Knows one user: id `"7"`, email `"a@b.com"`, password `"secret"`.
struct FakeProvider;

fn the_user() -> Arc<dyn Authenticatable> {
    Arc::new(TestUser { id: "7".into() })
}

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

fn guard() -> SessionGuard {
    SessionGuard::new(Arc::new(FakeProvider))
}

/// Drive `fut` inside the three task-local scopes a real request installs:
/// the session, the pending-cookies bag, and the auth request state.
async fn run_in_request<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let session_slot = suprnova::session::new_session_slot_for_test();
    let pending_slot = suprnova::session::new_pending_cookies_slot_for_test();
    suprnova::session::session_scope_for_test(
        session_slot,
        suprnova::session::pending_cookies_scope_for_test(
            pending_slot,
            request_state::request_state_scope_for_test(fut),
        ),
    )
    .await
}

#[test]
fn attempt_with_valid_credentials_dispatches_attempting_login_authenticated() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        run_in_request(async {
            let g = guard();
            let user = g
                .attempt(&Credentials::password("a@b.com", "secret"), false)
                .await
                .unwrap();
            assert_eq!(user.map(|u| u.get_auth_identifier()), Some("7".to_string()));
        })
        .await;

        assert_dispatched::<Attempting>(|e| e.guard == "web" && !e.remember);
        assert_dispatched::<Login>(|e| e.guard == "web" && e.user_id == "7" && !e.remember);
        assert_dispatched::<Authenticated>(|e| e.guard == "web" && e.user_id == "7");
        assert_not_dispatched::<Failed>(|_| true);
    });
}

#[test]
fn attempt_with_wrong_password_dispatches_failed_with_user_id() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        run_in_request(async {
            let g = guard();
            let user = g
                .attempt(&Credentials::password("a@b.com", "wrong"), false)
                .await
                .unwrap();
            assert!(user.is_none());
        })
        .await;

        assert_dispatched::<Attempting>(|_| true);
        // Identifier matched but the password did not → Failed carries the id.
        assert_dispatched::<Failed>(|e| e.guard == "web" && e.user_id.as_deref() == Some("7"));
        assert_not_dispatched::<Login>(|_| true);
        assert_not_dispatched::<Authenticated>(|_| true);
    });
}

#[test]
fn attempt_with_unknown_user_dispatches_failed_without_user_id() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        run_in_request(async {
            let g = guard();
            let user = g
                .attempt(&Credentials::password("nobody@b.com", "secret"), false)
                .await
                .unwrap();
            assert!(user.is_none());
        })
        .await;

        assert_dispatched::<Attempting>(|_| true);
        // No user matched → Failed carries no id.
        assert_dispatched::<Failed>(|e| e.guard == "web" && e.user_id.is_none());
        assert_not_dispatched::<Login>(|_| true);
    });
}

#[test]
fn once_dispatches_attempting_and_authenticated_but_not_login() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        run_in_request(async {
            let g = guard();
            assert!(
                g.once(&Credentials::password("a@b.com", "secret"))
                    .await
                    .unwrap()
            );
        })
        .await;

        assert_dispatched::<Attempting>(|_| true);
        assert_dispatched::<Authenticated>(|e| e.user_id == "7");
        // `once` does not persist, so it is not a Login.
        assert_not_dispatched::<Login>(|_| true);
        assert_not_dispatched::<Failed>(|_| true);
    });
}

#[test]
fn login_then_logout_dispatches_login_and_logout_with_user_id() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        run_in_request(async {
            let g = guard();
            g.login(the_user(), false).await.unwrap();
            // logout revokes remember-me tokens (DB) then clears the session.
            g.logout().await.unwrap();
        })
        .await;

        assert_dispatched::<Login>(|e| e.user_id == "7");
        assert_dispatched::<Logout>(|e| e.guard == "web" && e.user_id.as_deref() == Some("7"));
    });
}

// ── Static-facade delegation ────────────────────────────────────────────────
//
// The Laravel-shaped `Auth::attempt/login/once/login_using_id/logout` facade
// methods must route through the *default* guard resolved from the container
// `AuthManager` (registered in SETUP), producing the same events + request
// state as calling the guard directly. `Auth::id()` reads the request-scoped
// user, so it doubles as a probe that the facade actually authenticated.

#[test]
fn facade_attempt_routes_through_default_guard() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        run_in_request(async {
            let user = Auth::attempt(&Credentials::password("a@b.com", "secret"), false)
                .await
                .unwrap();
            assert_eq!(user.map(|u| u.get_auth_identifier()), Some("7".to_string()));
            // Routed through the guard → the request user is cached and the
            // static facade sees it.
            assert_eq!(Auth::id(), Some("7".to_string()));
        })
        .await;

        assert_dispatched::<Attempting>(|e| e.guard == "web" && !e.remember);
        assert_dispatched::<Login>(|e| e.guard == "web" && e.user_id == "7" && !e.remember);
        assert_dispatched::<Authenticated>(|e| e.guard == "web" && e.user_id == "7");
        assert_not_dispatched::<Failed>(|_| true);
    });
}

#[test]
fn facade_attempt_wrong_password_routes_failed() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        run_in_request(async {
            let user = Auth::attempt(&Credentials::password("a@b.com", "wrong"), false)
                .await
                .unwrap();
            assert!(user.is_none());
        })
        .await;

        assert_dispatched::<Failed>(|e| e.guard == "web" && e.user_id.as_deref() == Some("7"));
        assert_not_dispatched::<Login>(|_| true);
    });
}

#[test]
fn facade_login_using_id_routes_through_default_guard() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        run_in_request(async {
            let user = Auth::login_using_id("7", false).await.unwrap();
            assert_eq!(user.map(|u| u.get_auth_identifier()), Some("7".to_string()));
            assert_eq!(Auth::id(), Some("7".to_string()));
        })
        .await;

        assert_dispatched::<Login>(|e| e.guard == "web" && e.user_id == "7");
        assert_dispatched::<Authenticated>(|e| e.user_id == "7");
    });
}

#[test]
fn facade_once_authenticates_without_login_event() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        run_in_request(async {
            assert!(
                Auth::once(&Credentials::password("a@b.com", "secret"))
                    .await
                    .unwrap()
            );
            assert_eq!(Auth::id(), Some("7".to_string()));
        })
        .await;

        assert_dispatched::<Authenticated>(|e| e.user_id == "7");
        // `once` does not persist, so it is not a Login.
        assert_not_dispatched::<Login>(|_| true);
    });
}

// Bare `Auth::login` + `Auth::logout`: login fires Login, logout fires Logout
// AND clears the request-scoped user (so `Auth::id()` reports `None` after) —
// the request-state clear that the bare facade previously skipped.
#[test]
fn facade_login_then_logout_fires_events_and_clears_request_user() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        run_in_request(async {
            Auth::login(the_user(), false).await.unwrap();
            assert_eq!(Auth::id(), Some("7".to_string()));

            Auth::logout().await.unwrap();
            assert_eq!(Auth::id(), None, "logout must clear the request user");
        })
        .await;

        assert_dispatched::<Login>(|e| e.user_id == "7");
        assert_dispatched::<Logout>(|e| e.guard == "web" && e.user_id.as_deref() == Some("7"));
    });
}

// `Auth::logout_and_invalidate` is the "complete session destruction"
// variant — distinct from `Auth::logout` (which keeps the session for
// flash messages etc.). Its contract requires a fresh session id on
// the way out so the same id cannot be reused after the wipe. Without
// this, `flush()` only clears `data` + `user_id` but keeps `session.id`,
// and the post-logout cookie still names the now-empty session row,
// effectively reviving the id for whatever fresh content lands next.
// Mirrors Laravel's `session()->invalidate()` = `flush()` + `regenerate()`.
#[test]
fn facade_logout_and_invalidate_rotates_session_id() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        run_in_request(async {
            Auth::login(the_user(), false).await.unwrap();
            let pre_id = suprnova::session::session()
                .map(|s| s.id)
                .expect("session scope installed");

            Auth::logout_and_invalidate().await.unwrap();

            let post_id = suprnova::session::session()
                .map(|s| s.id)
                .expect("session scope installed");

            assert_ne!(
                pre_id, post_id,
                "logout_and_invalidate must rotate the session id (Laravel \
                 session()->invalidate() semantic); plain Auth::logout does NOT"
            );
            assert_eq!(
                Auth::id(),
                None,
                "logout_and_invalidate must clear the request user"
            );
        })
        .await;

        assert_dispatched::<Logout>(|e| e.guard == "web" && e.user_id.as_deref() == Some("7"));
    });
}

/// Action that the inner handler runs against the per-request session
/// scope — boxed so the harness can dispatch any async fn (login,
/// logout_and_invalidate, both) without the type signature mentioning
/// each one. Returns a `Pin<Box<dyn Future>>` so the harness can `.await`
/// it; takes `()` (the closures own everything they need).
type HandlerAction =
    Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;

/// Drive `action` inside a real cookie-bearing `Request` through
/// `SessionMiddleware::handle`. The cookie carries the encrypted form of
/// `seed_id`; the middleware loads the row, runs `action` inside the
/// per-request session + auth-request-state scopes, then (if the action
/// rotated the id) writes the new row + destroys the old one. Returns
/// the post-rotation session id observed inside the scope and the
/// middleware's response.
///
/// `Next` is `Arc<dyn Fn(Request) -> Pin<Box<...>>>` — it's stored
/// in `MiddlewareChain` so it MUST be `Send + Sync + 'static` and
/// independent of any per-request lifetimes.
async fn drive_middleware_with_session_cookie(
    seed_id: &str,
    seed_user_id: Option<&str>,
    middleware: &SessionMiddleware,
    action: HandlerAction,
) -> (
    Option<String>,
    Result<suprnova::HttpResponse, suprnova::HttpResponse>,
) {
    use bytes::Bytes;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use suprnova::Request;
    use suprnova::middleware::Middleware;
    use tokio::io::AsyncWriteExt;
    use tokio::sync::oneshot;

    // Step 1: seed the store row so `read(seed_id)` returns it.
    let store = middleware.store();
    let mut seed =
        suprnova::session::SessionData::new(seed_id.to_string(), "seed-csrf-token".to_string());
    seed.user_id = seed_user_id.map(|s| s.to_string());
    store
        .write(&seed)
        .await
        .expect("seed pre-rotation session row");

    // Step 2: encrypt the seed id into the wire format the middleware
    // will read off the inbound `suprnova_session` cookie.
    let encrypted_cookie = suprnova::Crypt::encrypt_string(suprnova::CryptPurpose::Cookie, seed_id)
        .expect("encrypt seed session id");

    // Step 3: build a real `Request` over a duplex pipe — same shape
    // as `framework/tests/remember_me.rs::middleware_hydrates_session_from_remember_cookie`.
    let mut http_bytes = Vec::new();
    http_bytes.extend_from_slice(b"GET / HTTP/1.1\r\n");
    http_bytes.extend_from_slice(b"Host: localhost\r\n");
    http_bytes
        .extend_from_slice(format!("Cookie: suprnova_session={encrypted_cookie}\r\n").as_bytes());
    http_bytes.extend_from_slice(b"Content-Length: 0\r\n\r\n");

    let (req_tx, req_rx) = oneshot::channel::<Request>();
    let req_tx = std::sync::Mutex::new(Some(req_tx));
    let duplex_cap = http_bytes.len() + 64 * 1024;
    let (client_io, server_io) = tokio::io::duplex(duplex_cap);

    tokio::spawn(async move {
        let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
            let wrapped = Request::new(req);
            if let Ok(mut guard) = req_tx.lock()
                && let Some(tx) = guard.take()
            {
                let _ = tx.send(wrapped);
            }
            async {
                std::future::pending::<()>().await;
                Ok::<_, Infallible>(hyper::Response::new(http_body_util::Empty::<Bytes>::new()))
            }
        });
        let _ = http1::Builder::new()
            .serve_connection(TokioIo::new(server_io), svc)
            .await;
    });

    {
        let mut client = client_io;
        client.write_all(&http_bytes).await.unwrap();
    }
    let request = req_rx.await.expect("server received request");

    // Step 4: the handler runs the caller's `action` (e.g. `Auth::login`),
    // then captures the rotated session id from the per-request scope and
    // hands it back through the shared cell.
    let observed = Arc::new(std::sync::Mutex::new(None::<String>));
    let observed_clone = observed.clone();
    let next: suprnova::middleware::Next = Arc::new(move |_req| {
        let observed = observed_clone.clone();
        let action = action.clone();
        Box::pin(async move {
            // Auth request-state scope must wrap the inner work so
            // `Auth::id()` / `request_state::clear_current_user()` behave
            // the way they do in a real request — they're task-locals,
            // and the middleware doesn't install them itself.
            request_state::request_state_scope_for_test(async move {
                action().await;
                let id = suprnova::session::session().map(|s| s.id);
                *observed.lock().unwrap() = id;
                Ok(suprnova::HttpResponse::text("ok"))
            })
            .await
        })
    });

    let response = middleware.handle(request, next).await;
    let new_id = observed.lock().unwrap().clone();
    (new_id, response)
}

/// `Auth::logout_and_invalidate` rotates the session id; the middleware
/// MUST destroy the row keyed on the OLD id so an attacker holding the
/// prior encrypted cookie cannot replay it to remain authenticated.
/// Mirrors Laravel `Store::migrate(true)` semantics and closes HIGH H3.
#[test]
fn logout_and_invalidate_destroys_old_session_row() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        // Seed id MUST match `is_valid_session_id`'s shape (40
        // lowercase-alphanumeric, no underscores) so the L8 cookie
        // shape gate accepts it as a legitimate inbound id rather
        // than minting a fresh one.
        let seed_id = "h3oldsessionidaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
        let middleware = SessionMiddleware::with_store(
            // `cookie_secure(false)` keeps the test HTTPS-agnostic, same
            // as `remember_me.rs::middleware_hydrates...`.
            suprnova::session::SessionConfig {
                cookie_secure: false,
                ..suprnova::session::SessionConfig::default()
            },
            Arc::new(suprnova::session::DatabaseSessionDriver::new(
                std::time::Duration::from_secs(3600),
            )),
        );

        // Pre-flight: seed an authed session, then drive a request that
        // calls `logout_and_invalidate`. The middleware-side fix MUST
        // destroy the seed row keyed on the OLD id.
        let action: HandlerAction = Arc::new(|| {
            Box::pin(async move {
                Auth::login(the_user(), false).await.unwrap();
                Auth::logout_and_invalidate().await.unwrap();
            })
        });
        let (new_id, _response) =
            drive_middleware_with_session_cookie(&seed_id, Some("7"), &middleware, action).await;

        let new_id = new_id.expect("handler observed a rotated session id");
        assert_ne!(
            new_id, seed_id,
            "logout_and_invalidate must rotate the session id away from the inbound cookie's id"
        );

        // The real assertion: the OLD row keyed on `seed_id` is gone.
        // Without the middleware fix, this row would survive — still
        // carrying `user_id = Some("7")` from when we seeded it — and
        // the prior cookie would replay successfully.
        let store = middleware.store();
        let leftover = store
            .read(&seed_id)
            .await
            .expect("read seed id after rotation");
        assert!(
            leftover.is_none(),
            "old session row at {seed_id} must be destroyed once the id rotates; \
             leaving it lets an attacker replay the prior encrypted cookie"
        );
        // And the new row exists — proving we actually persisted to the
        // new id rather than just dropping everything.
        let post = store.read(&new_id).await.expect("read post-rotation id");
        assert!(
            post.is_some(),
            "rotated session row at {new_id} must exist; cookie would otherwise name a phantom"
        );
    });
}

/// `Auth::login` rotates the session id at pre-auth → post-auth (session
/// fixation defence). The pre-auth row must be destroyed for the same
/// reason as H3, even though `login` is the well-trodden path: leaving
/// the pre-auth row alive accumulates a DB-row leak at the request rate
/// and (if any anonymous state was stashed there) lets it survive past
/// the rotation. Closes MEDIUM M3.
#[test]
fn login_destroys_pre_auth_session_row() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        // Seed id MUST match `is_valid_session_id`'s shape (40
        // lowercase-alphanumeric, no underscores) so the L8 cookie
        // shape gate accepts it as a legitimate inbound id.
        let seed_id = "m3preauthsessionidbbbbbbbbbbbbbbbbbbbbbb".to_string();
        let middleware = SessionMiddleware::with_store(
            suprnova::session::SessionConfig {
                cookie_secure: false,
                ..suprnova::session::SessionConfig::default()
            },
            Arc::new(suprnova::session::DatabaseSessionDriver::new(
                std::time::Duration::from_secs(3600),
            )),
        );

        // Seed a pre-auth (anonymous) session row, then drive a request
        // that calls `Auth::login`. The middleware fix MUST destroy the
        // pre-auth row.
        let action: HandlerAction = Arc::new(|| {
            Box::pin(async move {
                Auth::login(the_user(), false).await.unwrap();
            })
        });
        let (new_id, _response) =
            drive_middleware_with_session_cookie(&seed_id, None, &middleware, action).await;

        let new_id = new_id.expect("handler observed a rotated session id");
        assert_ne!(
            new_id, seed_id,
            "Auth::login must rotate the session id (fixation defence)"
        );

        let store = middleware.store();
        let leftover = store
            .read(&seed_id)
            .await
            .expect("read pre-auth id after login");
        assert!(
            leftover.is_none(),
            "pre-auth session row at {seed_id} must be destroyed once login rotates the id; \
             leaving it lets stale anonymous state survive AND leaks one DB row per login at TTL"
        );
        let post = store.read(&new_id).await.expect("read post-login id");
        assert!(
            post.is_some(),
            "post-login session row at {new_id} must exist"
        );
    });
}

// Logout clears BOTH 2FA pending slots (user_id + remember preference) — they
// are auth state too, and a tear-down that drops one but leaves the other
// strands the auth state machine for the next request. The pending_remember
// slot was added with the 2FA challenge integration; without an explicit
// clear in `clear_authentication` it would survive logout and bleed into a
// next user's login on the same browser.
#[test]
fn facade_logout_clears_both_two_factor_pending_slots() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        run_in_request(async {
            Auth::login(the_user(), false).await.unwrap();
            // Install both pending slots — typically `start_challenge`
            // would do this, but we set them directly to assert the
            // tear-down clears each one independently.
            suprnova::session::set_two_factor_pending("7");
            suprnova::session::set_two_factor_pending_remember(true);
            assert_eq!(
                suprnova::session::two_factor_pending_user_id(),
                Some("7".to_string())
            );
            assert!(suprnova::session::two_factor_pending_remember());

            Auth::logout().await.unwrap();

            assert_eq!(
                suprnova::session::two_factor_pending_user_id(),
                None,
                "logout must clear pending user-id slot"
            );
            assert!(
                !suprnova::session::two_factor_pending_remember(),
                "logout must clear pending remember-me slot"
            );
        })
        .await;
    });
}
