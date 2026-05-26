//! Phase 10C T3 — local scopes via `#[suprnova::scopes(Model)]`.
//!
//! Pins the four shipped behaviours plus the non-scope passthrough:
//!
//! 1. **Static entry point**: `T3User::active()` starts a builder
//!    pre-filtered to active rows.
//! 2. **Chainable extension**: `T3User::query().filter_op(...).active()`
//!    composes after an existing filter clause.
//! 3. **Extra args thread through**: `T3User::popular(500)` and
//!    `Builder.popular(500)` both bind the `threshold` parameter.
//! 4. **Scopes chain**: `T3User::active().popular(500).get()` — the
//!    static helper returns a builder, the trait extension carries the
//!    second scope onto it.
//! 5. **Non-scope methods pass through**: an ordinary `&self` method
//!    declared in the same `impl` block keeps compiling and stays
//!    callable on instances — proving the macro's signature filter is
//!    strict.

use suprnova::Model;
use suprnova::testing::TestDatabase;

#[suprnova::model(table = "t3_users")]
pub struct T3User {
    pub id: i64,
    pub name: String,
    pub active: bool,
    pub followers_count: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[suprnova::scopes(T3User)]
impl T3User {
    /// Scope: active users.
    pub fn active(query: suprnova::Builder<Self>) -> suprnova::Builder<Self> {
        query.filter("active", true)
    }

    /// Scope: users with more than `threshold` followers.
    pub fn popular(query: suprnova::Builder<Self>, threshold: i64) -> suprnova::Builder<Self> {
        query.filter_op("followers_count", ">", threshold)
    }

    /// Non-scope method — shape doesn't match the scope signature
    /// (first arg is `&self`, return is `String`). Must pass through
    /// the macro unchanged so it stays callable on instances.
    pub fn display_name(&self) -> String {
        self.name.clone()
    }
}

async fn fixture() -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t3_users (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            name TEXT NOT NULL, \
            active INTEGER NOT NULL, \
            followers_count INTEGER NOT NULL, \
            created_at TEXT NOT NULL, \
            updated_at TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();

    T3User::create(suprnova::attrs! { name: "alice", active: true, followers_count: 1000 })
        .await
        .unwrap();
    T3User::create(suprnova::attrs! { name: "bob", active: true, followers_count: 50 })
        .await
        .unwrap();
    T3User::create(suprnova::attrs! { name: "carol", active: false, followers_count: 9000 })
        .await
        .unwrap();
    T3User::create(suprnova::attrs! { name: "dave", active: false, followers_count: 10 })
        .await
        .unwrap();

    db
}

#[tokio::test]
async fn static_scope_returns_builder() {
    let _db = fixture().await;
    let users = T3User::active().get().await.unwrap();
    let names: Vec<_> = users.iter().map(|u| u.name.clone()).collect();
    assert_eq!(names, vec!["alice", "bob"]);
}

#[tokio::test]
async fn builder_scope_chains_after_filter() {
    let _db = fixture().await;
    let users = T3User::query()
        .filter_op("followers_count", ">", 100)
        .active()
        .get()
        .await
        .unwrap();
    let names: Vec<_> = users.iter().map(|u| u.name.clone()).collect();
    assert_eq!(names, vec!["alice"]);
}

#[tokio::test]
async fn scope_with_arg_passes_through() {
    let _db = fixture().await;
    let users = T3User::popular(500).get().await.unwrap();
    let names: Vec<_> = users.iter().map(|u| u.name.clone()).collect();
    assert_eq!(names, vec!["alice", "carol"]);
}

#[tokio::test]
async fn scopes_compose_via_chain() {
    let _db = fixture().await;
    let users = T3User::active().popular(500).get().await.unwrap();
    let names: Vec<_> = users.iter().map(|u| u.name.clone()).collect();
    assert_eq!(names, vec!["alice"]);
}

#[tokio::test]
async fn non_scope_method_passes_through() {
    let _db = fixture().await;
    // The same impl block holds both scopes AND a non-scope method.
    // If the macro's signature filter were sloppy it would either fail
    // to compile (renaming `display_name` to `__scope_display_name`
    // would orphan the `&self` body) or hide the method behind a
    // mangled name; either outcome means this assertion never reaches
    // here.
    let alice = T3User::active().first().await.unwrap().unwrap();
    assert_eq!(alice.display_name(), "alice");
}
