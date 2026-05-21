//! Phase 10C audit-fix AF1 — eager relation arms honor CURRENT_TX.
//!
//! The Phase 10C closeout audit found that several macro-emitted eager
//! arms issue raw `query_all(db, stmt)` against the pool connection,
//! bypassing the ambient `CURRENT_TX` task-local. Inside a
//! `DB::transaction` closure that means an in-flight `with_count` /
//! `with_sum` / Through eager load reads the *pre-transaction* state,
//! and rollback never un-writes data that was never on the tx in the
//! first place — both serious correctness leaks.
//!
//! ## Why a custom multi-connection pool
//!
//! [`TestDatabase::sqlite_memory`] uses `max_connections(1)`. With a
//! single pool connection, an active `DB::transaction` holds it, and
//! every subsequent `DB::connection()?` lookup returns the same handle
//! — so even leaky arms accidentally observe the in-tx state because
//! there's literally one physical connection. That masks the bug.
//!
//! These tests instead build a file-backed SQLite database with
//! `cache=shared` and `max_connections(4)`, so the pool hands out
//! independent physical connections. With multiple connections, the
//! tx's BEGIN is isolated and other connections see only committed
//! state. That's the configuration that actually surfaces (and pins
//! the fix for) the leak.
//!
//! Each test follows the same shape:
//!
//! 1. Seed one parent + one initial child outside the tx (pre-tx
//!    baseline).
//! 2. Open `DB::transaction(|_tx| ... )` and INSIDE the closure:
//!    a. Insert a second child.
//!    b. Run the eager-load query against the parent.
//!    c. Assert the count/sum/eager-loaded slice sees BOTH children
//!    (the in-tx insert + the pre-tx row) — pre-fix, this fails
//!    because the eager SQL ran against the pool.
//!    d. Return `Err(...)` to roll back.
//! 3. Re-run the eager-load OUTSIDE the closure and assert the count
//!    is back to the pre-tx baseline (the in-tx insert rolled back
//!    cleanly).

use std::time::Duration;

use sea_orm::{ConnectOptions, ConnectionTrait, Database};
use suprnova::container::testing::{TestContainer, TestContainerGuard};
use suprnova::{attrs, model, DbConnection, FrameworkError, Model, DB};

/// Build a fresh multi-connection SQLite database against a per-test
/// temp file with WAL journal mode. Returns the guard so the test
/// container is restored on drop, plus the temp-dir handle so the
/// file outlives the test body.
///
/// WAL mode is critical here. With the default ROLLBACK journal, a
/// `BEGIN IMMEDIATE` (which SeaORM uses) acquires a RESERVED lock that
/// blocks every other connection's read until the tx commits — so the
/// pool-pinned leaky eager arm just deadlocks instead of returning the
/// stale state. WAL allows concurrent readers against the last-
/// committed snapshot while a writer holds the WAL, which is the
/// behaviour that matches Postgres / MySQL MVCC: the eager arm sees a
/// pre-tx view, NOT the in-tx writes. That's the symptom these tests
/// pin against.
async fn fresh_multiconn_sqlite() -> (DbConnection, TestContainerGuard, tempfile::TempDir) {
    let guard = TestContainer::fake();
    let tmpdir = tempfile::tempdir().expect("temp dir");
    let path = tmpdir.path().join("af1.sqlite");
    let url = format!("sqlite://{}?mode=rwc", path.to_string_lossy());
    let mut opt = ConnectOptions::new(&url);
    opt.max_connections(4)
        .min_connections(1)
        .connect_timeout(Duration::from_secs(5))
        .sqlx_logging(false);
    let raw = Database::connect(opt).await.expect("connect sqlite file");
    // Enable WAL so concurrent readers don't block on the writer's
    // BEGIN. Required for the "stale read on separate connection"
    // symptom to surface instead of a deadlock.
    raw.execute_unprepared("PRAGMA journal_mode=WAL;")
        .await
        .expect("set WAL");
    let conn = DbConnection::from_raw(raw);
    TestContainer::singleton(conn.clone());
    (conn, guard, tmpdir)
}

async fn execute_ddl(conn: &DbConnection, sql: &str) {
    conn.inner().execute_unprepared(sql).await.unwrap();
}

// ---- HasMany fixtures ---------------------------------------------------

#[model(table = "af1_users", relations = {
    posts: HasMany<Af1Post>,
})]
pub struct Af1User {
    pub id: i64,
    pub name: String,
}

#[model(table = "af1_posts")]
pub struct Af1Post {
    pub id: i64,
    pub af1_user_id: i64,
    pub title: String,
    pub views: i64,
}

async fn migrate_has_many(conn: &DbConnection) {
    execute_ddl(
        conn,
        "CREATE TABLE af1_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await;
    execute_ddl(
        conn,
        "CREATE TABLE af1_posts (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            af1_user_id INTEGER NOT NULL, \
            title TEXT NOT NULL, \
            views INTEGER NOT NULL DEFAULT 0\
         )",
    )
    .await;
}

#[tokio::test]
async fn has_many_with_count_inside_tx_sees_in_tx_inserts() {
    let (conn, _guard, _tmp) = fresh_multiconn_sqlite().await;
    migrate_has_many(&conn).await;

    let u = Af1User::create(attrs! { name: "Alice" }).await.unwrap();
    let _ = Af1Post::create(attrs! { af1_user_id: u.id, title: "pre-tx", views: 10i64 })
        .await
        .unwrap();

    let user_id = u.id;
    let result: Result<(), FrameworkError> = DB::transaction(move |_tx| {
        Box::pin(async move {
            let _ = Af1Post::create(
                attrs! { af1_user_id: user_id, title: "in-tx", views: 20i64 },
            )
            .await?;

            let row = Af1User::query()
                .filter("id", user_id)
                .with_count(["posts"])
                .first()
                .await?
                .unwrap();
            assert_eq!(
                row.posts_count(),
                2,
                "with_count must see the in-tx insert; pool-pinned arm would return 1"
            );

            Err(FrameworkError::internal("rollback"))
        })
    })
    .await;
    assert!(result.is_err());

    let row = Af1User::query()
        .filter("id", user_id)
        .with_count(["posts"])
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.posts_count(), 1, "rollback must un-write the in-tx insert");
}

#[tokio::test]
async fn has_many_with_sum_inside_tx_sees_in_tx_inserts() {
    let (conn, _guard, _tmp) = fresh_multiconn_sqlite().await;
    migrate_has_many(&conn).await;

    let u = Af1User::create(attrs! { name: "Bob" }).await.unwrap();
    let _ = Af1Post::create(attrs! { af1_user_id: u.id, title: "pre", views: 10i64 })
        .await
        .unwrap();

    let user_id = u.id;
    let result: Result<(), FrameworkError> = DB::transaction(move |_tx| {
        Box::pin(async move {
            let _ = Af1Post::create(
                attrs! { af1_user_id: user_id, title: "in-tx", views: 25i64 },
            )
            .await?;

            let row = Af1User::query()
                .filter("id", user_id)
                .with_sum(("posts", "views"))
                .first()
                .await?
                .unwrap();
            assert_eq!(
                row.posts_sum_of("views"),
                Some(35.0),
                "with_sum must see the in-tx insert; pool-pinned arm would return Some(10.0)"
            );

            Err(FrameworkError::internal("rollback"))
        })
    })
    .await;
    assert!(result.is_err());

    let row = Af1User::query()
        .filter("id", user_id)
        .with_sum(("posts", "views"))
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.posts_sum_of("views"), Some(10.0));
}

// ---- BelongsToMany fixtures ---------------------------------------------

#[model(table = "af1_b2m_users", relations = {
    tags: BelongsToMany<Af1Tag, Af1UserTag>,
})]
pub struct Af1B2mUser {
    pub id: i64,
    pub name: String,
}

#[model(table = "af1_tags")]
pub struct Af1Tag {
    pub id: i64,
    pub label: String,
}

#[model(table = "af1_user_tags")]
pub struct Af1UserTag {
    pub id: i64,
    pub af1_b2m_user_id: i64,
    pub af1_tag_id: i64,
}

async fn migrate_b2m(conn: &DbConnection) {
    execute_ddl(
        conn,
        "CREATE TABLE af1_b2m_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await;
    execute_ddl(
        conn,
        "CREATE TABLE af1_tags (id INTEGER PRIMARY KEY AUTOINCREMENT, label TEXT NOT NULL)",
    )
    .await;
    execute_ddl(
        conn,
        "CREATE TABLE af1_user_tags (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            af1_b2m_user_id INTEGER NOT NULL, \
            af1_tag_id INTEGER NOT NULL\
         )",
    )
    .await;
}

#[tokio::test]
async fn belongs_to_many_with_count_inside_tx_sees_in_tx_attaches() {
    let (conn, _guard, _tmp) = fresh_multiconn_sqlite().await;
    migrate_b2m(&conn).await;

    let u = Af1B2mUser::create(attrs! { name: "Carol" }).await.unwrap();
    let t1 = Af1Tag::create(attrs! { label: "pre-tx" }).await.unwrap();
    let t2 = Af1Tag::create(attrs! { label: "in-tx" }).await.unwrap();
    let _ = Af1UserTag::create(attrs! { af1_b2m_user_id: u.id, af1_tag_id: t1.id })
        .await
        .unwrap();

    let user_id = u.id;
    let t2_id = t2.id;
    let result: Result<(), FrameworkError> = DB::transaction(move |_tx| {
        Box::pin(async move {
            let _ = Af1UserTag::create(
                attrs! { af1_b2m_user_id: user_id, af1_tag_id: t2_id },
            )
            .await?;

            let row = Af1B2mUser::query()
                .filter("id", user_id)
                .with_count(["tags"])
                .first()
                .await?
                .unwrap();
            assert_eq!(
                row.tags_count(),
                2,
                "BelongsToMany with_count must see in-tx attaches"
            );

            Err(FrameworkError::internal("rollback"))
        })
    })
    .await;
    assert!(result.is_err());

    let row = Af1B2mUser::query()
        .filter("id", user_id)
        .with_count(["tags"])
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.tags_count(), 1);
}

// ---- HasManyThrough fixtures --------------------------------------------

#[model(table = "af1_countries", relations = {
    posts: HasManyThrough<Af1Author, Af1Article>,
})]
pub struct Af1Country {
    pub id: i64,
    pub name: String,
}

#[model(table = "af1_authors")]
pub struct Af1Author {
    pub id: i64,
    pub af1_country_id: i64,
    pub name: String,
}

#[model(table = "af1_articles")]
pub struct Af1Article {
    pub id: i64,
    pub af1_author_id: i64,
    pub title: String,
}

async fn migrate_through(conn: &DbConnection) {
    execute_ddl(
        conn,
        "CREATE TABLE af1_countries (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            name TEXT NOT NULL\
         )",
    )
    .await;
    execute_ddl(
        conn,
        "CREATE TABLE af1_authors (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            af1_country_id INTEGER NOT NULL, \
            name TEXT NOT NULL\
         )",
    )
    .await;
    execute_ddl(
        conn,
        "CREATE TABLE af1_articles (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            af1_author_id INTEGER NOT NULL, \
            title TEXT NOT NULL\
         )",
    )
    .await;
}

#[tokio::test]
async fn has_many_through_inside_tx_sees_in_tx_inserts() {
    let (conn, _guard, _tmp) = fresh_multiconn_sqlite().await;
    migrate_through(&conn).await;

    let c = Af1Country::create(attrs! { name: "Argentina" }).await.unwrap();
    let a = Af1Author::create(attrs! { af1_country_id: c.id, name: "Borges" })
        .await
        .unwrap();
    let _ = Af1Article::create(attrs! { af1_author_id: a.id, title: "pre-tx" })
        .await
        .unwrap();

    let country_id = c.id;
    let author_id = a.id;
    let result: Result<(), FrameworkError> = DB::transaction(move |_tx| {
        Box::pin(async move {
            let _ = Af1Article::create(
                attrs! { af1_author_id: author_id, title: "in-tx" },
            )
            .await?;

            let row = Af1Country::query()
                .filter("id", country_id)
                .with(["posts"])
                .first()
                .await?
                .unwrap();
            assert_eq!(
                row.posts_loaded().len(),
                2,
                "HasManyThrough eager arm must see in-tx insert; pool-pinned JOIN \
                 would return 1"
            );

            Err(FrameworkError::internal("rollback"))
        })
    })
    .await;
    assert!(result.is_err());

    let row = Af1Country::query()
        .filter("id", country_id)
        .with(["posts"])
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.posts_loaded().len(), 1);
}
