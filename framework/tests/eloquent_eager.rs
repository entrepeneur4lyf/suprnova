//! Phase 10B T9 — eager-load orchestrator end-to-end.
//!
//! Pins the full eager-load surface the user calls:
//!
//! - `with` (flat + nested dotted paths)
//! - `with_count` / `with_sum` / `with_avg` / `with_min` / `with_max`
//! - `with_where` (typed predicate against the relation builder)
//! - `Collection::load` / `Collection::load_missing`
//!
//! Storage contract (cache key = relation NAME):
//!
//! - `with_count`: read via `<rel>_count() -> u64`
//! - `with_sum` / `with_avg`: read via
//!   `__eager.get_aggregate::<f64>(name)` (Sum/Avg over zero rows
//!   land as `0.0`)
//! - `with_min` / `with_max`: read via
//!   `__eager.get_aggregate::<Option<f64>>(name)` (None on empty group)
//!
//! The cache key is the relation name (`"posts"`), NOT
//! `"posts_sum_views"`. Loading multiple aggregates on the same
//! relation overwrites the cell — issue separate queries if you need
//! both `posts_sum` and `posts_avg` on the same row. This matches the
//! contract laid down in the dispatcher arm comments (eg.
//! `__aggregate_relation` HasMany arm).

use suprnova::testing::TestDatabase;
use suprnova::{attrs, model, Builder, Collection, Model};

#[model(table = "eg_users", relations = {
    posts: HasMany<EgPost>,
    profile: HasOne<EgProfile>,
})]
pub struct EgUser {
    pub id: i64,
    pub name: String,
}

#[model(table = "eg_posts", relations = {
    comments: HasMany<EgComment>,
    user: BelongsTo<EgUser>,
})]
pub struct EgPost {
    pub id: i64,
    pub eg_user_id: i64,
    pub title: String,
    pub views: i64,
}

#[model(table = "eg_comments", relations = {
    author: BelongsTo<EgUser>,
})]
pub struct EgComment {
    pub id: i64,
    pub eg_post_id: i64,
    pub eg_user_id: i64,
    pub body: String,
}

#[model(table = "eg_profiles")]
pub struct EgProfile {
    pub id: i64,
    pub eg_user_id: i64,
    pub bio: String,
}

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE eg_users (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE eg_posts (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         eg_user_id INTEGER NOT NULL, title TEXT NOT NULL, \
         views INTEGER NOT NULL DEFAULT 0)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE eg_comments (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         eg_post_id INTEGER NOT NULL, eg_user_id INTEGER NOT NULL, \
         body TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE eg_profiles (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         eg_user_id INTEGER NOT NULL, bio TEXT NOT NULL)",
    )
    .await
    .unwrap();
}

async fn fixture() -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&db).await;
    // Two users — u1 (with two posts and a profile), u2 (one post + a
    // profile). All three posts have at least one comment authored by
    // the corresponding owner so nested `posts.comments.author` paths
    // resolve cleanly.
    let u1 = EgUser::create(attrs! { name: "u1" }).await.unwrap();
    let u2 = EgUser::create(attrs! { name: "u2" }).await.unwrap();
    let _ = EgProfile::create(attrs! { eg_user_id: u1.id, bio: "u1bio" })
        .await
        .unwrap();
    let _ = EgProfile::create(attrs! { eg_user_id: u2.id, bio: "u2bio" })
        .await
        .unwrap();
    let p1 = EgPost::create(attrs! { eg_user_id: u1.id, title: "p1", views: 10i64 })
        .await
        .unwrap();
    let p2 = EgPost::create(attrs! { eg_user_id: u1.id, title: "p2", views: 5i64 })
        .await
        .unwrap();
    let _p3 = EgPost::create(attrs! { eg_user_id: u2.id, title: "p3", views: 20i64 })
        .await
        .unwrap();
    let _ = EgComment::create(
        attrs! { eg_post_id: p1.id, eg_user_id: u1.id, body: "c1-p1" },
    )
    .await
    .unwrap();
    let _ = EgComment::create(
        attrs! { eg_post_id: p1.id, eg_user_id: u1.id, body: "c2-p1" },
    )
    .await
    .unwrap();
    let _ = EgComment::create(
        attrs! { eg_post_id: p2.id, eg_user_id: u1.id, body: "c1-p2" },
    )
    .await
    .unwrap();
    db
}

#[tokio::test]
async fn with_single_relation() {
    let _db = fixture().await;
    let users = EgUser::with(["posts"]).get().await.unwrap();
    assert_eq!(users.len(), 2);
    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    assert_eq!(u1.posts_loaded().len(), 2);
}

#[tokio::test]
async fn with_multiple_relations() {
    let _db = fixture().await;
    let users = EgUser::with(["posts", "profile"]).get().await.unwrap();
    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    assert_eq!(u1.posts_loaded().len(), 2);
    assert!(u1.profile_loaded().is_some());
}

#[tokio::test]
async fn with_nested_path() {
    // Dotted "posts.comments" loads users -> posts -> comments in
    // exactly three queries (zero N+1).
    let _db = fixture().await;
    let users = EgUser::with(["posts.comments"]).get().await.unwrap();
    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    let posts = u1.posts_loaded();
    let p1 = posts.iter().find(|p| p.title == "p1").unwrap();
    assert_eq!(p1.comments_loaded().len(), 2);
    let p2 = posts.iter().find(|p| p.title == "p2").unwrap();
    assert_eq!(p2.comments_loaded().len(), 1);
}

#[tokio::test]
async fn nested_three_levels() {
    // users -> posts -> comments -> author. Validates the recursion
    // peels one segment per step.
    let _db = fixture().await;
    let users = EgUser::with(["posts.comments.author"])
        .get()
        .await
        .unwrap();
    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    let total_comments: usize = u1
        .posts_loaded()
        .iter()
        .map(|p| p.comments_loaded().len())
        .sum();
    assert_eq!(total_comments, 3); // 2 (p1) + 1 (p2)
    // Each comment's author was loaded — BelongsTo single-value
    // relation. The author IS u1 for all three; the test just
    // confirms the BelongsTo recursion stored a Some(_) on each.
    let mut author_loaded_count = 0;
    for p in u1.posts_loaded() {
        for c in p.comments_loaded() {
            if c.author_loaded().is_some() {
                author_loaded_count += 1;
            }
        }
    }
    assert_eq!(author_loaded_count, 3);
}

#[tokio::test]
async fn with_count() {
    let _db = fixture().await;
    let users = EgUser::with_count(["posts"]).get().await.unwrap();
    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    assert_eq!(u1.posts_count(), 2);
    let u2 = users.iter().find(|u| u.name == "u2").unwrap();
    assert_eq!(u2.posts_count(), 1);
}

#[tokio::test]
async fn with_sum_aggregate() {
    // Sum over views: u1 has p1(10) + p2(5) = 15 — but storage shape
    // is f64 regardless of column type (the dispatcher arm coerces
    // INTEGER sums to f64 via `try_get::<i64>().map(|n| n as f64)`).
    let _db = fixture().await;
    let users = EgUser::with_sum(("posts", "views")).get().await.unwrap();
    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    let sum: f64 = *u1
        .__eager
        .get_aggregate::<f64>("posts")
        .expect("sum cache populated under the relation name");
    assert!((sum - 15.0).abs() < 0.001, "got sum = {sum}");
}

#[tokio::test]
async fn with_avg_aggregate() {
    // Avg over views: u1 has (10 + 5) / 2 = 7.5.
    let _db = fixture().await;
    let users = EgUser::with_avg(("posts", "views")).get().await.unwrap();
    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    let avg: f64 = *u1
        .__eager
        .get_aggregate::<f64>("posts")
        .expect("avg cache populated");
    assert!((avg - 7.5).abs() < 0.01, "got avg = {avg}");
}

#[tokio::test]
async fn with_min_max() {
    // Min over views: u1 has min(10, 5) = 5; max(10, 5) = 10.
    let _db = fixture().await;
    let users = EgUser::with_min(("posts", "views")).get().await.unwrap();
    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    let min_opt: Option<f64> = *u1
        .__eager
        .get_aggregate::<Option<f64>>("posts")
        .expect("min cache populated");
    assert!((min_opt.unwrap() - 5.0).abs() < 0.001);

    let users_max = EgUser::with_max(("posts", "views"))
        .get()
        .await
        .unwrap();
    let u1_max = users_max.iter().find(|u| u.name == "u1").unwrap();
    let max_opt: Option<f64> = *u1_max
        .__eager
        .get_aggregate::<Option<f64>>("posts")
        .expect("max cache populated");
    assert!((max_opt.unwrap() - 10.0).abs() < 0.001);
}

#[tokio::test]
async fn with_where_filters_loaded_relation() {
    // The predicate is applied to the inner Builder<EgPost> before
    // the IN-query lands — only views=10 posts reach the cache.
    let _db = fixture().await;
    let users = EgUser::query()
        .with_where(("posts", |q: Builder<EgPost>| q.filter("views", 10i64)))
        .get()
        .await
        .unwrap();
    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    let posts = u1.posts_loaded();
    assert_eq!(posts.len(), 1, "only views=10 posts should be loaded");
    assert_eq!(posts[0].views, 10);
    // u2's single post has views=20 — predicate filters it out.
    let u2 = users.iter().find(|u| u.name == "u2").unwrap();
    assert_eq!(u2.posts_loaded().len(), 0);
}

#[tokio::test]
async fn load_after_fetch() {
    // Plain fetch, then a follow-up load() populates the cache.
    // Collection wraps Vec<M>; the load method delegates to the same
    // orchestrator Builder::get uses.
    let _db = fixture().await;
    let users_vec = EgUser::all().await.unwrap();
    let mut users: Collection<EgUser> = Collection::from(users_vec);
    users.load(["posts"]).await.unwrap();
    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    assert_eq!(u1.posts_loaded().len(), 2);
}

#[tokio::test]
async fn load_missing_skips_already_loaded() {
    // First fetch with `with(["posts"])` populates the cache. The
    // subsequent `load_missing(["posts"])` should be a no-op (the v1
    // collection-wide skip — Laravel's per-row skip is v2).
    //
    // We can't intercept the SQL from this test, so the assertion is
    // semantic: calling twice doesn't break anything AND the loaded
    // count stays correct.
    let _db = fixture().await;
    let users_vec = EgUser::with(["posts"]).get().await.unwrap();
    let mut users: Collection<EgUser> = Collection::from(users_vec);
    users.load_missing(["posts"]).await.unwrap();
    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    assert_eq!(u1.posts_loaded().len(), 2);
}

#[tokio::test]
async fn load_loads_when_no_row_has_it() {
    // load_missing only skips when AT LEAST one row already has the
    // relation cached. A fresh collection with no cache hits gets
    // the full eager-load treatment.
    let _db = fixture().await;
    let users_vec = EgUser::all().await.unwrap();
    let mut users: Collection<EgUser> = Collection::from(users_vec);
    users.load_missing(["posts"]).await.unwrap();
    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    assert_eq!(u1.posts_loaded().len(), 2);
}

#[tokio::test]
async fn load_missing_recurses_into_loaded_head_to_fill_tail() {
    // After `with(["posts"])`, calling `load_missing(["posts.comments"])`
    // must skip the (cached) head bulk-load but still drive the tail
    // load on each cached post — the previous flat-skip behaviour
    // silently dropped the comments load and left `comments_loaded()`
    // panic-on-read.
    let _db = fixture().await;
    let users_vec = EgUser::with(["posts"]).get().await.unwrap();
    let mut users: Collection<EgUser> = Collection::from(users_vec);

    // Posts are loaded but comments aren't — calling
    // `comments_loaded()` on any loaded post must panic.
    {
        let u1 = users.iter().find(|u| u.name == "u1").unwrap();
        let p = u1.posts_loaded().first().expect("posts loaded");
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            p.comments_loaded()
        }));
        assert!(res.is_err(), "comments should not be loaded yet");
    }

    users.load_missing(["posts.comments"]).await.unwrap();

    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    let total: usize = u1
        .posts_loaded()
        .iter()
        .map(|p| p.comments_loaded().len())
        .sum();
    // u1 has p1 (2 comments) + p2 (1 comment) = 3.
    assert_eq!(total, 3, "tail loaded via recursion into cached head");
}

#[tokio::test]
async fn load_missing_dotted_no_head_loads_full_path() {
    // Counterpart to the recursion test: when nothing is cached yet,
    // `load_missing(["posts.comments"])` falls through to the regular
    // full-path eager loader.
    let _db = fixture().await;
    let users_vec = EgUser::all().await.unwrap();
    let mut users: Collection<EgUser> = Collection::from(users_vec);
    users.load_missing(["posts.comments"]).await.unwrap();
    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    let total: usize = u1
        .posts_loaded()
        .iter()
        .map(|p| p.comments_loaded().len())
        .sum();
    assert_eq!(total, 3);
}

#[tokio::test]
async fn with_where_on_belongs_to_filters_loaded_parent() {
    // The closure runs against `Builder<EgUser>` because `user` is a
    // BelongsTo<EgUser> on EgPost. Predicate matches `name = "u1"` —
    // posts whose user is u2 should resolve to a None parent.
    let _db = fixture().await;
    let posts = EgPost::query()
        .with_where(("user", |q: Builder<EgUser>| q.filter("name", "u1")))
        .get()
        .await
        .unwrap();
    for p in &posts {
        if p.title == "p3" {
            // u2 owns p3; the filter excludes u2, so the loaded
            // parent collapses to None.
            assert!(p.user_loaded().is_none());
        } else {
            // p1 / p2 are u1's; predicate keeps them.
            assert!(p.user_loaded().is_some());
            assert_eq!(p.user_loaded().unwrap().name, "u1");
        }
    }
}

#[tokio::test]
async fn static_helpers_match_query_chain() {
    // The macro emits static `Self::with_count` / `Self::with_sum`
    // helpers — they should be equivalent to `Self::query().with_count(...)`.
    let _db = fixture().await;
    let via_static = EgUser::with_count(["posts"]).get().await.unwrap();
    let via_chain = EgUser::query()
        .with_count(["posts"])
        .get()
        .await
        .unwrap();
    let s = via_static.iter().find(|u| u.name == "u1").unwrap();
    let c = via_chain.iter().find(|u| u.name == "u1").unwrap();
    assert_eq!(s.posts_count(), c.posts_count());
}
