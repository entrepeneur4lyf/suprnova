//! Phase 10B T10 — End-to-end coverage for every relation kind against
//! the example app's real `Migrator` schema.
//!
//! Uses `TestDatabase::fresh::<Migrator>()` — same fresh-DB pattern Phase
//! 10A's `eloquent_dogfood.rs` established — so the schema the relations
//! resolve against can never drift from the migrator the dev DB actually
//! runs. Every test gets its own connection (the macro-emitted relation
//! method goes through `DB::connection()`, which `TestDatabase::fresh`
//! installs as the request-local override).
//!
//! Coverage matrix — one or more tests per relation kind:
//!
//! - `HasMany` — `User.posts()` count + get
//! - `BelongsToMany` + `Pivot` accessor — `User.roles()` attach_with +
//!   `.pivot::<RoleUser>()` reads pivot's `assigned_at`
//! - `MorphMany` on Post + Video — comments filter by `commentable_type`
//! - `MorphTo` returning the per-family `CommentableMorph` enum —
//!   Post variant, Video variant, Unknown variant
//! - `MorphToMany` on `Post.tags()` + `Video.tags()` — independent
//!   attaches against the shared `taggables` pivot
//! - `MorphedByMany` on `Tag.posts()` + `Tag.videos()` — cross-family
//!   isolation by `taggable_type`
//! - Eager `with(["posts", "roles"])` on User — no N+1
//! - Nested eager `with(["posts.comments"])` — three queries
//! - `with_count(["posts"])` — `posts_count()` reads server-side COUNT
//!
//! The schema lives in `app/src/migrations/m_2026_05_19_phase_10b_relations_schema.rs`;
//! the models in `app/src/models/{users,posts,roles,role_user,comments,videos,tags,taggables}.rs`.

use app::migrations::Migrator;
use app::models::comments::{Comment, CommentableMorph};
use app::models::posts::Post;
use app::models::profiles::Profile;
use app::models::role_user::RoleUser;
use app::models::roles::Role;
use app::models::tags::Tag;
use app::models::users::User;
use app::models::videos::Video;
use suprnova::testing::TestDatabase;
use suprnova::{attrs, model, Model};

/// Helper: a user named after the test. The user-side surface goes
/// through the framework's `User::create` → `DB::connection()` path, so
/// the `TestDatabase::fresh::<Migrator>()` connection installed by each
/// test resolves correctly without thread-local plumbing.
async fn make_user(name: &str) -> User {
    User::create(attrs! {
        name: name,
        email: format!("{name}@example.com"),
        password: "pw",
    })
    .await
    .unwrap()
}

// ---- HasMany: User.posts() ---------------------------------------------

#[tokio::test]
async fn has_many_user_posts_count_and_get() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    let u = make_user("hm_alice").await;
    for i in 0..3 {
        Post::create(attrs! {
            title: format!("post-{i}"),
            body: "...",
            is_public: true,
            author_id: u.id,
        })
        .await
        .unwrap();
    }

    // A second user with a single post — keeps the FK filter honest.
    let other = make_user("hm_bob").await;
    Post::create(attrs! {
        title: "bob-post",
        body: "...",
        is_public: false,
        author_id: other.id,
    })
    .await
    .unwrap();

    let count = u.posts().count().await.unwrap();
    assert_eq!(count, 3, "User.posts().count() must filter by author_id");

    let posts = u.posts().get().await.unwrap();
    assert_eq!(posts.len(), 3);
    assert!(posts.iter().all(|p| p.author_id == u.id));
}

// ---- HasOne: User.profile() -------------------------------------------
//
// Phase 10B P5 — closes the closeout self-audit gap. The Phase 10B T10
// dogfood exercised every relation kind EXCEPT HasOne; the existing
// models had no natural one-to-one shape. The Profile model
// (`app/src/models/profiles.rs`) + `m_2026_05_20_phase_10b_profiles`
// migration add one, and `User.profile: HasOne<Profile>` ties it
// together. These three tests cover the read paths the spec calls out:
//
//   1. `.first()` returns Some(_) when the row exists
//   2. `.first()` returns None when the row is absent
//   3. `User::with(["profile"]).get()` populates `profile_loaded()`
//      with the right Some/None per parent (single-value cache reads
//      via `__eager.get_one`, not `get_many`)

#[tokio::test]
async fn has_one_user_profile_returns_some_when_present() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    let u = make_user("ho_alice").await;
    Profile::create(attrs! {
        user_id: u.id,
        bio: "loves rust",
    })
    .await
    .unwrap();

    let p = u
        .profile()
        .first()
        .await
        .unwrap()
        .expect("profile present");
    assert_eq!(p.user_id, u.id, "profile.user_id must match parent.id");
    assert_eq!(p.bio, "loves rust", "fillable bio must round-trip");
}

#[tokio::test]
async fn has_one_user_profile_returns_none_when_absent() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    let u = make_user("ho_lonely").await;
    // No `Profile::create` — the FK row simply doesn't exist. HasOne's
    // `.first()` is `Option<T>` (NOT `Result<Option<T>, _>` unwrap-then-
    // unwrap), and the bare connection must return `Ok(None)` rather
    // than erroring on the empty scan.

    let p = u.profile().first().await.unwrap();
    assert!(p.is_none(), "no profile created, .first() must be None");
}

#[tokio::test]
async fn has_one_user_profile_eager_load_populates_loaded_accessor() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    let u1 = make_user("ho_eager_1").await;
    let u2 = make_user("ho_eager_2").await;
    Profile::create(attrs! {
        user_id: u1.id,
        bio: "u1 bio",
    })
    .await
    .unwrap();
    // u2 has no profile — the per-parent cache must distinguish
    // "present" from "absent" rather than collapsing both to None.

    let users = User::with(["profile"]).get().await.unwrap();
    let u1_loaded = users
        .iter()
        .find(|x| x.id == u1.id)
        .expect("u1 must surface in User::with([\"profile\"])");
    let u2_loaded = users
        .iter()
        .find(|x| x.id == u2.id)
        .expect("u2 must surface");

    let u1_profile = u1_loaded
        .profile_loaded()
        .expect("u1's profile must be in the eager cache");
    assert_eq!(u1_profile.user_id, u1.id);
    assert_eq!(u1_profile.bio, "u1 bio");
    assert!(
        u2_loaded.profile_loaded().is_none(),
        "u2 had no profile row — eager-load must surface None on the parent, \
         NOT borrow another user's profile",
    );
}

// ---- BelongsToMany + Pivot accessor: User.roles() ----------------------

#[tokio::test]
async fn belongs_to_many_user_roles_attach_with_pivot_data() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    let u = make_user("btm_alice").await;
    let admin = Role::create(attrs! { name: "admin" }).await.unwrap();
    let editor = Role::create(attrs! { name: "editor" }).await.unwrap();

    let when = chrono::Utc::now();
    u.roles()
        .attach_with(admin.id, attrs! { assigned_at: when })
        .await
        .unwrap();
    u.roles().attach(editor.id).await.unwrap();

    let roles = u.roles().get().await.unwrap();
    assert_eq!(roles.len(), 2);

    // Find the admin row and read pivot data through the `.pivot::<P>()`
    // accessor — confirms `with_pivot = ["assigned_at"]` was included
    // in the join and the type-erased Arc downcasts cleanly to RoleUser.
    let admin_row = roles
        .iter()
        .find(|r| r.name == "admin")
        .expect("admin role attached");
    let pivot: &RoleUser = admin_row.pivot::<RoleUser>();
    assert_eq!(pivot.user_id, u.id);
    assert_eq!(pivot.role_id, admin.id);
    assert_eq!(
        pivot.assigned_at.map(|t| t.timestamp()),
        Some(when.timestamp()),
        "assigned_at must round-trip via the pivot row",
    );

    // The editor row had no `assigned_at` — pivot read should still
    // return a populated pivot (the column is nullable on the schema).
    let editor_row = roles
        .iter()
        .find(|r| r.name == "editor")
        .expect("editor role attached");
    let p2: &RoleUser = editor_row.pivot::<RoleUser>();
    assert_eq!(p2.user_id, u.id);
    assert_eq!(p2.role_id, editor.id);
    assert!(p2.assigned_at.is_none(), "no assigned_at on plain attach");
}

// ---- MorphMany on Post + Video for comments ----------------------------

#[tokio::test]
async fn morph_many_post_and_video_comments_filter_by_type() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    let u = make_user("mm_alice").await;
    let post = Post::create(attrs! {
        title: "with-comments",
        body: "...",
        is_public: true,
        author_id: u.id,
    })
    .await
    .unwrap();
    let video = Video::create(attrs! { url: "/v/1.mp4" }).await.unwrap();

    // Two comments on the post, one on the video. `commentable_type`
    // distinguishes the families.
    Comment::create(attrs! {
        commentable_id: post.id,
        commentable_type: "post",
        body: "nice post",
    })
    .await
    .unwrap();
    Comment::create(attrs! {
        commentable_id: post.id,
        commentable_type: "post",
        body: "agreed",
    })
    .await
    .unwrap();
    Comment::create(attrs! {
        commentable_id: video.id,
        commentable_type: "video",
        body: "vid",
    })
    .await
    .unwrap();

    let post_comments = post.comments().get().await.unwrap();
    assert_eq!(post_comments.len(), 2);
    assert!(post_comments
        .iter()
        .all(|c| c.commentable_type == "post" && c.commentable_id == post.id));

    let video_comments = video.comments().get().await.unwrap();
    assert_eq!(video_comments.len(), 1);
    assert_eq!(video_comments[0].body, "vid");
    assert_eq!(video_comments[0].commentable_type, "video");
}

// ---- MorphTo → CommentableMorph enum -----------------------------------

#[tokio::test]
async fn morph_to_returns_post_variant() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    let u = make_user("mt_alice").await;
    let post = Post::create(attrs! {
        title: "morph-target",
        body: "...",
        is_public: true,
        author_id: u.id,
    })
    .await
    .unwrap();
    let c = Comment::create(attrs! {
        commentable_id: post.id,
        commentable_type: "post",
        body: "x",
    })
    .await
    .unwrap();

    match c.commentable().get().await.unwrap() {
        CommentableMorph::Post(parent) => {
            assert_eq!(parent.id, post.id);
            assert_eq!(parent.title, "morph-target");
        }
        CommentableMorph::Video(_) => panic!("expected Post variant"),
        CommentableMorph::Unknown(t, id) => {
            panic!("expected Post variant, got Unknown({t}, {id})");
        }
    }
}

#[tokio::test]
async fn morph_to_returns_video_variant() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    let v = Video::create(attrs! { url: "/v/morph.mp4" }).await.unwrap();
    let c = Comment::create(attrs! {
        commentable_id: v.id,
        commentable_type: "video",
        body: "vid",
    })
    .await
    .unwrap();

    match c.commentable().get().await.unwrap() {
        CommentableMorph::Video(parent) => {
            assert_eq!(parent.id, v.id);
            assert_eq!(parent.url, "/v/morph.mp4");
        }
        _ => panic!("expected Video variant"),
    }
}

#[tokio::test]
async fn morph_to_unknown_for_unregistered_type() {
    // Insert a row manually so the morph_type can hold a value that no
    // registered target matches. `Comment::create` would go through the
    // fillable filter — direct SQL is the safer path for this legacy
    // shape (mirrors framework/tests/eloquent_relations_morph.rs).
    let db = TestDatabase::fresh::<Migrator>().await.unwrap();
    // Direct INSERT bypasses the `fillable` allow-list so the row can
    // carry an unregistered morph_type. Stamps `created_at` /
    // `updated_at` in an RFC-3339 shape — SQLite's default
    // `CURRENT_TIMESTAMP` writes a space-separated form the framework's
    // `AsDateTime` cast can't parse on read.
    db.execute_unprepared(
        "INSERT INTO comments \
            (commentable_id, commentable_type, body, created_at, updated_at) \
         VALUES \
            (777, 'unknown_type', 'legacy', \
             '2026-05-20T00:00:00+00:00', '2026-05-20T00:00:00+00:00')",
    )
    .await
    .unwrap();
    let c = Comment::query()
        .filter("body", "legacy")
        .first()
        .await
        .unwrap()
        .expect("inserted comment");

    match c.commentable().get().await.unwrap() {
        CommentableMorph::Unknown(t, id) => {
            assert_eq!(t, "unknown_type");
            assert_eq!(id, 777);
        }
        other => panic!("expected Unknown variant, got {other:?}"),
    }
}

// ---- MorphToMany: Post.tags() + Video.tags() ---------------------------

#[tokio::test]
async fn morph_to_many_post_and_video_tags_independent_attach() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    let u = make_user("mtm_alice").await;
    let post = Post::create(attrs! {
        title: "tagged",
        body: "...",
        is_public: true,
        author_id: u.id,
    })
    .await
    .unwrap();
    let video = Video::create(attrs! { url: "/v/tagged.mp4" }).await.unwrap();

    // ONE tag attached to BOTH parents — two pivot rows, distinct by
    // `taggable_type`. Confirms the polymorphic m2m surface lets a
    // single related row span families.
    let t = Tag::create(attrs! { name: "shared" }).await.unwrap();
    post.tags().attach(t.id).await.unwrap();
    video.tags().attach(t.id).await.unwrap();

    let post_tags = post.tags().get().await.unwrap();
    let video_tags = video.tags().get().await.unwrap();
    assert_eq!(post_tags.len(), 1);
    assert_eq!(video_tags.len(), 1);
    assert_eq!(post_tags[0].id, t.id);
    assert_eq!(video_tags[0].id, t.id);
}

// ---- MorphedByMany: Tag.posts() + Tag.videos() -------------------------

#[tokio::test]
async fn morphed_by_many_cross_family_isolation() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    let u = make_user("mbm_alice").await;

    // Two posts and one video — all attached to the same tag. Inverse
    // side (`Tag.posts()` / `Tag.videos()`) must split by morph type:
    // posts gets 2, videos gets 1.
    let p1 = Post::create(attrs! {
        title: "p1",
        body: "...",
        is_public: true,
        author_id: u.id,
    })
    .await
    .unwrap();
    let p2 = Post::create(attrs! {
        title: "p2",
        body: "...",
        is_public: true,
        author_id: u.id,
    })
    .await
    .unwrap();
    let v = Video::create(attrs! { url: "/v/once.mp4" }).await.unwrap();

    let t = Tag::create(attrs! { name: "broad" }).await.unwrap();
    p1.tags().attach(t.id).await.unwrap();
    p2.tags().attach(t.id).await.unwrap();
    v.tags().attach(t.id).await.unwrap();

    let posts_via_tag = t.posts().get().await.unwrap();
    let videos_via_tag = t.videos().get().await.unwrap();
    assert_eq!(posts_via_tag.len(), 2);
    assert_eq!(videos_via_tag.len(), 1);
    let post_ids: Vec<i64> = posts_via_tag.iter().map(|p| p.id).collect();
    assert!(post_ids.contains(&p1.id));
    assert!(post_ids.contains(&p2.id));
    assert_eq!(videos_via_tag[0].id, v.id);
}

// ---- Eager `with(["posts", "roles"])` on User --------------------------

#[tokio::test]
async fn eager_with_posts_and_roles_populates_both_caches() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    let u = make_user("eager_alice").await;
    Post::create(attrs! {
        title: "eager-1",
        body: "...",
        is_public: true,
        author_id: u.id,
    })
    .await
    .unwrap();
    Post::create(attrs! {
        title: "eager-2",
        body: "...",
        is_public: true,
        author_id: u.id,
    })
    .await
    .unwrap();
    let r = Role::create(attrs! { name: "eager-role" }).await.unwrap();
    u.roles().attach(r.id).await.unwrap();

    let users = User::with(["posts", "roles"]).get().await.unwrap();
    let loaded = users
        .iter()
        .find(|x| x.id == u.id)
        .expect("inserted user must surface in User::all");

    assert_eq!(loaded.posts_loaded().len(), 2);
    assert_eq!(loaded.roles_loaded().len(), 1);
    assert_eq!(loaded.roles_loaded()[0].name, "eager-role");
}

// ---- Nested eager `with(["posts.comments"])` ---------------------------

#[tokio::test]
async fn nested_eager_posts_with_comments() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    let u = make_user("nested_alice").await;
    let p1 = Post::create(attrs! {
        title: "with-comments",
        body: "...",
        is_public: true,
        author_id: u.id,
    })
    .await
    .unwrap();
    let p2 = Post::create(attrs! {
        title: "no-comments",
        body: "...",
        is_public: true,
        author_id: u.id,
    })
    .await
    .unwrap();
    Comment::create(attrs! {
        commentable_id: p1.id,
        commentable_type: "post",
        body: "c1",
    })
    .await
    .unwrap();
    Comment::create(attrs! {
        commentable_id: p1.id,
        commentable_type: "post",
        body: "c2",
    })
    .await
    .unwrap();

    let users = User::with(["posts.comments"]).get().await.unwrap();
    let loaded = users
        .iter()
        .find(|x| x.id == u.id)
        .expect("user must surface");
    let posts = loaded.posts_loaded();
    assert_eq!(posts.len(), 2);

    let p1_loaded = posts
        .iter()
        .find(|p| p.id == p1.id)
        .expect("p1 must be among eager-loaded posts");
    let p2_loaded = posts
        .iter()
        .find(|p| p.id == p2.id)
        .expect("p2 must be among eager-loaded posts");

    assert_eq!(p1_loaded.comments_loaded().len(), 2);
    assert_eq!(p2_loaded.comments_loaded().len(), 0);
}

// ---- `with_count(["posts"])` -------------------------------------------

#[tokio::test]
async fn with_count_posts_returns_server_side_count() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    let u1 = make_user("count_alice").await;
    let u2 = make_user("count_bob").await;
    for _ in 0..3 {
        Post::create(attrs! {
            title: "p",
            body: "...",
            is_public: true,
            author_id: u1.id,
        })
        .await
        .unwrap();
    }
    Post::create(attrs! {
        title: "single",
        body: "...",
        is_public: false,
        author_id: u2.id,
    })
    .await
    .unwrap();

    let users = User::with_count(["posts"]).get().await.unwrap();
    let alice = users
        .iter()
        .find(|x| x.id == u1.id)
        .expect("alice must surface");
    let bob = users
        .iter()
        .find(|x| x.id == u2.id)
        .expect("bob must surface");
    assert_eq!(alice.posts_count(), 3);
    assert_eq!(bob.posts_count(), 1);
}

// ---- `with_sum` / `with_min` aggregates over User.posts ----------------
//
// Closes the Phase 10B "exercise every relation kind end-to-end" gap on
// the aggregate eager-load surface (`with_sum` / `with_avg` / `with_min` /
// `with_max`). Framework tests already cover the four kinds and the
// Sum/Avg-vs-Min/Max storage-type split; here we just prove the static
// `User::with_sum` / `User::with_min` entry points work against the live
// app schema and that `__eager.get_aggregate::<T>("<rel>_<kind>_<col>")` reads back the
// right type per kind (`f64` for Sum, `Option<f64>` for Min).

#[tokio::test]
async fn with_sum_posts_id_returns_sum_of_ids() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    let u = make_user("agg_sum").await;
    let p1 = Post::create(attrs! {
        title: "p1",
        body: "...",
        is_public: true,
        author_id: u.id,
    })
    .await
    .unwrap();
    let p2 = Post::create(attrs! {
        title: "p2",
        body: "...",
        is_public: true,
        author_id: u.id,
    })
    .await
    .unwrap();

    let users = User::with_sum(("posts", "id")).get().await.unwrap();
    let loaded = users
        .iter()
        .find(|x| x.id == u.id)
        .expect("user must surface");
    let sum: &f64 = loaded
        .__eager
        .get_aggregate::<f64>("posts_sum_id")
        .expect("sum cache populated under <rel>_<kind>_<col>");
    let expected = (p1.id + p2.id) as f64;
    assert!(
        (sum - expected).abs() < 0.001,
        "sum(posts.id) must equal p1.id + p2.id, got {sum} vs {expected}",
    );
}

#[tokio::test]
async fn with_min_posts_id_returns_smallest() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    let u = make_user("agg_min").await;
    let p1 = Post::create(attrs! {
        title: "p1",
        body: "...",
        is_public: true,
        author_id: u.id,
    })
    .await
    .unwrap();
    let _ = Post::create(attrs! {
        title: "p2",
        body: "...",
        is_public: true,
        author_id: u.id,
    })
    .await
    .unwrap();

    let users = User::with_min(("posts", "id")).get().await.unwrap();
    let loaded = users
        .iter()
        .find(|x| x.id == u.id)
        .expect("user must surface");
    // Min stores as Option<f64> — None on empty group; here the group
    // is non-empty so the smallest of {p1.id, p2.id} must round-trip.
    let min: &Option<f64> = loaded
        .__eager
        .get_aggregate::<Option<f64>>("posts_min_id")
        .expect("min cache populated under <rel>_<kind>_<col>");
    assert_eq!(*min, Some(p1.id as f64), "min(posts.id) == p1.id");
}

// ---- HasManyThrough: country -> users -> posts ------------------------
//
// Closes the Phase 10B "every relation kind end-to-end" gap on Through
// relations. The app's real schema has no natural three-table chain
// (User -> Post is two tables; Comment is polymorphic), so we declare
// an inline test-only schema the same way the framework's Through
// tests do — `TestDatabase::sqlite_memory()` + raw `execute_unprepared`
// table creation. This is just a smoke test against the macro-emitted
// dispatcher arms in an app-binary context. Exhaustive semantics
// (custom keys, GROUP BY aggregates, eager distribution, String-PK
// regression) live in `framework/tests/eloquent_relations_through.rs`.

#[model(table = "dogfood_th_countries", relations = {
    posts: HasManyThrough<DogfoodThUser, DogfoodThPost>,
})]
pub struct DogfoodThCountry {
    pub id: i64,
    pub name: String,
}

#[model(table = "dogfood_th_users")]
pub struct DogfoodThUser {
    pub id: i64,
    pub dogfood_th_country_id: i64,
    pub name: String,
}

#[model(table = "dogfood_th_posts")]
pub struct DogfoodThPost {
    pub id: i64,
    pub dogfood_th_user_id: i64,
    pub title: String,
}

#[tokio::test]
async fn has_many_through_dogfood_smoke() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE dogfood_th_countries (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            name TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE dogfood_th_users (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            dogfood_th_country_id INTEGER NOT NULL, \
            name TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE dogfood_th_posts (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            dogfood_th_user_id INTEGER NOT NULL, \
            title TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();

    let c = DogfoodThCountry::create(attrs! { name: "USA" })
        .await
        .unwrap();
    let u1 = DogfoodThUser::create(attrs! { dogfood_th_country_id: c.id, name: "u1" })
        .await
        .unwrap();
    let u2 = DogfoodThUser::create(attrs! { dogfood_th_country_id: c.id, name: "u2" })
        .await
        .unwrap();
    let _ = DogfoodThPost::create(attrs! { dogfood_th_user_id: u1.id, title: "p1" })
        .await
        .unwrap();
    let _ = DogfoodThPost::create(attrs! { dogfood_th_user_id: u2.id, title: "p2" })
        .await
        .unwrap();
    let _ = DogfoodThPost::create(attrs! { dogfood_th_user_id: u2.id, title: "p3" })
        .await
        .unwrap();

    // Isolation: a second country with its own user + post must not
    // leak into c's grandchildren.
    let c2 = DogfoodThCountry::create(attrs! { name: "CAN" })
        .await
        .unwrap();
    let u3 = DogfoodThUser::create(attrs! { dogfood_th_country_id: c2.id, name: "u3" })
        .await
        .unwrap();
    let _ = DogfoodThPost::create(attrs! { dogfood_th_user_id: u3.id, title: "ca-p" })
        .await
        .unwrap();

    let posts = c.posts().get().await.unwrap();
    assert_eq!(
        posts.len(),
        3,
        "country must see exactly its 3 grandchild posts via JOIN",
    );
    let titles: Vec<&str> = posts.iter().map(|p| p.title.as_str()).collect();
    assert!(titles.contains(&"p1"));
    assert!(titles.contains(&"p2"));
    assert!(titles.contains(&"p3"));
    assert!(
        !titles.contains(&"ca-p"),
        "WHERE first_key = ? must isolate parents",
    );

    let n = c.posts().count().await.unwrap();
    assert_eq!(n, 3);
}
