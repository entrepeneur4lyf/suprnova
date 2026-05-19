//! Phase 10A T6 — Fillable / Guarded + `unguarded` escape hatch.
//!
//! Mass-assignment guard tests. Each model declares its policy via
//! `#[model(fillable = [...])]` or `#[model(guarded = [...])]`. The
//! T6 macro emission wires the per-model `Fillable` constructor; the
//! filter is applied by `Model::create` / `Model::update` before the
//! ActiveModel is built.
//!
//! `unguarded(|| async { ... })` is the task-local escape hatch —
//! useful for migrations and seeders where the caller intentionally
//! controls every column. The `unguarded_scope_does_not_leak` test is
//! the discriminator that proves the bypass flag is task-local rather
//! than process-global.

use suprnova::eloquent::unguarded;
use suprnova::testing::TestDatabase;
use suprnova::{attrs, model, Model};

#[model(table = "t6_users", timestamps = false, fillable = ["name", "email"])]
pub struct T6User {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub admin: bool,
}

#[model(table = "t6_posts", timestamps = false, guarded = ["id", "user_id"])]
pub struct T6Post {
    pub id: i64,
    pub user_id: i64,
    pub title: String,
    pub body: String,
}

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE t6_users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            email TEXT NOT NULL,
            admin INTEGER NOT NULL DEFAULT 0
        )",
    )
    .await
    .expect("create t6_users");
    db.execute_unprepared(
        "CREATE TABLE t6_posts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id INTEGER NOT NULL DEFAULT 0,
            title TEXT NOT NULL,
            body TEXT NOT NULL
        )",
    )
    .await
    .expect("create t6_posts");
}

#[tokio::test]
async fn fillable_drops_unlisted_fields() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let user = T6User::create(attrs! {
        name: "Alice",
        email: "a@x.com",
        admin: true, // not in fillable — should be dropped
    })
    .await
    .expect("create");

    assert_eq!(user.name, "Alice");
    assert!(
        !user.admin,
        "admin should be Default::default() — dropped by Fillable"
    );
}

#[tokio::test]
async fn guarded_blocks_listed_fields() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let post = T6Post::create(attrs! {
        user_id: 99, // guarded — should be dropped
        title: "Hello",
        body: "World",
    })
    .await
    .expect("create");

    assert_eq!(post.title, "Hello");
    assert_eq!(post.user_id, 0, "guarded user_id should fall through to DEFAULT 0");
}

#[tokio::test]
async fn unguarded_bypasses_filter() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let user = unguarded(|| async {
        T6User::create(attrs! {
            name: "Boot",
            email: "boot@x.com",
            admin: true,
        })
        .await
    })
    .await
    .expect("create");

    assert!(user.admin, "admin should be set during unguarded scope");
}

#[tokio::test]
async fn unguarded_scope_does_not_leak() {
    let db = TestDatabase::sqlite_memory().await.expect("sqlite");
    migrate(&db).await;

    let inside = unguarded(|| async {
        T6User::create(attrs! { name: "A", email: "a@x.com", admin: true })
            .await
    })
    .await
    .expect("inside scope");
    assert!(inside.admin, "admin set inside unguarded scope");

    // After the scope closes, filter is back on.
    let outside = T6User::create(attrs! { name: "B", email: "b@x.com", admin: true })
        .await
        .expect("outside scope");
    assert!(
        !outside.admin,
        "admin should be dropped after unguarded scope ends"
    );
}
