//! `PasswordReset` facade integration tests — provider-backed.
//!
//! Exercises the facade end-to-end against a real `#[suprnova::model]` user in
//! in-memory SQLite + the framework's own `auth_flow_tokens` table, with the
//! configured [`EloquentUserProvider`] as the active "users" provider. No
//! `init_torii`: the facade mints tokens through the provider-agnostic
//! `TokenStore` and rotates passwords through the provider.
//!
//! # Serial execution
//!
//! `Mail::fake()` swaps the process-global mail transport, so two parallel
//! tests installing fakes would cross-capture each other's messages. The DB is
//! thread-local (per `TestDatabase`), so the mail fake is the only remaining
//! global — `#[serial]` serializes against it. `complete()` also dispatches a
//! `PasswordChangedMail` through that same global transport, which is another
//! reason every test here is `#[serial]`.

use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sea_orm_migration::prelude::*;
use serial_test::serial;

use suprnova::auth::AuthConfig;
use suprnova::auth_flows::PasswordReset;
use suprnova::auth_flows::token_store::create_auth_flow_tokens_table;
use suprnova::container::testing::TestContainer;
use suprnova::session::{DatabaseSessionDriver, SessionData, SessionStore};
use suprnova::testing::TestDatabase;
use suprnova::{
    Auth, AuthManager, Authenticatable, CanResetPassword, EloquentUserProvider, MustVerifyEmail,
    UserProvider, model,
};

// Schema for the `sessions` table — ported verbatim from
// `framework/tests/session_destroy_for_user.rs` (which mirrors the app's
// `m20251208_220000_create_sessions_table` migration). Used so the password
// reset can actually revoke real session rows rather than hit a missing table.
#[derive(DeriveIden)]
enum Sessions {
    Table,
    Id,
    UserId,
    Payload,
    CsrfToken,
    LastActivity,
}

// Schema for the `remember_tokens` table — ported verbatim from
// `framework/tests/remember_me.rs` (which mirrors the app's
// `m20251208_230000_create_remember_tokens_table` migration).
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

// The app's `User` shape: a typed model that is also Authenticatable +
// CanResetPassword. `email_verified_at` is a nullable datetime; the model macro
// auto-injects `AsOptionalDateTime` on `Option<DateTime<Utc>>` fields.
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

/// Held-for-the-test guard: the `TestDatabase` carries the thread-local
/// container scope (it installs one via `TestContainer::fake` internally and
/// registers the `DbConnection` in it). We register the `AuthManager` +
/// provider into that SAME container — without replacing it — so the facade's
/// `active_user_provider()` and `DB::connection()` both resolve.
struct Harness {
    _db: TestDatabase,
}

/// Fresh in-memory DB with a `users` table, the `auth_flow_tokens` table, and
/// an `EloquentUserProvider::<TestUser>` registered as the active "users"
/// provider. Seeds one user (`ada@x.com` / bcrypt(`oldpass`)). Also sets
/// `MAIL_FROM` (the facade fails closed without it).
async fn setup() -> Harness {
    use sea_orm::ConnectionTrait;

    // SAFETY: every test in this file is `#[serial]`; no parallel observer.
    unsafe {
        std::env::set_var("MAIL_FROM", "test-mailer@example.com");
    }

    let db = TestDatabase::sqlite_memory().await.expect("sqlite_memory");
    let conn = db.conn();
    conn.execute_unprepared(
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
    conn.execute(conn.get_database_backend().build(&create))
        .await
        .expect("create auth_flow_tokens table");

    // The `sessions` + `remember_tokens` tables, so a completed reset can
    // actually delete real session/remember rows (the revocation paths in
    // `PasswordReset::complete` target these). Built from the same DeriveIden
    // schemas the dedicated session/remember tests use, executed against the
    // same connection via the `auth_flow_tokens` idiom above.
    let create_sessions = Table::create()
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
        .to_owned();
    conn.execute(conn.get_database_backend().build(&create_sessions))
        .await
        .expect("create sessions table");

    let create_remember = Table::create()
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
        .to_owned();
    conn.execute(conn.get_database_backend().build(&create_remember))
        .await
        .expect("create remember_tokens table");

    let create_remember_idx = Index::create()
        .name("idx_test_pwreset_remember_tokens_selector")
        .table(RememberTokens::Table)
        .col(RememberTokens::Selector)
        .unique()
        .to_owned();
    conn.execute(conn.get_database_backend().build(&create_remember_idx))
        .await
        .expect("create remember_tokens selector index");

    let hash = suprnova::hash("oldpass").expect("hash");
    conn.execute_unprepared(&format!(
        "INSERT INTO users (email, password) VALUES ('ada@x.com', '{hash}')"
    ))
    .await
    .expect("seed user");

    // Register the Eloquent provider as the active "users" provider into the
    // SAME thread-local container `TestDatabase` already installed (do NOT call
    // `TestContainer::fake()` again — that would replace the container and drop
    // the DB binding). `AuthConfig::default()`'s "web" guard points at "users".
    TestContainer::singleton(AuthManager::new(AuthConfig::default()));
    Auth::register_provider("users", Arc::new(EloquentUserProvider::<TestUser>::new()))
        .expect("register provider");

    Harness { _db: db }
}

/// The seeded user's id (as the auth identifier string the facade returns).
async fn ada_id() -> String {
    EloquentUserProvider::<TestUser>::new()
        .retrieve_by_email("ada@x.com")
        .await
        .expect("lookup")
        .expect("ada exists")
        .id
}

/// Reload the seeded user from the DB by email — to assert the password rotation
/// persisted through the provider. Returns the bcrypt hash on file.
async fn reload_ada_hash() -> String {
    let p = EloquentUserProvider::<TestUser>::new();
    let user = p
        .retrieve_by_id("1")
        .await
        .expect("by id")
        .expect("ada exists");
    user.get_auth_password()
        .expect("password hash present")
        .to_string()
}

/// Count `remember_tokens` rows for a given user id. Mirrors
/// `remember_me.rs::count_tokens_for` — goes through the same entity surface.
async fn count_remember_for(user_id: &str) -> u64 {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    let conn = suprnova::DB::connection().expect("db connection");
    suprnova::auth::remember::entity::Entity::find()
        .filter(suprnova::auth::remember::entity::Column::UserId.eq(user_id))
        .all(conn.inner())
        .await
        .expect("count remember tokens query")
        .len() as u64
}

/// Pull the plaintext reset token out of the first captured mail's rendered
/// link. The text body renders the URL verbatim (the HTML body HTML-escapes
/// slashes).
fn token_from_fake(fake: &suprnova::mail::MailFake) -> String {
    let captured = fake.captured();
    let text = captured
        .first()
        .expect("at least one reset mail")
        .text
        .as_deref()
        .expect("reset mail has a text body");
    let link = text
        .lines()
        .find(|l| l.contains("token="))
        .expect("a line with the token link");
    link.rsplit("token=")
        .next()
        .expect("token after marker")
        .trim()
        .to_string()
}

#[tokio::test]
#[serial]
async fn reset_updates_password_revokes_and_is_single_use() {
    let _h = setup().await;
    let id = ada_id().await;

    // Seed a live session row + a live remember-me token belonging to Ada, so
    // the revocation paths in `complete()` have real rows to delete. Both are
    // keyed on the same auth identifier string (`id`) the facade revokes on.
    let session_driver = DatabaseSessionDriver::new(Duration::from_secs(3600));
    let mut ada_session = SessionData::new("ada-sess-1".into(), "ada-csrf".into());
    ada_session.user_id = Some(id.clone());
    session_driver
        .write(&ada_session)
        .await
        .expect("seed ada session");
    suprnova::auth::remember::issue(&id, 60 * 24)
        .await
        .expect("seed ada remember-me token");

    // Precondition: the seeded rows are really present. Without this the
    // post-complete "rows gone" assertions could pass vacuously against an
    // empty table — which is exactly the gap this test exists to close.
    assert!(
        session_driver
            .read("ada-sess-1")
            .await
            .expect("read seeded session")
            .is_some(),
        "the seeded session must exist before the reset completes"
    );
    assert_eq!(
        count_remember_for(&id).await,
        1,
        "exactly one seeded remember-me token must exist before the reset completes"
    );

    // Known email → a reset mail is sent.
    let fake = suprnova::mail::Mail::fake();
    PasswordReset::send_link("ada@x.com", "https://app.test/reset-password")
        .await
        .expect("send_link");
    fake.assert_sent_to("ada@x.com");
    let token = token_from_fake(&fake);

    // Unknown email → anti-enumeration: nothing sent, still Ok.
    {
        let fake2 = suprnova::mail::Mail::fake();
        PasswordReset::send_link("nobody@x.com", "https://app.test/reset-password")
            .await
            .expect("send_link unknown returns Ok (no leak)");
        assert_eq!(
            fake2.count(),
            0,
            "unknown email must not send any mail (anti-enumeration)"
        );
    }

    // check() is non-consuming: valid before AND after a second check.
    assert!(
        PasswordReset::check(&token).await.expect("check"),
        "the freshly issued token must be valid"
    );
    assert!(
        PasswordReset::check(&token).await.expect("check again"),
        "check() must not consume the token"
    );

    // complete() consumes the token, rotates the password, returns the id.
    let returned = PasswordReset::complete(&token, "newpass")
        .await
        .expect("complete");
    assert_eq!(returned, id, "complete() returns the user id string");

    // The new password verifies; the old one no longer does.
    let stored = reload_ada_hash().await;
    assert!(
        suprnova::hashing::verify("newpass", &stored).expect("verify new"),
        "the new password must verify against the stored hash"
    );
    assert!(
        !suprnova::hashing::verify("oldpass", &stored).expect("verify old"),
        "the old password must no longer verify"
    );

    // Revocation actually ran: Ada's session row and her remember-me token are
    // both gone. This proves `complete()` drove `session::destroy_all_for_user`
    // + `auth::remember::revoke_all_for_user` to completion against real rows.
    assert!(
        session_driver
            .read("ada-sess-1")
            .await
            .expect("read session post-reset")
            .is_none(),
        "the user's session must be revoked after a completed password reset"
    );
    assert_eq!(
        count_remember_for(&id).await,
        0,
        "the user's remember-me tokens must be revoked after a completed password reset"
    );

    // complete() also dispatches the PasswordChangedMail security notification,
    // addressed to the user via flow_user_by_id.
    assert!(
        fake.captured().iter().any(|m| m
            .text
            .as_deref()
            .is_some_and(|t| t.contains("password was just changed"))),
        "complete() must dispatch the password-changed notification"
    );

    // Single-use: the token is spent and a second complete must fail.
    assert!(
        !PasswordReset::check(&token).await.expect("check spent"),
        "a consumed token is no longer valid"
    );
    assert!(
        PasswordReset::complete(&token, "again").await.is_err(),
        "a consumed reset token must not complete again"
    );
}

#[tokio::test]
#[serial]
async fn complete_rejects_empty_password_and_garbage_token() {
    let _h = setup().await;

    // Empty/whitespace password is rejected before the token is even consumed.
    assert!(
        PasswordReset::complete("anything", "   ").await.is_err(),
        "an empty password must be rejected"
    );

    // An unknown token errors (nothing to consume).
    assert!(
        PasswordReset::complete("not-a-real-token", "newpass")
            .await
            .is_err(),
        "an unknown token must be rejected"
    );

    // The seeded password is untouched after the failed attempts.
    let stored = reload_ada_hash().await;
    assert!(
        suprnova::hashing::verify("oldpass", &stored).expect("verify old"),
        "a failed reset must not rotate the password"
    );
}
