//! Phase 10B T9 — eager-load orchestrator end-to-end.
//!
//! Pins the full eager-load surface the user calls:
//!
//! - `with` (flat + nested dotted paths)
//! - `with_count` / `with_sum` / `with_avg` / `with_min` / `with_max`
//! - `with_where` (typed predicate against the relation builder)
//! - `Collection::load` / `Collection::load_missing`
//!
//! Storage contract:
//!
//! - `with_count`: read via `<rel>_count() -> u64` (cache key =
//!   relation NAME).
//! - `with_sum` / `with_avg`: read via
//!   `__eager.get_aggregate::<f64>("<rel>_<kind>_<col>")` (Sum/Avg
//!   over zero rows land as `0.0`).
//! - `with_min` / `with_max`: read via
//!   `__eager.get_aggregate::<Option<f64>>("<rel>_<kind>_<col>")`
//!   (None on empty group).
//!
//! Aggregate cache keys are the wide `<rel>_<kind>_<col>` form (e.g.
//! `"posts_sum_views"`) — built by
//! `eloquent::relations::aggregate_cache_key`. Multiple aggregates on
//! the same relation (e.g. `with_sum` then `with_avg` over the same
//! column) coexist on the same row without colliding on the cache
//! cell. The macro also emits per-kind-per-col accessors —
//! `<rel>_sum_of(col)` / `_avg_of` / `_min_of` / `_max_of` — which
//! read using the same helper.

use suprnova::testing::TestDatabase;
use suprnova::{Builder, Collection, Model, attrs, model};

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
    let _ = EgComment::create(attrs! { eg_post_id: p1.id, eg_user_id: u1.id, body: "c1-p1" })
        .await
        .unwrap();
    let _ = EgComment::create(attrs! { eg_post_id: p1.id, eg_user_id: u1.id, body: "c2-p1" })
        .await
        .unwrap();
    let _ = EgComment::create(attrs! { eg_post_id: p2.id, eg_user_id: u1.id, body: "c1-p2" })
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
    let users = EgUser::with(["posts.comments.author"]).get().await.unwrap();
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
        .get_aggregate::<f64>("posts_sum_views")
        .expect("sum cache populated under <rel>_<kind>_<col>");
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
        .get_aggregate::<f64>("posts_avg_views")
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
        .get_aggregate::<Option<f64>>("posts_min_views")
        .expect("min cache populated");
    assert!((min_opt.unwrap() - 5.0).abs() < 0.001);

    let users_max = EgUser::with_max(("posts", "views")).get().await.unwrap();
    let u1_max = users_max.iter().find(|u| u.name == "u1").unwrap();
    let max_opt: Option<f64> = *u1_max
        .__eager
        .get_aggregate::<Option<f64>>("posts_max_views")
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
    let mut users = EgUser::all().await.unwrap();
    users.load(["posts"]).await.unwrap();
    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    assert_eq!(u1.posts_loaded().len(), 2);
}

#[tokio::test]
async fn load_missing_idempotent_when_all_have_relation() {
    // First fetch with `with(["posts"])` populates the cache on every
    // row. The subsequent `load_missing(["posts"])` should be a no-op
    // (every row's partition lands in the already-loaded bucket).
    //
    // We can't intercept the SQL from this test, so the assertion is
    // semantic: calling twice doesn't break anything AND the loaded
    // count stays correct.
    let _db = fixture().await;
    let mut users = EgUser::with(["posts"]).get().await.unwrap();
    users.load_missing(["posts"]).await.unwrap();
    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    assert_eq!(u1.posts_loaded().len(), 2);
}

#[tokio::test]
async fn load_missing_per_row_loads_only_uncached_rows() {
    // Mixed-state collection: u1 was fetched with `with(["posts"])`,
    // u2 was fetched plain. After `load_missing(["posts"])`, BOTH
    // rows must have posts cached — the per-row partition loads
    // posts on u2 only, u1 stays untouched.
    //
    // Pre-P3 (collection-wide skip): the call would silently no-op
    // because u1 already had posts cached, leaving u2's
    // `posts_loaded()` panic-on-read.
    let _db = fixture().await;
    let u1_with = EgUser::query()
        .filter("name", "u1")
        .with(["posts"])
        .get()
        .await
        .unwrap();
    let u2_plain = EgUser::query().filter("name", "u2").get().await.unwrap();
    let mut combined: Vec<EgUser> = u1_with.into_vec();
    combined.extend(u2_plain.into_vec());
    let mut users: Collection<EgUser> = Collection::from(combined);

    // Sanity: u2 currently lacks posts in its cache.
    {
        let u2 = users.iter().find(|u| u.name == "u2").unwrap();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| u2.posts_loaded()));
        assert!(res.is_err(), "u2 should not have posts cached yet");
    }

    users.load_missing(["posts"]).await.unwrap();

    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    assert_eq!(u1.posts_loaded().len(), 2, "u1 stays at 2 posts");
    let u2 = users.iter().find(|u| u.name == "u2").unwrap();
    assert_eq!(u2.posts_loaded().len(), 1, "u2 now has its 1 post");
}

#[tokio::test]
async fn load_missing_nested_partitions_at_every_level() {
    // Mixed-state nested partition: u1 has posts cached AND comments
    // cached on p1 only; u2 has nothing. After
    // `load_missing(["posts.comments"])`:
    //   - u1: posts stay (already cached); comments fill on p2 only
    //     (p1's are already cached, p2's are not).
    //   - u2: full path loads (posts AND their comments).
    //
    // Pre-P3 (with the macro-arm any-row skip on children_vec): the
    // recursion into u1's cached posts would notice p1 has comments
    // cached and silently skip the bulk-load for p2 too.
    let _db = fixture().await;

    // u1 with posts only.
    let u1_with_posts = EgUser::query()
        .filter("name", "u1")
        .with(["posts"])
        .get()
        .await
        .unwrap();
    assert_eq!(u1_with_posts.len(), 1);

    // Now hand-cache comments on u1's first post only. We do this by
    // querying u1 fresh through the relation chain — fetching with
    // `posts.comments` then surgically clearing the comments cache on
    // p2 isn't possible without poking internals. The cleanest
    // route: fetch a separate u1 with `posts.comments` so we know the
    // expected total (3), then assert on a different mixed-state
    // collection. For the partition assertion proper we fetch u1
    // again with posts-only and load p1's comments via __eager_load
    // directly so p2's stays empty.
    let mut combined: Vec<EgUser> = u1_with_posts.into_vec();
    // u2 plain (no posts).
    let u2_plain = EgUser::query().filter("name", "u2").get().await.unwrap();
    combined.extend(u2_plain);

    // Cache comments on u1's p1 specifically. Reach through the
    // model's eager-load dispatcher: collect a Vec<&mut EgPost>
    // pointing at p1 only and call EgPost::__eager_load("comments", ...).
    let db = suprnova::DB::connection().unwrap();
    {
        let u1 = combined.iter_mut().find(|u| u.name == "u1").unwrap();
        // SAFETY: get_many_mut returns the cached children vec; this
        // is the cleanest read path the framework exposes.
        let posts: &mut Vec<EgPost> = u1
            .__eager
            .get_many_mut::<EgPost>("posts")
            .expect("posts cached on u1");
        let p1_refs: Vec<&mut EgPost> = posts.iter_mut().filter(|p| p.title == "p1").collect();
        let mut p1_refs = p1_refs;
        EgPost::__eager_load("comments", p1_refs.as_mut_slice(), db.inner(), None)
            .await
            .unwrap();
    }

    // Sanity: p2 has no comments cached, p1 does.
    {
        let u1 = combined.iter().find(|u| u.name == "u1").unwrap();
        let posts = u1.posts_loaded();
        let p1 = posts.iter().find(|p| p.title == "p1").unwrap();
        let p2 = posts.iter().find(|p| p.title == "p2").unwrap();
        assert_eq!(p1.comments_loaded().len(), 2);
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p2.comments_loaded()));
        assert!(res.is_err(), "p2 comments should not be cached yet");
    }

    let mut users: Collection<EgUser> = Collection::from(combined);
    users.load_missing(["posts.comments"]).await.unwrap();

    // u1: p1 still 2 comments, p2 now 1 comment.
    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    let u1_posts = u1.posts_loaded();
    let p1 = u1_posts.iter().find(|p| p.title == "p1").unwrap();
    let p2 = u1_posts.iter().find(|p| p.title == "p2").unwrap();
    assert_eq!(p1.comments_loaded().len(), 2, "p1 untouched");
    assert_eq!(p2.comments_loaded().len(), 1, "p2 filled by partition");

    // u2: full path loaded. Has 1 post with 0 comments.
    let u2 = users.iter().find(|u| u.name == "u2").unwrap();
    let u2_posts = u2.posts_loaded();
    assert_eq!(u2_posts.len(), 1);
    assert_eq!(u2_posts[0].comments_loaded().len(), 0);
}

#[tokio::test]
async fn load_loads_when_no_row_has_it() {
    // load_missing only skips when AT LEAST one row already has the
    // relation cached. A fresh collection with no cache hits gets
    // the full eager-load treatment.
    let _db = fixture().await;
    let mut users = EgUser::all().await.unwrap();
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
    let mut users = EgUser::with(["posts"]).get().await.unwrap();

    // Posts are loaded but comments aren't — calling
    // `comments_loaded()` on any loaded post must panic.
    {
        let u1 = users.iter().find(|u| u.name == "u1").unwrap();
        let p = u1.posts_loaded().first().expect("posts loaded");
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.comments_loaded()));
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
    let mut users = EgUser::all().await.unwrap();
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
async fn with_where_typed_method_infers_closure_target() {
    // P4: the macro emits `<Self>::with_where_<rel>(closure)` so the
    // closure's parameter type is inferred from the method signature —
    // users no longer need to spell out `Builder<EgPost>` on the
    // closure param.
    let _db = fixture().await;
    let users = EgUser::with_where_posts(|q| q.filter("views", 10i64))
        .get()
        .await
        .unwrap();
    let u1 = users.iter().find(|u| u.name == "u1").unwrap();
    let posts = u1.posts_loaded();
    assert_eq!(posts.len(), 1, "only views=10 posts should be loaded");
    assert!(posts.iter().all(|p| p.views == 10));
    let u2 = users.iter().find(|u| u.name == "u2").unwrap();
    assert_eq!(u2.posts_loaded().len(), 0);
}

#[tokio::test]
async fn with_where_typed_method_then_chain_filter() {
    // P4: the static helper returns a `Builder<Self>` so users can
    // chain further base-query filters/sorts after the eager-load
    // predicate. The closure parameter type is still inferred.
    let _db = fixture().await;
    let users = EgUser::with_where_posts(|q| q.filter("views", 10i64))
        .filter("name", "u1")
        .get()
        .await
        .unwrap();
    assert_eq!(users.len(), 1);
    let u1 = &users[0];
    assert_eq!(u1.name, "u1");
    assert!(u1.posts_loaded().iter().all(|p| p.views == 10));
}

#[tokio::test]
async fn static_helpers_match_query_chain() {
    // The macro emits static `Self::with_count` / `Self::with_sum`
    // helpers — they should be equivalent to `Self::query().with_count(...)`.
    let _db = fixture().await;
    let via_static = EgUser::with_count(["posts"]).get().await.unwrap();
    let via_chain = EgUser::query().with_count(["posts"]).get().await.unwrap();
    let s = via_static.iter().find(|u| u.name == "u1").unwrap();
    let c = via_chain.iter().find(|u| u.name == "u1").unwrap();
    assert_eq!(s.posts_count(), c.posts_count());
}

#[tokio::test]
async fn with_where_closure_type_mismatch_is_loud() {
    // `posts` is declared `HasMany<EgPost>` on `EgUser`, but the user
    // wrote `Builder<EgComment>` in the closure parameter. The macro
    // boxes the closure type-erased, and the per-relation dispatcher
    // arm downcasts to the statically-known `Builder<EgPost>`. The
    // wrong-typed box must NOT silently drop the predicate (which
    // would run an unfiltered IN-query against `eg_posts`) — it must
    // return a loud `FrameworkError` naming the relation and the
    // expected target type.
    let _db = fixture().await;
    let result = EgUser::query()
        .with_where((
            "posts",
            // Wrong target type: EgComment instead of EgPost. The
            // filter column would be meaningless for `eg_posts` either
            // way; the test exists to prove the dispatcher errors
            // BEFORE the IN-query lands, not silently corrupts.
            |q: Builder<EgComment>| q.filter("body", "anything"),
        ))
        .get()
        .await;
    let err = result.expect_err("with_where closure type mismatch should error");
    let msg = err.to_string();
    assert!(
        msg.contains("with_where(`posts`"),
        "error should name the relation: {msg}"
    );
    assert!(
        msg.contains("type mismatch"),
        "error should describe the failure mode: {msg}"
    );
    assert!(
        msg.contains("Builder<"),
        "error should name the expected typed Builder: {msg}"
    );
}
