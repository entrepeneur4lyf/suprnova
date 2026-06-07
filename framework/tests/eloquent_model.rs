//! Phase 10A T4 — Model trait CRUD lifecycle.
//!
//! Each test uses an in-memory SQLite database for hermetic isolation. The
//! `TestDatabase::sqlite_memory()` constructor registers itself in the
//! per-test container, so any `DB::connection()` inside the trait methods
//! resolves to this DB.

use suprnova::testing::TestDatabase;
use suprnova::{FirstOrCreate, Model, attrs, model};

#[model(table = "t4_users", timestamps = false)]
pub struct T4User {
    pub id: i64,
    pub name: String,
    pub email: String,
}

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared(
        r#"CREATE TABLE t4_users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            email TEXT NOT NULL UNIQUE
        )"#,
    )
    .await
    .expect("create table");
}

#[tokio::test]
async fn create_then_find() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let alice = T4User::create(attrs! {
        name: "Alice",
        email: "alice@example.com",
    })
    .await
    .expect("create");

    assert!(alice.id > 0);
    assert_eq!(alice.name, "Alice");

    let fetched = T4User::find(alice.id).await.expect("find");
    assert_eq!(fetched.unwrap().email, "alice@example.com");
}

#[tokio::test]
async fn find_or_fail_returns_error_on_missing() {
    let _db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&_db).await;
    let result = T4User::find_or_fail(99_999i64).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, suprnova::FrameworkError::ModelNotFound { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn find_many_preserves_order_of_request_args() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let a = T4User::create(attrs! { name: "A", email: "a@x.com" })
        .await
        .unwrap();
    let b = T4User::create(attrs! { name: "B", email: "b@x.com" })
        .await
        .unwrap();
    let c = T4User::create(attrs! { name: "C", email: "c@x.com" })
        .await
        .unwrap();

    let users = T4User::find_many([c.id, a.id, b.id]).await.unwrap();
    assert_eq!(users.len(), 3);
    let names: Vec<&str> = users.iter().map(|u| u.name.as_str()).collect();
    assert_eq!(names, vec!["C", "A", "B"]);
}

#[tokio::test]
async fn all_returns_every_row() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    T4User::create(attrs! { name: "A", email: "a@x.com" })
        .await
        .unwrap();
    T4User::create(attrs! { name: "B", email: "b@x.com" })
        .await
        .unwrap();

    let users = T4User::all().await.unwrap();
    assert_eq!(users.len(), 2);
}

#[tokio::test]
async fn save_persists_changes() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let mut alice = T4User::create(attrs! { name: "Alice", email: "a@x.com" })
        .await
        .unwrap();
    alice.name = "Alice B".into();
    alice.save().await.unwrap();

    let reread = T4User::find(alice.id).await.unwrap().unwrap();
    assert_eq!(reread.name, "Alice B");
}

#[tokio::test]
async fn update_applies_partial_attrs() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let alice = T4User::create(attrs! { name: "Alice", email: "a@x.com" })
        .await
        .unwrap();
    let id = alice.id;
    alice.update(attrs! { name: "Alice C" }).await.unwrap();

    let reread = T4User::find(id).await.unwrap().unwrap();
    assert_eq!(reread.name, "Alice C");
    assert_eq!(reread.email, "a@x.com"); // unchanged
}

#[tokio::test]
async fn delete_removes_row() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let alice = T4User::create(attrs! { name: "A", email: "a@x.com" })
        .await
        .unwrap();
    let id = alice.id;
    alice.delete().await.unwrap();
    assert!(T4User::find(id).await.unwrap().is_none());
}

#[tokio::test]
async fn first_or_create_creates_when_missing() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let user = T4User::first_or_create(
        attrs! { email: "new@example.com" },
        attrs! { name: "Newcomer" },
    )
    .await
    .unwrap();

    assert_eq!(user.name, "Newcomer");
    assert!(user.id > 0);
}

#[tokio::test]
async fn first_or_create_returns_existing_when_present() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let existing = T4User::create(attrs! { name: "Existing", email: "e@x.com" })
        .await
        .unwrap();

    let same = T4User::first_or_create(
        attrs! { email: "e@x.com" },
        attrs! { name: "ShouldNotApply" },
    )
    .await
    .unwrap();

    assert_eq!(same.id, existing.id);
    assert_eq!(same.name, "Existing"); // create-side extras ignored when matching
}

#[tokio::test]
async fn update_or_create_updates_when_present() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    T4User::create(attrs! { name: "Old", email: "u@x.com" })
        .await
        .unwrap();

    let result = T4User::update_or_create(attrs! { email: "u@x.com" }, attrs! { name: "New" })
        .await
        .unwrap();

    assert_eq!(result.name, "New");
    assert_eq!(T4User::all().await.unwrap().len(), 1);
}

#[tokio::test]
async fn first_or_new_returns_unsaved_when_missing() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let user = T4User::first_or_new(attrs! { email: "ghost@example.com", name: "Ghost" })
        .await
        .unwrap();

    // No row persisted; PK is the default (0 for i64).
    assert_eq!(user.id, 0);
    assert_eq!(user.email, "ghost@example.com");
    assert_eq!(user.name, "Ghost");
    assert_eq!(T4User::all().await.unwrap().len(), 0);
}

#[tokio::test]
async fn refresh_pulls_fresh_data() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let alice = T4User::create(attrs! { name: "Alice", email: "a@x.com" })
        .await
        .unwrap();
    let mut alice_handle = alice.clone();

    alice.update(attrs! { name: "Alice X" }).await.unwrap();
    alice_handle.refresh().await.unwrap();
    assert_eq!(alice_handle.name, "Alice X");
}

#[tokio::test]
async fn fresh_returns_copy_without_mutating() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let alice = T4User::create(attrs! { name: "Alice", email: "a@x.com" })
        .await
        .unwrap();
    alice
        .clone()
        .update(attrs! { name: "Mutated" })
        .await
        .unwrap();

    let fresh = alice.fresh().await.unwrap();
    assert_eq!(fresh.unwrap().name, "Mutated");
    assert_eq!(alice.name, "Alice"); // original handle unchanged
}

#[tokio::test]
async fn replicate_clones_with_reset_pk() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let alice = T4User::create(attrs! { name: "Alice", email: "a@x.com" })
        .await
        .unwrap();
    let replica = alice.replicate_except(["email"]).await.unwrap();
    assert_eq!(replica.id, 0); // PK reset
    assert_eq!(replica.name, "Alice");
    assert_eq!(replica.email, ""); // dropped via except
}

#[tokio::test]
async fn replicate_clones_full_row_with_reset_pk() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let alice = T4User::create(attrs! { name: "Alice", email: "a@x.com" })
        .await
        .unwrap();
    let replica = alice.replicate().await.unwrap();
    assert_eq!(replica.id, 0);
    assert_eq!(replica.name, "Alice");
    assert_eq!(replica.email, "a@x.com");
}

#[model(table = "t4_counters", timestamps = false)]
pub struct T4Counter {
    pub id: i64,
    pub hits: i64,
}

#[model(table = "t4_user_drafts", timestamps = false)]
pub struct T4UserDraft {
    pub id: i64,
    pub name: String,
    pub email: String,
}

#[tokio::test]
async fn replicate_into_other_model_resets_pk() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;
    db.execute_unprepared(
        r#"CREATE TABLE t4_user_drafts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            email TEXT NOT NULL UNIQUE
        )"#,
    )
    .await
    .unwrap();

    let alice = T4User::create(attrs! { name: "Alice", email: "alice@example.com" })
        .await
        .unwrap();

    let draft: T4UserDraft = alice.replicate_into().await.expect("replicate_into");

    // PK reset on the replica even though the source had id > 0.
    assert_eq!(draft.id, 0);
    assert_eq!(draft.name, "Alice");
    assert_eq!(draft.email, "alice@example.com");

    // Unsaved — replicate_into never touches the database.
    assert_eq!(T4UserDraft::all().await.unwrap().len(), 0);
}

#[tokio::test]
async fn increment_atomic_update() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    db.execute_unprepared(
        "CREATE TABLE t4_counters (id INTEGER PRIMARY KEY AUTOINCREMENT, hits INTEGER NOT NULL DEFAULT 0)",
    )
    .await
    .unwrap();

    let counter = T4Counter::create(attrs! { hits: 0 }).await.unwrap();
    counter.increment("hits", 3).await.unwrap();
    let reread = T4Counter::find(counter.id).await.unwrap().unwrap();
    assert_eq!(reread.hits, 3);
}

#[tokio::test]
async fn decrement_atomic_update() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    db.execute_unprepared(
        "CREATE TABLE t4_counters (id INTEGER PRIMARY KEY AUTOINCREMENT, hits INTEGER NOT NULL DEFAULT 0)",
    )
    .await
    .unwrap();

    let counter = T4Counter::create(attrs! { hits: 10 }).await.unwrap();
    counter.decrement("hits", 4).await.unwrap();
    let reread = T4Counter::find(counter.id).await.unwrap().unwrap();
    assert_eq!(reread.hits, 6);
}

#[tokio::test]
async fn force_delete_alias_calls_hard_delete() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let alice = T4User::create(attrs! { name: "A", email: "a@x.com" })
        .await
        .unwrap();
    let id = alice.id;
    alice.force_delete().await.unwrap();
    assert!(T4User::find(id).await.unwrap().is_none());
}

#[tokio::test]
async fn create_rejects_unknown_column() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let result = T4User::create(attrs! {
        name: "X",
        email: "x@example.com",
        nonexistent: "bad",
    })
    .await;
    assert!(result.is_err(), "expected error for unknown column");
}

// ---- create_or_first narrows on FrameworkError::Database --------------
//
// Regression — the original implementation matched `Err(_)`
// indiscriminately and re-queried by `lookup` for every error. A
// non-DB failure (validation, listener cancel) had its original
// error swallowed and the re-query result returned instead. The fix
// narrows the match to `FrameworkError::Database(_)` and propagates
// every other variant.

#[tokio::test]
async fn create_or_first_returns_existing_row_on_unique_violation() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let existing = T4User::create(attrs! { name: "Alice", email: "race@x.com" })
        .await
        .unwrap();

    // Second create_or_first hits the UNIQUE constraint on email and
    // re-queries by lookup; the existing row is returned.
    let same = T4User::create_or_first(
        attrs! { email: "race@x.com" },
        attrs! { name: "Replacement" },
    )
    .await
    .expect("DB error must trigger re-query path");

    assert_eq!(same.id, existing.id);
    assert_eq!(same.name, "Alice");
}

#[tokio::test]
async fn create_or_first_propagates_non_database_errors_unchanged() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    // Unknown column triggers `FrameworkError::Domain` (not
    // `Database`) from the fillable/column validation path. The
    // create_or_first matcher must NOT re-query by lookup; it must
    // surface the original error unchanged.
    let err = T4User::create_or_first(
        attrs! { email: "validate@x.com" },
        attrs! { name: "Newcomer", nonexistent: "bad" },
    )
    .await
    .expect_err("unknown column must surface as a non-DB error");

    // Surfaced error must not be the synthetic "row is not present"
    // internal error — that's the original buggy behaviour.
    let msg = err.to_string();
    assert!(
        !msg.contains("row is not present"),
        "non-DB errors must propagate unchanged; got: {msg}",
    );
    // And the lookup must NOT have been re-run; the row must not exist.
    assert!(
        T4User::query()
            .filter("email", "validate@x.com")
            .first()
            .await
            .unwrap()
            .is_none(),
    );
}
