//! Laravel-13 parity sweep — additions that landed in the per-domain
//! Eloquent audit.
//!
//! Each test covers ONE of the new surfaces added by the parity work:
//! - `Builder::sole` / `sole_value` / `value_or_fail`
//! - `Builder::upsert` / `update_all` / `delete_all`
//! - `Builder::increment_each` / `decrement_each`
//! - `Builder::without` / `with_only`
//! - `Builder::where_key` / `where_key_not` / `latest` / `oldest`
//! - `Builder::qualify_column` / `qualify_columns`
//! - `Model::destroy` / `is` / `is_not`
//! - `Model::save_quietly` / `update_quietly` / `delete_quietly`
//! - `Model::update_or_fail` / `delete_or_fail`
//! - `Model::to_array_except` / `to_array_only`
//! - `FirstOrCreate::find_or` / `find_or_new` / `create_or_first`
//! - `without_touching` scope

use suprnova::eloquent::{FirstOrCreate, without_touching};
use suprnova::testing::TestDatabase;
use suprnova::{Model, attrs, model};

#[model(table = "par_users")]
pub struct ParUser {
    pub id: i64,
    pub name: String,
    pub views: i64,
    pub likes: i64,
}

#[model(table = "par_upserts")]
pub struct ParUpsert {
    pub id: i64,
    pub name: String,
    pub views: i64,
}

#[model(table = "par_ts", timestamps)]
pub struct ParTs {
    pub id: i64,
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[model(table = "par_touch_ts", timestamps)]
pub struct ParTouchTs {
    pub id: i64,
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE par_users (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         name TEXT NOT NULL, views INTEGER NOT NULL DEFAULT 0, \
         likes INTEGER NOT NULL DEFAULT 0)",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn sole_succeeds_on_single_match() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let alice = ParUser::create(attrs! { name: "Alice", views: 1, likes: 0 })
        .await
        .unwrap();
    let _ = ParUser::create(attrs! { name: "Bob", views: 2, likes: 0 })
        .await
        .unwrap();

    let only = ParUser::query()
        .filter("name", "Alice")
        .sole()
        .await
        .unwrap();
    assert_eq!(only.id, alice.id);
}

#[tokio::test]
async fn sole_errors_on_multiple_matches() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    ParUser::create(attrs! { name: "Twin", views: 1, likes: 0 })
        .await
        .unwrap();
    ParUser::create(attrs! { name: "Twin", views: 2, likes: 0 })
        .await
        .unwrap();

    let err = ParUser::query()
        .filter("name", "Twin")
        .sole()
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("multiple"));
}

#[tokio::test]
async fn sole_errors_on_zero_matches() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;

    let err = ParUser::query()
        .filter("name", "ghost")
        .sole()
        .await
        .unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("not"));
}

#[tokio::test]
async fn sole_value_returns_typed_column() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    ParUser::create(attrs! { name: "Alice", views: 42, likes: 0 })
        .await
        .unwrap();

    let views: i64 = ParUser::query()
        .filter("name", "Alice")
        .sole_value("views")
        .await
        .unwrap();
    assert_eq!(views, 42);
}

#[tokio::test]
async fn value_or_fail_errors_when_missing() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;

    let err = ParUser::query()
        .filter("name", "ghost")
        .value_or_fail::<i64>("views")
        .await
        .unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("no value"));
}

#[tokio::test]
async fn update_all_runs_set_clause() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    ParUser::create(attrs! { name: "A", views: 1, likes: 0 })
        .await
        .unwrap();
    ParUser::create(attrs! { name: "B", views: 2, likes: 0 })
        .await
        .unwrap();

    let n = ParUser::query()
        .filter_op("views", ">=", 1)
        .update_all(attrs! { likes: 99 })
        .await
        .unwrap();
    assert_eq!(n, 2);
    let rows = ParUser::all().await.unwrap();
    assert!(rows.iter().all(|u| u.likes == 99));
}

#[tokio::test]
async fn delete_all_removes_matching_rows() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    ParUser::create(attrs! { name: "A", views: 1, likes: 0 })
        .await
        .unwrap();
    ParUser::create(attrs! { name: "B", views: 2, likes: 0 })
        .await
        .unwrap();

    let n = ParUser::query()
        .filter("name", "A")
        .delete_all()
        .await
        .unwrap();
    assert_eq!(n, 1);
    let rows = ParUser::all().await.unwrap();
    assert_eq!(rows.len(), 1);
}

#[tokio::test]
async fn increment_each_bumps_columns_atomically() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = ParUser::create(attrs! { name: "A", views: 10, likes: 5 })
        .await
        .unwrap();

    let n = ParUser::query()
        .filter("id", u.id)
        .increment_each(vec![("views", 7), ("likes", 3)])
        .await
        .unwrap();
    assert_eq!(n, 1);
    let fresh = ParUser::find(u.id).await.unwrap().unwrap();
    assert_eq!(fresh.views, 17);
    assert_eq!(fresh.likes, 8);
}

#[tokio::test]
async fn decrement_each_subtracts() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = ParUser::create(attrs! { name: "A", views: 10, likes: 5 })
        .await
        .unwrap();
    let n = ParUser::query()
        .filter("id", u.id)
        .decrement_each(vec![("views", 4)])
        .await
        .unwrap();
    assert_eq!(n, 1);
    let fresh = ParUser::find(u.id).await.unwrap().unwrap();
    assert_eq!(fresh.views, 6);
}

#[tokio::test]
async fn upsert_inserts_then_updates_on_conflict() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    _db.execute_unprepared(
        "CREATE TABLE par_upserts (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, views INTEGER NOT NULL DEFAULT 0)",
    ).await.unwrap();

    // Initial insert.
    let n = ParUpsert::query()
        .upsert(vec![attrs! { name: "Bob", views: 1 }], vec!["name"], None)
        .await
        .unwrap();
    assert!(n >= 1);
    let row = ParUpsert::query()
        .filter("name", "Bob")
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.views, 1);

    // Second upsert with same name updates views.
    let _ = ParUpsert::query()
        .upsert(vec![attrs! { name: "Bob", views: 42 }], vec!["name"], None)
        .await
        .unwrap();
    let row = ParUpsert::query()
        .filter("name", "Bob")
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.views, 42);
}

#[tokio::test]
async fn where_key_filters_by_pk() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let a = ParUser::create(attrs! { name: "A", views: 0, likes: 0 })
        .await
        .unwrap();
    let _ = ParUser::create(attrs! { name: "B", views: 0, likes: 0 })
        .await
        .unwrap();

    let row = ParUser::query()
        .where_key(a.id)
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.name, "A");
}

#[tokio::test]
async fn where_key_not_excludes_pk() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let a = ParUser::create(attrs! { name: "A", views: 0, likes: 0 })
        .await
        .unwrap();
    let _ = ParUser::create(attrs! { name: "B", views: 0, likes: 0 })
        .await
        .unwrap();

    let rows = ParUser::query().where_key_not(a.id).get().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "B");
}

#[tokio::test]
async fn latest_orders_by_created_at_desc() {
    // We use a model with a real created_at column.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    _db.execute_unprepared(
        "CREATE TABLE par_ts (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, \
         created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
    )
    .await
    .unwrap();

    let _a = ParTs::create(attrs! { name: "first" }).await.unwrap();
    // Sleep a millisecond so the timestamps differ.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let _b = ParTs::create(attrs! { name: "second" }).await.unwrap();

    let rows = ParTs::query().latest().get().await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].name, "second");
}

#[tokio::test]
async fn qualify_column_prepends_table() {
    let s = suprnova::Builder::<ParUser>::qualify_column("name");
    assert_eq!(s, "par_users.name");
    let v = suprnova::Builder::<ParUser>::qualify_columns(["name", "views"]);
    assert_eq!(v, vec!["par_users.name", "par_users.views"]);
}

#[tokio::test]
async fn destroy_removes_by_ids() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let a = ParUser::create(attrs! { name: "A", views: 0, likes: 0 })
        .await
        .unwrap();
    let b = ParUser::create(attrs! { name: "B", views: 0, likes: 0 })
        .await
        .unwrap();
    let _c = ParUser::create(attrs! { name: "C", views: 0, likes: 0 })
        .await
        .unwrap();

    let removed = ParUser::destroy(vec![a.id, b.id]).await.unwrap();
    assert_eq!(removed, 2);
    let rows = ParUser::all().await.unwrap();
    assert_eq!(rows.len(), 1);
}

#[tokio::test]
async fn is_and_is_not_compare_by_pk() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let a = ParUser::create(attrs! { name: "A", views: 0, likes: 0 })
        .await
        .unwrap();
    let same = ParUser::find(a.id).await.unwrap().unwrap();
    let b = ParUser::create(attrs! { name: "B", views: 0, likes: 0 })
        .await
        .unwrap();
    assert!(a.is(&same));
    assert!(a.is_not(&b));
}

#[tokio::test]
async fn to_array_except_drops_named() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let a = ParUser::create(attrs! { name: "A", views: 7, likes: 3 })
        .await
        .unwrap();
    let v = a.to_array_except(&["views"]);
    assert!(v.get("views").is_none());
    assert!(v.get("name").is_some());
}

#[tokio::test]
async fn to_array_only_keeps_named() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let a = ParUser::create(attrs! { name: "A", views: 7, likes: 3 })
        .await
        .unwrap();
    let v = a.to_array_only(&["name"]);
    assert_eq!(v.as_object().unwrap().len(), 1);
    assert_eq!(v.get("name").unwrap().as_str().unwrap(), "A");
}

#[tokio::test]
async fn update_or_fail_errors_when_row_gone() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let a = ParUser::create(attrs! { name: "A", views: 0, likes: 0 })
        .await
        .unwrap();
    let snap = a.clone();
    snap.delete().await.unwrap();
    let err = a.update_or_fail(attrs! { name: "Z" }).await.unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("not"));
}

#[tokio::test]
async fn delete_or_fail_errors_when_row_gone() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let a = ParUser::create(attrs! { name: "A", views: 0, likes: 0 })
        .await
        .unwrap();
    let snap = a.clone();
    snap.delete().await.unwrap();
    let err = a.delete_or_fail().await.unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("not"));
}

#[tokio::test]
async fn find_or_runs_fallback_when_missing() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;

    let row = ParUser::find_or(99999i64, || async {
        ParUser::create(attrs! { name: "fallback", views: 0, likes: 0 }).await
    })
    .await
    .unwrap();
    assert_eq!(row.name, "fallback");
}

#[tokio::test]
async fn find_or_new_builds_unsaved_when_missing() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;

    let row = ParUser::find_or_new(99999i64, attrs! { name: "draft", views: 0, likes: 0 })
        .await
        .unwrap();
    assert_eq!(row.name, "draft");
    // Verify it's unsaved: id is 0 (default for i64 PK).
    assert_eq!(row.id, 0);
}

#[tokio::test]
async fn without_eager_drops_relation_from_plan() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    ParUser::create(attrs! { name: "A", views: 0, likes: 0 })
        .await
        .unwrap();

    // No relations declared on ParUser, so `with(["foo"])` records
    // a name but `__eager_load` is a no-op match. The .without call
    // must still strip the name from the plan without erroring.
    let rows = ParUser::query()
        .with(["foo"])
        .without(["foo"])
        .get()
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
}

#[tokio::test]
async fn without_touching_is_a_noop_for_touch_calls() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    _db.execute_unprepared(
        "CREATE TABLE par_touch_ts (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, \
         created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
    )
    .await
    .unwrap();

    use suprnova::Touchable;
    let row = ParTouchTs::create(attrs! { name: "T" }).await.unwrap();
    let before = row.updated_at;

    // Touch inside the scope should not change updated_at.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    without_touching(async { row.touch().await }).await.unwrap();
    let fresh = ParTouchTs::find(row.id).await.unwrap().unwrap();
    assert_eq!(fresh.updated_at, before);

    // Touch outside the scope bumps updated_at.
    row.touch().await.unwrap();
    let fresh = ParTouchTs::find(row.id).await.unwrap().unwrap();
    assert!(fresh.updated_at > before);
}
