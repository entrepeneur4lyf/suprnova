//! Phase 10B T3 — `HasMany<L, R>` + chainable builder + eager-load
//! grouping.
//!
//! T2 shipped HasOne / BelongsTo and established the patterns this
//! file exercises against the new one-to-many flavour:
//!
//! - JSON-pluck FK reading (so the macro doesn't have to know the
//!   target struct's field layout).
//! - `serde_json::Value` for parent key values flowing through the
//!   inner builder's `WhereTerm` storage.
//! - Sum/Avg vs Min/Max aggregate-cache branching (Sum/Avg store `f64`
//!   with `0.0` empty default; Min/Max store `Option<f64>` with `None`
//!   empty default).
//!
//! T3 adds the per-parent grouping the m2o flavour needs:
//!
//! - `__eager_load`: build the `HashMap<key, Vec<row>>` and stuff each
//!   parent's slice into `set_many`.
//! - `__count_relation`: `GROUP BY fk` counting, distributed via
//!   `set_count`. Parents with no children get 0.
//! - `__aggregate_relation`: real SUM/AVG/MIN/MAX over the per-parent
//!   group — NOT T2's "record the single row's column" pattern.
//!   Empty groups fall back through the Sum|Avg vs Min|Max branch.

use suprnova::testing::TestDatabase;
use suprnova::{attrs, model, AggregateKind, Direction, Model};

#[model(table = "otm_users", relations = {
    posts: HasMany<OtmPost>,
})]
pub struct OtmUser {
    pub id: i64,
    pub name: String,
}

// `created_at` / `updated_at` are user-declared (not auto-injected by
// the `timestamps` flag) so the `latest()` / `oldest()` aliases on
// `HasMany` have a column to resolve against in tests. The macro
// recognises both names and treats them as timestamp columns.
#[model(table = "otm_posts")]
pub struct OtmPost {
    pub id: i64,
    pub otm_user_id: i64,
    pub title: String,
    pub views: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE otm_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE otm_posts (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         otm_user_id INTEGER NOT NULL, title TEXT NOT NULL, views INTEGER NOT NULL DEFAULT 0, \
         created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn has_many_get_returns_all_related() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtmUser::create(attrs! { name: "Alice" }).await.unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: u.id, title: "p1" })
        .await
        .unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: u.id, title: "p2" })
        .await
        .unwrap();

    let posts = u.posts().get().await.unwrap();
    assert_eq!(posts.len(), 2);
}

#[tokio::test]
async fn has_many_chainable_filter_order_limit() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtmUser::create(attrs! { name: "Bob" }).await.unwrap();
    for i in 1..=5 {
        let _ = OtmPost::create(attrs! { otm_user_id: u.id, title: format!("p{i}") })
            .await
            .unwrap();
    }

    let top_two = u
        .posts()
        .order_by("id", Direction::Desc)
        .limit(2)
        .get()
        .await
        .unwrap();
    assert_eq!(top_two.len(), 2);
    assert_eq!(top_two[0].title, "p5");
    assert_eq!(top_two[1].title, "p4");
}

#[tokio::test]
async fn has_many_chainable_db_where_alias() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtmUser::create(attrs! { name: "Eli" }).await.unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: u.id, title: "alpha" })
        .await
        .unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: u.id, title: "beta" })
        .await
        .unwrap();

    let only_alpha = u.posts().db_where("title", "alpha").get().await.unwrap();
    assert_eq!(only_alpha.len(), 1);
    assert_eq!(only_alpha[0].title, "alpha");
}

#[tokio::test]
async fn has_many_take_alias() {
    // `.take(n)` is the Laravel-shape alias for `.limit(n)`.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtmUser::create(attrs! { name: "Fae" }).await.unwrap();
    for i in 1..=3 {
        let _ = OtmPost::create(attrs! { otm_user_id: u.id, title: format!("p{i}") })
            .await
            .unwrap();
    }
    let taken = u
        .posts()
        .order_by("id", Direction::Asc)
        .take(2)
        .get()
        .await
        .unwrap();
    assert_eq!(taken.len(), 2);
    assert_eq!(taken[0].title, "p1");
}

#[tokio::test]
async fn has_many_first_returns_one() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtmUser::create(attrs! { name: "Carol" }).await.unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: u.id, title: "only" })
        .await
        .unwrap();

    let post = u
        .posts()
        .first()
        .await
        .unwrap()
        .expect("post present");
    assert_eq!(post.title, "only");
}

#[tokio::test]
async fn has_many_count() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtmUser::create(attrs! { name: "Dan" }).await.unwrap();
    for i in 1..=3 {
        let _ = OtmPost::create(attrs! { otm_user_id: u.id, title: format!("p{i}") })
            .await
            .unwrap();
    }
    assert_eq!(u.posts().count().await.unwrap(), 3);
}

#[tokio::test]
async fn has_many_returns_empty_for_unrelated_parent() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtmUser::create(attrs! { name: "Eve" }).await.unwrap();
    let posts = u.posts().get().await.unwrap();
    assert!(posts.is_empty());
}

// Custom FK / LK overrides — pin the macro option plumbing for HasMany
// the same way `has_one_custom_fk_lk_resolves` does for HasOne.
#[model(table = "otm_owners", relations = {
    posts: HasMany<OtmPost> { fk = "otm_user_id", lk = "id" },
})]
pub struct OtmOwner {
    pub id: i64,
    pub name: String,
}

#[tokio::test]
async fn has_many_custom_keys_resolve() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    _db.execute_unprepared(
        "CREATE TABLE otm_owners (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    let owner = OtmOwner::create(attrs! { name: "ZZ" }).await.unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: owner.id, title: "from-owner" })
        .await
        .unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: owner.id, title: "from-owner-2" })
        .await
        .unwrap();

    let posts = owner.posts().get().await.unwrap();
    assert_eq!(posts.len(), 2);
}

#[tokio::test]
async fn has_many_latest_orders_desc_by_created_at() {
    // `latest()` is sugar for `order_by("created_at", Desc)`. Use raw
    // SQL inserts with explicit timestamps so the test is
    // deterministic without sleeping past chrono->TEXT's 1-second
    // resolution; OtmPost::create's auto-managed timestamps would
    // stamp NOW after applying attrs, defeating the override.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtmUser::create(attrs! { name: "G" }).await.unwrap();
    _db.execute_unprepared(&format!(
        "INSERT INTO otm_posts (otm_user_id, title, views, created_at, updated_at) VALUES \
         ({uid}, 'first', 0, '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z'), \
         ({uid}, 'middle', 0, '2021-01-01T00:00:00Z', '2021-01-01T00:00:00Z'), \
         ({uid}, 'latest', 0, '2022-01-01T00:00:00Z', '2022-01-01T00:00:00Z')",
        uid = u.id,
    ))
    .await
    .unwrap();

    let by_latest = u.posts().latest().get().await.unwrap();
    assert_eq!(by_latest.len(), 3);
    assert_eq!(by_latest[0].title, "latest", "latest() must put newest first");
    assert_eq!(by_latest[1].title, "middle");
    assert_eq!(by_latest[2].title, "first");
}

#[tokio::test]
async fn has_many_oldest_orders_asc_by_created_at() {
    // Mirror of `latest()`, asserting ascending order.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtmUser::create(attrs! { name: "H" }).await.unwrap();
    _db.execute_unprepared(&format!(
        "INSERT INTO otm_posts (otm_user_id, title, views, created_at, updated_at) VALUES \
         ({uid}, 'oldest', 0, '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z'), \
         ({uid}, 'middle', 0, '2021-01-01T00:00:00Z', '2021-01-01T00:00:00Z'), \
         ({uid}, 'newest', 0, '2022-01-01T00:00:00Z', '2022-01-01T00:00:00Z')",
        uid = u.id,
    ))
    .await
    .unwrap();

    let by_oldest = u.posts().oldest().get().await.unwrap();
    assert_eq!(by_oldest.len(), 3);
    assert_eq!(by_oldest[0].title, "oldest", "oldest() must put oldest first");
    assert_eq!(by_oldest[1].title, "middle");
    assert_eq!(by_oldest[2].title, "newest");
}

#[tokio::test]
async fn has_many_order_by_desc_chains_through_wrapper() {
    // Direct `order_by` exercise — separate from latest()/oldest()
    // so a future regression in created_at handling doesn't mask a
    // regression in the explicit `order_by` chain through the wrapper.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtmUser::create(attrs! { name: "G" }).await.unwrap();
    for i in 1..=3 {
        let _ = OtmPost::create(attrs! { otm_user_id: u.id, title: format!("p{i}") })
            .await
            .unwrap();
    }
    let desc = u
        .posts()
        .order_by("id", Direction::Desc)
        .get()
        .await
        .unwrap();
    assert_eq!(desc[0].title, "p3");
    assert_eq!(desc[2].title, "p1");
}

// ---- Eager loading ------------------------------------------------------
//
// The defining behaviour of the HasMany dispatcher arm: parents and
// children are loaded in two queries, then results are grouped by FK
// and distributed into per-parent `__eager.set_many` slots.

#[tokio::test]
async fn has_many_eager_load_groups_by_parent() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u1 = OtmUser::create(attrs! { name: "u1" }).await.unwrap();
    let u2 = OtmUser::create(attrs! { name: "u2" }).await.unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: u1.id, title: "u1-a" })
        .await
        .unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: u1.id, title: "u1-b" })
        .await
        .unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: u2.id, title: "u2-only" })
        .await
        .unwrap();

    let users = OtmUser::with(["posts"]).get().await.unwrap();
    assert_eq!(users.len(), 2);
    let u1_loaded = users.iter().find(|u| u.id == u1.id).unwrap();
    assert_eq!(u1_loaded.posts_loaded().len(), 2);
    let u2_loaded = users.iter().find(|u| u.id == u2.id).unwrap();
    assert_eq!(u2_loaded.posts_loaded().len(), 1);
    assert_eq!(u2_loaded.posts_loaded()[0].title, "u2-only");
}

#[tokio::test]
async fn has_many_eager_load_empty_parent_gets_empty_slice() {
    // A parent with no children must still get an empty slice in
    // `posts_loaded()`, not a panic. The dispatcher must explicitly
    // populate the cache for every parent — otherwise the accessor's
    // "you forgot `with()`" panic would fire incorrectly.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u_with = OtmUser::create(attrs! { name: "with" }).await.unwrap();
    let u_without = OtmUser::create(attrs! { name: "without" }).await.unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: u_with.id, title: "only-child" })
        .await
        .unwrap();

    let users = OtmUser::with(["posts"]).get().await.unwrap();
    let without_loaded = users.iter().find(|u| u.id == u_without.id).unwrap();
    assert!(without_loaded.posts_loaded().is_empty());
}

#[tokio::test]
#[should_panic(expected = "was not eager-loaded")]
async fn has_many_loaded_accessor_panics_without_with() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtmUser::create(attrs! { name: "x" }).await.unwrap();
    let _ = u.posts_loaded();
}

// ---- Count dispatcher --------------------------------------------------
//
// `__count_relation` must populate `__eager.set_count` for EVERY
// parent — including those with zero children (which get 0). Mirrors
// the T2 HasOne / BelongsTo behaviour but over GROUP-BY-style counts.

#[tokio::test]
async fn has_many_count_dispatcher_distributes_counts() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u1 = OtmUser::create(attrs! { name: "u1" }).await.unwrap();
    let u2 = OtmUser::create(attrs! { name: "u2" }).await.unwrap();
    let u3 = OtmUser::create(attrs! { name: "u3" }).await.unwrap();
    for i in 1..=3 {
        let _ = OtmPost::create(attrs! { otm_user_id: u1.id, title: format!("u1-{i}") })
            .await
            .unwrap();
    }
    let _ = OtmPost::create(attrs! { otm_user_id: u2.id, title: "u2" })
        .await
        .unwrap();
    // u3 has zero children.

    let mut users = OtmUser::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut OtmUser> = users.iter_mut().collect();
        OtmUser::__count_relation("posts", refs.as_mut_slice(), _db.conn())
            .await
            .unwrap();
    }
    for u in users.iter() {
        let expected = match u.id {
            id if id == u1.id => 3,
            id if id == u2.id => 1,
            id if id == u3.id => 0,
            _ => unreachable!(),
        };
        assert_eq!(u.posts_count(), expected, "user {} count", u.id);
    }
}

// ---- Aggregate dispatcher ---------------------------------------------
//
// HasMany's aggregate is the real-deal SUM/AVG/MIN/MAX over the
// per-parent group — distinct from T2's "record the single row's
// column" pattern. Empty groups still fall through the Sum|Avg vs
// Min|Max branch (Sum/Avg → 0.0, Min/Max → None).

#[tokio::test]
async fn has_many_sum_aggregates_correctly() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u1 = OtmUser::create(attrs! { name: "u1" }).await.unwrap();
    let u2 = OtmUser::create(attrs! { name: "u2" }).await.unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: u1.id, title: "a", views: 5i64 })
        .await
        .unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: u1.id, title: "b", views: 10i64 })
        .await
        .unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: u2.id, title: "c", views: 7i64 })
        .await
        .unwrap();

    let mut users = OtmUser::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut OtmUser> = users.iter_mut().collect();
        OtmUser::__aggregate_relation(
            "posts",
            "views",
            AggregateKind::Sum,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let u1_sum = *users
        .iter()
        .find(|u| u.id == u1.id)
        .unwrap()
        .__eager
        .get_aggregate::<f64>("posts")
        .expect("sum cache populated");
    let u2_sum = *users
        .iter()
        .find(|u| u.id == u2.id)
        .unwrap()
        .__eager
        .get_aggregate::<f64>("posts")
        .expect("sum cache populated");
    assert_eq!(u1_sum, 15.0);
    assert_eq!(u2_sum, 7.0);
}

#[tokio::test]
async fn has_many_avg_returns_correct_mean() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtmUser::create(attrs! { name: "u" }).await.unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: u.id, title: "a", views: 4i64 })
        .await
        .unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: u.id, title: "b", views: 6i64 })
        .await
        .unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: u.id, title: "c", views: 8i64 })
        .await
        .unwrap();

    let mut users = OtmUser::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut OtmUser> = users.iter_mut().collect();
        OtmUser::__aggregate_relation(
            "posts",
            "views",
            AggregateKind::Avg,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let avg = *users[0]
        .__eager
        .get_aggregate::<f64>("posts")
        .expect("avg cache populated");
    assert_eq!(avg, 6.0);
}

#[tokio::test]
async fn has_many_min_max_some_on_nonempty() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtmUser::create(attrs! { name: "u" }).await.unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: u.id, title: "a", views: 3i64 })
        .await
        .unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: u.id, title: "b", views: 11i64 })
        .await
        .unwrap();
    let _ = OtmPost::create(attrs! { otm_user_id: u.id, title: "c", views: 7i64 })
        .await
        .unwrap();

    let mut users = OtmUser::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut OtmUser> = users.iter_mut().collect();
        OtmUser::__aggregate_relation(
            "posts",
            "views",
            AggregateKind::Min,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let min = users[0]
        .__eager
        .get_aggregate::<Option<f64>>("posts")
        .expect("min cache populated")
        .expect("min over non-empty group is Some");
    assert_eq!(min, 3.0);

    {
        let mut refs: Vec<&mut OtmUser> = users.iter_mut().collect();
        OtmUser::__aggregate_relation(
            "posts",
            "views",
            AggregateKind::Max,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let max = users[0]
        .__eager
        .get_aggregate::<Option<f64>>("posts")
        .expect("max cache populated")
        .expect("max over non-empty group is Some");
    assert_eq!(max, 11.0);
}

#[tokio::test]
async fn has_many_min_max_none_on_empty_group() {
    // Parent with zero children → Min/Max must store
    // Option::<f64>::None, NOT 0.0. Mirrors T2's HasOne
    // `has_one_aggregate_min_max_none_on_empty` semantics.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let _u = OtmUser::create(attrs! { name: "lonely" }).await.unwrap();
    // No children.

    let mut users = OtmUser::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut OtmUser> = users.iter_mut().collect();
        OtmUser::__aggregate_relation(
            "posts",
            "views",
            AggregateKind::Min,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let min: &Option<f64> = users[0]
        .__eager
        .get_aggregate::<Option<f64>>("posts")
        .expect("min cache populated as Option<f64>");
    assert!(min.is_none(), "min over empty group must be None, got: {min:?}");

    {
        let mut refs: Vec<&mut OtmUser> = users.iter_mut().collect();
        OtmUser::__aggregate_relation(
            "posts",
            "views",
            AggregateKind::Max,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let max: &Option<f64> = users[0]
        .__eager
        .get_aggregate::<Option<f64>>("posts")
        .expect("max cache populated as Option<f64>");
    assert!(max.is_none());
}

#[tokio::test]
async fn has_many_sum_avg_zero_on_empty_group() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let _u = OtmUser::create(attrs! { name: "lonely" }).await.unwrap();
    // No children.

    let mut users = OtmUser::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut OtmUser> = users.iter_mut().collect();
        OtmUser::__aggregate_relation(
            "posts",
            "views",
            AggregateKind::Sum,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let sum: f64 = *users[0]
        .__eager
        .get_aggregate::<f64>("posts")
        .expect("sum cache populated");
    assert_eq!(sum, 0.0);

    {
        let mut refs: Vec<&mut OtmUser> = users.iter_mut().collect();
        OtmUser::__aggregate_relation(
            "posts",
            "views",
            AggregateKind::Avg,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let avg: f64 = *users[0]
        .__eager
        .get_aggregate::<f64>("posts")
        .expect("avg cache populated");
    assert_eq!(avg, 0.0);
}
