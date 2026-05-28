//! End-to-end tests for the 2FA challenge promotion flow.
//!
//! Covers the path `Auth::password().register(...)` → `TwoFactor::enroll`
//! → `TwoFactor::confirm` → `TwoFactor::start_challenge(_, remember)` →
//! `TwoFactor::complete_challenge(valid_totp)` and asserts the contract
//! the framework promises for the final step:
//!
//! * the session id rotates (session fixation defence);
//! * the CSRF token rotates;
//! * the standard `Auth\Login` + `Auth\Authenticated` lifecycle events
//!   fire, in addition to the 2FA-specific `TwoFactor\Challenged`;
//! * a fresh remember-me cookie is queued when the original login form
//!   set `remember=true`, and **no** cookie is queued when it was
//!   `false`.
//!
//! Shared-runtime + shared-DB + `TEST_LOCK` pattern from
//! `tests/auth_session_guard.rs` so the `EventFacade::fake` store is
//! serialised. Torii init follows `tests/email_verified_middleware.rs`.

use once_cell::sync::Lazy;
use sea_orm_migration::MigratorTrait;
use sea_orm_migration::prelude::*;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

use suprnova::auth::events::{Authenticated, Login};
use suprnova::auth_flows::events::{TwoFactorChallengeFailed, TwoFactorChallenged};
use suprnova::auth_flows::two_factor::migration::Migration as TwoFactorMigration;
use suprnova::auth_flows::two_factor::migration_replay::Migration as TwoFactorReplayMigration;
use suprnova::auth_flows::{BruteForce, TwoFactor, TwoFactorUser};
use suprnova::events::testing::{assert_dispatched, assert_not_dispatched};
use suprnova::http::cookie::Cookie;
use suprnova::torii_integration::{ToriiConfig, init_torii};
use suprnova::{Auth, Crypt, EncryptionKey, EventFacade};

/// Shared runtime — SQLx pools die with their creating runtime, so
/// every DB-touching path runs here.
static RT: Lazy<Runtime> = Lazy::new(|| Runtime::new().expect("tokio runtime"));

/// Serialises the event-fake critical sections; the fake store is
/// process-global.
static TEST_LOCK: Mutex<()> = Mutex::const_new(());

/// One-shot init: `Crypt`, framework DB (with 2FA + replay + remember
/// migrations), torii in-memory.
static SETUP: Lazy<()> = Lazy::new(|| {
    Crypt::init(EncryptionKey::generate());

    RT.block_on(async {
        // Framework DB — the `App::singleton(DbConnection)` install is
        // what backs `DB::connection()` for the 2FA + remember-me code.
        let config = suprnova::database::DatabaseConfig::builder()
            .url("sqlite::memory:")
            .max_connections(1)
            .min_connections(1)
            .logging(false)
            .build();
        let conn = suprnova::database::DbConnection::connect(&config)
            .await
            .expect("connect framework db");
        LocalMigrator::up(conn.inner(), None)
            .await
            .expect("run local migrator");
        suprnova::App::singleton(conn);

        // Torii — `complete_challenge` calls `find_user_by_id` to
        // resolve the pending user, so torii has to be initialised
        // with a working backend.
        let torii_config = ToriiConfig::sqlite_in_memory()
            .await
            .expect("torii in-memory connection")
            .apply_migrations(true);
        init_torii(torii_config).await.expect("init_torii");
    });
});

struct LocalMigrator;

#[async_trait::async_trait]
impl MigratorTrait for LocalMigrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(TwoFactorMigration),
            Box::new(TwoFactorReplayMigration),
            Box::new(CreateRememberTokensTable),
        ]
    }
}

/// Mirrors the canonical `remember_tokens` shape from
/// `tests/auth_session_guard.rs` / `tests/remember_me.rs` — the schema
/// consumer apps own and ship with their own migrator. The framework
/// does not ship this migration; tests recreate it.
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
                    .name("idx_two_factor_challenge_remember_selector")
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

/// Minimal `TwoFactorUser` for the enroll/confirm steps. The
/// challenge promotion itself reads pending state from the session;
/// it does not need a `TwoFactorUser` impl.
struct ChallengeUser {
    user_id: String,
    email: String,
}

impl TwoFactorUser for ChallengeUser {
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn email(&self) -> &str {
        &self.email
    }
}

/// Compute the live TOTP for an otpauth URL exactly like an
/// authenticator app would.
fn totp_code_for(otpauth_url: &str) -> String {
    use totp_rs::{Algorithm, Secret, TOTP};
    let url = url::Url::parse(otpauth_url).unwrap();
    let secret = url
        .query_pairs()
        .find(|(k, _)| k == "secret")
        .map(|(_, v)| v.into_owned())
        .expect("otpauth url must contain a secret query param");
    let bytes = Secret::Encoded(secret).to_bytes().unwrap();
    TOTP::new(Algorithm::SHA1, 6, 1, 30, bytes, None, "user".into())
        .unwrap()
        .generate_current()
        .unwrap()
}

/// Drive `fut` inside the three task-local scopes a real request
/// installs: the session, the pending-cookies bag, and the auth
/// request state. The caller passes the pending-cookies slot in so
/// they can keep an `Arc` clone outside the closure and inspect the
/// queued cookies live from inside the closure — same pattern as
/// `tests/remember_me.rs`.
async fn run_in_request_with_slot<F, T>(pending_slot: Arc<StdMutex<Vec<Cookie>>>, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let session_slot = suprnova::session::new_session_slot_for_test();
    suprnova::session::session_scope_for_test(
        session_slot,
        suprnova::session::pending_cookies_scope_for_test(
            pending_slot,
            suprnova::auth::request_state::request_state_scope_for_test(fut),
        ),
    )
    .await
}

/// Convenience: tests that don't care about pending cookies don't have
/// to thread an inspector slot through. Creates a throwaway slot.
async fn run_in_request<F, T>(fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let pending_slot = suprnova::session::new_pending_cookies_slot_for_test();
    run_in_request_with_slot(pending_slot, fut).await
}

/// Helper: read the current session id (panics if no scope installed).
fn current_session_id() -> String {
    suprnova::session::session()
        .map(|s| s.id)
        .expect("session scope must be installed")
}

/// Helper: read the current CSRF token (panics if no scope installed).
fn current_csrf() -> String {
    suprnova::session::session()
        .map(|s| s.csrf_token)
        .expect("session scope must be installed")
}

/// Helper: register a fresh torii user + enroll/confirm 2FA against
/// it. Returns `(user_id, email, otpauth_url)` so the caller can drive
/// `start_challenge` / `complete_challenge` with valid codes.
async fn register_and_enroll(label: &str) -> (String, String, String) {
    let email = format!("{label}@2fa.test");
    let user = Auth::password()
        .register(&email, "p@ssw0rd")
        .await
        .expect("torii register");
    let user_id = user.id.to_string();

    let tf_user = ChallengeUser {
        user_id: user_id.clone(),
        email: email.clone(),
    };
    let resp = TwoFactor::enroll(&tf_user).await.expect("enroll");
    let confirm_code = totp_code_for(&resp.otpauth_url);
    TwoFactor::confirm(&tf_user, &confirm_code)
        .await
        .expect("confirm");
    (user_id, email, resp.otpauth_url)
}

#[test]
fn complete_challenge_rotates_session_id_and_csrf() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        let (user_id, _email, otpauth_url) = register_and_enroll("rotate").await;

        let (before_id, before_csrf, after_id, after_csrf) = run_in_request(async {
            let before_id = current_session_id();
            let before_csrf = current_csrf();

            TwoFactor::start_challenge(&user_id, false)
                .await
                .expect("start_challenge");
            // confirm above stamped no replay claim; verify inside
            // complete_challenge will stamp the current timestep.
            let totp = totp_code_for(&otpauth_url);
            TwoFactor::complete_challenge(&totp)
                .await
                .expect("complete_challenge");

            let after_id = current_session_id();
            let after_csrf = current_csrf();
            (before_id, before_csrf, after_id, after_csrf)
        })
        .await;

        assert_ne!(
            before_id, after_id,
            "session id must rotate on challenge complete to defeat session fixation"
        );
        assert_ne!(
            before_csrf, after_csrf,
            "CSRF token must rotate on challenge complete"
        );
    });
}

#[test]
fn complete_challenge_dispatches_login_and_authenticated_and_challenged() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        let (user_id, _email, otpauth_url) = register_and_enroll("events").await;
        let captured_user_id = user_id.clone();

        run_in_request(async {
            TwoFactor::start_challenge(&user_id, false)
                .await
                .expect("start_challenge");
            let totp = totp_code_for(&otpauth_url);
            TwoFactor::complete_challenge(&totp)
                .await
                .expect("complete_challenge");
        })
        .await;

        assert_dispatched::<Login>(|e| e.user_id == captured_user_id && !e.remember);
        assert_dispatched::<Authenticated>(|e| e.user_id == captured_user_id);
        assert_dispatched::<TwoFactorChallenged>(|e| e.user_id == captured_user_id);
    });
}

#[test]
fn complete_challenge_with_remember_true_reissues_remember_me_cookie() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        let (user_id, _email, otpauth_url) = register_and_enroll("remember-true").await;
        let captured_user_id = user_id.clone();

        // Pre-create the slot so we can clone it for live inspection
        // inside the closure — the scope retains the original, our
        // clone gives a read window from outside the scope's borrow.
        let pending_slot = suprnova::session::new_pending_cookies_slot_for_test();
        let inspector = pending_slot.clone();

        let (after_start, after_complete) = run_in_request_with_slot(pending_slot, async move {
            TwoFactor::start_challenge(&user_id, true)
                .await
                .expect("start_challenge");
            let after_start = inspector.lock().unwrap().clone();
            let totp = totp_code_for(&otpauth_url);
            TwoFactor::complete_challenge(&totp)
                .await
                .expect("complete_challenge");
            let after_complete = inspector.lock().unwrap().clone();
            (after_start, after_complete)
        })
        .await;

        // start_challenge queues a clear cookie (revoke); complete_
        // challenge with remember=true must add a FRESH remember_me
        // cookie on top of whatever's already there.
        assert!(
            after_complete.len() > after_start.len(),
            "remember=true must push an additional cookie at complete_challenge; \
             start={start}, complete={complete}",
            start = after_start.len(),
            complete = after_complete.len(),
        );
        // The new entry is the remember-me cookie carrying a value
        // (not the empty-value clear cookie).
        assert!(
            after_complete
                .iter()
                .any(|c| c.name() == "remember_me" && !c.value().is_empty()),
            "remember=true must queue a fresh remember_me cookie with a non-empty value"
        );

        assert_dispatched::<Login>(|e| e.user_id == captured_user_id && e.remember);
        assert_dispatched::<Authenticated>(|e| e.user_id == captured_user_id);
    });
}

#[test]
fn complete_challenge_with_remember_false_does_not_issue_remember_me_cookie() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        let (user_id, _email, otpauth_url) = register_and_enroll("remember-false").await;
        let captured_user_id = user_id.clone();

        let pending_slot = suprnova::session::new_pending_cookies_slot_for_test();
        let inspector = pending_slot.clone();

        let (after_start, after_complete) = run_in_request_with_slot(pending_slot, async move {
            TwoFactor::start_challenge(&user_id, false)
                .await
                .expect("start_challenge");
            let after_start = inspector.lock().unwrap().clone();
            let totp = totp_code_for(&otpauth_url);
            TwoFactor::complete_challenge(&totp)
                .await
                .expect("complete_challenge");
            let after_complete = inspector.lock().unwrap().clone();
            (after_start, after_complete)
        })
        .await;

        // remember=false → complete_challenge must NOT push a new
        // cookie. The slot may still hold the clear cookie that
        // start_challenge queued; complete_challenge adds nothing.
        assert_eq!(
            after_complete.len(),
            after_start.len(),
            "remember=false must not queue any cookie at complete_challenge; \
             before={before}, after={after}",
            before = after_start.len(),
            after = after_complete.len(),
        );

        assert_dispatched::<Login>(|e| e.user_id == captured_user_id && !e.remember);
        assert_dispatched::<Authenticated>(|e| e.user_id == captured_user_id);
        // Sanity: `Login{remember:true}` was NOT dispatched.
        assert_not_dispatched::<Login>(|e| e.remember);
    });
}

#[test]
fn complete_challenge_with_bad_code_records_single_brute_force_attempt() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        let (user_id, email, _otpauth_url) = register_and_enroll("bf-single").await;

        // Baseline: zero failed attempts.
        let before = BruteForce::get_lockout_status(&email).await.unwrap();
        assert_eq!(
            before.failed_attempts, 0,
            "fresh user must start with zero failed attempts"
        );

        run_in_request(async {
            TwoFactor::start_challenge(&user_id, false)
                .await
                .expect("start_challenge");
            // "000000" is overwhelmingly likely to not be the current
            // TOTP and not a recovery code (recovery codes are 8-char
            // alnum). Both validation paths reject it.
            let err = TwoFactor::complete_challenge("000000")
                .await
                .expect_err("bad code must fail");
            assert_eq!(err.status_code(), 401, "wrong code is 401, not 429");
        })
        .await;

        // The single bad submission must count as ONE attempt, not two
        // (one from verify failing + one from consume_recovery_code
        // failing). The fix factors out silent verify/consume_recovery
        // cores and records the canonical attempt at the outer layer.
        let after = BruteForce::get_lockout_status(&email).await.unwrap();
        assert_eq!(
            after.failed_attempts, 1,
            "bad code must record exactly one failed attempt; got {}",
            after.failed_attempts
        );
    });
}

#[test]
fn complete_challenge_with_bad_code_dispatches_failed_event_and_no_login() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        let (user_id, _email, _otpauth_url) = register_and_enroll("failed-event").await;
        let captured_user_id = user_id.clone();

        run_in_request(async {
            TwoFactor::start_challenge(&user_id, false)
                .await
                .expect("start_challenge");
            let err = TwoFactor::complete_challenge("000000")
                .await
                .expect_err("bad code must fail");
            assert_eq!(err.status_code(), 401);
        })
        .await;

        assert_dispatched::<TwoFactorChallengeFailed>(|e| e.user_id == captured_user_id);
        // The standard auth lifecycle events MUST NOT fire on a
        // failed challenge — listeners would otherwise see a "Login"
        // for a user who never actually got in.
        assert_not_dispatched::<Login>(|_| true);
        assert_not_dispatched::<Authenticated>(|_| true);
        assert_not_dispatched::<TwoFactorChallenged>(|_| true);
    });
}

#[test]
fn complete_challenge_rejects_locked_account_without_checking_code() {
    Lazy::force(&SETUP);
    RT.block_on(async {
        let _serial = TEST_LOCK.lock().await;
        let _fake = EventFacade::fake();

        let (user_id, email, otpauth_url) = register_and_enroll("locked").await;
        let captured_user_id = user_id.clone();

        // Drive the failed-attempt counter past the default threshold
        // (5) so the account is genuinely locked. Mirrors the lockout
        // setup pattern in `tests/brute_force.rs`.
        for _ in 0..6 {
            BruteForce::record_failed_attempt(&email, None)
                .await
                .expect("record_failed_attempt");
        }
        assert!(
            BruteForce::is_locked(&email).await.unwrap(),
            "lockout precondition: account must be locked before complete_challenge"
        );

        // Even the CORRECT TOTP code must be rejected with 429 — a
        // locked account cannot bypass the lockout by submitting the
        // right code. This is the symmetric counterpart of the
        // password path's `LoginThrottleMiddleware` gate.
        run_in_request(async {
            TwoFactor::start_challenge(&user_id, false)
                .await
                .expect("start_challenge");
            let valid_totp = totp_code_for(&otpauth_url);
            let err = TwoFactor::complete_challenge(&valid_totp)
                .await
                .expect_err("locked account must be rejected");
            assert_eq!(
                err.status_code(),
                429,
                "locked-account rejection must be 429 Too Many Requests, not 401"
            );
        })
        .await;

        assert_dispatched::<TwoFactorChallengeFailed>(|e| e.user_id == captured_user_id);
        assert_not_dispatched::<Login>(|_| true);
        assert_not_dispatched::<Authenticated>(|_| true);
        assert_not_dispatched::<TwoFactorChallenged>(|_| true);
    });
}
