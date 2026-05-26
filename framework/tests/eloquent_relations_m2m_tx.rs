//! Phase 10C audit-fix AF2 — lazy m2m relation paths honor CURRENT_TX.
//!
//! `BelongsToMany::attach` / `detach` / `sync` / `count` and the
//! `MorphToMany` / `MorphedByMany` equivalents currently call
//! `DB::connection()?` explicitly and run raw `INSERT` / `DELETE` /
//! `SELECT COUNT` against the pool — bypassing the ambient
//! `CURRENT_TX`. Under a `DB::transaction` closure, that means
//! pivot writes silently auto-commit on a separate pool connection
//! while the closure's BEGIN holds its own connection, so a
//! subsequent rollback never un-writes them.
//!
//! These tests pin the contract: every lazy m2m write that happens
//! inside `DB::transaction { ... }` rolls back atomically with the
//! closure, and every lazy m2m read sees the in-tx state.
//!
//! Like AF1's test, this fixture builds its own multi-conn SQLite
//! file with WAL mode. `TestDatabase::sqlite_memory` uses a
//! `max_connections(1)` pool that accidentally masks the leak (the
//! single connection is shared between tx and pool).

use std::time::Duration;

use sea_orm::{ConnectOptions, ConnectionTrait, Database};
use suprnova::container::testing::{TestContainer, TestContainerGuard};
use suprnova::{DB, DbConnection, FrameworkError, Model, attrs, model};

async fn fresh_multiconn_sqlite() -> (DbConnection, TestContainerGuard, tempfile::TempDir) {
    let guard = TestContainer::fake();
    let tmpdir = tempfile::tempdir().expect("temp dir");
    let path = tmpdir.path().join("af2.sqlite");
    let url = format!("sqlite://{}?mode=rwc", path.to_string_lossy());
    let mut opt = ConnectOptions::new(&url);
    opt.max_connections(4)
        .min_connections(1)
        .connect_timeout(Duration::from_secs(5))
        .sqlx_logging(false);
    let raw = Database::connect(opt).await.expect("connect sqlite file");
    raw.execute_unprepared("PRAGMA journal_mode=WAL;")
        .await
        .expect("set WAL");
    let conn = DbConnection::from_raw(raw);
    TestContainer::singleton(conn.clone());
    (conn, guard, tmpdir)
}

// ---- BelongsToMany dogfood -----------------------------------------------

#[model(table = "af2_users", relations = {
    tags: BelongsToMany<Af2Tag, Af2UserTag>,
})]
pub struct Af2User {
    pub id: i64,
    pub name: String,
}

#[model(table = "af2_tags")]
pub struct Af2Tag {
    pub id: i64,
    pub label: String,
}

#[model(table = "af2_user_tags")]
pub struct Af2UserTag {
    pub id: i64,
    pub af2_user_id: i64,
    pub af2_tag_id: i64,
}

async fn migrate_b2m(conn: &DbConnection) {
    conn.inner()
        .execute_unprepared(
            "CREATE TABLE af2_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
        )
        .await
        .unwrap();
    conn.inner()
        .execute_unprepared(
            "CREATE TABLE af2_tags (id INTEGER PRIMARY KEY AUTOINCREMENT, label TEXT NOT NULL)",
        )
        .await
        .unwrap();
    conn.inner()
        .execute_unprepared(
            "CREATE TABLE af2_user_tags (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                af2_user_id INTEGER NOT NULL, \
                af2_tag_id INTEGER NOT NULL\
             )",
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn belongs_to_many_attach_inside_tx_rolls_back() {
    let (conn, _guard, _tmp) = fresh_multiconn_sqlite().await;
    migrate_b2m(&conn).await;

    let u = Af2User::create(attrs! { name: "Alice" }).await.unwrap();
    let t = Af2Tag::create(attrs! { label: "blue" }).await.unwrap();

    let user_id = u.id;
    let tag_id = t.id;
    let result: Result<(), FrameworkError> = DB::transaction(move |_tx| {
        Box::pin(async move {
            let user = Af2User::find(user_id).await?.unwrap();
            user.tags().attach(tag_id).await?;
            Err(FrameworkError::internal("rollback"))
        })
    })
    .await;
    assert!(result.is_err());

    // Pre-fix: attach silently committed on a pool connection — count is 1.
    // Post-fix: attach landed on the tx and rolled back — count is 0.
    let user = Af2User::find(u.id).await.unwrap().unwrap();
    let count = user.tags().count().await.unwrap();
    assert_eq!(
        count, 0,
        "attach must roll back atomically with the enclosing tx"
    );
}

#[tokio::test]
async fn belongs_to_many_detach_inside_tx_rolls_back() {
    let (conn, _guard, _tmp) = fresh_multiconn_sqlite().await;
    migrate_b2m(&conn).await;

    let u = Af2User::create(attrs! { name: "Bob" }).await.unwrap();
    let t = Af2Tag::create(attrs! { label: "red" }).await.unwrap();
    Af2UserTag::create(attrs! { af2_user_id: u.id, af2_tag_id: t.id })
        .await
        .unwrap();

    let user_id = u.id;
    let tag_id = t.id;
    let result: Result<(), FrameworkError> = DB::transaction(move |_tx| {
        Box::pin(async move {
            let user = Af2User::find(user_id).await?.unwrap();
            user.tags().detach(tag_id).await?;
            Err(FrameworkError::internal("rollback"))
        })
    })
    .await;
    assert!(result.is_err());

    // Pre-fix: detach silently committed on the pool — count is 0.
    // Post-fix: detach rolled back — count is back to 1.
    let user = Af2User::find(u.id).await.unwrap().unwrap();
    let count = user.tags().count().await.unwrap();
    assert_eq!(
        count, 1,
        "detach must roll back atomically with the enclosing tx"
    );
}

#[tokio::test]
async fn belongs_to_many_sync_inside_tx_rolls_back() {
    let (conn, _guard, _tmp) = fresh_multiconn_sqlite().await;
    migrate_b2m(&conn).await;

    let u = Af2User::create(attrs! { name: "Carol" }).await.unwrap();
    let t1 = Af2Tag::create(attrs! { label: "alpha" }).await.unwrap();
    let t2 = Af2Tag::create(attrs! { label: "beta" }).await.unwrap();
    let t3 = Af2Tag::create(attrs! { label: "gamma" }).await.unwrap();

    // Initial state: t1 attached.
    Af2UserTag::create(attrs! { af2_user_id: u.id, af2_tag_id: t1.id })
        .await
        .unwrap();

    let user_id = u.id;
    let t2_id = t2.id;
    let t3_id = t3.id;
    let result: Result<(), FrameworkError> = DB::transaction(move |_tx| {
        Box::pin(async move {
            let user = Af2User::find(user_id).await?.unwrap();
            // Replace the full set: detach t1, attach t2 + t3.
            user.tags().sync([t2_id, t3_id]).await?;
            Err(FrameworkError::internal("rollback"))
        })
    })
    .await;
    assert!(result.is_err());

    let user = Af2User::find(u.id).await.unwrap().unwrap();
    let tags = user.tags().get().await.unwrap();
    let labels: Vec<&str> = tags.iter().map(|t| t.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["alpha"],
        "sync must roll back atomically — t1 still attached, t2/t3 not"
    );
}

#[tokio::test]
async fn belongs_to_many_count_inside_tx_sees_in_tx_state() {
    let (conn, _guard, _tmp) = fresh_multiconn_sqlite().await;
    migrate_b2m(&conn).await;

    let u = Af2User::create(attrs! { name: "Dave" }).await.unwrap();
    let t1 = Af2Tag::create(attrs! { label: "pre-tx" }).await.unwrap();
    let t2 = Af2Tag::create(attrs! { label: "in-tx" }).await.unwrap();
    Af2UserTag::create(attrs! { af2_user_id: u.id, af2_tag_id: t1.id })
        .await
        .unwrap();

    let user_id = u.id;
    let t2_id = t2.id;
    let result: Result<(), FrameworkError> = DB::transaction(move |_tx| {
        Box::pin(async move {
            let user = Af2User::find(user_id).await?.unwrap();
            // Attach a second tag inside the tx.
            user.tags().attach(t2_id).await?;
            // The lazy count() must see the in-tx attach.
            let n = user.tags().count().await?;
            assert_eq!(
                n, 2,
                "lazy count() must see in-tx attaches; pool-pinned arm would return 1"
            );
            Err(FrameworkError::internal("rollback"))
        })
    })
    .await;
    assert!(result.is_err());

    let user = Af2User::find(u.id).await.unwrap().unwrap();
    let after = user.tags().count().await.unwrap();
    assert_eq!(after, 1, "rollback must un-write the in-tx attach");
}

// ---- MorphToMany dogfood -------------------------------------------------

#[model(table = "af2_posts", relations = {
    labels: MorphToMany<Af2Label, Af2Taggable> { name = "taggable" },
})]
pub struct Af2Post {
    pub id: i64,
    pub title: String,
}

#[model(table = "af2_labels")]
pub struct Af2Label {
    pub id: i64,
    pub name: String,
}

#[model(table = "af2_taggables")]
pub struct Af2Taggable {
    pub id: i64,
    pub af2_label_id: i64,
    pub taggable_id: i64,
    pub taggable_type: String,
}

async fn migrate_morph_m2m(conn: &DbConnection) {
    conn.inner()
        .execute_unprepared(
            "CREATE TABLE af2_posts (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL)",
        )
        .await
        .unwrap();
    conn.inner()
        .execute_unprepared(
            "CREATE TABLE af2_labels (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
        )
        .await
        .unwrap();
    conn.inner()
        .execute_unprepared(
            "CREATE TABLE af2_taggables (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                af2_label_id INTEGER NOT NULL, \
                taggable_id INTEGER NOT NULL, \
                taggable_type TEXT NOT NULL\
             )",
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn morph_to_many_attach_inside_tx_rolls_back() {
    let (conn, _guard, _tmp) = fresh_multiconn_sqlite().await;
    migrate_morph_m2m(&conn).await;

    let p = Af2Post::create(attrs! { title: "hello" }).await.unwrap();
    let l = Af2Label::create(attrs! { name: "urgent" }).await.unwrap();

    let post_id = p.id;
    let label_id = l.id;
    let result: Result<(), FrameworkError> = DB::transaction(move |_tx| {
        Box::pin(async move {
            let post = Af2Post::find(post_id).await?.unwrap();
            post.labels().attach(label_id).await?;
            Err(FrameworkError::internal("rollback"))
        })
    })
    .await;
    assert!(result.is_err());

    let post = Af2Post::find(p.id).await.unwrap().unwrap();
    let count = post.labels().count().await.unwrap();
    assert_eq!(
        count, 0,
        "MorphToMany::attach must roll back atomically with the enclosing tx"
    );
}
