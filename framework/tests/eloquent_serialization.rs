//! Phase 10C T6 — Serialization with hidden / visible / appends.
//!
//! Pins the six contracts the task's plan calls out:
//!
//! 1. [`to_array_returns_full_map_by_default`] — the trait default
//!    serialises the whole struct, stripping the macro-injected
//!    `__eager` / `__pivot` scratch fields.
//! 2. [`to_json_returns_serialized_string`] — `to_json` returns the
//!    string form of `to_array`.
//! 3. [`hidden_fields_are_removed_from_to_array`] — declared
//!    `hidden = [...]` columns drop out of the serialised map.
//! 4. [`visible_keeps_only_listed_fields`] — declared
//!    `visible = [...]` columns are the only survivors.
//! 5. [`appends_invoke_accessors_and_inject_into_to_array`] —
//!    `#[suprnova::accessor]`-tagged methods named in `appends = [...]`
//!    are called and their values inserted after the filter passes.
//! 6. [`eager_cache_stays_out_of_serialization`] — the Phase 10B P6
//!    contract holds under the new filter pipeline: eager-loaded
//!    relation rows never leak into the parent's `to_array`.
//!
//! Plus two contract pins beyond the plan that capture invariants
//! easy to regress when someone touches the macro:
//!
//! 7. [`appends_win_over_hidden_when_names_collide`] — when a name
//!    appears in both `hidden` and `appends`, the append wins (it
//!    runs after the hidden strip). Laravel parity.
//! 8. [`collection_to_array_applies_per_row_filters`] — a
//!    `Collection<M>` serialises through each row's `to_array`, so
//!    hidden columns are dropped on every row of the array output
//!    (regression guard for the load-bearing change in
//!    `framework/src/eloquent/collection.rs`).

use suprnova::eloquent::Collection;
use suprnova::testing::TestDatabase;
use suprnova::{Model, accessor, attrs, model};

// ---- Models -------------------------------------------------------------

#[model(table = "t6_basic")]
pub struct T6Basic {
    pub id: i64,
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[model(
    table = "t6_users_hidden",
    hidden = ["password_hash", "ssn"],
)]
pub struct T6HiddenUser {
    pub id: i64,
    pub email: String,
    pub password_hash: String,
    pub ssn: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[model(
    table = "t6_users_visible",
    visible = ["id", "name"],
)]
pub struct T6VisibleUser {
    pub id: i64,
    pub name: String,
    pub internal_notes: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[model(
    table = "t6_users_appends",
    appends = ["full_name", "display_age"],
)]
pub struct T6AppendUser {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
    pub age: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl T6AppendUser {
    #[accessor]
    pub fn full_name(&self) -> String {
        format!("{} {}", self.first_name, self.last_name)
    }

    #[accessor]
    pub fn display_age(&self) -> String {
        format!("{} years old", self.age)
    }
}

/// Collision: `secret` appears in BOTH `hidden` and `appends`. The
/// hidden strip runs first, then the append injects — so the append
/// wins. Matches Laravel.
#[model(
    table = "t6_collide",
    hidden = ["secret"],
    appends = ["secret"],
)]
pub struct T6Collide {
    pub id: i64,
    pub name: String,
    pub secret: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl T6Collide {
    #[accessor]
    pub fn secret(&self) -> String {
        "[redacted]".to_string()
    }
}

#[model(table = "t6_posts", relations = {
    comments: HasMany<T6Comment>,
}, hidden = ["body"])]
pub struct T6Post {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[model(table = "t6_comments")]
pub struct T6Comment {
    pub id: i64,
    pub t6_post_id: i64,
    pub message: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

// ---- Fixtures ------------------------------------------------------------

async fn basic_fixture() -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t6_basic (id INTEGER PRIMARY KEY, name TEXT, created_at TEXT, updated_at TEXT)",
    )
    .await
    .unwrap();
    db
}

async fn hidden_fixture() -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t6_users_hidden (id INTEGER PRIMARY KEY, email TEXT, password_hash TEXT, ssn TEXT, created_at TEXT, updated_at TEXT)",
    )
    .await
    .unwrap();
    db
}

async fn visible_fixture() -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t6_users_visible (id INTEGER PRIMARY KEY, name TEXT, internal_notes TEXT, created_at TEXT, updated_at TEXT)",
    )
    .await
    .unwrap();
    db
}

async fn appends_fixture() -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t6_users_appends (id INTEGER PRIMARY KEY, first_name TEXT, last_name TEXT, age INTEGER, created_at TEXT, updated_at TEXT)",
    )
    .await
    .unwrap();
    db
}

async fn collide_fixture() -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t6_collide (id INTEGER PRIMARY KEY, name TEXT, secret TEXT, created_at TEXT, updated_at TEXT)",
    )
    .await
    .unwrap();
    db
}

async fn posts_fixture() -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t6_posts (id INTEGER PRIMARY KEY, title TEXT, body TEXT, created_at TEXT, updated_at TEXT)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE t6_comments (id INTEGER PRIMARY KEY, t6_post_id INTEGER, message TEXT, created_at TEXT, updated_at TEXT)",
    )
    .await
    .unwrap();
    db
}

// ---- Tests ---------------------------------------------------------------

#[tokio::test]
async fn to_array_returns_full_map_by_default() {
    let _db = basic_fixture().await;
    let m = T6Basic::create(attrs! { name: "alice" }).await.unwrap();
    let arr = m.to_array();
    let map = arr.as_object().unwrap();
    assert!(map.contains_key("id"));
    assert!(map.contains_key("name"));
    assert!(map.contains_key("created_at"));
    assert!(map.contains_key("updated_at"));
    // Trait default explicitly strips the macro-injected scratch fields
    // even though they carry #[serde(skip)] — belt-and-braces against a
    // future hand-rolled Serialize impl.
    assert!(!map.contains_key("__eager"));
    assert!(!map.contains_key("__pivot"));
}

#[tokio::test]
async fn to_json_returns_serialized_string() {
    let _db = basic_fixture().await;
    let m = T6Basic::create(attrs! { name: "alice" }).await.unwrap();
    let json = m.to_json();
    // String shape — to_json delegates to to_array and stringifies.
    assert!(json.contains("\"name\":\"alice\""));
    // Round-trip pin: parsing the string back to a Value yields the
    // same shape as to_array().
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, m.to_array());
}

#[tokio::test]
async fn collection_to_json_empty_returns_valid_array() {
    // Pre-fix `Collection::to_json` returned `""` on a serialise
    // failure. `[]` is the canonical empty-collection JSON; this also
    // guards against the empty-vs-failure path producing different
    // strings for consumers that parse the result downstream.
    let _db = basic_fixture().await;
    let empty = T6Basic::query().get().await.unwrap();
    assert_eq!(empty.to_json(), "[]");
    // try_to_json mirrors the success path; on the (currently
    // impossible) failure path the caller gets the error directly
    // instead of a silent fallback.
    assert_eq!(empty.try_to_json().unwrap(), "[]");
}

#[tokio::test]
async fn hidden_fields_are_removed_from_to_array() {
    let _db = hidden_fixture().await;
    let u = T6HiddenUser::create(attrs! {
        email: "alice@x.com",
        password_hash: "$2y$..",
        ssn: "123-45-6789",
    })
    .await
    .unwrap();

    let arr = u.to_array();
    let m = arr.as_object().unwrap();
    assert!(m.contains_key("email"));
    assert!(m.contains_key("id"));
    assert!(!m.contains_key("password_hash"));
    assert!(!m.contains_key("ssn"));
    // Non-listed columns survive — hidden is a denylist, not a
    // whitelist.
    assert!(m.contains_key("created_at"));
}

#[tokio::test]
async fn visible_keeps_only_listed_fields() {
    let _db = visible_fixture().await;
    let u = T6VisibleUser::create(attrs! {
        name: "alice",
        internal_notes: "loyal customer",
    })
    .await
    .unwrap();

    let arr = u.to_array();
    let m = arr.as_object().unwrap();

    assert!(m.contains_key("id"));
    assert!(m.contains_key("name"));
    assert!(!m.contains_key("internal_notes"));
    // Auto-timestamps are dropped — visible is an exact allowlist.
    assert!(!m.contains_key("created_at"));
    assert!(!m.contains_key("updated_at"));

    // Exactly two keys.
    let mut keys: Vec<&str> = m.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, vec!["id", "name"]);
}

#[tokio::test]
async fn appends_invoke_accessors_and_inject_into_to_array() {
    let _db = appends_fixture().await;
    let u = T6AppendUser::create(attrs! {
        first_name: "Alice",
        last_name: "Liddell",
        age: 30,
    })
    .await
    .unwrap();

    let arr = u.to_array();
    let m = arr.as_object().unwrap();

    assert_eq!(
        m.get("full_name").and_then(|v| v.as_str()),
        Some("Alice Liddell"),
    );
    assert_eq!(
        m.get("display_age").and_then(|v| v.as_str()),
        Some("30 years old"),
    );

    // Base columns still present — appends inject in addition to the
    // base map, they don't replace it.
    assert_eq!(m.get("first_name").and_then(|v| v.as_str()), Some("Alice"));
    assert_eq!(m.get("age").and_then(|v| v.as_i64()), Some(30));
}

#[tokio::test]
async fn eager_cache_stays_out_of_serialization() {
    let _db = posts_fixture().await;

    let p = T6Post::create(attrs! { title: "hello", body: "secret" })
        .await
        .unwrap();
    T6Comment::create(attrs! { t6_post_id: p.id, message: "hi" })
        .await
        .unwrap();

    let loaded = T6Post::query()
        .with(["comments"])
        .first()
        .await
        .unwrap()
        .unwrap();

    let arr = loaded.to_array();
    let m = arr.as_object().unwrap();

    // hidden = ["body"] dropped the body column.
    assert!(!m.contains_key("body"));

    // P6 contract holds even with a populated eager cache.
    assert!(!m.contains_key("__eager"));

    // Loaded relations don't accidentally serialise either. To
    // surface relation data, the user opts in via `appends` +
    // accessor.
    assert!(!m.contains_key("comments"));

    // Loaded title isn't lost (hidden only stripped `body`).
    assert_eq!(m.get("title").and_then(|v| v.as_str()), Some("hello"));
    assert!(m.contains_key("id"));
}

#[tokio::test]
async fn appends_win_over_hidden_when_names_collide() {
    // Both `hidden = ["secret"]` AND `appends = ["secret"]` are
    // declared. The hidden strip runs first (removes the raw column),
    // then the append re-injects with the accessor's transformed
    // value. Laravel parity: `$appends` always serialises.
    let _db = collide_fixture().await;
    let u = T6Collide::create(attrs! { name: "alice", secret: "raw-value" })
        .await
        .unwrap();

    let arr = u.to_array();
    let m = arr.as_object().unwrap();

    // The append wins — the value is the accessor's output, not the
    // raw column value.
    assert_eq!(m.get("secret").and_then(|v| v.as_str()), Some("[redacted]"));
}

#[tokio::test]
async fn collection_to_array_applies_per_row_filters() {
    // Regression guard for collection.rs:to_array. The naive shape
    // (`serde_json::to_value(&self.0)`) bypasses the per-model
    // override and would surface `password_hash` in the array
    // output. The fix routes per-row through Model::to_array.
    let _db = hidden_fixture().await;

    T6HiddenUser::create(attrs! {
        email: "alice@x.com",
        password_hash: "$2y$..a",
        ssn: "1",
    })
    .await
    .unwrap();
    T6HiddenUser::create(attrs! {
        email: "bob@x.com",
        password_hash: "$2y$..b",
        ssn: "2",
    })
    .await
    .unwrap();

    let rows: Collection<T6HiddenUser> = T6HiddenUser::all().await.unwrap();
    let arr = rows.to_array();
    let elements = arr.as_array().expect("collection to_array is an Array");
    assert_eq!(elements.len(), 2);
    for elem in elements {
        let obj = elem.as_object().expect("each element is a map");
        // hidden filter propagated to every row.
        assert!(
            !obj.contains_key("password_hash"),
            "collection element leaked password_hash: {elem}",
        );
        assert!(
            !obj.contains_key("ssn"),
            "collection element leaked ssn: {elem}",
        );
        // Real column survives.
        assert!(obj.contains_key("email"));
    }
}
