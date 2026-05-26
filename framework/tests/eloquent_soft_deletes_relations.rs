//! Phase 10B P11 — Soft deletes + relations interaction.
//!
//! Pins how `#[model(soft_deletes)]` composes with the relation
//! surface from Phase 10B. The question Laravel-coming users ask first:
//!
//! - Does `User::all()` hide trashed users? — yes, T10 already pinned.
//! - Does `user.posts().get()` work on a *trashed* user? — yes,
//!   because the trashed user is still a fully-materialised Rust
//!   value; the scope only filters reads.
//! - Does eager-loading children skip trashed children? — yes,
//!   because the per-relation arm builds `R::query()` which routes
//!   through the soft-delete scope.
//! - Does `with_trashed()` propagate into relation builders? — yes,
//!   P11 ships [`Builder<M>::with_trashed`] / [`only_trashed`] plus
//!   forwarding methods on every relation wrapper, so
//!   `user.posts().with_trashed().get()` and the closure form
//!   `User::query().with_where(("posts", |q| q.with_trashed()))`
//!   both work.
//! - Does `force_delete` / `restore` / `delete` cascade to children?
//!   — NO. Pinned here. Laravel doesn't cascade either; cascade is a
//!   per-app concern users can handle via the event surface (10C).
//!
//! The tests below use throwaway tables (`sdrel_*` prefix) to avoid
//! colliding with any other inventory-registered model. SeaORM's
//! reflection layer cares about table uniqueness within the test
//! binary, not within the workspace.

use chrono::{DateTime, Utc};
use suprnova::eloquent::SoftDeletes;
use suprnova::testing::TestDatabase;
use suprnova::{Model, attrs, model};

// ---- Schema -------------------------------------------------------------

#[model(table = "sdrel_users", soft_deletes, fillable = ["name", "email"], relations = {
    posts: HasMany<SdRelPost>,
})]
pub struct SdRelUser {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[model(table = "sdrel_posts", soft_deletes, fillable = ["sd_rel_user_id", "title"], relations = {
    user: BelongsTo<SdRelUser>,
})]
pub struct SdRelPost {
    pub id: i64,
    pub sd_rel_user_id: i64,
    pub title: String,
    pub deleted_at: Option<DateTime<Utc>>,
}

async fn migrate(db: &TestDatabase) {
    // Schema mirrors what the macro emits for these structs:
    // no auto-timestamps because neither struct declares
    // `created_at` / `updated_at`, so the macro auto-disables them
    // (see `parse.rs` timestamps auto-detect). Tables therefore
    // omit those columns too.
    db.execute_unprepared(
        "CREATE TABLE sdrel_users (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            name TEXT NOT NULL, \
            email TEXT NOT NULL, \
            deleted_at TEXT\
         )",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE sdrel_posts (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            sd_rel_user_id INTEGER NOT NULL, \
            title TEXT NOT NULL, \
            deleted_at TEXT\
         )",
    )
    .await
    .unwrap();
}

async fn seed(_db: &TestDatabase) -> (SdRelUser, SdRelPost, SdRelPost) {
    let u = SdRelUser::create(attrs! { name: "Alice", email: "a@x.com" })
        .await
        .unwrap();
    let p1 = SdRelPost::create(attrs! { sd_rel_user_id: u.id, title: "alive" })
        .await
        .unwrap();
    let p2 = SdRelPost::create(attrs! { sd_rel_user_id: u.id, title: "trashed" })
        .await
        .unwrap();
    (u, p1, p2)
}

// ---- 1: lazy relation call on a trashed parent --------------------------

#[tokio::test]
async fn relation_call_on_trashed_parent_returns_alive_children() {
    // Holding a soft-deleted parent value, calling `.posts().get()` on
    // it still reads from the child table. The scope only hides the
    // parent from collection reads — it doesn't prevent navigation
    // from a Rust value you already have. Trashed children are still
    // filtered by THEIR own scope (so we only see the alive one).
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&db).await;
    let (u, _p1, p2) = seed(&db).await;
    let user_id = u.id;

    // Trash one post so we can confirm child scope still applies.
    p2.delete().await.unwrap();
    // Trash the parent itself.
    let u = SdRelUser::find(user_id)
        .await
        .unwrap()
        .expect("alive before delete");
    u.delete().await.unwrap();

    // Pull the trashed user out via the unscoped query.
    let trashed_user = SdRelUser::with_trashed()
        .filter("id", user_id)
        .first()
        .await
        .unwrap()
        .expect("with_trashed sees the row");
    assert!(trashed_user.is_trashed(), "parent is trashed");

    // Navigation works; child scope hides the trashed child.
    let posts = trashed_user.posts().get().await.unwrap();
    assert_eq!(posts.len(), 1, "trashed child stays hidden by default");
    assert_eq!(posts[0].title, "alive");
}

// ---- 2: lazy BelongsTo from child to trashed parent ---------------------

#[tokio::test]
async fn belongs_to_trashed_parent_returns_none_by_default() {
    // Real-world soft-delete-relations need: "show the original
    // author of a comment even if their account is gone." Today's
    // default scope hides the trashed parent so `.user().first()`
    // returns None. The escape hatch is `.with_trashed()` on the
    // relation builder (added by P11).
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&db).await;
    let (u, p1, _p2) = seed(&db).await;
    let user_id = u.id;
    u.delete().await.unwrap();

    let p = SdRelPost::find(p1.id).await.unwrap().expect("post alive");
    let author = p.user().first().await.unwrap();
    assert!(author.is_none(), "default scope hides the trashed parent");

    // With escape hatch: see the trashed parent.
    let p2 = SdRelPost::find(p1.id).await.unwrap().unwrap();
    let trashed_author = p2.user().with_trashed().first().await.unwrap();
    let trashed_author = trashed_author.expect("with_trashed reveals trashed parent");
    assert_eq!(trashed_author.id, user_id);
    assert!(trashed_author.is_trashed());
}

// ---- 3: eager load excludes trashed children by default -----------------

#[tokio::test]
async fn eager_load_excludes_trashed_children_by_default() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&db).await;
    let (_u, _p1, p2) = seed(&db).await;
    p2.delete().await.unwrap();

    let users = SdRelUser::query().with(["posts"]).get().await.unwrap();
    assert_eq!(users.len(), 1);
    let posts = users[0].posts_loaded();
    assert_eq!(posts.len(), 1, "trashed child excluded from eager load");
    assert_eq!(posts[0].title, "alive");
}

// ---- 4: with_trashed on the parent query reveals trashed parents --------

#[tokio::test]
async fn with_trashed_collection_eager_loads_for_trashed_parents() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&db).await;
    let (u, _p1, _p2) = seed(&db).await;
    let user_id = u.id;
    u.delete().await.unwrap();

    // Default query: trashed parent invisible.
    let alive = SdRelUser::query().with(["posts"]).get().await.unwrap();
    assert!(alive.is_empty(), "trashed parent not in default scope");

    // with_trashed: parent reappears, eager-load still works.
    let all = SdRelUser::with_trashed()
        .with(["posts"])
        .get()
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, user_id);
    // Children are alive (we only trashed the parent).
    assert_eq!(all[0].posts_loaded().len(), 2);
}

// ---- 5: relation builder honors with_trashed for trashed CHILDREN -------

#[tokio::test]
async fn relation_builder_with_trashed_includes_trashed_children() {
    // P11 surface: `user.posts().with_trashed().get()` widens the
    // child scope to include trashed posts. This is the forwarding
    // path we ship — relation wrappers expose the same modifier as
    // the underlying Builder<R>.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&db).await;
    let (u, _p1, p2) = seed(&db).await;
    p2.delete().await.unwrap();

    let alive = u.clone().posts().get().await.unwrap();
    assert_eq!(alive.len(), 1);

    let all = u.posts().with_trashed().get().await.unwrap();
    assert_eq!(all.len(), 2);
}

// ---- 6: relation builder only_trashed sees only trashed children --------

#[tokio::test]
async fn relation_builder_only_trashed_filters_to_dead_children() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&db).await;
    let (u, _p1, p2) = seed(&db).await;
    p2.delete().await.unwrap();

    let dead = u.posts().only_trashed().get().await.unwrap();
    assert_eq!(dead.len(), 1);
    assert_eq!(dead[0].title, "trashed");
}

// ---- 7: with_where closure can apply with_trashed via Builder<R> --------

#[tokio::test]
async fn with_where_closure_can_widen_to_trashed_children() {
    // Laravel: `User::with(['posts' => fn($q) => $q->withTrashed()])`.
    // Suprnova: `with_where(("posts", |q| q.with_trashed()))`.
    // The closure receives a `Builder<R>` — to support this, the
    // Builder itself has to expose `with_trashed()` on soft-delete
    // models. P11 adds the method.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&db).await;
    let (_u, _p1, p2) = seed(&db).await;
    p2.delete().await.unwrap();

    let users = SdRelUser::query()
        .with_where(("posts", |q: suprnova::Builder<SdRelPost>| q.with_trashed()))
        .get()
        .await
        .unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(
        users[0].posts_loaded().len(),
        2,
        "eager-load closure widens scope to trashed children"
    );
}

// ---- 8: parent soft delete does NOT cascade to children -----------------

#[tokio::test]
async fn parent_soft_delete_does_not_cascade_to_children() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&db).await;
    let (u, _p1, _p2) = seed(&db).await;
    u.delete().await.unwrap();

    // Children remain alive — they have their own scope and were
    // never touched by the parent's `delete()`.
    let kids = SdRelPost::all().await.unwrap();
    assert_eq!(kids.len(), 2);
    assert!(kids.iter().all(|p| !p.is_trashed()));
}

// ---- 9: parent force_delete does NOT cascade to children ----------------

#[tokio::test]
async fn parent_force_delete_does_not_cascade_to_children() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&db).await;
    let (u, _p1, _p2) = seed(&db).await;
    u.force_delete().await.unwrap();

    // Children remain alive; we never registered an ON DELETE
    // CASCADE constraint and the framework never injects one. Users
    // wanting cascade write their migration with a real FK.
    let kids = SdRelPost::all().await.unwrap();
    assert_eq!(kids.len(), 2);
}

// ---- 10: parent restore does NOT cascade to children --------------------

#[tokio::test]
async fn parent_restore_does_not_cascade_to_children() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&db).await;
    let (u, _p1, p2) = seed(&db).await;
    let user_id = u.id;
    let post2_id = p2.id;

    // Trash both parent and one child, then restore only the parent.
    p2.delete().await.unwrap();
    let u = SdRelUser::find(user_id).await.unwrap().unwrap();
    u.delete().await.unwrap();

    let trashed_user = SdRelUser::with_trashed()
        .filter("id", user_id)
        .first()
        .await
        .unwrap()
        .unwrap();
    trashed_user.restore().await.unwrap();

    // Parent alive again, child still trashed.
    let alive_user = SdRelUser::find(user_id).await.unwrap().unwrap();
    assert!(!alive_user.is_trashed());

    let p2_now = SdRelPost::with_trashed()
        .filter("id", post2_id)
        .first()
        .await
        .unwrap()
        .unwrap();
    assert!(
        p2_now.is_trashed(),
        "restore on parent does not un-trash the previously-trashed child"
    );
}

// ---- 11: Builder<M>::with_trashed retains other WHERE terms -------------

#[tokio::test]
async fn builder_with_trashed_preserves_other_filters() {
    // Sanity: with_trashed() removes ONLY the auto-applied deleted_at
    // null filter — not unrelated WHERE terms the user has chained.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&db).await;
    let alice = SdRelUser::create(attrs! { name: "Alice", email: "a@x.com" })
        .await
        .unwrap();
    let bob = SdRelUser::create(attrs! { name: "Bob", email: "b@x.com" })
        .await
        .unwrap();
    alice.delete().await.unwrap();
    let _ = bob; // bob stays alive

    // Without with_trashed: only Bob comes back.
    let alive = SdRelUser::query().get().await.unwrap();
    assert_eq!(alive.len(), 1);

    // With with_trashed + a name filter: only Alice (because the
    // name filter still applies).
    let widened = SdRelUser::query()
        .with_trashed()
        .filter("name", "Alice")
        .get()
        .await
        .unwrap();
    assert_eq!(widened.len(), 1);
    assert_eq!(widened[0].name, "Alice");
}
