//! End-to-end tests for remember-me cookie flow (codex review finding #13).
//!
//! Covers:
//!
//! 1. `login_remember_issues_cookie_and_persists_token` — issuing a
//!    token writes a hashed row and the middleware emits an encrypted
//!    `remember_me` cookie.
//! 2. `remember_cookie_authenticates_after_session_expiry` — when the
//!    session cookie is absent, a valid `remember_me` cookie hydrates
//!    the session through verify_and_rotate.
//! 3. `remember_cookie_rotates_on_use` — a successful verify deletes
//!    the matched row and issues a fresh one; the old cookie cannot
//!    authenticate twice.
//! 4. `revoke_remember_tokens_clears_all_rows_for_user` — calling the
//!    revoke helper deletes every row for the user (multi-device
//!    "log out everywhere").
//! 5. `expired_token_rejected_and_cleaned_up_by_prune` — `expires_at`
//!    in the past never authenticates and is removed by `prune_expired`.
//! 6. `forged_cookie_does_not_authenticate` — a random plaintext does
//!    not match any hashed row; verify returns None.
//!
//! # Harness
//!
//! - One tokio `Runtime` (`RT`) shared across the binary; the SQLx
//!   pool is bound to the runtime that created it (mirrors
//!   `torii_integration.rs`).
//! - `LocalMigrator` materialises only the `remember_tokens` and
//!   `sessions` tables — `Auth::login_remember` writes to one and the
//!   middleware reads from the other. We do not need users/torii to
//!   exercise the remember-me path; remember-me operates on an opaque
//!   `user_id: String`.
//! - `Crypt` is installed once via `_test_install_key` (the test-only
//!   helper exposed at `framework/src/crypto/mod.rs`). The `OnceLock`
//!   is process-wide so subsequent calls are silent no-ops.

use once_cell::sync::Lazy;
use sea_orm_migration::prelude::*;
use sea_orm_migration::MigratorTrait;
use tokio::runtime::Runtime;

use suprnova::http::cookie::Cookie;
use suprnova::session::SessionConfig;
use suprnova::Auth;

/// Shared runtime — SQLx pools die with their creating runtime.
static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("tokio runtime"));

/// One-shot setup: install Crypt, build a shared in-memory SQLite
/// connection registered in the global App container, run the local
/// migrator. All tests reuse the same DB; each test inserts under a
/// unique `user_id` to avoid cross-test interference on the verify
/// scan.
///
/// We bypass `TestDatabase` because it registers the connection in a
/// thread-local `TestContainer`. cargo test spreads tests across
/// worker threads, so a thread-local registration is invisible to
/// every test except the one that wrote it. Registering directly in
/// `App::singleton` (process-global, RwLock-backed) makes the
/// connection visible to all worker threads.
static SETUP: Lazy<()> = Lazy::new(|| {
    // Install Crypt with a fresh key. `_test_install_key` is
    // idempotent — returns false if a key already exists, which is
    // fine.
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
        // Migrate before publishing — every test reads through
        // `DB::connection()` and assumes the tables already exist.
        LocalMigrator::up(conn.inner(), None)
            .await
            .expect("run local migrator");
        // Publish to the process-global App container. `App::resolve`
        // and `DB::connection` will return this connection from every
        // worker thread.
        suprnova::App::singleton(conn);
    });
});

/// Local migrator: just the `sessions` and `remember_tokens` tables.
/// The framework's auth/remember code does not need anything else.
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
                    .col(ColumnDef::new(RememberTokens::TokenHash).string().not_null())
                    .col(ColumnDef::new(RememberTokens::ExpiresAt).timestamp().not_null())
                    .col(
                        ColumnDef::new(RememberTokens::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(RememberTokens::LastUsedAt).timestamp().null())
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
    TokenHash,
    ExpiresAt,
    CreatedAt,
    LastUsedAt,
}

// Helpers

/// Count rows in `remember_tokens` for a specific `user_id`.
async fn count_tokens_for(user_id: &str) -> u64 {
    use sea_orm::ColumnTrait;
    use sea_orm::EntityTrait;
    use sea_orm::QueryFilter;
    let conn = suprnova::DB::connection().expect("db connection");
    suprnova::auth::remember::entity::Entity::find()
        .filter(suprnova::auth::remember::entity::Column::UserId.eq(user_id))
        .all(conn.inner())
        .await
        .expect("count tokens query")
        .len() as u64
}

/// Drive `fut` inside a fresh session-scope AND pending-cookies-scope.
/// Returns `(handler_result, captured_pending_cookies)`. The pending
/// cookies are what the session middleware would have attached to the
/// outgoing response.
async fn run_in_request<F, T>(fut: F) -> (T, Vec<Cookie>)
where
    F: std::future::Future<Output = T>,
{
    let session_slot = suprnova::session::new_session_slot_for_test();
    let pending_slot = suprnova::session::new_pending_cookies_slot_for_test();
    let result = suprnova::session::session_scope_for_test(
        session_slot,
        suprnova::session::pending_cookies_scope_for_test(pending_slot.clone(), fut),
    )
    .await;
    let pending = std::mem::take(&mut *pending_slot.lock().unwrap());
    (result, pending)
}

/// Extract the encrypted plaintext from a `remember_me` cookie that
/// `Auth::login_remember` queued. Panics if no such cookie was queued
/// or if it does not decrypt — the test should have placed one.
fn decode_remember_cookie(cookies: &[Cookie]) -> String {
    let cookie = cookies
        .iter()
        .find(|c| c.name() == "remember_me")
        .expect("a remember_me cookie should have been queued");
    Cookie::read_encrypted(cookie.value()).expect("remember_me cookie must decrypt")
}

/// Insert a raw row directly into `remember_tokens` (bypassing
/// `issue`). Used for the expired-token scenario where we need a row
/// whose `expires_at` is in the past — `issue` always generates fresh
/// future-expiring rows.
async fn insert_raw_token(
    user_id: &str,
    token_hash: &str,
    expires_at: chrono::DateTime<chrono::Utc>,
) {
    use sea_orm::EntityTrait;
    use sea_orm::Set;
    let conn = suprnova::DB::connection().expect("db connection");
    let now = chrono::Utc::now();
    let model = suprnova::auth::remember::entity::ActiveModel {
        user_id: Set(user_id.to_string()),
        token_hash: Set(token_hash.to_string()),
        expires_at: Set(expires_at.naive_utc()),
        created_at: Set(now.naive_utc()),
        last_used_at: Set(None),
        ..Default::default()
    };
    suprnova::auth::remember::entity::Entity::insert(model)
        .exec(conn.inner())
        .await
        .expect("insert raw token");
}

// Tests

/// Test 1: `login_remember` writes a hashed row and queues an
/// encrypted `remember_me` cookie. The cookie is HttpOnly, has a
/// Max-Age, and its value is NOT the raw plaintext token (it's
/// encrypted under Crypt).
#[test]
fn login_remember_issues_cookie_and_persists_token() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let user_id = "test-user-issue";
        let ttl_minutes: i64 = 60 * 24; // 1 day

        let (result, pending) =
            run_in_request(async { Auth::login_remember(user_id, ttl_minutes).await }).await;
        result.expect("login_remember should succeed");

        // Row inserted.
        let count = count_tokens_for(user_id).await;
        assert_eq!(count, 1, "exactly one remember_tokens row expected");

        // Cookie queued and decrypts to a 43-char base64 plaintext.
        let plaintext = decode_remember_cookie(&pending);
        assert_eq!(plaintext.len(), 43, "32-byte token -> 43 base64 chars");

        let cookie = pending
            .iter()
            .find(|c| c.name() == "remember_me")
            .expect("remember_me cookie queued");

        // Wire-format value must NOT equal the plaintext — that would
        // mean we stored a bearer credential in cleartext.
        assert_ne!(
            cookie.value(),
            plaintext,
            "cookie value must be the encrypted blob, never the plaintext token"
        );

        let header = cookie.to_header_value();
        assert!(header.contains("HttpOnly"), "cookie must be HttpOnly");
        assert!(header.contains("SameSite=Lax"), "default SameSite=Lax");
        // Cookie's Max-Age must MATCH the row's TTL — codex finding
        // #13 required "expires-at matches token expiration." 1 day
        // = 86400 s.
        let expected_max_age = (ttl_minutes as u64) * 60;
        assert!(
            header.contains(&format!("Max-Age={expected_max_age}")),
            "Max-Age must match ttl_minutes -> seconds (expected {expected_max_age}), got: {header}"
        );
    });
}

/// Test 2: with no session active, a valid `remember_me` cookie
/// hydrates a new session and the response carries a freshly-rotated
/// cookie. Simulates the "browser was closed, session cookie evicted,
/// user returns" path.
#[test]
fn remember_cookie_authenticates_after_session_expiry() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let user_id = "test-user-reauth";
        let ttl_minutes: i64 = 60 * 24;

        // Step 1: issue a token directly (no session — simulating the
        // server-side state right after the original login_remember).
        let plaintext = suprnova::auth::remember::issue(user_id, ttl_minutes)
            .await
            .expect("issue token");
        assert_eq!(count_tokens_for(user_id).await, 1);

        // Step 2: drive the middleware path — verify_and_rotate is
        // what the middleware calls when the session is missing.
        let result = suprnova::auth::remember::verify_and_rotate(&plaintext, ttl_minutes)
            .await
            .expect("verify_and_rotate query");

        let (hydrated_user_id, new_plaintext) = result.expect("token should match");
        assert_eq!(hydrated_user_id, user_id);
        assert_eq!(
            count_tokens_for(user_id).await,
            1,
            "rotation: old row deleted + new row inserted = still 1"
        );

        // The new plaintext is different and itself a valid token.
        assert_ne!(new_plaintext, plaintext, "rotation must mint a new token");
        let third = suprnova::auth::remember::verify_and_rotate(&new_plaintext, ttl_minutes)
            .await
            .expect("verify new plaintext")
            .expect("new plaintext must verify");
        assert_eq!(third.0, user_id);
    });
}

/// Test 3 (rotation invariant): an already-used cookie cannot
/// authenticate again. The matched row is DELETED on first use; replay
/// returns `Ok(None)`.
#[test]
fn remember_cookie_rotates_on_use() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let user_id = "test-user-rotate";
        let ttl_minutes: i64 = 60 * 24;

        let plaintext_a = suprnova::auth::remember::issue(user_id, ttl_minutes)
            .await
            .expect("issue A");

        // First use: succeeds, mints plaintext_b.
        let (uid, plaintext_b) =
            suprnova::auth::remember::verify_and_rotate(&plaintext_a, ttl_minutes)
                .await
                .expect("verify A")
                .expect("A must match");
        assert_eq!(uid, user_id);
        assert_ne!(plaintext_a, plaintext_b);

        // Second use of plaintext_a: row gone, must NOT verify.
        let replay = suprnova::auth::remember::verify_and_rotate(&plaintext_a, ttl_minutes)
            .await
            .expect("verify A replay");
        assert!(
            replay.is_none(),
            "already-rotated token must not re-authenticate"
        );

        // plaintext_b is the new live token.
        let (uid_b, _) = suprnova::auth::remember::verify_and_rotate(&plaintext_b, ttl_minutes)
            .await
            .expect("verify B")
            .expect("B must match");
        assert_eq!(uid_b, user_id);
    });
}

/// Test 4: `revoke_all_for_user` deletes EVERY row for the user
/// (two-device scenario). Subsequent verify of either captured
/// plaintext fails.
#[test]
fn revoke_remember_tokens_clears_all_rows_for_user() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let user_id = "test-user-revoke";
        let ttl_minutes: i64 = 60 * 24;

        let pt1 = suprnova::auth::remember::issue(user_id, ttl_minutes)
            .await
            .expect("issue 1");
        let pt2 = suprnova::auth::remember::issue(user_id, ttl_minutes)
            .await
            .expect("issue 2");
        assert_eq!(count_tokens_for(user_id).await, 2);

        let removed = suprnova::auth::remember::revoke_all_for_user(user_id)
            .await
            .expect("revoke_all");
        assert_eq!(removed, 2, "both rows must be removed");
        assert_eq!(count_tokens_for(user_id).await, 0);

        assert!(suprnova::auth::remember::verify_and_rotate(&pt1, ttl_minutes)
            .await
            .expect("verify post-revoke pt1")
            .is_none());
        assert!(suprnova::auth::remember::verify_and_rotate(&pt2, ttl_minutes)
            .await
            .expect("verify post-revoke pt2")
            .is_none());
    });
}

/// Test 5: a token whose `expires_at` is already in the past must NOT
/// authenticate (verify filters on `expires_at > now`).
/// `prune_expired` then removes it.
#[test]
fn expired_token_rejected_and_cleaned_up_by_prune() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let user_id = "test-user-expired";
        let ttl_minutes: i64 = 60 * 24;

        // Generate a real token + hash, but insert with expires_at in
        // the past. Bypasses `issue` (which always uses now + TTL).
        let (plaintext, hash) =
            suprnova::auth::remember::generate_token().expect("generate token");
        let past_expiry = chrono::Utc::now() - chrono::Duration::seconds(60);
        insert_raw_token(user_id, &hash, past_expiry).await;
        assert_eq!(count_tokens_for(user_id).await, 1);

        // Verify rejects expired rows up front (the WHERE expires_at > now
        // filter excludes them — they never reach the bcrypt compare).
        let result = suprnova::auth::remember::verify_and_rotate(&plaintext, ttl_minutes)
            .await
            .expect("verify expired");
        assert!(result.is_none(), "expired token must not authenticate");

        // Row is still there until pruned.
        assert_eq!(count_tokens_for(user_id).await, 1);

        let removed = suprnova::auth::remember::prune_expired()
            .await
            .expect("prune");
        assert!(
            removed >= 1,
            "prune must remove at least our expired row (removed={removed})"
        );
        assert_eq!(count_tokens_for(user_id).await, 0);
    });
}

/// Test 6: a forged plaintext that does not match any hashed row must
/// not authenticate. `verify_and_rotate` returns `Ok(None)` and no DB
/// rows change.
#[test]
fn forged_cookie_does_not_authenticate() {
    Lazy::force(&SETUP);

    RT.block_on(async {
        let user_id = "test-user-forged";
        let ttl_minutes: i64 = 60 * 24;

        // Issue one legitimate token so the verify scan has something
        // to compare against (proves the rejection isn't from an empty
        // table).
        let _legit = suprnova::auth::remember::issue(user_id, ttl_minutes)
            .await
            .expect("issue legit");
        let before = count_tokens_for(user_id).await;
        assert_eq!(before, 1);

        // A forged plaintext (43 chars to match the real shape, but
        // with a deterministic value that cannot collide with any
        // bcrypt-hashed real token).
        let forged = "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF";
        let result = suprnova::auth::remember::verify_and_rotate(forged, ttl_minutes)
            .await
            .expect("verify forged");
        assert!(result.is_none(), "forged token must not authenticate");

        // Row count unchanged — verify on a non-match must not mutate.
        assert_eq!(count_tokens_for(user_id).await, before);
    });
}

/// Test 7: the middleware helper `create_forget_remember_cookie`
/// produces a Max-Age=0 cookie. Wired as a unit test here rather than
/// the e2e suite because it exercises the helper directly — no DB
/// needed.
#[test]
fn forget_remember_cookie_clears_the_cookie() {
    Lazy::force(&SETUP);
    let config = SessionConfig::default();
    let clear = suprnova::session::middleware::create_forget_remember_cookie(&config);
    assert_eq!(clear.name(), "remember_me");
    let header = clear.to_header_value();
    assert!(
        header.contains("Max-Age=0"),
        "forget cookie must carry Max-Age=0"
    );
}

/// Test 8: `create_remember_cookie` respects
/// `SessionConfig::cookie_secure` — when secure=true the Set-Cookie
/// header carries the `Secure` attribute; when secure=false (local
/// dev), it doesn't.
#[test]
fn remember_cookie_respects_secure_flag() {
    Lazy::force(&SETUP);

    let secure_config = SessionConfig::default(); // cookie_secure = true
    let plaintext = "any-encrypted-plaintext";
    let max_age = std::time::Duration::from_secs(60 * 60); // 1 hour
    let cookie = suprnova::session::middleware::create_remember_cookie(
        &secure_config,
        plaintext,
        max_age,
    )
    .expect("encrypted cookie");
    let header = cookie.to_header_value();
    assert!(header.contains("Secure"), "production: cookie must be Secure");
    assert!(header.contains("HttpOnly"));
    assert!(header.contains("SameSite=Lax"));
    assert!(
        header.contains("Max-Age=3600"),
        "max_age parameter must control Max-Age, got: {header}"
    );

    let insecure_config = SessionConfig {
        cookie_secure: false,
        ..SessionConfig::default()
    };
    let cookie = suprnova::session::middleware::create_remember_cookie(
        &insecure_config,
        plaintext,
        max_age,
    )
    .expect("encrypted cookie");
    let header = cookie.to_header_value();
    assert!(
        !header.contains("Secure"),
        "local dev: Secure flag must be absent so cookies work over http"
    );
}

// ── End-to-end middleware test ────────────────────────────────────────

/// Test 9 (end-to-end): drive a real request through `SessionMiddleware`
/// carrying ONLY a `remember_me` cookie (no session cookie). The
/// middleware must:
///
/// 1. Decrypt the cookie and find the matching row.
/// 2. Rotate the token (delete + insert) so DB row count stays at 1.
/// 3. Hydrate the request-scoped session with the user's id so the
///    inner handler can call `Auth::id()` and observe the user.
/// 4. Attach a freshly-encrypted `remember_me` cookie to the response.
///
/// This is the only test that exercises the ~50 lines of remember-me
/// hydration logic in `SessionMiddleware::handle`. The other 8 tests
/// drive the underlying helpers directly. Without this test, a
/// regression in the middleware's cookie name lookup, decrypt
/// fallback, or task-local scoping ships untested.
#[test]
fn middleware_hydrates_session_from_remember_cookie() {
    use bytes::Bytes;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use std::sync::Arc;
    use suprnova::middleware::Middleware;
    use suprnova::Request;
    use tokio::io::AsyncWriteExt;
    use tokio::sync::oneshot;

    Lazy::force(&SETUP);

    RT.block_on(async {
        let user_id = "test-user-middleware";
        let ttl_minutes: i64 = 60 * 24; // 1 day

        // Step 1: issue a token directly and encrypt the plaintext
        // into the wire format the middleware will receive.
        let plaintext = suprnova::auth::remember::issue(user_id, ttl_minutes)
            .await
            .expect("issue token");
        let encrypted = suprnova::Crypt::encrypt_string(&plaintext).expect("encrypt cookie");
        assert_eq!(
            count_tokens_for(user_id).await,
            1,
            "fixture: one row before middleware runs"
        );

        // Step 2: build a real `Request` carrying just the remember-me
        // cookie. Use the same duplex-pipe pattern as
        // `framework/tests/common.rs::request_from_http_bytes`,
        // inlined here so this test does not pull in `common.rs`
        // (which is module-private).
        let mut http_bytes = Vec::new();
        http_bytes.extend_from_slice(b"GET / HTTP/1.1\r\n");
        http_bytes.extend_from_slice(b"Host: localhost\r\n");
        http_bytes.extend_from_slice(
            format!("Cookie: remember_me={encrypted}\r\n").as_bytes(),
        );
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
                    Ok::<_, Infallible>(hyper::Response::new(
                        http_body_util::Empty::<Bytes>::new(),
                    ))
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

        // Step 3: build a tiny handler that captures `Auth::id()` —
        // proof that the middleware hydrated the session before
        // calling next.
        let observed = Arc::new(std::sync::Mutex::new(None::<String>));
        let observed_clone = observed.clone();
        let next: suprnova::middleware::Next = Arc::new(move |_req| {
            let observed = observed_clone.clone();
            Box::pin(async move {
                let id = suprnova::Auth::id();
                *observed.lock().unwrap() = id;
                Ok(suprnova::HttpResponse::text("ok"))
            })
        });

        // Step 4: run the middleware. Use `cookie_secure(false)` so
        // we don't have to think about HTTPS in the test.
        let config = SessionConfig {
            cookie_secure: false,
            remember_lifetime: std::time::Duration::from_secs((ttl_minutes as u64) * 60),
            ..SessionConfig::default()
        };
        let middleware = suprnova::SessionMiddleware::new(config);
        let response = middleware.handle(request, next).await;

        // Step 5: handler must have observed the hydrated user id.
        let captured = observed.lock().unwrap().clone();
        assert_eq!(
            captured.as_deref(),
            Some(user_id),
            "middleware must hydrate the session BEFORE calling next"
        );

        // Step 6: rotation invariant — still exactly one row for the
        // user (old row deleted, new row inserted).
        assert_eq!(
            count_tokens_for(user_id).await,
            1,
            "rotation: old row deleted + new row inserted = still 1"
        );

        // Step 7: response carries a fresh remember_me cookie whose
        // ciphertext is different from the inbound one (verifying we
        // rotated, not just echoed the input back). `HttpResponse`
        // does not expose its headers directly — go through
        // `into_hyper()` which gives access to `hyper::HeaderMap`.
        let response = match response {
            Ok(r) => r,
            Err(_) => panic!("middleware should not short-circuit the request"),
        };
        let hyper_resp = response.into_hyper();
        let remember_cookies: Vec<String> = hyper_resp
            .headers()
            .get_all("Set-Cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .filter(|c| c.starts_with("remember_me="))
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            remember_cookies.len(),
            1,
            "exactly one rotated remember_me cookie expected, got: {remember_cookies:?}"
        );

        let rotated_header = &remember_cookies[0];
        // Extract the cookie value: "remember_me=<value>; Path=...".
        let value_segment = rotated_header
            .split(';')
            .next()
            .expect("at least one segment");
        let new_ciphertext = value_segment
            .strip_prefix("remember_me=")
            .expect("starts with remember_me=");
        assert_ne!(
            new_ciphertext, encrypted,
            "rotated cookie must carry a different ciphertext than the input"
        );

        // Rotated cookie's Max-Age must match the new row's TTL so
        // the browser stops sending the cookie when the row expires.
        let expected_max_age = (ttl_minutes as u64) * 60;
        assert!(
            rotated_header.contains(&format!("Max-Age={expected_max_age}")),
            "rotated cookie's Max-Age must match the TTL (expected {expected_max_age}), got: {rotated_header}"
        );

        // The rotated cookie's plaintext must verify against the live
        // row (the post-rotation row).
        let rotated_plaintext = suprnova::Crypt::decrypt_string(new_ciphertext)
            .expect("rotated cookie must decrypt");
        let third =
            suprnova::auth::remember::verify_and_rotate(&rotated_plaintext, ttl_minutes)
                .await
                .expect("verify rotated plaintext")
                .expect("rotated plaintext must match the live row");
        assert_eq!(third.0, user_id);
    });
}

/// Test 10 (end-to-end, negative): a forged `remember_me` cookie does
/// NOT authenticate AND the middleware queues a clear cookie so the
/// client stops sending garbage.
#[test]
fn middleware_clears_forged_remember_cookie() {
    use bytes::Bytes;
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use std::convert::Infallible;
    use std::sync::Arc;
    use suprnova::middleware::Middleware;
    use suprnova::Request;
    use tokio::io::AsyncWriteExt;
    use tokio::sync::oneshot;

    Lazy::force(&SETUP);

    RT.block_on(async {
        // A forged plaintext encrypted under the legitimate key —
        // ciphertext is valid, but the plaintext does not match any
        // hashed row. The middleware must reject AND clear.
        let forged_plaintext = "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF";
        let encrypted =
            suprnova::Crypt::encrypt_string(forged_plaintext).expect("encrypt forged");

        let mut http_bytes = Vec::new();
        http_bytes.extend_from_slice(b"GET / HTTP/1.1\r\n");
        http_bytes.extend_from_slice(b"Host: localhost\r\n");
        http_bytes.extend_from_slice(
            format!("Cookie: remember_me={encrypted}\r\n").as_bytes(),
        );
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
                    Ok::<_, Infallible>(hyper::Response::new(
                        http_body_util::Empty::<Bytes>::new(),
                    ))
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

        let observed = Arc::new(std::sync::Mutex::new(None::<String>));
        let observed_clone = observed.clone();
        let next: suprnova::middleware::Next = Arc::new(move |_req| {
            let observed = observed_clone.clone();
            Box::pin(async move {
                *observed.lock().unwrap() = suprnova::Auth::id();
                Ok(suprnova::HttpResponse::text("ok"))
            })
        });

        let config = SessionConfig {
            cookie_secure: false,
            ..SessionConfig::default()
        };
        let middleware = suprnova::SessionMiddleware::new(config);
        let response = middleware.handle(request, next).await;

        // Handler must NOT have seen a user — the cookie didn't match.
        let captured = observed.lock().unwrap().clone();
        assert_eq!(captured, None, "forged cookie must not authenticate");

        // Response must clear the remember cookie (Max-Age=0).
        let response = match response {
            Ok(r) => r,
            Err(_) => panic!("middleware should not short-circuit the request"),
        };
        let hyper_resp = response.into_hyper();
        let cleared = hyper_resp
            .headers()
            .get_all("Set-Cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(|c| c.starts_with("remember_me=") && c.contains("Max-Age=0"));
        assert!(
            cleared,
            "middleware must clear the cookie when the token does not match"
        );
    });
}
