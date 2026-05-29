//! Eager loading must not require a default pool when a model routes
//! through a named / per-model connection.
//!
//! Reproduces the original bug: an app that registered only named
//! connections (no `DB::init`) and used `#[model(connection = "...")]`
//! to route every query through them got a "default connection not
//! initialised" error from the eager-load orchestrator the moment any
//! `Builder::get` was called with an active eager-load spec, even
//! though every SQL leaf would have routed through the model's named
//! connection successfully if it had been reached.
//!
//! The fix routes the orchestrator-level pool lookup through
//! `ExecutorChoice::resolve_read` so the per-builder and per-model
//! connection-override chain runs the same way it runs for the parent
//! SELECT. The regression bites if a future change reverts the
//! orchestrator's lookup back to `DB::connection()?`.

use serial_test::serial;
use suprnova::DbConnection;
use suprnova::database::ConnectionRegistry;
use suprnova::testing::TestContainer;
use suprnova::{Model, attrs, model};

#[model(table = "een_users", connection = "named_only", relations = {
    posts: HasMany<EenPost>,
})]
pub struct EenUser {
    pub id: i64,
    pub name: String,
}

#[model(table = "een_posts", connection = "named_only")]
pub struct EenPost {
    pub id: i64,
    pub een_user_id: i64,
    pub title: String,
}

async fn fresh_named_connection_without_default_pool() -> DbConnection {
    // Empty container — NO default pool registered. `DB::connection()`
    // will error if anything reaches it. This mirrors the production
    // configuration that surfaced the bug: only named pools, no
    // primary.
    let _ = TestContainer::fake();
    ConnectionRegistry::clear();

    let conn = sea_orm::Database::connect("sqlite::memory:?mode=rwc")
        .await
        .expect("named-only in-memory connection");
    let db = DbConnection::from_raw(conn);
    use sea_orm::ConnectionTrait;
    db.inner()
        .execute_unprepared(
            "CREATE TABLE een_users (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                name TEXT NOT NULL\
             )",
        )
        .await
        .unwrap();
    db.inner()
        .execute_unprepared(
            "CREATE TABLE een_posts (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                een_user_id INTEGER NOT NULL, \
                title TEXT NOT NULL\
             )",
        )
        .await
        .unwrap();
    ConnectionRegistry::register_existing("named_only", db.clone())
        .await
        .unwrap();
    db
}

#[tokio::test]
#[serial]
async fn eager_load_works_when_only_named_connection_registered() {
    // Provoking the original bug: the orchestrator unconditionally
    // called `DB::connection()?` before reaching the leaf arm. With no
    // default pool registered that lookup failed and the eager spec
    // never ran, even though every leaf would have routed through the
    // named connection just fine.
    let _db = fresh_named_connection_without_default_pool().await;

    // Use unguarded so the `id` field is not stripped by the default
    // primary-key guard (we want to assert against deterministic ids).
    suprnova::eloquent::unguarded(|| async {
        let alice = EenUser::create(attrs! { id: 1, name: "Alice" })
            .await
            .unwrap();
        let _ = EenPost::create(attrs! { id: 1, een_user_id: alice.id, title: "First" })
            .await
            .unwrap();
        let _ = EenPost::create(attrs! { id: 2, een_user_id: alice.id, title: "Second" })
            .await
            .unwrap();
    })
    .await;

    // Builder::get + with([...]) — the orchestrator's pool resolve was
    // what crashed before the fix.
    let users = EenUser::with(["posts"]).get().await.unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(
        users[0].posts_loaded().len(),
        2,
        "eager-loaded posts must materialize on the named connection"
    );

    // Collection::load also went through the buggy `DB::connection()?`
    // path; verify the fix covers it too.
    let mut bare = EenUser::query().get().await.unwrap();
    bare.load(["posts"]).await.unwrap();
    assert_eq!(bare[0].posts_loaded().len(), 2);

    // Collection::load_missing shares the same orchestrator entrypoint.
    let mut bare2 = EenUser::query().get().await.unwrap();
    bare2.load_missing(["posts"]).await.unwrap();
    assert_eq!(bare2[0].posts_loaded().len(), 2);

    ConnectionRegistry::clear();
}
