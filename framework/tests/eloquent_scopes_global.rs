//! Phase 10C T4 — global scopes via `GlobalScope<M>` + `ScopeRegistry`.
//!
//! Pins:
//!
//! 1. **Static scope applies automatically**: register at boot, every
//!    `Model::query()` call runs the closure.
//! 2. **Opt out by type**: `Model::without_global_scope::<S>()` masks
//!    one registered scope, the others still apply.
//! 3. **Opt out of all**: `Model::without_global_scopes()` bypasses
//!    the registry entirely.
//! 4. **Registration order matters**: scopes apply in the order
//!    they were registered.
//! 5. **`find` / `all` bypass the registry**: PK lookups go through
//!    SeaORM directly, matching Laravel's `Eloquent\Model::find`.
//!
//! Each test uses a unique model type (`T4Article`, `T4Order`,
//! `T4Audit`, `T4Tagged`, `T4Pk`) so the process-global
//! `ScopeRegistry` doesn't bleed between tests.

use std::sync::atomic::{AtomicI64, Ordering};

use suprnova::eloquent::scopes::{GlobalScope, ScopeRegistry};
use suprnova::testing::TestDatabase;
use suprnova::{Builder, Model};

// ============================================================
// Test 1: basic registration + automatic application.
// ============================================================

#[suprnova::model(table = "t4_articles")]
pub struct T4Article {
    pub id: i64,
    pub tenant_id: i64,
    pub title: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

static T4_CURRENT_TENANT: AtomicI64 = AtomicI64::new(0);

pub struct T4TenantScope;

impl GlobalScope<T4Article> for T4TenantScope {
    fn apply(&self, query: Builder<T4Article>) -> Builder<T4Article> {
        query.filter("tenant_id", T4_CURRENT_TENANT.load(Ordering::SeqCst))
    }
}

async fn t4_article_fixture() -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t4_articles (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            tenant_id INTEGER NOT NULL, \
            title TEXT NOT NULL, \
            created_at TEXT NOT NULL, \
            updated_at TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();

    T4Article::create(suprnova::attrs! { tenant_id: 1, title: "t1-a" })
        .await
        .unwrap();
    T4Article::create(suprnova::attrs! { tenant_id: 1, title: "t1-b" })
        .await
        .unwrap();
    T4Article::create(suprnova::attrs! { tenant_id: 2, title: "t2-a" })
        .await
        .unwrap();

    db
}

#[tokio::test]
async fn global_scope_filters_query_results() {
    let _db = t4_article_fixture().await;
    ScopeRegistry::register::<T4Article, _>(T4TenantScope);

    T4_CURRENT_TENANT.store(1, Ordering::SeqCst);
    let rows = T4Article::query().get().await.unwrap();
    assert_eq!(rows.len(), 2);

    T4_CURRENT_TENANT.store(2, Ordering::SeqCst);
    let rows = T4Article::query().get().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "t2-a");
}

// ============================================================
// Test 2: opt out by scope type.
// ============================================================

#[suprnova::model(table = "t4_orders")]
pub struct T4Order {
    pub id: i64,
    pub tenant_id: i64,
    pub title: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

static T4_ORDER_TENANT: AtomicI64 = AtomicI64::new(0);

pub struct T4OrderTenantScope;

impl GlobalScope<T4Order> for T4OrderTenantScope {
    fn apply(&self, query: Builder<T4Order>) -> Builder<T4Order> {
        query.filter("tenant_id", T4_ORDER_TENANT.load(Ordering::SeqCst))
    }
}

pub struct T4OrderDraftScope;

impl GlobalScope<T4Order> for T4OrderDraftScope {
    fn apply(&self, query: Builder<T4Order>) -> Builder<T4Order> {
        // Excludes any row whose title starts with "t1".
        query.filter_not_like("title", "t1%")
    }
}

#[tokio::test]
async fn without_global_scope_by_type_excludes_only_that_scope() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t4_orders (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            tenant_id INTEGER NOT NULL, \
            title TEXT NOT NULL, \
            created_at TEXT NOT NULL, \
            updated_at TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
    T4Order::create(suprnova::attrs! { tenant_id: 1, title: "t1-a" })
        .await
        .unwrap();
    T4Order::create(suprnova::attrs! { tenant_id: 1, title: "t1-b" })
        .await
        .unwrap();
    T4Order::create(suprnova::attrs! { tenant_id: 2, title: "t2-a" })
        .await
        .unwrap();

    ScopeRegistry::register::<T4Order, _>(T4OrderTenantScope);
    ScopeRegistry::register::<T4Order, _>(T4OrderDraftScope);

    T4_ORDER_TENANT.store(1, Ordering::SeqCst);

    // Both scopes apply — tenant=1 AND title NOT LIKE 't1%' → 0 rows
    // because every tenant-1 row is titled 't1-*'.
    let with_both = T4Order::query().get().await.unwrap();
    assert_eq!(with_both.len(), 0);

    // Opt out of DraftScope — tenant=1 alone returns 2 rows.
    let no_draft = T4Order::without_global_scope::<T4OrderDraftScope>()
        .get()
        .await
        .unwrap();
    assert_eq!(no_draft.len(), 2);
    let mut no_draft_titles: Vec<_> = no_draft.iter().map(|r| r.title.clone()).collect();
    no_draft_titles.sort();
    assert_eq!(no_draft_titles, vec!["t1-a", "t1-b"]);

    // Opt out of TenantScope — DraftScope still applies, so the only
    // surviving row is the tenant=2 one titled 't2-a'.
    let no_tenant = T4Order::without_global_scope::<T4OrderTenantScope>()
        .get()
        .await
        .unwrap();
    assert_eq!(no_tenant.len(), 1);
    assert_eq!(no_tenant[0].title, "t2-a");
}

// ============================================================
// Test 3: opt out of every scope at once.
// ============================================================

#[suprnova::model(table = "t4_audits")]
pub struct T4Audit {
    pub id: i64,
    pub tenant_id: i64,
    pub title: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub struct T4AuditTenantScope;

impl GlobalScope<T4Audit> for T4AuditTenantScope {
    fn apply(&self, query: Builder<T4Audit>) -> Builder<T4Audit> {
        // Hard-coded tenant=1 filter; doesn't matter for this test —
        // we just need a registered scope so without_global_scopes()
        // has something to bypass.
        query.filter("tenant_id", 1)
    }
}

#[tokio::test]
async fn without_global_scopes_excludes_all() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t4_audits (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            tenant_id INTEGER NOT NULL, \
            title TEXT NOT NULL, \
            created_at TEXT NOT NULL, \
            updated_at TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
    T4Audit::create(suprnova::attrs! { tenant_id: 1, title: "a" })
        .await
        .unwrap();
    T4Audit::create(suprnova::attrs! { tenant_id: 1, title: "b" })
        .await
        .unwrap();
    T4Audit::create(suprnova::attrs! { tenant_id: 2, title: "c" })
        .await
        .unwrap();

    ScopeRegistry::register::<T4Audit, _>(T4AuditTenantScope);

    // Default query honours the registered scope → tenant=1 only.
    let scoped = T4Audit::query().get().await.unwrap();
    assert_eq!(scoped.len(), 2);

    // without_global_scopes() bypasses every registered scope.
    let all = T4Audit::without_global_scopes().get().await.unwrap();
    assert_eq!(all.len(), 3);
}

// ============================================================
// Test 4: registration order is application order.
//
// Uses `limit` as the order-sensitive observable: SeaORM's limit is
// last-write-wins, so registering ScopeLimit3 then ScopeLimit1 caps
// the result at 1, while the opposite order caps at 3. AND-composed
// filters alone wouldn't pin this — they're commutative — so this
// test wouldn't catch a regression that swapped `Vec` storage for
// a `HashMap` and re-introduced non-deterministic iteration.
// ============================================================

#[suprnova::model(table = "t4_tagged")]
pub struct T4Tagged {
    pub id: i64,
    pub tenant_id: i64,
    pub title: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[suprnova::model(table = "t4_tagged_reverse")]
pub struct T4TaggedReverse {
    pub id: i64,
    pub tenant_id: i64,
    pub title: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub struct ScopeLimit3;
pub struct ScopeLimit1;

impl GlobalScope<T4Tagged> for ScopeLimit3 {
    fn apply(&self, query: Builder<T4Tagged>) -> Builder<T4Tagged> {
        query.limit(3)
    }
}

impl GlobalScope<T4Tagged> for ScopeLimit1 {
    fn apply(&self, query: Builder<T4Tagged>) -> Builder<T4Tagged> {
        query.limit(1)
    }
}

pub struct ScopeRevLimit3;
pub struct ScopeRevLimit1;

impl GlobalScope<T4TaggedReverse> for ScopeRevLimit3 {
    fn apply(&self, query: Builder<T4TaggedReverse>) -> Builder<T4TaggedReverse> {
        query.limit(3)
    }
}

impl GlobalScope<T4TaggedReverse> for ScopeRevLimit1 {
    fn apply(&self, query: Builder<T4TaggedReverse>) -> Builder<T4TaggedReverse> {
        query.limit(1)
    }
}

#[tokio::test]
async fn scopes_apply_in_registration_order() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t4_tagged (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            tenant_id INTEGER NOT NULL, \
            title TEXT NOT NULL, \
            created_at TEXT NOT NULL, \
            updated_at TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE t4_tagged_reverse (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            tenant_id INTEGER NOT NULL, \
            title TEXT NOT NULL, \
            created_at TEXT NOT NULL, \
            updated_at TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
    for i in 0..5 {
        let title = format!("row-{i}");
        T4Tagged::create(suprnova::attrs! { tenant_id: 1, title: title.clone() })
            .await
            .unwrap();
        T4TaggedReverse::create(suprnova::attrs! { tenant_id: 1, title: title })
            .await
            .unwrap();
    }

    // Order A: register limit(3) first, limit(1) second → limit(1) wins
    // → expect 1 row.
    ScopeRegistry::register::<T4Tagged, _>(ScopeLimit3);
    ScopeRegistry::register::<T4Tagged, _>(ScopeLimit1);
    let rows = T4Tagged::query().get().await.unwrap();
    assert_eq!(
        rows.len(),
        1,
        "limit(1) registered LAST should win — last-write-wins on Builder::limit",
    );

    // Order B (on a sibling model with separate registry entry):
    // register limit(1) first, limit(3) second → limit(3) wins
    // → expect 3 rows.
    ScopeRegistry::register::<T4TaggedReverse, _>(ScopeRevLimit1);
    ScopeRegistry::register::<T4TaggedReverse, _>(ScopeRevLimit3);
    let rows = T4TaggedReverse::query().get().await.unwrap();
    assert_eq!(
        rows.len(),
        3,
        "limit(3) registered LAST should win — proves order is preserved both ways",
    );
}

// ============================================================
// Test 5: PK lookup paths bypass the registry.
//
// Pins the locked decision: global scopes apply through `query()`,
// not through `find` / `find_many` / `all`. Matches Laravel — those
// PK paths go through SeaORM directly.
// ============================================================

#[suprnova::model(table = "t4_pk")]
pub struct T4Pk {
    pub id: i64,
    pub tenant_id: i64,
    pub title: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub struct T4PkTenantScope;

impl GlobalScope<T4Pk> for T4PkTenantScope {
    fn apply(&self, query: Builder<T4Pk>) -> Builder<T4Pk> {
        // Locked to tenant=1 so any row reachable via this scope is a
        // tenant=1 row only.
        query.filter("tenant_id", 1)
    }
}

// ============================================================
// Test 6: soft-deletes + global scopes coexist correctly.
//
// Soft-deletes use a separate (string-tag) bypass mechanism, not
// the typed registry. Verify both apply on `query()` and that
// `without_global_scopes()` only drops registry scopes — soft-
// delete filtering is preserved.
// ============================================================

#[suprnova::model(table = "t4_soft_articles", soft_deletes)]
pub struct T4SoftArticle {
    pub id: i64,
    pub tenant_id: i64,
    pub title: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub struct T4SoftTenantScope;

impl GlobalScope<T4SoftArticle> for T4SoftTenantScope {
    fn apply(&self, query: Builder<T4SoftArticle>) -> Builder<T4SoftArticle> {
        query.filter("tenant_id", 1)
    }
}

#[tokio::test]
async fn soft_deletes_and_global_scopes_coexist() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t4_soft_articles (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            tenant_id INTEGER NOT NULL, \
            title TEXT NOT NULL, \
            created_at TEXT NOT NULL, \
            updated_at TEXT NOT NULL, \
            deleted_at TEXT NULL\
         )",
    )
    .await
    .unwrap();

    let live_t1 = T4SoftArticle::create(suprnova::attrs! { tenant_id: 1, title: "live-t1" })
        .await
        .unwrap();
    let trashed_t1 = T4SoftArticle::create(suprnova::attrs! { tenant_id: 1, title: "trashed-t1" })
        .await
        .unwrap();
    let _live_t2 = T4SoftArticle::create(suprnova::attrs! { tenant_id: 2, title: "live-t2" })
        .await
        .unwrap();

    // Trash one tenant-1 row.
    trashed_t1.delete().await.unwrap();

    ScopeRegistry::register::<T4SoftArticle, _>(T4SoftTenantScope);

    // query() honours BOTH the soft-delete filter AND the tenant scope:
    // tenant=1 AND deleted_at IS NULL → only `live-t1`.
    let scoped = T4SoftArticle::query().get().await.unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].id, live_t1.id);

    // without_global_scopes() drops the typed registry scope only —
    // the soft-delete filter still applies. Expect both live rows
    // (both tenants), but NOT the trashed tenant-1 row.
    let no_typed_scopes = T4SoftArticle::without_global_scopes().get().await.unwrap();
    assert_eq!(no_typed_scopes.len(), 2);
    let mut titles: Vec<_> = no_typed_scopes.iter().map(|r| r.title.clone()).collect();
    titles.sort();
    assert_eq!(titles, vec!["live-t1", "live-t2"]);
}

#[tokio::test]
async fn find_and_all_bypass_global_scopes() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t4_pk (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            tenant_id INTEGER NOT NULL, \
            title TEXT NOT NULL, \
            created_at TEXT NOT NULL, \
            updated_at TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
    let row1 = T4Pk::create(suprnova::attrs! { tenant_id: 1, title: "tenant-1" })
        .await
        .unwrap();
    let row2 = T4Pk::create(suprnova::attrs! { tenant_id: 1, title: "still-tenant-1" })
        .await
        .unwrap();
    let row3 = T4Pk::create(suprnova::attrs! { tenant_id: 2, title: "tenant-2" })
        .await
        .unwrap();

    ScopeRegistry::register::<T4Pk, _>(T4PkTenantScope);

    // Sanity check: through query() the scope DOES apply (tenant=1
    // only, so row3 is invisible).
    let via_query = T4Pk::query().get().await.unwrap();
    assert_eq!(via_query.len(), 2);
    assert!(via_query.iter().all(|r| r.tenant_id == 1));

    // find() goes through SeaORM directly — global scope does NOT
    // apply. The tenant=2 row IS reachable by its primary key.
    let by_id = T4Pk::find(row3.id).await.unwrap();
    assert!(by_id.is_some(), "find() must reach tenant=2 row by PK");
    assert_eq!(by_id.unwrap().tenant_id, 2);

    // all() goes through SeaORM directly too — should return every row.
    let everything = T4Pk::all().await.unwrap();
    assert_eq!(everything.len(), 3);

    // find_many() also bypasses scopes — returns rows for both
    // tenants when PKs span them.
    let many = T4Pk::find_many([row1.id, row2.id, row3.id]).await.unwrap();
    assert_eq!(many.len(), 3);
}
