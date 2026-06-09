//! Integration tests for [`suprnova::EloquentUserProvider`] against a
//! real `#[suprnova::model]` user type in in-memory SQLite.
//!
//! Uses the framework's `TestDatabase` (thread-local connection) + the
//! `#[tokio::test]` current-thread runtime, the established Eloquent
//! integration-test pattern.

use std::any::Any;

use chrono::{DateTime, Utc};
use suprnova::testing::TestDatabase;
use suprnova::{
    Authenticatable, CanResetPassword, Credentials, EloquentUserProvider, MustVerifyEmail,
    UserProvider, model,
};

// The app's `User` shape: a typed model that is also Authenticatable.
// The table carries an extra `is_admin` column the model doesn't map —
// it exists only to prove the credential allowlist never filters on it.
// `email_verified_at` is a nullable datetime — the model macro auto-injects
// `AsOptionalDateTime` on `Option<DateTime<Utc>>` fields, so no explicit cast
// is needed.
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
    fn into_arc_any(self: std::sync::Arc<Self>) -> std::sync::Arc<dyn Any + Send + Sync> {
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
}

impl CanResetPassword for TestUser {
    fn email_for_reset(&self) -> &str {
        &self.email
    }
    fn set_password_hash(&mut self, hash: &str) {
        self.password = hash.to_string();
    }
}

/// Fresh in-memory DB with a `users` table and one seeded user
/// (`a@b.com` / bcrypt(`secret`), not admin). The returned guard must be
/// held for the duration of the test.
async fn setup() -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE users (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            email TEXT NOT NULL, \
            password TEXT NOT NULL, \
            email_verified_at TEXT, \
            is_admin INTEGER NOT NULL DEFAULT 0\
         )",
    )
    .await
    .unwrap();

    // bcrypt hashes contain `$`, `/`, `.` and alphanumerics — never a
    // single quote — so direct interpolation is safe here.
    let hash = suprnova::hash("secret").unwrap();
    db.execute_unprepared(&format!(
        "INSERT INTO users (email, password, is_admin) VALUES ('a@b.com', '{hash}', 0)"
    ))
    .await
    .unwrap();

    db
}

fn provider() -> EloquentUserProvider<TestUser> {
    EloquentUserProvider::<TestUser>::new()
}

#[tokio::test]
async fn retrieve_by_id_resolves_known_and_unknown() {
    let _db = setup().await;
    let p = provider();

    let user = p.retrieve_by_id("1").await.unwrap().expect("user 1 exists");
    assert_eq!(user.get_auth_identifier(), "1");
    assert!(user.get_auth_password().is_some());

    assert!(p.retrieve_by_id("999").await.unwrap().is_none());
}

#[tokio::test]
async fn retrieve_by_credentials_matches_on_email() {
    let _db = setup().await;
    let p = provider();

    let found = p
        .retrieve_by_credentials(&Credentials::password("a@b.com", "ignored").as_value())
        .await
        .unwrap();
    assert_eq!(
        found.map(|u| u.get_auth_identifier()),
        Some("1".to_string())
    );

    let missing = p
        .retrieve_by_credentials(&Credentials::password("nobody@b.com", "x").as_value())
        .await
        .unwrap();
    assert!(missing.is_none());
}

#[tokio::test]
async fn validate_credentials_checks_the_password_hash() {
    let _db = setup().await;
    let p = provider();
    let user = p.retrieve_by_id("1").await.unwrap().unwrap();

    assert!(
        p.validate_credentials(
            &*user,
            &Credentials::password("a@b.com", "secret").as_value()
        )
        .await
        .unwrap()
    );
    assert!(
        !p.validate_credentials(
            &*user,
            &Credentials::password("a@b.com", "wrong").as_value()
        )
        .await
        .unwrap()
    );
}

// A hostile `{email, is_admin: true}` (the seeded user is NOT admin) must
// still resolve by email alone — `is_admin` is not in the allowlist.
#[tokio::test]
async fn credential_allowlist_ignores_non_allowlisted_keys() {
    let _db = setup().await;
    let p = provider();

    let creds = Credentials::new()
        .insert("email", "a@b.com")
        .insert("is_admin", true)
        .as_value();
    let found = p.retrieve_by_credentials(&creds).await.unwrap();
    assert_eq!(
        found.map(|u| u.get_auth_identifier()),
        Some("1".to_string()),
        "is_admin must be ignored; lookup filters on email only"
    );
}

#[tokio::test]
async fn no_allowlisted_credential_returns_none() {
    let _db = setup().await;
    let p = provider();
    let creds = Credentials::new().insert("is_admin", true).as_value();
    assert!(p.retrieve_by_credentials(&creds).await.unwrap().is_none());
}

// The auth-flow surface: lookup-by-email, the AuthFlowUser id round-trip,
// the email-verification toggle, and a password reset — all against the real
// SQLite-backed model, end to end.
#[tokio::test]
async fn eloquent_provider_supports_auth_flow_methods() {
    let _db = setup().await;
    let p = provider();
    let id = "1".to_string();

    // retrieve_by_email returns the AuthFlowUser carrier.
    let found = p
        .retrieve_by_email("a@b.com")
        .await
        .unwrap()
        .expect("user a@b.com exists");
    assert_eq!(found.email, "a@b.com");
    assert_eq!(found.id, id);
    let missing = p.retrieve_by_email("nobody@b.com").await.unwrap();
    assert!(missing.is_none());

    // flow_user_by_id round-trips the email by primary key.
    let by_id = p
        .flow_user_by_id(&id)
        .await
        .unwrap()
        .expect("user 1 exists");
    assert_eq!(by_id.email, "a@b.com");
    assert_eq!(by_id.id, id);

    // Not verified → verified. mark_email_verified persists through save().
    assert!(!p.is_email_verified(&id).await.unwrap());
    p.mark_email_verified(&id).await.unwrap();
    assert!(p.is_email_verified(&id).await.unwrap());

    // set_password stores the pre-hashed value verbatim: the new password
    // verifies, the old one no longer does.
    let new_hash = suprnova::hash("newpass").unwrap();
    p.set_password(&id, &new_hash).await.unwrap();
    let reloaded = p
        .retrieve_by_id(&id)
        .await
        .unwrap()
        .expect("user 1 still exists");
    let stored = reloaded.get_auth_password().expect("password hash present");
    assert!(suprnova::hashing::verify("newpass", stored).unwrap());
    assert!(!suprnova::hashing::verify("secret", stored).unwrap());
}

// The absent-user contract: the mutating flow methods are silent no-ops on a
// non-existent id (load returns None → nothing to mutate, `Ok(())`), and the
// read returns `false`. Locks the behaviour the password-reset / verification
// flows rely on when an id no longer resolves.
#[tokio::test]
async fn auth_flow_methods_are_no_ops_on_absent_user() {
    let _db = setup().await;
    let p = provider();
    let absent = "999";

    // No row → no mutation, no error.
    p.mark_email_verified(absent).await.unwrap();
    p.set_password(absent, &suprnova::hash("whatever").unwrap())
        .await
        .unwrap();

    // Read on a missing id is `false`, not an error.
    assert!(!p.is_email_verified(absent).await.unwrap());

    // The carriers resolve to None for an absent user too.
    assert!(p.flow_user_by_id(absent).await.unwrap().is_none());
}
