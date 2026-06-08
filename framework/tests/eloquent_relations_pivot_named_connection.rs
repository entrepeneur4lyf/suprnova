//! Pivot writes (`attach` / `detach` / `sync`) must route through the
//! parent model's per-model `#[model(connection = "...")]` default.
//!
//! Mirrors the shape of `eloquent_eager_named_connection.rs`: register
//! ONLY a named pool (no default), point every model at it, then
//! exercise the pivot surface. Pre-A2-H-001 the pivot writes called
//! `ExecutorChoice::resolve_write(None, None, None)` — dropping the
//! per-model default — and fell through to the primary pool. With no
//! primary registered, every pivot write would error with "default
//! connection not initialised". The fix threads
//! `L::default_connection_name()` so the named pool is consulted.

use serial_test::serial;
use suprnova::DbConnection;
use suprnova::database::ConnectionRegistry;
use suprnova::testing::TestContainer;
use suprnova::{Model, attrs, model};

#[model(table = "epnc_users", connection = "pivot_named_only", relations = {
    tags: BelongsToMany<EpncTag, EpncUserTag>,
})]
pub struct EpncUser {
    pub id: i64,
    pub name: String,
}

#[model(table = "epnc_tags", connection = "pivot_named_only")]
pub struct EpncTag {
    pub id: i64,
    pub name: String,
}

#[model(
    table = "epnc_user_tags",
    connection = "pivot_named_only",
    primary_key = "id"
)]
pub struct EpncUserTag {
    pub id: i64,
    pub epnc_user_id: i64,
    pub epnc_tag_id: i64,
}

async fn fresh_named_only_pool() -> DbConnection {
    let _ = TestContainer::fake();
    ConnectionRegistry::clear();

    let conn = sea_orm::Database::connect("sqlite::memory:?mode=rwc")
        .await
        .expect("named-only in-memory pool");
    let db = DbConnection::from_raw(conn);
    use sea_orm::ConnectionTrait;
    db.inner()
        .execute_unprepared(
            "CREATE TABLE epnc_users (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                name TEXT NOT NULL\
             )",
        )
        .await
        .unwrap();
    db.inner()
        .execute_unprepared(
            "CREATE TABLE epnc_tags (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                name TEXT NOT NULL\
             )",
        )
        .await
        .unwrap();
    db.inner()
        .execute_unprepared(
            "CREATE TABLE epnc_user_tags (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                epnc_user_id INTEGER NOT NULL, \
                epnc_tag_id INTEGER NOT NULL, \
                UNIQUE(epnc_user_id, epnc_tag_id)\
             )",
        )
        .await
        .unwrap();
    ConnectionRegistry::register_existing("pivot_named_only", db.clone())
        .await
        .unwrap();
    db
}

#[tokio::test]
#[serial]
async fn pivot_attach_routes_to_parent_model_named_connection() {
    let _db = fresh_named_only_pool().await;
    suprnova::eloquent::unguarded(|| async {
        let u = EpncUser::create(attrs! { id: 1i64, name: "Alice" })
            .await
            .unwrap();
        let _t = EpncTag::create(attrs! { id: 1i64, name: "rust" })
            .await
            .unwrap();
        let _t2 = EpncTag::create(attrs! { id: 2i64, name: "web" })
            .await
            .unwrap();
        // Pre-A2-H-001: this call would error with "default connection not
        // initialised" because resolve_write got `(None, None, None)` and
        // walked off the end of the routing chain into the primary pool.
        u.tags().attach(1i64).await.unwrap();
        u.tags().attach(2i64).await.unwrap();
        let count = EpncUser::find(1i64)
            .await
            .unwrap()
            .unwrap()
            .tags()
            .count()
            .await
            .unwrap();
        assert_eq!(count, 2, "both attaches must land on the named pool");
    })
    .await;
}

#[tokio::test]
#[serial]
async fn pivot_sync_routes_to_parent_model_named_connection() {
    let _db = fresh_named_only_pool().await;
    suprnova::eloquent::unguarded(|| async {
        let u = EpncUser::create(attrs! { id: 2i64, name: "Bob" })
            .await
            .unwrap();
        let _ = EpncTag::create(attrs! { id: 10i64, name: "x" })
            .await
            .unwrap();
        let _ = EpncTag::create(attrs! { id: 11i64, name: "y" })
            .await
            .unwrap();
        let _ = EpncTag::create(attrs! { id: 12i64, name: "z" })
            .await
            .unwrap();

        u.tags().attach(10i64).await.unwrap();
        u.tags().attach(11i64).await.unwrap();

        // sync walks both SELECT (current pivot rows) and the
        // attach/detach loop; all of it must hit the named pool.
        EpncUser::find(2i64)
            .await
            .unwrap()
            .unwrap()
            .tags()
            .sync(vec![11i64, 12i64])
            .await
            .unwrap();

        let count = EpncUser::find(2i64)
            .await
            .unwrap()
            .unwrap()
            .tags()
            .count()
            .await
            .unwrap();
        assert_eq!(count, 2, "sync should leave exactly the post-sync set");
    })
    .await;
}

#[tokio::test]
#[serial]
async fn pivot_detach_routes_to_parent_model_named_connection() {
    let _db = fresh_named_only_pool().await;
    suprnova::eloquent::unguarded(|| async {
        let u = EpncUser::create(attrs! { id: 3i64, name: "Carol" })
            .await
            .unwrap();
        let _ = EpncTag::create(attrs! { id: 20i64, name: "p" })
            .await
            .unwrap();
        let _ = EpncTag::create(attrs! { id: 21i64, name: "q" })
            .await
            .unwrap();
        u.tags().attach(20i64).await.unwrap();
        u.tags().attach(21i64).await.unwrap();

        EpncUser::find(3i64)
            .await
            .unwrap()
            .unwrap()
            .tags()
            .detach(20i64)
            .await
            .unwrap();

        let count = EpncUser::find(3i64)
            .await
            .unwrap()
            .unwrap()
            .tags()
            .count()
            .await
            .unwrap();
        assert_eq!(count, 1, "detach should drop one row on the named pool");
    })
    .await;
}
