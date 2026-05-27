//! Integration tests for [`suprnova::EloquentUserProvider`] against a
//! real `#[suprnova::model]` user type in in-memory SQLite.
//!
//! Uses the framework's `TestDatabase` (thread-local connection) + the
//! `#[tokio::test]` current-thread runtime, the established Eloquent
//! integration-test pattern.

use std::any::Any;

use suprnova::testing::TestDatabase;
use suprnova::{Authenticatable, Credentials, EloquentUserProvider, UserProvider, model};

// The app's `User` shape: a typed model that is also Authenticatable.
// The table carries an extra `is_admin` column the model doesn't map —
// it exists only to prove the credential allowlist never filters on it.
#[model(table = "users", fillable = ["email", "password"])]
pub struct TestUser {
    pub id: i64,
    pub email: String,
    pub password: String,
}

impl Authenticatable for TestUser {
    fn auth_identifier(&self) -> i64 {
        self.id
    }
    fn get_auth_identifier(&self) -> String {
        self.id.to_string()
    }
    fn get_auth_password(&self) -> Option<&str> {
        Some(&self.password)
    }
    fn as_any(&self) -> &dyn Any {
        self
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
