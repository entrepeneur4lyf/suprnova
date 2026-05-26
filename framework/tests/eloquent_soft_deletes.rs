//! Phase 10A T10 — Soft deletes + Prunable / MassPrunable + `model:prune`.
//!
//! `#[model(soft_deletes)]` enables tombstone semantics:
//! - `delete()` → UPDATE `deleted_at = NOW()` (no row removal)
//! - `force_delete()` → DELETE FROM
//! - `restore()` → UPDATE `deleted_at = NULL`
//! - `trashed()` → bool accessor
//! - Default query scope hides trashed rows; `with_trashed()` /
//!   `only_trashed()` opt out / opt in.
//!
//! `Prunable` + `MassPrunable` traits + `#[suprnova::prunable]` register
//! a type into the inventory-backed `PrunerEntry` registry. The
//! `prune_all` / `prune_all_dry` / `prune_one` runners walk that
//! registry; the `model:prune` console command exposes it on the CLI.
//!
//! Cross-test isolation note: `inventory::iter` is process-wide. Every
//! `#[prunable]` impl in this binary is visible to every test, so
//! invoking `prune_all` without setting up ALL tables would error on a
//! missing table. The per-pruner tests use `prune_one` for isolation;
//! one dedicated `prune_all` test below sets up BOTH tables explicitly.

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use suprnova::eloquent::{MassPrunable, Prunable};
use suprnova::testing::TestDatabase;
use suprnova::{Model, attrs, model};

// ---- Soft deletes -------------------------------------------------------

#[model(table = "t10_users", soft_deletes, fillable = ["name", "email"])]
pub struct T10User {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub deleted_at: Option<DateTime<Utc>>,
}

async fn migrate_users(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE t10_users (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            name TEXT, \
            email TEXT, \
            deleted_at TEXT\
         )",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn delete_marks_deleted_at_does_not_remove() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate_users(&db).await;

    let u = T10User::create(attrs! { name: "Alice", email: "a@x.com" })
        .await
        .unwrap();
    let id = u.id;
    u.delete().await.unwrap();

    // Default scope hides trashed rows.
    assert!(T10User::find(id).await.unwrap().is_none());

    // with_trashed sees it.
    let trashed = T10User::with_trashed()
        .filter("id", id)
        .first()
        .await
        .unwrap();
    assert!(trashed.is_some());
    assert!(trashed.unwrap().deleted_at.is_some());
}

#[tokio::test]
async fn force_delete_actually_removes() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate_users(&db).await;
    let u = T10User::create(attrs! { name: "A", email: "a@x.com" })
        .await
        .unwrap();
    let id = u.id;
    u.force_delete().await.unwrap();
    assert!(
        T10User::with_trashed()
            .filter("id", id)
            .first()
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn restore_clears_deleted_at() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate_users(&db).await;
    let u = T10User::create(attrs! { name: "A", email: "a@x.com" })
        .await
        .unwrap();
    let id = u.id;
    u.delete().await.unwrap();

    let trashed = T10User::with_trashed()
        .filter("id", id)
        .first()
        .await
        .unwrap()
        .unwrap();
    trashed.restore().await.unwrap();

    let alive = T10User::find(id).await.unwrap().unwrap();
    assert!(alive.deleted_at.is_none());
}

#[tokio::test]
async fn only_trashed_sees_only_dead() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate_users(&db).await;
    let alice = T10User::create(attrs! { name: "Alice", email: "a@x.com" })
        .await
        .unwrap();
    let _bob = T10User::create(attrs! { name: "Bob", email: "b@x.com" })
        .await
        .unwrap();
    alice.delete().await.unwrap();

    let dead = T10User::only_trashed().get().await.unwrap();
    assert_eq!(dead.len(), 1);
    assert_eq!(dead[0].name, "Alice");
}

#[tokio::test]
async fn all_hides_trashed_rows() {
    // The macro emits an inherent `all()` for soft-deletes models
    // that routes through `query()` so the global scope filters out
    // trashed rows. Without this override, the trait default's
    // `Self::Entity::find().all(...)` would bypass the scope and
    // return trashed rows alongside alive ones.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate_users(&db).await;
    let alice = T10User::create(attrs! { name: "Alice", email: "a@x.com" })
        .await
        .unwrap();
    let _bob = T10User::create(attrs! { name: "Bob", email: "b@x.com" })
        .await
        .unwrap();
    alice.delete().await.unwrap();

    let alive = T10User::all().await.unwrap();
    assert_eq!(alive.len(), 1);
    assert_eq!(alive[0].name, "Bob");
}

#[tokio::test]
async fn find_many_hides_trashed_rows() {
    // Same scope-application concern as `all()`. Without the
    // inherent override, `find_many` would happily return trashed
    // rows whose IDs match.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate_users(&db).await;
    let alice = T10User::create(attrs! { name: "Alice", email: "a@x.com" })
        .await
        .unwrap();
    let bob = T10User::create(attrs! { name: "Bob", email: "b@x.com" })
        .await
        .unwrap();
    let alice_id = alice.id;
    let bob_id = bob.id;
    alice.delete().await.unwrap();

    let rows = T10User::find_many(vec![alice_id, bob_id]).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "Bob");
}

#[tokio::test]
async fn trashed_returns_bool() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate_users(&db).await;
    let u = T10User::create(attrs! { name: "A", email: "a@x.com" })
        .await
        .unwrap();
    assert!(!u.trashed());
    let id = u.id;
    u.delete().await.unwrap();
    let t = T10User::with_trashed()
        .filter("id", id)
        .first()
        .await
        .unwrap()
        .unwrap();
    assert!(t.trashed());
}

// ---- Prunable -----------------------------------------------------------

#[model(table = "t10_sessions", fillable = ["token", "expires_at"], timestamps = false)]
pub struct T10Session {
    pub id: i64,
    pub token: String,
    pub expires_at: String,
}

#[suprnova::prunable]
#[async_trait]
impl Prunable for T10Session {
    fn prunable() -> suprnova::Builder<Self> {
        Self::query().filter_op(
            "expires_at",
            "<",
            (Utc::now() - Duration::days(30)).to_rfc3339(),
        )
    }
}

#[tokio::test]
async fn prunable_runs_against_scope() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t10_sessions (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            token TEXT, \
            expires_at TEXT\
         )",
    )
    .await
    .unwrap();

    T10Session::create(attrs! {
        token: "old",
        expires_at: (Utc::now() - Duration::days(60)).to_rfc3339(),
    })
    .await
    .unwrap();
    T10Session::create(attrs! {
        token: "current",
        expires_at: (Utc::now() + Duration::days(30)).to_rfc3339(),
    })
    .await
    .unwrap();

    let pruned = suprnova::eloquent::prune_one("T10Session", false)
        .await
        .unwrap();
    assert_eq!(pruned, Some(1), "expected 1 pruned, got {pruned:?}");
    assert!(
        T10Session::query()
            .filter("token", "current")
            .exists()
            .await
            .unwrap()
    );
    assert!(
        !T10Session::query()
            .filter("token", "old")
            .exists()
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn prune_dry_run_does_not_delete() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t10_sessions (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            token TEXT, \
            expires_at TEXT\
         )",
    )
    .await
    .unwrap();

    T10Session::create(attrs! {
        token: "old",
        expires_at: (Utc::now() - Duration::days(60)).to_rfc3339(),
    })
    .await
    .unwrap();

    let would_prune = suprnova::eloquent::prune_one("T10Session", true)
        .await
        .unwrap();
    assert_eq!(would_prune, Some(1));

    // Still present.
    assert!(
        T10Session::query()
            .filter("token", "old")
            .exists()
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn prune_all_iterates_every_registered_pruner() {
    // End-to-end smoke for `prune_all`. Sets up BOTH tables that have
    // a `#[prunable]` impl registered in this binary so the iteration
    // doesn't fail on a missing table. This is the only test that
    // exercises `prune_all` — the per-pruner cases above use
    // `prune_one` for isolation.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t10_sessions (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            token TEXT, \
            expires_at TEXT\
         )",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE t10_audit_logs (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            event TEXT, \
            occurred_at TEXT\
         )",
    )
    .await
    .unwrap();

    T10Session::create(attrs! {
        token: "old",
        expires_at: (Utc::now() - Duration::days(60)).to_rfc3339(),
    })
    .await
    .unwrap();
    T10AuditLog::create(attrs! {
        event: "stale",
        occurred_at: (Utc::now() - Duration::days(400)).to_rfc3339(),
    })
    .await
    .unwrap();

    let pruned = suprnova::eloquent::prune_all().await.unwrap();
    assert_eq!(
        pruned, 2,
        "expected 2 pruned across both pruners, got {pruned}"
    );
}

// ---- MassPrunable -------------------------------------------------------

#[model(table = "t10_audit_logs", fillable = ["event", "occurred_at"], timestamps = false)]
pub struct T10AuditLog {
    pub id: i64,
    pub event: String,
    pub occurred_at: String,
}

#[suprnova::prunable]
#[async_trait]
impl MassPrunable for T10AuditLog {
    fn prunable() -> suprnova::Builder<Self> {
        // Intentionally includes a `.select(...)` to prove the DELETE
        // renderer ignores SELECT-side state — the old SELECT→DELETE
        // string-rewrite trick would have broken on this scope.
        Self::query()
            .filter_op(
                "occurred_at",
                "<",
                (Utc::now() - Duration::days(365)).to_rfc3339(),
            )
            .select(["id", "event"])
    }
}

#[tokio::test]
async fn mass_prunable_uses_dedicated_delete_renderer() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t10_audit_logs (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            event TEXT, \
            occurred_at TEXT\
         )",
    )
    .await
    .unwrap();

    // Two old rows (would be pruned), one fresh row (would not).
    T10AuditLog::create(attrs! {
        event: "old.1",
        occurred_at: (Utc::now() - Duration::days(400)).to_rfc3339(),
    })
    .await
    .unwrap();
    T10AuditLog::create(attrs! {
        event: "old.2",
        occurred_at: (Utc::now() - Duration::days(500)).to_rfc3339(),
    })
    .await
    .unwrap();
    T10AuditLog::create(attrs! {
        event: "recent",
        occurred_at: Utc::now().to_rfc3339(),
    })
    .await
    .unwrap();

    let pruned = suprnova::eloquent::prune_one("T10AuditLog", false)
        .await
        .unwrap();
    assert_eq!(pruned, Some(2), "expected 2 old rows pruned");

    let remaining: Vec<String> = T10AuditLog::pluck::<String>("event").await.unwrap();
    assert_eq!(remaining, vec!["recent".to_string()]);
}
