//! Phase 10B T2 — `HasOne` / `BelongsTo` + `with_default` + custom
//! FK/LK + eager-load dispatcher arm.
//!
//! T1 shipped the relation infrastructure (sealed trait,
//! `EagerLoadCache`, dispatcher skeletons). T2 ships the first two
//! concrete relation flavours and the macro emission that wires them
//! into the per-model `__eager_load` dispatcher. Tests below pin the
//! Laravel-shape semantics: chainable inner builder, `with_default`
//! fallback for both null FK and missing parent rows, and eager-load
//! through `Self::with([...])`.

use suprnova::testing::TestDatabase;
use suprnova::{AggregateKind, Model, attrs, model};

#[model(table = "oto_users", relations = {
    profile: HasOne<OtoProfile>,
})]
pub struct OtoUser {
    pub id: i64,
    pub name: String,
}

#[model(table = "oto_profiles", relations = {
    user: BelongsTo<OtoUser>,
})]
pub struct OtoProfile {
    pub id: i64,
    pub oto_user_id: i64,
    pub bio: String,
}

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE oto_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE oto_profiles (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         oto_user_id INTEGER NOT NULL, bio TEXT NOT NULL)",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn has_one_first_returns_related_row() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtoUser::create(attrs! { name: "Alice" }).await.unwrap();
    let _p = OtoProfile::create(attrs! { oto_user_id: u.id, bio: "loves rust" })
        .await
        .unwrap();

    let loaded = u.profile().first().await.unwrap().expect("profile present");
    assert_eq!(loaded.bio, "loves rust");
}

#[tokio::test]
async fn has_one_first_returns_none_for_missing() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtoUser::create(attrs! { name: "Alone" }).await.unwrap();

    assert!(u.profile().first().await.unwrap().is_none());
}

#[tokio::test]
async fn has_one_chainable_filter() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtoUser::create(attrs! { name: "Bob" }).await.unwrap();
    let _ = OtoProfile::create(attrs! { oto_user_id: u.id, bio: "first" })
        .await
        .unwrap();

    let none = u.profile().filter("bio", "second").first().await.unwrap();
    assert!(none.is_none(), "filter rules out the only profile");
}

#[tokio::test]
async fn has_one_chainable_db_where_alias() {
    // The dual-API alias must work just like the primary filter.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtoUser::create(attrs! { name: "Cara" }).await.unwrap();
    let _ = OtoProfile::create(attrs! { oto_user_id: u.id, bio: "alpha" })
        .await
        .unwrap();

    let some = u.profile().db_where("bio", "alpha").first().await.unwrap();
    assert!(some.is_some(), "db_where must alias filter");
}

#[tokio::test]
async fn has_one_get_returns_all_matching_rows() {
    // Laravel's `hasOne` only returns the first match, but the
    // chainable inner builder exposes `.get()` for users that want
    // the unfiltered set (e.g. they wrote `hasOne` but want every
    // row anyway). Pin the behaviour so it doesn't regress.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtoUser::create(attrs! { name: "Dave" }).await.unwrap();
    let _ = OtoProfile::create(attrs! { oto_user_id: u.id, bio: "p1" })
        .await
        .unwrap();
    let _ = OtoProfile::create(attrs! { oto_user_id: u.id, bio: "p2" })
        .await
        .unwrap();

    let all = u.profile().get().await.unwrap();
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn belongs_to_first_returns_parent() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtoUser::create(attrs! { name: "Carol" }).await.unwrap();
    let p = OtoProfile::create(attrs! { oto_user_id: u.id, bio: "..." })
        .await
        .unwrap();

    let parent = p.user().first().await.unwrap().expect("parent present");
    assert_eq!(parent.id, u.id);
    assert_eq!(parent.name, "Carol");
}

// ---- `with_default(closure)` -----------------------------------------
//
// `with_default` covers two failure modes: null FK on the child row,
// and FK present but no parent row exists. The closure runs in both
// cases so callers get a consistent stand-in.

#[model(table = "oto_posts", relations = {
    user: BelongsTo<OtoUser> { with_default = || OtoUser {
        id: 0,
        name: "Guest".into(),
        __eager: ::core::default::Default::default(),
        __pivot: ::core::option::Option::None,
    } },
})]
pub struct OtoPost {
    pub id: i64,
    pub oto_user_id: Option<i64>,
    pub title: String,
}

#[tokio::test]
async fn belongs_to_with_default_returns_closure_when_fk_null() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    _db.execute_unprepared(
        "CREATE TABLE oto_posts (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         oto_user_id INTEGER, title TEXT NOT NULL)",
    )
    .await
    .unwrap();

    let p = OtoPost::create(attrs! { title: "Orphaned", oto_user_id: Option::<i64>::None })
        .await
        .unwrap();
    let default_user = p
        .user()
        .first()
        .await
        .unwrap()
        .expect("default returned even for null FK");
    assert_eq!(default_user.name, "Guest");
    assert_eq!(default_user.id, 0);
}

#[tokio::test]
async fn belongs_to_with_default_returns_closure_when_parent_missing() {
    // FK present but no matching parent row — `with_default` still
    // fires. Mirrors Laravel's `->withDefault()` semantics.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    _db.execute_unprepared(
        "CREATE TABLE oto_posts (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         oto_user_id INTEGER, title TEXT NOT NULL)",
    )
    .await
    .unwrap();

    // 999 is a bogus FK — no oto_users row matches.
    let p = OtoPost::create(attrs! { title: "Ghost", oto_user_id: 999i64 })
        .await
        .unwrap();
    let default_user = p
        .user()
        .first()
        .await
        .unwrap()
        .expect("default returned even when parent missing");
    assert_eq!(default_user.name, "Guest");
}

// ---- Custom FK / LK overrides ---------------------------------------

#[model(table = "oto_owners", relations = {
    profile: HasOne<OtoProfile> { fk = "oto_user_id", lk = "id" },
})]
pub struct OtoOwner {
    pub id: i64,
    pub name: String,
}

#[tokio::test]
async fn has_one_custom_fk_lk_resolves() {
    // Explicit FK / LK overrides — when the user names them
    // identical to the defaults, the relation must still resolve.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    _db.execute_unprepared(
        "CREATE TABLE oto_owners (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    let owner = OtoOwner::create(attrs! { name: "ZZ" }).await.unwrap();
    let _p = OtoProfile::create(attrs! { oto_user_id: owner.id, bio: "owner-bio" })
        .await
        .unwrap();

    let loaded = owner
        .profile()
        .first()
        .await
        .unwrap()
        .expect("profile present");
    assert_eq!(loaded.bio, "owner-bio");
}

// ---- Eager loading exercise — verifies T2 dispatcher arms -----------

#[tokio::test]
async fn has_one_eager_load_fills_cache() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u1 = OtoUser::create(attrs! { name: "u1" }).await.unwrap();
    let u2 = OtoUser::create(attrs! { name: "u2" }).await.unwrap();
    let _ = OtoProfile::create(attrs! { oto_user_id: u1.id, bio: "b1" })
        .await
        .unwrap();
    let _ = OtoProfile::create(attrs! { oto_user_id: u2.id, bio: "b2" })
        .await
        .unwrap();

    let users = OtoUser::with(["profile"]).get().await.unwrap();
    assert_eq!(users.len(), 2);
    for u in users.iter() {
        let p = u.profile_loaded().expect("profile loaded");
        assert!(
            p.bio == "b1" || p.bio == "b2",
            "got unexpected eager bio: {}",
            p.bio
        );
    }
}

#[tokio::test]
async fn belongs_to_eager_load_fills_cache() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtoUser::create(attrs! { name: "owner" }).await.unwrap();
    let _ = OtoProfile::create(attrs! { oto_user_id: u.id, bio: "p1" })
        .await
        .unwrap();
    let _ = OtoProfile::create(attrs! { oto_user_id: u.id, bio: "p2" })
        .await
        .unwrap();

    let profiles = OtoProfile::with(["user"]).get().await.unwrap();
    assert_eq!(profiles.len(), 2);
    for p in profiles.iter() {
        let parent = p.user_loaded().expect("user loaded");
        assert_eq!(parent.id, u.id);
        assert_eq!(parent.name, "owner");
    }
}

#[tokio::test]
async fn belongs_to_eager_load_honours_with_default_for_null_fk() {
    // Eager load must invoke `with_default` per row when the FK is
    // null — same semantics as the lazy `.first()` path.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    _db.execute_unprepared(
        "CREATE TABLE oto_posts (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         oto_user_id INTEGER, title TEXT NOT NULL)",
    )
    .await
    .unwrap();

    let _ = OtoPost::create(attrs! { title: "Orphan1", oto_user_id: Option::<i64>::None })
        .await
        .unwrap();
    let _ = OtoPost::create(attrs! { title: "Orphan2", oto_user_id: Option::<i64>::None })
        .await
        .unwrap();

    let posts = OtoPost::with(["user"]).get().await.unwrap();
    assert_eq!(posts.len(), 2);
    for p in posts.iter() {
        let parent = p
            .user_loaded()
            .expect("with_default fires in eager path too");
        assert_eq!(parent.name, "Guest");
    }
}

// ---- Aggregate cache semantics on empty + non-empty groups ----------
//
// The `__aggregate_relation` dispatcher branches on `AggregateKind`:
// Sum/Avg store `f64` (0.0 on empty, matching the framework's COALESCE
// behaviour); Min/Max store `Option<f64>` (None on empty, matching
// SQL's NULL-on-empty + `Builder::min`/`Builder::max`'s `Option<T>`
// return type). T2 wires the dispatcher arms for HasOne / BelongsTo;
// the user-facing `with_sum` / `with_avg` / `with_min` / `with_max`
// Builder surface lands in T9. The tests below call the dispatcher
// directly to lock the cache-layer semantics regardless of the Builder
// surface progress.

#[tokio::test]
async fn has_one_aggregate_sum_avg_zero_on_empty() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let _u = OtoUser::create(attrs! { name: "lonely" }).await.unwrap();
    // No profile created — empty aggregate group.

    let mut users = OtoUser::query().get().await.unwrap();
    assert_eq!(users.len(), 1);

    {
        let mut refs: Vec<&mut OtoUser> = users.iter_mut().collect();
        OtoUser::__aggregate_relation(
            "profile",
            "id",
            AggregateKind::Sum,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let stored_sum: &f64 = users[0]
        .__eager
        .get_aggregate::<f64>("profile_sum_id")
        .expect("sum cache populated");
    assert_eq!(*stored_sum, 0.0);

    {
        let mut refs: Vec<&mut OtoUser> = users.iter_mut().collect();
        OtoUser::__aggregate_relation(
            "profile",
            "id",
            AggregateKind::Avg,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let stored_avg: &f64 = users[0]
        .__eager
        .get_aggregate::<f64>("profile_avg_id")
        .expect("avg cache populated");
    assert_eq!(*stored_avg, 0.0);
}

#[tokio::test]
async fn has_one_aggregate_min_max_none_on_empty() {
    // Min/Max over zero rows must store Option::<f64>::None — the
    // pre-fix behaviour stored `0.0_f64` which conflicts with SQL
    // semantics and the existing Builder::min/max Option<T> return.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let _u = OtoUser::create(attrs! { name: "lonely" }).await.unwrap();

    let mut users = OtoUser::query().get().await.unwrap();
    assert_eq!(users.len(), 1);

    {
        let mut refs: Vec<&mut OtoUser> = users.iter_mut().collect();
        OtoUser::__aggregate_relation(
            "profile",
            "id",
            AggregateKind::Min,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let min: &Option<f64> = users[0]
        .__eager
        .get_aggregate::<Option<f64>>("profile_min_id")
        .expect("min cache populated as Option<f64>");
    assert!(
        min.is_none(),
        "min over empty set should be None, got: {min:?}",
    );

    {
        let mut refs: Vec<&mut OtoUser> = users.iter_mut().collect();
        OtoUser::__aggregate_relation(
            "profile",
            "id",
            AggregateKind::Max,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let max: &Option<f64> = users[0]
        .__eager
        .get_aggregate::<Option<f64>>("profile_max_id")
        .expect("max cache populated as Option<f64>");
    assert!(
        max.is_none(),
        "max over empty set should be None, got: {max:?}"
    );
}

#[tokio::test]
async fn has_one_aggregate_min_max_some_on_nonempty() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = OtoUser::create(attrs! { name: "Alice" }).await.unwrap();
    let p = OtoProfile::create(attrs! { oto_user_id: u.id, bio: "..." })
        .await
        .unwrap();

    let mut users = OtoUser::query().get().await.unwrap();

    {
        let mut refs: Vec<&mut OtoUser> = users.iter_mut().collect();
        OtoUser::__aggregate_relation(
            "profile",
            "id",
            AggregateKind::Min,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let min: &Option<f64> = users[0]
        .__eager
        .get_aggregate::<Option<f64>>("profile_min_id")
        .expect("min cache populated as Option<f64>");
    assert!(
        min.is_some(),
        "min over non-empty must be Some, got: {min:?}"
    );
    assert_eq!(min.unwrap(), p.id as f64);

    {
        let mut refs: Vec<&mut OtoUser> = users.iter_mut().collect();
        OtoUser::__aggregate_relation(
            "profile",
            "id",
            AggregateKind::Max,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let max: &Option<f64> = users[0]
        .__eager
        .get_aggregate::<Option<f64>>("profile_max_id")
        .expect("max cache populated as Option<f64>");
    assert!(max.is_some());
    assert_eq!(max.unwrap(), p.id as f64);
}

#[tokio::test]
async fn belongs_to_aggregate_sum_zero_min_none_when_parent_missing() {
    // Mirror the HasOne semantics on the BelongsTo arm. Two child
    // rows; both have null FK so there is no parent row to aggregate
    // over. Sum stores 0.0; Min/Max store None.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    _db.execute_unprepared(
        "CREATE TABLE oto_posts (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         oto_user_id INTEGER, title TEXT NOT NULL)",
    )
    .await
    .unwrap();
    let _ = OtoPost::create(attrs! { title: "Orphan1", oto_user_id: Option::<i64>::None })
        .await
        .unwrap();
    let _ = OtoPost::create(attrs! { title: "Orphan2", oto_user_id: Option::<i64>::None })
        .await
        .unwrap();

    let mut posts = OtoPost::query().get().await.unwrap();
    assert_eq!(posts.len(), 2);

    {
        let mut refs: Vec<&mut OtoPost> = posts.iter_mut().collect();
        OtoPost::__aggregate_relation(
            "user",
            "id",
            AggregateKind::Sum,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    for p in posts.iter() {
        let s: &f64 = p
            .__eager
            .get_aggregate::<f64>("user_sum_id")
            .expect("sum cache populated");
        assert_eq!(*s, 0.0);
    }

    {
        let mut refs: Vec<&mut OtoPost> = posts.iter_mut().collect();
        OtoPost::__aggregate_relation(
            "user",
            "id",
            AggregateKind::Min,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    for p in posts.iter() {
        let min: &Option<f64> = p
            .__eager
            .get_aggregate::<Option<f64>>("user_min_id")
            .expect("min cache populated as Option<f64>");
        assert!(min.is_none(), "min over empty must be None, got: {min:?}");
    }

    {
        let mut refs: Vec<&mut OtoPost> = posts.iter_mut().collect();
        OtoPost::__aggregate_relation(
            "user",
            "id",
            AggregateKind::Max,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    for p in posts.iter() {
        let max: &Option<f64> = p
            .__eager
            .get_aggregate::<Option<f64>>("user_max_id")
            .expect("max cache populated as Option<f64>");
        assert!(max.is_none());
    }
}

#[tokio::test]
async fn belongs_to_aggregate_min_some_when_parent_present() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    _db.execute_unprepared(
        "CREATE TABLE oto_posts (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         oto_user_id INTEGER, title TEXT NOT NULL)",
    )
    .await
    .unwrap();
    let u = OtoUser::create(attrs! { name: "owner" }).await.unwrap();
    let _ = OtoPost::create(attrs! { title: "P", oto_user_id: u.id })
        .await
        .unwrap();

    let mut posts = OtoPost::query().get().await.unwrap();

    {
        let mut refs: Vec<&mut OtoPost> = posts.iter_mut().collect();
        OtoPost::__aggregate_relation(
            "user",
            "id",
            AggregateKind::Min,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let min: &Option<f64> = posts[0]
        .__eager
        .get_aggregate::<Option<f64>>("user_min_id")
        .expect("min cache populated as Option<f64>");
    assert_eq!(min.unwrap(), u.id as f64);
}
