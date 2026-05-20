//! Phase 10B T5 — `HasOneThrough` / `HasManyThrough` + key inference.
//!
//! Two-hop relations: `A -> B -> C` traversed in a single `INNER JOIN`
//! query. Mirrors Laravel's `hasManyThrough` / `hasOneThrough` shape;
//! the relation method's terminal `.get()` issues one round trip,
//! eager loading uses two queries (intermediate FK map + Model
//! pipeline for the target rows) to keep deserialisation homogeneous.
//!
//! Tests in this file cover:
//!
//! - Default key inference (`country_id` on the intermediate, plus
//!   the target's own FK to the intermediate).
//! - Per-parent isolation — a parent through one intermediate set
//!   must not pull rows reachable from a different parent.
//! - HasOneThrough — single-row semantics over the same JOIN.
//! - Custom keys via `first_key = "..."` / `second_key = "..."`.
//! - Server-side `GROUP BY` for `__count_relation` (one round trip,
//!   one row per parent) — matches the T3/T4 contract.
//! - Server-side `GROUP BY` for `__aggregate_relation` (Sum/Avg → f64,
//!   Min/Max → Option<f64>, both with the empty-group defaults from
//!   the T3/T4 quality-fix commits).
//! - Eager-load distribution — `Self::with(["..."])` populates each
//!   parent's `__eager` cache; `<rel>_loaded()` reads back the right
//!   per-parent group.
//!
//! The user-facing `Builder::with_count` / `with_sum` / `with_avg` /
//! `with_min` / `with_max` surface lands in T9; until then the count
//! and aggregate paths are exercised directly through the
//! macro-emitted `__count_relation` / `__aggregate_relation`
//! dispatchers (matches the T3 / T4 test style at
//! `has_many_aggregate_via_server_side_group_by` and
//! `belongs_to_many_aggregate_sum_over_related_column`).

use suprnova::testing::TestDatabase;
use suprnova::{attrs, model, AggregateKind, Model};

// ---- Default-keys two-hop chain -----------------------------------------

#[model(table = "th_countries", relations = {
    users: HasMany<ThUser>,
    posts: HasManyThrough<ThUser, ThPost>,
})]
pub struct ThCountry {
    pub id: i64,
    pub name: String,
}

#[model(table = "th_users", relations = {
    posts: HasMany<ThPost>,
})]
pub struct ThUser {
    pub id: i64,
    pub th_country_id: i64,
    pub name: String,
}

#[model(table = "th_posts")]
pub struct ThPost {
    pub id: i64,
    pub th_user_id: i64,
    pub title: String,
    pub views: i64,
}

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE th_countries (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE th_users (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            th_country_id INTEGER NOT NULL, \
            name TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE th_posts (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            th_user_id INTEGER NOT NULL, \
            title TEXT NOT NULL, \
            views INTEGER NOT NULL DEFAULT 0\
         )",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn has_many_through_returns_all_grandchildren() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let c = ThCountry::create(attrs! { name: "USA" }).await.unwrap();
    let u1 = ThUser::create(attrs! { th_country_id: c.id, name: "u1" })
        .await
        .unwrap();
    let u2 = ThUser::create(attrs! { th_country_id: c.id, name: "u2" })
        .await
        .unwrap();
    let _ = ThPost::create(attrs! { th_user_id: u1.id, title: "p1-u1" })
        .await
        .unwrap();
    let _ = ThPost::create(attrs! { th_user_id: u2.id, title: "p1-u2" })
        .await
        .unwrap();
    let _ = ThPost::create(attrs! { th_user_id: u2.id, title: "p2-u2" })
        .await
        .unwrap();

    let posts = c.posts().get().await.unwrap();
    assert_eq!(posts.len(), 3, "country should see all 3 grandchild posts");
    let titles: Vec<&str> = posts.iter().map(|p| p.title.as_str()).collect();
    assert!(titles.contains(&"p1-u1"));
    assert!(titles.contains(&"p1-u2"));
    assert!(titles.contains(&"p2-u2"));
}

#[tokio::test]
async fn has_many_through_filters_by_intermediate_country() {
    // Two countries, each owning one user, each user owning one post.
    // The relation must return only the posts whose user belongs to
    // the requested country — the JOIN's `WHERE b.first_key = ?` is
    // what enforces isolation between parents.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let c1 = ThCountry::create(attrs! { name: "C1" }).await.unwrap();
    let c2 = ThCountry::create(attrs! { name: "C2" }).await.unwrap();
    let u1 = ThUser::create(attrs! { th_country_id: c1.id, name: "u1" })
        .await
        .unwrap();
    let u2 = ThUser::create(attrs! { th_country_id: c2.id, name: "u2" })
        .await
        .unwrap();
    let _ = ThPost::create(attrs! { th_user_id: u1.id, title: "p1" })
        .await
        .unwrap();
    let _ = ThPost::create(attrs! { th_user_id: u2.id, title: "p2" })
        .await
        .unwrap();

    let c1_posts = c1.posts().get().await.unwrap();
    assert_eq!(c1_posts.len(), 1);
    assert_eq!(c1_posts[0].title, "p1");

    let c2_posts = c2.posts().get().await.unwrap();
    assert_eq!(c2_posts.len(), 1);
    assert_eq!(c2_posts[0].title, "p2");
}

#[tokio::test]
async fn has_many_through_first_returns_one_or_none() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let c = ThCountry::create(attrs! { name: "USA" }).await.unwrap();
    let u = ThUser::create(attrs! { th_country_id: c.id, name: "u" })
        .await
        .unwrap();
    let _ = ThPost::create(attrs! { th_user_id: u.id, title: "p" })
        .await
        .unwrap();

    let first = c.posts().first().await.unwrap();
    assert!(first.is_some());
    assert_eq!(first.unwrap().title, "p");

    // Empty case — different country with no users.
    let c2 = ThCountry::create(attrs! { name: "Empty" }).await.unwrap();
    let none = c2.posts().first().await.unwrap();
    assert!(none.is_none());
}

#[tokio::test]
async fn has_many_through_count_returns_row_count() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let c = ThCountry::create(attrs! { name: "USA" }).await.unwrap();
    let u = ThUser::create(attrs! { th_country_id: c.id, name: "u" })
        .await
        .unwrap();
    for i in 0..5 {
        let _ = ThPost::create(attrs! { th_user_id: u.id, title: format!("p{i}") })
            .await
            .unwrap();
    }

    let n = c.posts().count().await.unwrap();
    assert_eq!(n, 5, "5 grandchild posts must round-trip via JOIN count");
}

// ---- Custom keys --------------------------------------------------------

#[model(table = "ck_owners", relations = {
    posts: HasManyThrough<CkAuthor, CkArticle> {
        first_key = "owner_uid",
        second_key = "author_uid",
    },
})]
pub struct CkOwner {
    pub id: i64,
    pub name: String,
}

#[model(table = "ck_authors")]
pub struct CkAuthor {
    pub id: i64,
    pub owner_uid: i64,
    pub display: String,
}

#[model(table = "ck_articles")]
pub struct CkArticle {
    pub id: i64,
    pub author_uid: i64,
    pub headline: String,
}

#[tokio::test]
async fn has_many_through_custom_keys() {
    // Schema renames the FK columns away from the snake-case defaults
    // (`ck_author_id`, `ck_owner_id`) — the relation only works if
    // the macro honours `first_key = "owner_uid"` / `second_key =
    // "author_uid"`. The hop chain otherwise has no `*_id` columns to
    // fall back on.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    _db.execute_unprepared(
        "CREATE TABLE ck_owners (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    _db.execute_unprepared(
        "CREATE TABLE ck_authors (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            owner_uid INTEGER NOT NULL, \
            display TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
    _db.execute_unprepared(
        "CREATE TABLE ck_articles (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            author_uid INTEGER NOT NULL, \
            headline TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();

    let o = CkOwner::create(attrs! { name: "Owner" }).await.unwrap();
    let a = CkAuthor::create(attrs! { owner_uid: o.id, display: "A" })
        .await
        .unwrap();
    let _ = CkArticle::create(attrs! { author_uid: a.id, headline: "h1" })
        .await
        .unwrap();
    let _ = CkArticle::create(attrs! { author_uid: a.id, headline: "h2" })
        .await
        .unwrap();

    let articles = o.posts().get().await.unwrap();
    assert_eq!(articles.len(), 2);
    let mut headlines: Vec<&str> = articles.iter().map(|x| x.headline.as_str()).collect();
    headlines.sort();
    assert_eq!(headlines, vec!["h1", "h2"]);

    let count = o.posts().count().await.unwrap();
    assert_eq!(count, 2);
}

// ---- HasOneThrough -------------------------------------------------------

#[model(table = "ho_users", relations = {
    profile: HasOneThrough<HoMembership, HoProfile>,
})]
pub struct HoUser {
    pub id: i64,
    pub name: String,
}

#[model(table = "ho_memberships")]
pub struct HoMembership {
    pub id: i64,
    pub ho_user_id: i64,
}

#[model(table = "ho_profiles")]
pub struct HoProfile {
    pub id: i64,
    pub ho_membership_id: i64,
    pub bio: String,
}

#[tokio::test]
async fn has_one_through_returns_single() {
    // HoUser -> HoMembership -> HoProfile. Default keys resolve to:
    //   first_key  = "ho_user_id"        (col on HoMembership pointing at HoUser)
    //   second_key = "ho_membership_id"  (col on HoProfile pointing at HoMembership)
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    _db.execute_unprepared(
        "CREATE TABLE ho_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    _db.execute_unprepared(
        "CREATE TABLE ho_memberships (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            ho_user_id INTEGER NOT NULL\
         )",
    )
    .await
    .unwrap();
    _db.execute_unprepared(
        "CREATE TABLE ho_profiles (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            ho_membership_id INTEGER NOT NULL, \
            bio TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();

    let u = HoUser::create(attrs! { name: "Alice" }).await.unwrap();
    let m = HoMembership::create(attrs! { ho_user_id: u.id })
        .await
        .unwrap();
    let _ = HoProfile::create(attrs! { ho_membership_id: m.id, bio: "Hello" })
        .await
        .unwrap();

    let profile = u.profile().get().await.unwrap();
    assert!(profile.is_some(), "HasOneThrough must traverse two hops");
    assert_eq!(profile.unwrap().bio, "Hello");

    // `.first()` is the alias for `.get()` on HasOneThrough — both
    // collapse to the first row from the JOIN.
    let via_first = u.profile().first().await.unwrap();
    assert!(via_first.is_some());

    // Empty case — second user with no membership row.
    let u2 = HoUser::create(attrs! { name: "Bob" }).await.unwrap();
    let none = u2.profile().get().await.unwrap();
    assert!(none.is_none(), "no membership chain => None");
}

// ---- Server-side aggregates (dispatcher-driven; Builder surface in T9) --

#[tokio::test]
async fn has_many_through_count_uses_server_side_group_by() {
    // The `__count_relation` dispatcher must hit a single GROUP BY
    // query, distribute per-parent counts via `set_count`, and surface
    // those counts via the macro-emitted `<rel>_count()` accessor.
    //
    // Driven through the dispatcher directly (Builder::with_count
    // ships in T9). Same pattern as the T4 m2m count test at
    // `belongs_to_many_count_dispatcher_uses_server_side_group_by`.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let c1 = ThCountry::create(attrs! { name: "C1" }).await.unwrap();
    let c2 = ThCountry::create(attrs! { name: "C2" }).await.unwrap();
    let c3 = ThCountry::create(attrs! { name: "C3-empty" }).await.unwrap();
    let u1 = ThUser::create(attrs! { th_country_id: c1.id, name: "u1" })
        .await
        .unwrap();
    let u2 = ThUser::create(attrs! { th_country_id: c2.id, name: "u2" })
        .await
        .unwrap();
    for i in 0..3 {
        let _ = ThPost::create(attrs! { th_user_id: u1.id, title: format!("c1-p{i}") })
            .await
            .unwrap();
    }
    let _ = ThPost::create(attrs! { th_user_id: u2.id, title: "c2-p" })
        .await
        .unwrap();

    let mut countries = ThCountry::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut ThCountry> = countries.iter_mut().collect();
        ThCountry::__count_relation("posts", refs.as_mut_slice(), _db.conn())
            .await
            .unwrap();
    }
    for country in &countries {
        let expected = if country.id == c1.id {
            3
        } else if country.id == c2.id {
            1
        } else if country.id == c3.id {
            0
        } else {
            unreachable!()
        };
        assert_eq!(
            country.posts_count(),
            expected,
            "country {} via JOIN GROUP BY",
            country.name,
        );
    }
}

#[tokio::test]
async fn has_many_through_aggregate_via_server_side_group_by() {
    // Verify the four aggregate kinds round-trip via the JOIN +
    // GROUP BY path. Sum/Avg → f64 with 0.0 empty default; Min/Max →
    // Option<f64> with None empty default — matches T3 (HasMany) and
    // T4 (BelongsToMany).
    //
    // Driven through the dispatcher directly (Builder::with_sum etc.
    // ships in T9). Same pattern as T4's
    // `belongs_to_many_aggregate_sum_over_related_column`.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let c = ThCountry::create(attrs! { name: "C" }).await.unwrap();
    let c_empty = ThCountry::create(attrs! { name: "Empty" }).await.unwrap();
    let u = ThUser::create(attrs! { th_country_id: c.id, name: "u" })
        .await
        .unwrap();
    let _ = ThPost::create(attrs! { th_user_id: u.id, title: "p1", views: 5_i64 })
        .await
        .unwrap();
    let _ = ThPost::create(attrs! { th_user_id: u.id, title: "p2", views: 10_i64 })
        .await
        .unwrap();
    let _ = ThPost::create(attrs! { th_user_id: u.id, title: "p3", views: 15_i64 })
        .await
        .unwrap();

    // SUM: 5 + 10 + 15 = 30
    let mut countries = ThCountry::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut ThCountry> = countries.iter_mut().collect();
        ThCountry::__aggregate_relation(
            "posts",
            "views",
            AggregateKind::Sum,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let sum_c = *countries
        .iter()
        .find(|x| x.id == c.id)
        .unwrap()
        .__eager
        .get_aggregate::<f64>("posts")
        .expect("sum cache populated for populated parent");
    assert_eq!(sum_c, 30.0);
    let sum_empty = *countries
        .iter()
        .find(|x| x.id == c_empty.id)
        .unwrap()
        .__eager
        .get_aggregate::<f64>("posts")
        .expect("sum cache populated for empty parent");
    assert_eq!(sum_empty, 0.0, "Sum over empty group => 0.0");

    // AVG: (5 + 10 + 15) / 3 = 10
    let mut countries = ThCountry::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut ThCountry> = countries.iter_mut().collect();
        ThCountry::__aggregate_relation(
            "posts",
            "views",
            AggregateKind::Avg,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let avg_c = *countries
        .iter()
        .find(|x| x.id == c.id)
        .unwrap()
        .__eager
        .get_aggregate::<f64>("posts")
        .expect("avg cache populated");
    assert_eq!(avg_c, 10.0);

    // MIN: 5 → Some; empty → None
    let mut countries = ThCountry::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut ThCountry> = countries.iter_mut().collect();
        ThCountry::__aggregate_relation(
            "posts",
            "views",
            AggregateKind::Min,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let min_c = *countries
        .iter()
        .find(|x| x.id == c.id)
        .unwrap()
        .__eager
        .get_aggregate::<Option<f64>>("posts")
        .expect("min cache populated");
    assert_eq!(min_c, Some(5.0));
    let min_empty = *countries
        .iter()
        .find(|x| x.id == c_empty.id)
        .unwrap()
        .__eager
        .get_aggregate::<Option<f64>>("posts")
        .expect("min cache populated for empty parent");
    assert_eq!(min_empty, None, "Min over empty group => None");

    // MAX: 15
    let mut countries = ThCountry::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut ThCountry> = countries.iter_mut().collect();
        ThCountry::__aggregate_relation(
            "posts",
            "views",
            AggregateKind::Max,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let max_c = *countries
        .iter()
        .find(|x| x.id == c.id)
        .unwrap()
        .__eager
        .get_aggregate::<Option<f64>>("posts")
        .expect("max cache populated");
    assert_eq!(max_c, Some(15.0));
}

#[tokio::test]
async fn has_many_through_eager_load_distributes_by_parent() {
    // The two-query eager-load strategy must group C rows back to
    // the right parent via the b_to_parent map. Two countries, each
    // with their own users + posts — no rows from one should leak
    // into the other's `<rel>_loaded()` slice. Third country has
    // zero users (and thus zero posts) — its loaded slice must be
    // empty, not panic.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let c1 = ThCountry::create(attrs! { name: "C1" }).await.unwrap();
    let c2 = ThCountry::create(attrs! { name: "C2" }).await.unwrap();
    let c3 = ThCountry::create(attrs! { name: "C3-empty" }).await.unwrap();
    let u1 = ThUser::create(attrs! { th_country_id: c1.id, name: "u1" })
        .await
        .unwrap();
    let u2 = ThUser::create(attrs! { th_country_id: c2.id, name: "u2" })
        .await
        .unwrap();
    let _ = ThPost::create(attrs! { th_user_id: u1.id, title: "c1-p1" })
        .await
        .unwrap();
    let _ = ThPost::create(attrs! { th_user_id: u1.id, title: "c1-p2" })
        .await
        .unwrap();
    let _ = ThPost::create(attrs! { th_user_id: u2.id, title: "c2-p1" })
        .await
        .unwrap();

    let loaded: Vec<ThCountry> = ThCountry::query()
        .with(["posts"])
        .get()
        .await
        .unwrap();
    let by_name: std::collections::HashMap<&str, &ThCountry> = loaded
        .iter()
        .map(|r| (r.name.as_str(), r))
        .collect();

    let c1_loaded = by_name.get("C1").unwrap();
    assert_eq!(c1_loaded.id, c1.id, "C1 round-trips through query");
    let c1_posts = c1_loaded.posts_loaded();
    assert_eq!(c1_posts.len(), 2);
    let mut c1_titles: Vec<&str> = c1_posts.iter().map(|p| p.title.as_str()).collect();
    c1_titles.sort();
    assert_eq!(c1_titles, vec!["c1-p1", "c1-p2"]);

    let c2_loaded = by_name.get("C2").unwrap();
    assert_eq!(c2_loaded.id, c2.id);
    let c2_posts = c2_loaded.posts_loaded();
    assert_eq!(c2_posts.len(), 1);
    assert_eq!(c2_posts[0].title, "c2-p1");

    // C3 has no users at all => empty intermediate set => loaded
    // slice returns &[].
    let c3_loaded = by_name.get("C3-empty").unwrap();
    assert_eq!(c3_loaded.id, c3.id);
    assert_eq!(c3_loaded.posts_loaded().len(), 0);
}

// ---- Non-`id` intermediate primary key ---------------------------------

#[model(table = "slk_companies", relations = {
    devices: HasManyThrough<SlkOffice, SlkDevice> {
        first_key = "company_id",
        second_key = "office_uid",
        second_local_key = "uid",
    },
})]
pub struct SlkCompany {
    pub id: i64,
    pub name: String,
}

#[model(table = "slk_offices", primary_key = "uid")]
pub struct SlkOffice {
    pub uid: i64,
    pub company_id: i64,
    pub label: String,
}

#[model(table = "slk_devices")]
pub struct SlkDevice {
    pub id: i64,
    pub office_uid: i64,
    pub serial: String,
    pub watts: i64,
}

#[tokio::test]
async fn has_many_through_honours_second_local_key() {
    // Intermediate `SlkOffice` declares `primary_key = "uid"` —
    // without `second_local_key = "uid"` the dispatcher's JOIN reads
    // `__sn_b.id` (which doesn't exist on this table) and silently
    // produces empty results. This test pins the override flow
    // through `.get()` (single-JOIN) AND the count + aggregate
    // dispatchers (Group-By JOINs).
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    _db.execute_unprepared(
        "CREATE TABLE slk_companies (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    _db.execute_unprepared(
        "CREATE TABLE slk_offices (\
            uid INTEGER PRIMARY KEY AUTOINCREMENT, \
            company_id INTEGER NOT NULL, \
            label TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
    _db.execute_unprepared(
        "CREATE TABLE slk_devices (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            office_uid INTEGER NOT NULL, \
            serial TEXT NOT NULL, \
            watts INTEGER NOT NULL DEFAULT 0\
         )",
    )
    .await
    .unwrap();

    let co = SlkCompany::create(attrs! { name: "Acme" }).await.unwrap();
    let off = SlkOffice::create(attrs! { company_id: co.id, label: "HQ" })
        .await
        .unwrap();
    let _ = SlkDevice::create(attrs! { office_uid: off.uid, serial: "S1", watts: 10_i64 })
        .await
        .unwrap();
    let _ = SlkDevice::create(attrs! { office_uid: off.uid, serial: "S2", watts: 20_i64 })
        .await
        .unwrap();

    // Single-JOIN .get()
    let devices = co.devices().get().await.unwrap();
    assert_eq!(devices.len(), 2, "non-`id` intermediate PK round-trips");
    let mut serials: Vec<&str> = devices.iter().map(|d| d.serial.as_str()).collect();
    serials.sort();
    assert_eq!(serials, vec!["S1", "S2"]);

    // .count() over the same JOIN
    let n = co.devices().count().await.unwrap();
    assert_eq!(n, 2);

    // Count dispatcher (server-side GROUP BY)
    let mut companies = SlkCompany::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut SlkCompany> = companies.iter_mut().collect();
        SlkCompany::__count_relation("devices", refs.as_mut_slice(), _db.conn())
            .await
            .unwrap();
    }
    let co_loaded = companies.iter().find(|x| x.id == co.id).unwrap();
    assert_eq!(co_loaded.devices_count(), 2);

    // Aggregate dispatcher (SUM via JOIN GROUP BY)
    let mut companies = SlkCompany::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut SlkCompany> = companies.iter_mut().collect();
        SlkCompany::__aggregate_relation(
            "devices",
            "watts",
            AggregateKind::Sum,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let sum = *companies
        .iter()
        .find(|x| x.id == co.id)
        .unwrap()
        .__eager
        .get_aggregate::<f64>("devices")
        .expect("aggregate cache populated");
    assert_eq!(sum, 30.0, "sum(watts) over the JOIN = 10 + 20");

    // Eager-load distribution (two-query strategy via Query 1's
    // SELECT CAST({slk} AS ...) AS __sn_b_id).
    let loaded: Vec<SlkCompany> = SlkCompany::query()
        .with(["devices"])
        .get()
        .await
        .unwrap();
    let co_eager = loaded.iter().find(|x| x.id == co.id).unwrap();
    assert_eq!(co_eager.devices_loaded().len(), 2);
}

#[tokio::test]
async fn has_one_through_eager_load_distributes_single_row() {
    // HasOneThrough eager-load must distribute via `set_one` — each
    // parent gets `Option<C>`, not a slice. Different from
    // HasManyThrough where `set_many` distributes `Vec<C>`.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    _db.execute_unprepared(
        "CREATE TABLE ho_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    _db.execute_unprepared(
        "CREATE TABLE ho_memberships (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            ho_user_id INTEGER NOT NULL\
         )",
    )
    .await
    .unwrap();
    _db.execute_unprepared(
        "CREATE TABLE ho_profiles (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            ho_membership_id INTEGER NOT NULL, \
            bio TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();

    let u1 = HoUser::create(attrs! { name: "u1" }).await.unwrap();
    let u2 = HoUser::create(attrs! { name: "u2-no-chain" })
        .await
        .unwrap();
    let m1 = HoMembership::create(attrs! { ho_user_id: u1.id })
        .await
        .unwrap();
    let _ = HoProfile::create(attrs! { ho_membership_id: m1.id, bio: "Alice bio" })
        .await
        .unwrap();

    let loaded: Vec<HoUser> = HoUser::query().with(["profile"]).get().await.unwrap();
    let by_id: std::collections::HashMap<i64, &HoUser> =
        loaded.iter().map(|r| (r.id, r)).collect();
    let u1_profile = by_id.get(&u1.id).unwrap().profile_loaded();
    assert!(u1_profile.is_some(), "u1 has a profile chain");
    assert_eq!(u1_profile.unwrap().bio, "Alice bio");
    let u2_profile = by_id.get(&u2.id).unwrap().profile_loaded();
    assert!(
        u2_profile.is_none(),
        "u2 has no membership row => profile is None",
    );
}

// ---- String-PK parent through eager load (T5 audit fix) -----------------
//
// Regression: the Through `__eager_load` distribute block previously built
// the per-parent lookup key as `serde_json::to_value(&p.pk).to_string()`.
// For `i64` PKs that produces `"42"` — matches the `CAST(parent_id AS TEXT)`
// result on the b->parent map. For `String` PKs like `"A1"` it produced the
// JSON-quoted `"\"A1\""` while the SQL CAST result was the raw `"A1"`. The
// HashMap lookup missed → every parent silently received an empty
// `posts_loaded()` slice. The fix routes the key through the existing
// `__sn_parent_key_to_match_cast` helper (already used by the count and
// aggregate arms). This test pins that wiring.

#[suprnova::model(
    table = "th_str_owners",
    primary_key = "code",
    key_type = "String",
    auto_increment = false,
    timestamps = false,
    relations = {
        posts: HasManyThrough<ThStrIntermediate, ThStrPost> {
            first_key = "th_str_owner_code",
        },
    },
)]
pub struct ThStrOwner {
    pub code: String,
    pub name: String,
}

#[suprnova::model(table = "th_str_intermediates", timestamps = false)]
pub struct ThStrIntermediate {
    pub id: i64,
    pub th_str_owner_code: String,
}

#[suprnova::model(table = "th_str_posts", timestamps = false)]
pub struct ThStrPost {
    pub id: i64,
    pub th_str_intermediate_id: i64,
    pub title: String,
}

async fn migrate_str_through(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE th_str_owners (code TEXT PRIMARY KEY, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE th_str_intermediates (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            th_str_owner_code TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE th_str_posts (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            th_str_intermediate_id INTEGER NOT NULL, \
            title TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn has_many_through_eager_load_with_string_parent_pk() {
    // Raw-SQL setup — the String-PK `create()` path is orthogonal to the
    // bug under test; `ThStrOwner::all()` then `query().with(...).get()`
    // exercise the Builder + `__eager_load` path that does the lookup.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate_str_through(&db).await;

    db.execute_unprepared(
        "INSERT INTO th_str_owners (code, name) VALUES ('A1', 'owner-A1')",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "INSERT INTO th_str_owners (code, name) VALUES ('B2', 'owner-B2')",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "INSERT INTO th_str_intermediates (id, th_str_owner_code) VALUES (1, 'A1')",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "INSERT INTO th_str_intermediates (id, th_str_owner_code) VALUES (2, 'B2')",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "INSERT INTO th_str_posts (th_str_intermediate_id, title) \
         VALUES (1, 'A1-post1')",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "INSERT INTO th_str_posts (th_str_intermediate_id, title) \
         VALUES (1, 'A1-post2')",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "INSERT INTO th_str_posts (th_str_intermediate_id, title) \
         VALUES (2, 'B2-post1')",
    )
    .await
    .unwrap();

    let loaded: Vec<ThStrOwner> = ThStrOwner::query()
        .with(["posts"])
        .get()
        .await
        .unwrap();
    let by_code: std::collections::HashMap<&str, &ThStrOwner> = loaded
        .iter()
        .map(|r| (r.code.as_str(), r))
        .collect();

    let a1 = by_code.get("A1").expect("A1 round-trips through query");
    let a1_posts = a1.posts_loaded();
    assert_eq!(
        a1_posts.len(),
        2,
        "String-PK parent must receive its grandchildren — \
         if this regresses to 0, the distribute-key shape no longer \
         matches the b_to_parent CAST output (bug pre-T5-audit-fix)"
    );
    let mut a1_titles: Vec<&str> = a1_posts.iter().map(|p| p.title.as_str()).collect();
    a1_titles.sort();
    assert_eq!(a1_titles, vec!["A1-post1", "A1-post2"]);

    let b2 = by_code.get("B2").expect("B2 round-trips through query");
    let b2_posts = b2.posts_loaded();
    assert_eq!(b2_posts.len(), 1);
    assert_eq!(b2_posts[0].title, "B2-post1");
}
