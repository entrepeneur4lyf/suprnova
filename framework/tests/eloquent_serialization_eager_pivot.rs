//! Phase 10B P6 — serialization audit for the auto-injected
//! `__eager` and `__pivot` fields.
//!
//! Every `#[suprnova::model]` struct gains two private slots:
//!
//! - `__eager: EagerLoadCache` — eager-loaded relation rows / counts /
//!   aggregates.
//! - `__pivot: Option<Arc<dyn Any + Send + Sync>>` — pivot row when
//!   loaded via `BelongsToMany`.
//!
//! Both carry `#[serde(skip)]`, so the trait-derived `Serialize` impl
//! drops them before any downstream serializer sees them. This file
//! pins that the exclusion holds across every user-facing serialization
//! path Phase 10A wired up:
//!
//! - `to_array()` — Laravel-shape JSON object emitter (returns
//!   `serde_json::Value`).
//! - `to_json()` — string form, returns `serde_json::to_string(&to_array())`.
//! - The `hidden = [...]` / `visible = [...]` / `appends = [...]`
//!   visibility filters.
//! - Real-world loaded state: parent rows after `with([...])`, related
//!   rows with `__pivot` stamped by a m2m-style loader.
//!
//! Phase 10C T6 moved both methods from inherent `impl #struct_ident`
//! blocks onto the [`Model`] trait. `to_array` is the primary surface;
//! `to_json` delegates to it. The trait default explicitly removes
//! `__eager` / `__pivot` keys (belt-and-braces on top of `#[serde(skip)]`)
//! so this contract holds even against a future hand-rolled `Serialize`
//! impl. These tests pin both layers of the exclusion.

use std::sync::Arc;

use suprnova::testing::TestDatabase;
use suprnova::{attrs, model, EagerLoadCache, Model};

// ---- Models -------------------------------------------------------------
//
// Models are declared at module scope (NOT inside test fns) — the macro
// emits an inner module whose `use super::*;` only sees the file's
// top-level imports. See `eloquent_accessors.rs` for the same
// constraint.

/// Parent with a HasMany relation. Used to exercise the "loaded
/// eager rows must not leak into the parent's JSON" path.
#[model(table = "ser_users", relations = {
    posts: HasMany<SerPost>,
})]
pub struct SerUser {
    pub id: i64,
    pub name: String,
    pub email: String,
}

#[model(table = "ser_posts")]
pub struct SerPost {
    pub id: i64,
    pub ser_user_id: i64,
    pub title: String,
}

/// Visibility-filtered model with both `hidden` and `appends`. Pins
/// that `hidden` need not enumerate the framework's auto-injected
/// fields — the `#[serde(skip)]` exclusion runs before the filter
/// touches the JSON map.
#[model(
    table = "ser_visible_filtered",
    timestamps = false,
    hidden = ["password"],
    appends = ["display_name"],
    fillable = ["name", "password"],
)]
pub struct SerHiddenUser {
    pub id: i64,
    pub name: String,
    pub password: String,
}

impl SerHiddenUser {
    #[suprnova::accessor]
    pub fn display_name(&self) -> String {
        format!("@{}", self.name)
    }
}

/// Allowlist-filtered model. Pins that the `visible` allowlist drops
/// non-listed columns and that `__eager` / `__pivot` are absent even
/// though the user didn't (and shouldn't have to) list them.
#[model(
    table = "ser_allowlist",
    timestamps = false,
    visible = ["id", "name"],
    fillable = ["name", "secret"],
)]
pub struct SerVisibleUser {
    pub id: i64,
    pub name: String,
    pub secret: String,
}

// ---- Migrations ---------------------------------------------------------

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE ser_users (\
         id INTEGER PRIMARY KEY AUTOINCREMENT, \
         name TEXT NOT NULL, \
         email TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE ser_posts (\
         id INTEGER PRIMARY KEY AUTOINCREMENT, \
         ser_user_id INTEGER NOT NULL, \
         title TEXT NOT NULL)",
    )
    .await
    .unwrap();
}

// ---- Tests --------------------------------------------------------------

/// Pin: a freshly-instantiated model produces EXACTLY the user's
/// declared columns — neither `__eager` nor `__pivot` appears in the
/// output JSON, and no other extra key sneaks in either.
///
/// Stronger than the existing smoke test in
/// `eloquent_macro_smoke_relations.rs`: that test only asserts
/// `__eager` / `__pivot` absent. This one also asserts the JSON's key
/// set matches the user's declared field set exactly — anything the
/// macro might add later would fail this.
#[tokio::test]
async fn to_json_excludes_eager_and_pivot_fields() {
    let u = SerUser {
        id: 1,
        name: "Alice".into(),
        email: "a@x.test".into(),
        __eager: EagerLoadCache::new(),
        __pivot: None,
    };

    let v = u.to_array();
    let obj = v
        .as_object()
        .expect("to_array must produce a JSON object");

    // Auto-injected fields must be absent.
    assert!(
        !obj.contains_key("__eager"),
        "__eager must be excluded from to_array — got {v}",
    );
    assert!(
        !obj.contains_key("__pivot"),
        "__pivot must be excluded from to_json — got {v}",
    );

    // The user's declared columns ARE present.
    assert_eq!(obj["id"], 1);
    assert_eq!(obj["name"], "Alice");
    assert_eq!(obj["email"], "a@x.test");

    // Exactly the user's declared fields. If the macro accidentally
    // adds a new key in a future refactor, this fires.
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["email", "id", "name"],
        "to_json must produce exactly the user's declared fields",
    );
}

/// Pin: an eager-loaded parent's `to_json()` does NOT leak the loaded
/// child rows. The eager cache lives on `__eager` (which is
/// `#[serde(skip)]`), so the parent's JSON output is unchanged from
/// the un-loaded shape. To surface a relation in JSON, the user must
/// opt in via `appends = [...]`.
#[tokio::test]
async fn to_json_does_not_leak_loaded_relations() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;

    // Seed: one user + two posts.
    let u = SerUser::create(attrs! { name: "Alice", email: "a@x.test" })
        .await
        .unwrap();
    let _ = SerPost::create(attrs! { ser_user_id: u.id, title: "post-1" })
        .await
        .unwrap();
    let _ = SerPost::create(attrs! { ser_user_id: u.id, title: "post-2" })
        .await
        .unwrap();

    // Eager-load posts. The cache is populated on each row's
    // `__eager` slot.
    let users = SerUser::with(["posts"]).get().await.unwrap();
    let loaded = users.iter().find(|x| x.name == "Alice").unwrap();

    // Sanity: the cache really is populated (so the leak check below
    // isn't trivially passing for the wrong reason).
    assert_eq!(
        loaded.posts_loaded().len(),
        2,
        "fixture broken: expected 2 posts in the eager cache",
    );

    let v = loaded.to_array();
    let obj = v.as_object().expect("to_array must be an object");

    // The auto-injected slots are absent.
    assert!(!obj.contains_key("__eager"), "got: {v}");
    assert!(!obj.contains_key("__pivot"), "got: {v}");

    // The relation NAME ("posts") must not appear under any guise —
    // not as a top-level key, not as anything else. To surface
    // related rows in JSON the user opts in explicitly via
    // `appends = ["posts"]` + an accessor that reads
    // `self.posts_loaded()`.
    assert!(
        !obj.contains_key("posts"),
        "loaded relation `posts` must not bleed into to_array — got {v}",
    );

    // The post titles must not appear anywhere in the JSON string
    // representation — catches any accidental nested-relation leak
    // that a key-only check would miss.
    let s = v.to_string();
    assert!(
        !s.contains("post-1") && !s.contains("post-2"),
        "post titles leaked into parent to_array: {s}",
    );

    // The parent's own fields are still there.
    assert_eq!(obj["id"], u.id);
    assert_eq!(obj["name"], "Alice");
    assert_eq!(obj["email"], "a@x.test");
}

/// Pin: `to_array()` honours the `__eager` / `__pivot` exclusion, and
/// `to_json()` is its serialised-string counterpart. Phase 10C T6
/// moved both methods onto the [`Model`] trait — `to_array()` returns
/// the `Value`, `to_json()` returns `serde_json::to_string(&to_array())`.
/// This test pins both the auto-exclusion AND the to_json-as-stringified-
/// to_array contract.
#[tokio::test]
async fn to_array_excludes_eager_and_pivot_fields() {
    let u = SerUser {
        id: 7,
        name: "Bob".into(),
        email: "b@x.test".into(),
        __eager: EagerLoadCache::new(),
        __pivot: None,
    };

    let v = u.to_array();
    let obj = v.as_object().expect("to_array must produce an object");

    assert!(!obj.contains_key("__eager"));
    assert!(!obj.contains_key("__pivot"));

    // `to_json()` returns the string form of `to_array()` — pinning
    // the delegation so both surfaces stay coherent.
    assert_eq!(u.to_json(), serde_json::to_string(&v).unwrap());
}

/// Pin: when the user declares `hidden = ["password"]`, they don't
/// need to also list `__eager` / `__pivot` — those are auto-excluded
/// at the serde layer before the `hidden` filter runs. The two
/// exclusion mechanisms compose without the user having to know
/// about the framework's internal field names.
#[tokio::test]
async fn hidden_attribute_does_not_need_to_exclude_eager_explicitly() {
    let u = SerHiddenUser {
        id: 1,
        name: "Alice".into(),
        password: "shh".into(),
        __eager: EagerLoadCache::new(),
        __pivot: None,
    };

    let v = u.to_array();
    let obj = v.as_object().expect("to_array must produce an object");

    // The user's `hidden = ["password"]` did its job.
    assert!(
        !obj.contains_key("password"),
        "hidden field must be excluded from to_array — got {v}",
    );

    // The framework's auto-injected fields are excluded too — even
    // though the user didn't enumerate them in `hidden`.
    assert!(!obj.contains_key("__eager"));
    assert!(!obj.contains_key("__pivot"));

    // The appended accessor still fires — appends bypass the
    // hidden/visible filter (matches Laravel).
    assert_eq!(obj["display_name"], "@Alice");

    // The other real columns are still there.
    assert_eq!(obj["id"], 1);
    assert_eq!(obj["name"], "Alice");
}

/// Pin: the `visible = [...]` allowlist drops every column not
/// listed. The auto-injected `__eager` / `__pivot` are absent even
/// though they weren't enumerated — the user's allowlist controls
/// user-facing columns only, the framework's internal fields are
/// excluded at the serde layer.
#[tokio::test]
async fn visible_allowlist_does_not_need_to_exclude_eager_explicitly() {
    let u = SerVisibleUser {
        id: 9,
        name: "Carol".into(),
        secret: "private".into(),
        __eager: EagerLoadCache::new(),
        __pivot: None,
    };

    let v = u.to_array();
    let obj = v.as_object().expect("to_array must produce an object");

    // Allowlist did its job.
    assert_eq!(obj["id"], 9);
    assert_eq!(obj["name"], "Carol");
    assert!(
        !obj.contains_key("secret"),
        "visible allowlist must drop non-listed columns",
    );

    // Auto-injected fields are absent — user didn't have to list
    // them.
    assert!(!obj.contains_key("__eager"));
    assert!(!obj.contains_key("__pivot"));

    // Exactly the allowlisted keys, nothing else.
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, vec!["id", "name"]);
}

/// Pin: a `__pivot` slot loaded with a real `Arc<dyn Any + Send +
/// Sync>` (the shape `BelongsToMany` loaders produce on every related
/// row) is still excluded from `to_json()`. The existing smoke test
/// in `eloquent_macro_smoke_relations.rs` covers only the `None`
/// case; this one closes the `Some(_)` half of the matrix.
#[tokio::test]
async fn to_json_excludes_populated_pivot_slot() {
    /// Stand-in pivot type — any `'static + Send + Sync` value will
    /// do, since the `__pivot` slot only constrains those bounds.
    #[derive(Debug)]
    #[allow(dead_code)]
    struct FakePivot {
        assigned_at: i64,
        role: &'static str,
    }

    let pivot: Arc<dyn std::any::Any + Send + Sync> = Arc::new(FakePivot {
        assigned_at: 123,
        role: "admin",
    });

    let u = SerUser {
        id: 5,
        name: "Dan".into(),
        email: "d@x.test".into(),
        __eager: EagerLoadCache::new(),
        __pivot: Some(pivot),
    };

    let v = u.to_array();
    let obj = v.as_object().expect("to_array must produce an object");

    // Populated pivot is still excluded.
    assert!(
        !obj.contains_key("__pivot"),
        "populated __pivot must still be excluded from to_array — got {v}",
    );

    // And the pivot's payload field names / values don't leak via
    // any other path.
    let s = v.to_string();
    assert!(
        !s.contains("admin") && !s.contains("assigned_at"),
        "pivot payload leaked into to_json: {s}",
    );

    // Real columns intact.
    assert_eq!(obj["id"], 5);
    assert_eq!(obj["name"], "Dan");
}
