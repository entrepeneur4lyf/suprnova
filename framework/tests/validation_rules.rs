//! Integration tests for the rule-object primitives in
//! `suprnova::validation::rule`.

use sea_orm::{ConnectionTrait, Database, DbBackend, Statement, Value};
use suprnova::testing::TestContainer;
use suprnova::validation::rule::{
    async_rules::Unique,
    rules::{Email, Max, Min, Required},
    AsyncRule, Rule,
};
use suprnova::DbConnection;

#[test]
fn required_passes_on_present() {
    let r = Required;
    assert!(r.passes("not empty").is_ok());
    assert!(r.passes("").is_err());
    assert!(
        r.passes("   ").is_err(),
        "all-whitespace counts as empty"
    );
}

#[test]
fn email_accepts_well_formed_addresses() {
    let r = Email;
    assert!(r.passes("user@example.com").is_ok());
    assert!(r.passes("user+filter@sub.example.co.uk").is_ok());
}

#[test]
fn email_rejects_malformed_addresses() {
    let r = Email;
    // The `validator` crate rejects these:
    assert!(r.passes("not-an-email").is_err());
    assert!(r.passes("@nodomain").is_err());
    assert!(r.passes("noatsign.com").is_err());
    assert!(r.passes("trailing.dot@x.").is_err());
}

#[test]
fn min_max_check_length() {
    let r = Min(8);
    assert!(r.passes("longenough").is_ok());
    assert!(r.passes("short").is_err());

    let r = Max(5);
    assert!(r.passes("hi").is_ok());
    assert!(r.passes("toolong").is_err());
}

// --- async rules (Unique) ---
//
// `TestContainer` is thread-local. The test harness runs tests on a
// thread pool, so each `#[tokio::test]` builds a fresh in-memory
// SQLite, wires it into the current thread's container with
// `TestContainer::singleton`, and runs the assertion. The guard
// returned by `TestContainer::fake()` clears the container on drop.

async fn fresh_db() -> DbConnection {
    let raw = Database::connect("sqlite::memory:").await.unwrap();
    raw.execute(Statement::from_string(
        DbBackend::Sqlite,
        "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL)"
            .to_string(),
    ))
    .await
    .unwrap();
    DbConnection::from_raw(raw)
}

async fn seed_user_with_email(db: &DbConnection, email: &str) -> i64 {
    let backend = db.inner().get_database_backend();
    let stmt = Statement::from_sql_and_values(
        backend,
        "INSERT INTO users (email) VALUES (?)",
        vec![Value::from(email.to_string())],
    );
    let result = db.inner().execute(stmt).await.unwrap();
    result.last_insert_id() as i64
}

#[tokio::test]
async fn unique_passes_when_no_row_exists() {
    let _guard = TestContainer::fake();
    let db = fresh_db().await;
    TestContainer::singleton(db);

    let rule = Unique {
        table: "users",
        column: "email",
        except_id: None,
    };
    assert!(rule.passes("nobody@example.com").await.is_ok());
}

#[tokio::test]
async fn unique_fails_when_row_exists() {
    let _guard = TestContainer::fake();
    let db = fresh_db().await;
    seed_user_with_email(&db, "taken@example.com").await;
    TestContainer::singleton(db);

    let rule = Unique {
        table: "users",
        column: "email",
        except_id: None,
    };
    let err = rule.passes("taken@example.com").await.unwrap_err();
    assert!(
        err.contains("already"),
        "expected duplicate-error message, got: {err}"
    );
}

#[tokio::test]
async fn unique_ignores_except_id() {
    let _guard = TestContainer::fake();
    let db = fresh_db().await;
    let id = seed_user_with_email(&db, "self@example.com").await;
    TestContainer::singleton(db);

    let rule = Unique {
        table: "users",
        column: "email",
        except_id: Some(id),
    };
    assert!(rule.passes("self@example.com").await.is_ok());
}
