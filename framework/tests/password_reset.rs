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

use chrono::{DateTime, Utc};
use serial_test::serial;

use suprnova::auth::AuthConfig;
use suprnova::auth_flows::token_store::create_auth_flow_tokens_table;
use suprnova::auth_flows::PasswordReset;
use suprnova::container::testing::TestContainer;
use suprnova::testing::TestDatabase;
use suprnova::{
    model, Auth, AuthManager, Authenticatable, CanResetPassword, EloquentUserProvider,
    MustVerifyEmail, UserProvider,
};

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
