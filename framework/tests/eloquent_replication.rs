//! Phase 10C T13 — Model::replicate becomes async + fires
//! `Replicating` event.
//!
//! 10A T4 shipped `replicate` / `replicate_except` / `replicate_into`
//! as a sync (Self) signature. T13 makes them async + Result, and
//! wires `Replicating { source, replica }` to fire from
//! `replicate` and `replicate_except`. `replicate_into<T>` skips
//! the event because it's per-source-type — see the docstring on
//! `Model::replicate_into`.
//!
//! ## Test isolation
//!
//! The dispatcher is process-global. Each scenario uses a distinct
//! per-test model (`T13Post` vs `T13MutPost` vs `T13Draft`) so
//! listeners registered in one test never see events from another
//! test's `create` / `replicate` calls.

use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use suprnova::events::{EventFacade, Listener};
use suprnova::testing::TestDatabase;
use suprnova::{FrameworkError, Model, attrs};

// ---- Model 1: pure event-firing assertion --------------------------------

#[suprnova::model(table = "t13_posts")]
pub struct T13Post {
    pub id: i64,
    pub title: String,
    pub author_id: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

async fn create_posts_table(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE t13_posts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            author_id INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )",
    )
    .await
    .unwrap();
}

static REPLICATING_FIRED: AtomicUsize = AtomicUsize::new(0);

pub struct CountReplicating;

#[async_trait]
impl Listener<t13_post::events::Replicating> for CountReplicating {
    async fn handle(&self, _event: &t13_post::events::Replicating) -> Result<(), FrameworkError> {
        REPLICATING_FIRED.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn replicate_fires_replicating_event() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    create_posts_table(&db).await;

    EventFacade::listen::<t13_post::events::Replicating, _>(std::sync::Arc::new(CountReplicating))
        .await;

    let p = T13Post::create(attrs! { title: "original", author_id: 1 })
        .await
        .unwrap();

    // Snapshot to zero so we count only the dispatch from replicate().
    REPLICATING_FIRED.store(0, Ordering::SeqCst);

    let replica = p.replicate().await.unwrap();

    assert_eq!(REPLICATING_FIRED.load(Ordering::SeqCst), 1);
    assert_eq!(replica.id, 0, "PK must reset on the replica");
    assert_eq!(replica.title, "original");
    assert_eq!(replica.author_id, 1);
}

// ---- Model 2: replicate_except drops listed fields -----------------------

#[suprnova::model(table = "t13_except_posts")]
pub struct T13ExceptPost {
    pub id: i64,
    pub title: String,
    pub author_id: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[tokio::test]
async fn replicate_except_drops_listed_fields() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t13_except_posts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            author_id INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )",
    )
    .await
    .unwrap();

    let p = T13ExceptPost::create(attrs! { title: "secret", author_id: 42 })
        .await
        .unwrap();
    let replica = p.replicate_except(["author_id"]).await.unwrap();
    assert_eq!(replica.title, "secret");
    assert_eq!(
        replica.author_id, 0,
        "author_id reset because it was listed in `except`"
    );
    assert_eq!(replica.id, 0);
}

// ---- Model 3: listener mutates the replica before return -----------------

#[suprnova::model(table = "t13_mut_posts")]
pub struct T13MutPost {
    pub id: i64,
    pub title: String,
    pub author_id: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub struct TitleMutatingListener;

#[async_trait]
impl Listener<t13_mut_post::events::Replicating> for TitleMutatingListener {
    async fn handle(
        &self,
        event: &t13_mut_post::events::Replicating,
    ) -> Result<(), FrameworkError> {
        let mut replica = event.replica.lock().await;
        replica.title = format!("(copy) {}", replica.title);
        Ok(())
    }
}

#[tokio::test]
async fn replicating_listener_can_mutate_replica_before_return() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t13_mut_posts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            author_id INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )",
    )
    .await
    .unwrap();

    EventFacade::listen::<t13_mut_post::events::Replicating, _>(std::sync::Arc::new(
        TitleMutatingListener,
    ))
    .await;

    let p = T13MutPost::create(attrs! { title: "hello", author_id: 1 })
        .await
        .unwrap();
    let replica = p.replicate().await.unwrap();
    assert_eq!(
        replica.title, "(copy) hello",
        "listener mutation through Arc<Mutex<Self>> must reflect in caller's value"
    );
}

// ---- Model 4: replicate_into cross-type bridge ---------------------------

#[suprnova::model(table = "t13_into_posts")]
pub struct T13IntoPost {
    pub id: i64,
    pub title: String,
    pub author_id: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[suprnova::model(table = "t13_drafts", timestamps = false)]
pub struct T13Draft {
    pub id: i64,
    pub title: String,
    pub author_id: i64,
}

#[tokio::test]
async fn replicate_into_copies_matching_fields_and_drops_target_extras() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t13_into_posts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            author_id INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE t13_drafts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            author_id INTEGER NOT NULL
        )",
    )
    .await
    .unwrap();

    let p = T13IntoPost::create(attrs! { title: "publish me", author_id: 7 })
        .await
        .unwrap();

    let draft: T13Draft = p.replicate_into().await.expect("replicate_into");
    assert_eq!(draft.title, "publish me");
    assert_eq!(draft.author_id, 7);
    assert_eq!(draft.id, 0, "PK reset on the cross-type replica");

    // Unsaved — replicate_into never touches the database.
    assert_eq!(T13Draft::all().await.unwrap().len(), 0);
}

// ---- Model 5: eager-loaded relations survive replicate -------------------

#[suprnova::model(table = "t13_rel_users", relations = {
    posts: HasMany<T13RelPost>,
})]
pub struct T13RelUser {
    pub id: i64,
    pub name: String,
}

#[suprnova::model(table = "t13_rel_posts", timestamps = false)]
pub struct T13RelPost {
    pub id: i64,
    pub t13_rel_user_id: i64,
    pub title: String,
}

#[tokio::test]
async fn replicate_preserves_eager_loaded_relations() {
    // Laravel parity: `clone $user` retains `$user->posts`. The
    // replica must carry the source's eager-load cache (deep-cloned
    // by `EagerLoadCache::clone`) so callers don't pay a re-fetch
    // cost just to look at relations they already loaded.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t13_rel_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE t13_rel_posts (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         t13_rel_user_id INTEGER NOT NULL, title TEXT NOT NULL)",
    )
    .await
    .unwrap();

    let u = T13RelUser::create(attrs! { name: "alice" }).await.unwrap();
    let _ = T13RelPost::create(attrs! { t13_rel_user_id: u.id, title: "post-a" })
        .await
        .unwrap();
    let _ = T13RelPost::create(attrs! { t13_rel_user_id: u.id, title: "post-b" })
        .await
        .unwrap();

    // Eager-load posts on the source row.
    let users = T13RelUser::with(["posts"]).get().await.unwrap();
    let source = users.iter().find(|x| x.id == u.id).unwrap();
    assert_eq!(source.posts_loaded().len(), 2, "fixture eager-load sanity");

    // Replicate. If `replicate_with` reset `__eager` to
    // `EagerLoadCache::default()` instead of cloning it, this
    // assertion would see zero loaded posts on the replica.
    let replica = source.replicate().await.unwrap();
    assert_eq!(replica.id, 0, "PK reset on the replica");
    assert_eq!(
        replica.posts_loaded().len(),
        2,
        "eager-loaded `posts` must survive replicate (Laravel parity)"
    );

    // `replicate_except` follows the same parity rule — the `except`
    // list filters column values, not relation cache entries.
    let replica2 = source.replicate_except(["name"]).await.unwrap();
    assert_eq!(replica2.name, "", "name cleared by `except`");
    assert_eq!(
        replica2.posts_loaded().len(),
        2,
        "eager-loaded `posts` must survive replicate_except"
    );

    // Mutating the replica's cache must not bleed back into the
    // source — `EagerLoadCache::clone` is deep, not Arc-shared.
    let mut owned = replica;
    owned.__eager = Default::default();
    assert_eq!(
        source.posts_loaded().len(),
        2,
        "clearing the replica's cache must not affect the source"
    );
}
