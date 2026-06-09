//! `EmailVerification` facade integration tests — provider-backed.
//!
//! Exercises the facade end-to-end against a real `#[suprnova::model]` user in
//! in-memory SQLite + the framework's own `auth_flow_tokens` table, with the
//! configured [`EloquentUserProvider`] as the active "users" provider. No
//! `init_torii`: the facade mints tokens through the provider-agnostic
//! `TokenStore` and marks users verified through the provider.
//!
//! # Serial execution
//!
//! `Mail::fake()` swaps the process-global mail transport, so two parallel
//! tests installing fakes would cross-capture each other's messages. The DB is
//! thread-local (per `TestDatabase`), so the mail fake is the only remaining
//! global — `#[serial]` serializes against it.

use std::any::Any;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serial_test::serial;

use suprnova::auth::AuthConfig;
use suprnova::auth_flows::token_store::create_auth_flow_tokens_table;
use suprnova::auth_flows::EmailVerification;
use suprnova::container::testing::TestContainer;
use suprnova::testing::TestDatabase;
use suprnova::{
    model, Auth, AuthManager, Authenticatable, CanResetPassword, EloquentUserProvider,
    MustVerifyEmail, UserProvider,
};

// The app's `User` shape: a typed model that is also Authenticatable +
// MustVerifyEmail. `email_verified_at` is a nullable datetime; the model macro
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
/// provider. Seeds one user (`ada@x.com`, not yet verified). Also sets
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

    let hash = suprnova::hash("secret").expect("hash");
    conn.execute_unprepared(&format!(
        "INSERT INTO users (email, password) VALUES ('ada@x.com', '{hash}')"
    ))
    .await
    .expect("seed user");

    // Register the Eloquent provider as the active "users" provider into the
    // SAME thread-local container `TestDatabase` already installed (do NOT call
    // `TestContainer::fake()` again — that would replace the container and drop
    // the DB binding). `TestContainer::singleton` writes into the active scope.
    // `AuthConfig::default()`'s "web" guard points at the "users" provider.
    TestContainer::singleton(AuthManager::new(AuthConfig::default()));
    Auth::register_provider("users", Arc::new(EloquentUserProvider::<TestUser>::new()))
        .expect("register provider");

    Harness { _db: db }
}

/// Reload the seeded user from the DB by email — to assert the verification
/// stamp persisted through the provider.
async fn reload_ada() -> TestUser {
    let p = EloquentUserProvider::<TestUser>::new();
    let flow = p
        .retrieve_by_email("ada@x.com")
        .await
        .expect("lookup")
        .expect("ada exists");
    let user = p
        .retrieve_by_id(&flow.id)
        .await
        .expect("by id")
        .expect("ada exists");
    user.as_any()
        .downcast_ref::<TestUser>()
        .expect("TestUser")
        .clone()
}

#[tokio::test]
#[serial]
async fn send_link_then_verify_marks_verified_single_use() {
    let _h = setup().await;

    // Look the user up so `send_link` has a `MustVerifyEmail` to mint against.
    let p = EloquentUserProvider::<TestUser>::new();
    let user = p
        .retrieve_by_id("1")
        .await
        .expect("by id")
        .expect("ada exists");
    let user = user
        .as_any()
        .downcast_ref::<TestUser>()
        .expect("TestUser")
        .clone();

    let fake = suprnova::mail::Mail::fake();
    EmailVerification::send_link(&user, "https://app.test/verify-email/verify")
        .await
        .expect("send_link");
    fake.assert_sent_to("ada@x.com");

    // Pull the plaintext token out of the captured mail's rendered link. The
    // text body renders the URL verbatim (the HTML body HTML-escapes slashes).
    let captured = fake.captured();
    assert_eq!(captured.len(), 1, "exactly one verification mail");
    let text = captured[0]
        .text
        .as_deref()
        .expect("verification mail has a text body");
    let link = text
        .lines()
        .find(|l| l.contains("token="))
        .expect("a line with the token link");
    let token = link.rsplit("token=").next().expect("token after marker").trim();

    // Not yet verified.
    assert!(!reload_ada().await.is_email_verified());

    // verify() consumes the token, marks the user verified, returns the id.
    let id = EmailVerification::verify(token).await.expect("verify");
    assert_eq!(id, user.get_auth_identifier());
    assert!(
        reload_ada().await.is_email_verified(),
        "verify() must persist email_verified_at through the provider"
    );

    // Single-use: a second verify on the same token must fail.
    assert!(
        EmailVerification::verify(token).await.is_err(),
        "a consumed token must not verify again"
    );
}

#[tokio::test]
#[serial]
async fn check_reports_validity_without_consuming() {
    let _h = setup().await;

    let p = EloquentUserProvider::<TestUser>::new();
    let user = p
        .retrieve_by_id("1")
        .await
        .expect("by id")
        .expect("ada exists");
    let user = user
        .as_any()
        .downcast_ref::<TestUser>()
        .expect("TestUser")
        .clone();

    let fake = suprnova::mail::Mail::fake();
    EmailVerification::send_link(&user, "https://app.test/verify")
        .await
        .expect("send_link");

    let captured = fake.captured();
    let text = captured[0].text.as_deref().expect("text body");
    let link = text
        .lines()
        .find(|l| l.contains("token="))
        .expect("token link");
    let token = link.rsplit("token=").next().expect("token").trim();

    // check() is non-consuming: true before, still true after, and verify
    // still works afterwards.
    assert!(EmailVerification::check(token).await.expect("check"));
    assert!(EmailVerification::check(token).await.expect("check again"));
    EmailVerification::verify(token).await.expect("verify");
    // Spent now.
    assert!(!EmailVerification::check(token).await.expect("check spent"));
}

#[tokio::test]
#[serial]
async fn resend_sends_for_known_email_and_is_silent_for_unknown() {
    let _h = setup().await;

    // Known email → a mail is sent.
    {
        let fake = suprnova::mail::Mail::fake();
        EmailVerification::resend("ada@x.com", "https://app.test/verify")
            .await
            .expect("resend known");
        assert_eq!(fake.count(), 1, "known email must trigger a mail");
        fake.assert_sent_to("ada@x.com");
    }

    // Unknown email → anti-enumeration: nothing sent, still Ok.
    {
        let fake = suprnova::mail::Mail::fake();
        EmailVerification::resend("nobody@x.com", "https://app.test/verify")
            .await
            .expect("resend unknown returns Ok (no leak)");
        assert_eq!(
            fake.count(),
            0,
            "unknown email must not send any mail (anti-enumeration)"
        );
    }
}

#[tokio::test]
#[serial]
async fn verify_rejects_garbage_token() {
    let _h = setup().await;
    assert!(
        EmailVerification::verify("not-a-real-token").await.is_err(),
        "an unknown token must be rejected"
    );
}
