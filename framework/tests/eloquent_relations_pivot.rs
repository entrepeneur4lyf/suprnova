//! Phase 10B T4 — `BelongsToMany<L, R, P>` + first-class Pivot model.
//!
//! Exercises the full m2m surface:
//!
//! - `.attach(id)` / `.attach_with(id, attrs!{...})` / `.detach(id)` /
//!   `.sync([...])` mutate the pivot table. `sync` is transactional —
//!   a failing INSERT rolls back the corresponding DELETE.
//! - `.get()` runs the two-query strategy (related rows by IN, pivot
//!   rows separately) and stamps `__pivot = Some(Arc::new(pivot))` on
//!   each returned R so `r.pivot::<P>()` returns the matching pivot.
//! - `.count()` is a single `SELECT COUNT(*)` against the pivot table.
//! - The eager `__eager_load("roles", ...)` dispatcher clones R per
//!   attachment so multiple parents sharing one R each get their own
//!   pivot context — pin this with the
//!   `belongs_to_many_eager_load_clones_per_attachment` test.
//! - `__count_relation` and `__aggregate_relation` use server-side
//!   GROUP BY against the pivot (count) / pivot+related JOIN
//!   (aggregate).

use suprnova::testing::TestDatabase;
use suprnova::{AggregateKind, Model, attrs, model};

#[model(table = "btm_users", relations = {
    roles: BelongsToMany<BtmRole, BtmRoleUserPivot> {
        with_pivot = ["assigned_at"],
        with_timestamps,
    },
})]
pub struct BtmUser {
    pub id: i64,
    pub name: String,
}

#[model(table = "btm_roles", relations = {
    users: BelongsToMany<BtmUser, BtmRoleUserPivot>,
})]
pub struct BtmRole {
    pub id: i64,
    pub name: String,
    pub weight: i64,
}

#[model(table = "btm_role_user", primary_key = "id")]
pub struct BtmRoleUserPivot {
    pub id: i64,
    pub btm_user_id: i64,
    pub btm_role_id: i64,
    pub assigned_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

async fn migrate(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE btm_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE btm_roles (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         name TEXT NOT NULL, weight INTEGER NOT NULL DEFAULT 0)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE btm_role_user (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            btm_user_id INTEGER NOT NULL, \
            btm_role_id INTEGER NOT NULL, \
            assigned_at TEXT, \
            created_at TEXT NOT NULL, \
            updated_at TEXT NOT NULL, \
            UNIQUE(btm_user_id, btm_role_id)\
         )",
    )
    .await
    .unwrap();
}

// ---- Basic CRUD on the pivot --------------------------------------------

#[tokio::test]
async fn belongs_to_many_attach_creates_pivot_row() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = BtmUser::create(attrs! { name: "Alice" }).await.unwrap();
    let r = BtmRole::create(attrs! { name: "admin", weight: 10i64 })
        .await
        .unwrap();
    u.roles().attach(r.id).await.unwrap();

    let roles = u.roles().get().await.unwrap();
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0].name, "admin");
}

#[tokio::test]
async fn belongs_to_many_detach_removes_pivot() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = BtmUser::create(attrs! { name: "Bob" }).await.unwrap();
    let r1 = BtmRole::create(attrs! { name: "r1", weight: 1i64 })
        .await
        .unwrap();
    let r2 = BtmRole::create(attrs! { name: "r2", weight: 2i64 })
        .await
        .unwrap();
    u.roles().attach(r1.id).await.unwrap();
    u.roles().attach(r2.id).await.unwrap();
    assert_eq!(u.roles().get().await.unwrap().len(), 2);

    u.roles().detach(r1.id).await.unwrap();
    let remaining = u.roles().get().await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, r2.id);
}

#[tokio::test]
async fn belongs_to_many_sync_attaches_missing_and_detaches_extra() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = BtmUser::create(attrs! { name: "Carol" }).await.unwrap();
    let r1 = BtmRole::create(attrs! { name: "r1", weight: 1i64 })
        .await
        .unwrap();
    let r2 = BtmRole::create(attrs! { name: "r2", weight: 2i64 })
        .await
        .unwrap();
    let r3 = BtmRole::create(attrs! { name: "r3", weight: 3i64 })
        .await
        .unwrap();
    u.roles().attach(r1.id).await.unwrap();
    u.roles().attach(r2.id).await.unwrap();

    // After sync([r2, r3]): r1 detached, r2 stays, r3 attached.
    u.roles().sync([r2.id, r3.id]).await.unwrap();
    let after: Vec<i64> = u
        .roles()
        .get()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert!(after.contains(&r2.id), "r2 must remain after sync");
    assert!(after.contains(&r3.id), "r3 must be attached after sync");
    assert!(!after.contains(&r1.id), "r1 must be detached after sync");
    assert_eq!(after.len(), 2);
}

#[tokio::test]
async fn belongs_to_many_pivot_accessor_returns_pivot_data() {
    // THE key test — verifies `.pivot::<P>()` works after `.get()`.
    // The Arc<dyn Any + Send + Sync> stamped on __pivot must downcast
    // to the user's Pivot type cleanly.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = BtmUser::create(attrs! { name: "Dan" }).await.unwrap();
    let r = BtmRole::create(attrs! { name: "Admin", weight: 100i64 })
        .await
        .unwrap();
    let when = chrono::Utc::now();
    u.roles()
        .attach_with(r.id, attrs! { assigned_at: when })
        .await
        .unwrap();

    let roles = u.roles().get().await.unwrap();
    assert_eq!(roles.len(), 1);
    let pivot: &BtmRoleUserPivot = roles[0].pivot::<BtmRoleUserPivot>();
    assert_eq!(pivot.btm_user_id, u.id);
    assert_eq!(pivot.btm_role_id, r.id);
    // RFC3339 round-trip resolves to second precision through TEXT;
    // compare via timestamp() so sub-second variation doesn't flake.
    assert_eq!(
        pivot.assigned_at.map(|t| t.timestamp()),
        Some(when.timestamp()),
        "assigned_at must round-trip via the pivot row",
    );
}

#[tokio::test]
async fn belongs_to_many_attach_idempotent_or_explicit() {
    // attach twice = UNIQUE-constraint violation, surfaced as Err.
    // The framework doesn't dedupe at the Rust layer — users
    // requiring dedup use `sync()`.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = BtmUser::create(attrs! { name: "Eve" }).await.unwrap();
    let r = BtmRole::create(attrs! { name: "r", weight: 1i64 })
        .await
        .unwrap();
    let result1 = u.roles().attach(r.id).await;
    let result2 = u.roles().attach(r.id).await;
    assert!(result1.is_ok(), "first attach must succeed");
    let err = result2.expect_err("second attach must violate UNIQUE(btm_user_id, btm_role_id)");
    // Pin the error MESSAGE — a generic "table not found" would
    // satisfy `is_err()` but wouldn't prove the contract that the
    // UNIQUE constraint is what rejected the duplicate. SQLite's
    // exact wording is "UNIQUE constraint failed: btm_role_user...",
    // but the check is permissive enough to survive Postgres/MySQL
    // ("duplicate key value violates unique constraint" / "Duplicate
    // entry ... for key").
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("unique") || msg.contains("constraint") || msg.contains("duplicate"),
        "expected UNIQUE-related error, got: {msg}",
    );
}

#[tokio::test]
async fn belongs_to_many_inverse_works() {
    // Role -> Users direction. Same pivot, opposite traversal.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u1 = BtmUser::create(attrs! { name: "u1" }).await.unwrap();
    let u2 = BtmUser::create(attrs! { name: "u2" }).await.unwrap();
    let r = BtmRole::create(attrs! { name: "admin", weight: 5i64 })
        .await
        .unwrap();
    u1.roles().attach(r.id).await.unwrap();
    u2.roles().attach(r.id).await.unwrap();

    let users = r.users().get().await.unwrap();
    assert_eq!(users.len(), 2);
    let ids: Vec<i64> = users.iter().map(|u| u.id).collect();
    assert!(ids.contains(&u1.id));
    assert!(ids.contains(&u2.id));
}

#[tokio::test]
async fn belongs_to_many_count_lazy_returns_attached_count() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = BtmUser::create(attrs! { name: "F" }).await.unwrap();
    let r1 = BtmRole::create(attrs! { name: "r1", weight: 1i64 })
        .await
        .unwrap();
    let r2 = BtmRole::create(attrs! { name: "r2", weight: 2i64 })
        .await
        .unwrap();
    u.roles().attach(r1.id).await.unwrap();
    u.roles().attach(r2.id).await.unwrap();
    assert_eq!(u.roles().count().await.unwrap(), 2);
}

#[tokio::test]
async fn belongs_to_many_first_returns_one() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = BtmUser::create(attrs! { name: "G" }).await.unwrap();
    let r = BtmRole::create(attrs! { name: "only", weight: 9i64 })
        .await
        .unwrap();
    u.roles().attach(r.id).await.unwrap();
    let one = u.roles().first().await.unwrap();
    assert!(one.is_some());
    assert_eq!(one.unwrap().name, "only");
}

#[tokio::test]
async fn belongs_to_many_get_empty_for_unrelated_parent() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u = BtmUser::create(attrs! { name: "lonely" }).await.unwrap();
    assert!(u.roles().get().await.unwrap().is_empty());
}

// ---- Eager loading ------------------------------------------------------

#[tokio::test]
async fn belongs_to_many_eager_load_populates_loaded_accessor() {
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u1 = BtmUser::create(attrs! { name: "u1" }).await.unwrap();
    let u2 = BtmUser::create(attrs! { name: "u2" }).await.unwrap();
    let r1 = BtmRole::create(attrs! { name: "r1", weight: 1i64 })
        .await
        .unwrap();
    let r2 = BtmRole::create(attrs! { name: "r2", weight: 2i64 })
        .await
        .unwrap();
    u1.roles().attach(r1.id).await.unwrap();
    u1.roles().attach(r2.id).await.unwrap();
    u2.roles().attach(r1.id).await.unwrap();

    let users = BtmUser::with(["roles"]).get().await.unwrap();
    assert_eq!(users.len(), 2);
    let u1_loaded = users.iter().find(|u| u.id == u1.id).unwrap();
    assert_eq!(u1_loaded.roles_loaded().len(), 2);
    let u2_loaded = users.iter().find(|u| u.id == u2.id).unwrap();
    assert_eq!(u2_loaded.roles_loaded().len(), 1);
}

#[tokio::test]
async fn belongs_to_many_eager_load_empty_parent_gets_empty_slice() {
    // Parent with no attached roles must still get an empty slice on
    // `roles_loaded()` — the dispatcher explicitly seeds every parent's
    // cache so the accessor's "not eager-loaded" panic doesn't fire.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u_with = BtmUser::create(attrs! { name: "with" }).await.unwrap();
    let u_without = BtmUser::create(attrs! { name: "without" }).await.unwrap();
    let r = BtmRole::create(attrs! { name: "lone", weight: 1i64 })
        .await
        .unwrap();
    u_with.roles().attach(r.id).await.unwrap();

    let users = BtmUser::with(["roles"]).get().await.unwrap();
    let without_loaded = users.iter().find(|u| u.id == u_without.id).unwrap();
    assert!(without_loaded.roles_loaded().is_empty());
}

#[tokio::test]
async fn belongs_to_many_eager_load_stamps_pivot_per_attachment() {
    // CRITICAL: when one R is attached to multiple Ls via different
    // pivot rows, each L's copy of R must carry its OWN pivot context.
    // The dispatcher arm must clone R per attachment and stamp the
    // matching pivot row — NOT share a single instance.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u1 = BtmUser::create(attrs! { name: "u1" }).await.unwrap();
    let u2 = BtmUser::create(attrs! { name: "u2" }).await.unwrap();
    let r = BtmRole::create(attrs! { name: "shared", weight: 1i64 })
        .await
        .unwrap();
    let t1 = chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let t2 = chrono::DateTime::parse_from_rfc3339("2024-06-15T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    u1.roles()
        .attach_with(r.id, attrs! { assigned_at: t1 })
        .await
        .unwrap();
    u2.roles()
        .attach_with(r.id, attrs! { assigned_at: t2 })
        .await
        .unwrap();

    let users = BtmUser::with(["roles"]).get().await.unwrap();
    let u1_loaded = users.iter().find(|u| u.id == u1.id).unwrap();
    let u2_loaded = users.iter().find(|u| u.id == u2.id).unwrap();
    assert_eq!(u1_loaded.roles_loaded().len(), 1);
    assert_eq!(u2_loaded.roles_loaded().len(), 1);
    let p1 = u1_loaded.roles_loaded()[0].pivot::<BtmRoleUserPivot>();
    let p2 = u2_loaded.roles_loaded()[0].pivot::<BtmRoleUserPivot>();
    assert_eq!(
        p1.assigned_at.map(|t| t.timestamp()),
        Some(t1.timestamp()),
        "u1's pivot must carry t1",
    );
    assert_eq!(
        p2.assigned_at.map(|t| t.timestamp()),
        Some(t2.timestamp()),
        "u2's pivot must carry t2 (NOT t1)",
    );
}

#[tokio::test]
async fn belongs_to_many_count_dispatcher_uses_server_side_group_by() {
    // The dispatcher emits a single SELECT/GROUP BY against the pivot
    // table — no client-side counting. The test checks the result
    // surface (which is identical to a client-side counter); the
    // contract is the SQL shape, pinned by code review and the
    // workspace gate.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u1 = BtmUser::create(attrs! { name: "u1" }).await.unwrap();
    let u2 = BtmUser::create(attrs! { name: "u2" }).await.unwrap();
    let u3 = BtmUser::create(attrs! { name: "u3" }).await.unwrap();
    let r1 = BtmRole::create(attrs! { name: "r1", weight: 1i64 })
        .await
        .unwrap();
    let r2 = BtmRole::create(attrs! { name: "r2", weight: 2i64 })
        .await
        .unwrap();
    let r3 = BtmRole::create(attrs! { name: "r3", weight: 3i64 })
        .await
        .unwrap();
    u1.roles().attach(r1.id).await.unwrap();
    u1.roles().attach(r2.id).await.unwrap();
    u1.roles().attach(r3.id).await.unwrap();
    u2.roles().attach(r1.id).await.unwrap();
    // u3 has none.

    let mut users = BtmUser::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut BtmUser> = users.iter_mut().collect();
        BtmUser::__count_relation("roles", refs.as_mut_slice(), _db.conn())
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
        assert_eq!(u.roles_count(), expected, "user {} count", u.id);
    }
}

#[tokio::test]
async fn belongs_to_many_aggregate_sum_over_related_column() {
    // Aggregate is over R's columns (Laravel parity — users want
    // sum(role.weight), not sum(pivot.assigned_at)). The dispatcher
    // JOINs pivot to related and groups by pivot's FK.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u1 = BtmUser::create(attrs! { name: "u1" }).await.unwrap();
    let u2 = BtmUser::create(attrs! { name: "u2" }).await.unwrap();
    let r1 = BtmRole::create(attrs! { name: "r1", weight: 5i64 })
        .await
        .unwrap();
    let r2 = BtmRole::create(attrs! { name: "r2", weight: 10i64 })
        .await
        .unwrap();
    u1.roles().attach(r1.id).await.unwrap();
    u1.roles().attach(r2.id).await.unwrap();
    u2.roles().attach(r1.id).await.unwrap();

    let mut users = BtmUser::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut BtmUser> = users.iter_mut().collect();
        BtmUser::__aggregate_relation(
            "roles",
            "weight",
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
        .get_aggregate::<f64>("roles_sum_weight")
        .expect("sum cache populated");
    let u2_sum = *users
        .iter()
        .find(|u| u.id == u2.id)
        .unwrap()
        .__eager
        .get_aggregate::<f64>("roles_sum_weight")
        .expect("sum cache populated");
    assert_eq!(u1_sum, 15.0);
    assert_eq!(u2_sum, 5.0);
}

#[tokio::test]
async fn belongs_to_many_aggregate_min_max_branches_to_option() {
    // Min/Max → Option<f64> per the HasMany contract. Empty groups
    // produce None.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    migrate(&_db).await;
    let u_with = BtmUser::create(attrs! { name: "with" }).await.unwrap();
    let u_empty = BtmUser::create(attrs! { name: "empty" }).await.unwrap();
    let r = BtmRole::create(attrs! { name: "r", weight: 42i64 })
        .await
        .unwrap();
    u_with.roles().attach(r.id).await.unwrap();

    let mut users = BtmUser::query().get().await.unwrap();
    {
        let mut refs: Vec<&mut BtmUser> = users.iter_mut().collect();
        BtmUser::__aggregate_relation(
            "roles",
            "weight",
            AggregateKind::Min,
            refs.as_mut_slice(),
            _db.conn(),
        )
        .await
        .unwrap();
    }
    let with_min = users
        .iter()
        .find(|u| u.id == u_with.id)
        .unwrap()
        .__eager
        .get_aggregate::<Option<f64>>("roles_min_weight")
        .expect("min cache populated");
    let empty_min = users
        .iter()
        .find(|u| u.id == u_empty.id)
        .unwrap()
        .__eager
        .get_aggregate::<Option<f64>>("roles_min_weight")
        .expect("min cache populated for empty parent too");
    assert_eq!(*with_min, Some(42.0));
    assert!(empty_min.is_none(), "min over empty group must be None");
}

// ---- Sync transactionality ---------------------------------------------

#[tokio::test]
async fn belongs_to_many_sync_rolls_back_on_attach_failure() {
    // Force a UNIQUE-violation during sync's attach phase and assert
    // the detach phase rolls back.
    //
    // Setup: pre-attach r1+r2. Then write a NULL-FK pivot row via
    // raw SQL to seed an INSERT collision on a fabricated role ID
    // that won't appear in the SELECT (because the SELECT only reads
    // current pivot rows for THIS parent).
    //
    // Actually the simpler force-failure path: drop the pivot table's
    // `assigned_at` column on a fresh schema by re-migrating with a
    // NOT NULL constraint, then sync with `with_timestamps` on a
    // declaration that omits assigned_at. The INSERT would lack a
    // value for assigned_at and SQLite would reject it.
    //
    // Cleanest approach: use a SECOND pivot table whose schema forces
    // an attach-time failure. We define a sibling model with a NOT
    // NULL column the framework doesn't fill — the INSERT fails on
    // every attach attempt, and we can observe the pre-state survives.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    _db.execute_unprepared(
        "CREATE TABLE btm_tx_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    _db.execute_unprepared(
        "CREATE TABLE btm_tx_roles (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    // Pivot table has a NOT NULL `required_data` column with no
    // default. The framework's INSERT path doesn't supply it, so any
    // attach to a NEW (parent, related) pair fails on the NOT NULL
    // constraint. The transactional rollback contract requires that
    // any prior detach in the same `sync` call is undone.
    _db.execute_unprepared(
        "CREATE TABLE btm_tx_role_user (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            btm_tx_user_id INTEGER NOT NULL, \
            btm_tx_role_id INTEGER NOT NULL, \
            required_data TEXT NOT NULL, \
            UNIQUE(btm_tx_user_id, btm_tx_role_id)\
         )",
    )
    .await
    .unwrap();

    let u = BtmTxUser::create(attrs! { name: "tx" }).await.unwrap();
    let r1 = BtmTxRole::create(attrs! { name: "r1" }).await.unwrap();
    let r2 = BtmTxRole::create(attrs! { name: "r2" }).await.unwrap();

    // Pre-attach r1 via raw SQL (supplying the required column).
    _db.execute_unprepared(&format!(
        "INSERT INTO btm_tx_role_user (btm_tx_user_id, btm_tx_role_id, required_data) \
         VALUES ({uid}, {rid}, 'pre')",
        uid = u.id,
        rid = r1.id,
    ))
    .await
    .unwrap();

    // sync([r2]) — plans detach(r1), attach(r2). The attach(r2) fails
    // on the NOT NULL constraint. The transaction rollback restores
    // (u, r1) — the detach must NOT commit.
    let result = u.roles_tx().sync([r2.id]).await;
    assert!(
        result.is_err(),
        "sync attach must surface NOT NULL violation",
    );

    // r1 must still be attached because the detach rolled back.
    let after: Vec<i64> = u
        .roles_tx()
        .get()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert!(
        after.contains(&r1.id),
        "r1 must still be attached after sync failure — detach rolled back transactionally; \
         got: {after:?}",
    );
    assert!(
        !after.contains(&r2.id),
        "r2 must NOT be attached — attach failed",
    );
}

// Tx-rollback models — separate from the main BtmUser/Role/Pivot so
// the NOT NULL column doesn't pollute every other test.
#[model(table = "btm_tx_users", relations = {
    roles_tx: BelongsToMany<BtmTxRole, BtmTxRoleUserPivot>,
})]
pub struct BtmTxUser {
    pub id: i64,
    pub name: String,
}

#[model(table = "btm_tx_roles")]
pub struct BtmTxRole {
    pub id: i64,
    pub name: String,
}

#[model(table = "btm_tx_role_user", primary_key = "id")]
pub struct BtmTxRoleUserPivot {
    pub id: i64,
    pub btm_tx_user_id: i64,
    pub btm_tx_role_id: i64,
    pub required_data: String,
}

// ---- Custom pivot keys ---------------------------------------------------

#[model(table = "btm_custom_users", relations = {
    perms: BelongsToMany<BtmPerm, BtmPermPivot> {
        pivot_table = "btm_user_perm",
        pivot_foreign_key = "owner_id",
        pivot_related_key = "perm_ref_id",
    },
})]
pub struct BtmCustomUser {
    pub id: i64,
    pub name: String,
}

#[model(table = "btm_perms")]
pub struct BtmPerm {
    pub id: i64,
    pub name: String,
}

#[model(table = "btm_user_perm", primary_key = "id")]
pub struct BtmPermPivot {
    pub id: i64,
    pub owner_id: i64,
    pub perm_ref_id: i64,
}

#[tokio::test]
async fn belongs_to_many_custom_pivot_keys_resolve() {
    // Pin the three pivot-customisation options: `pivot_table`
    // (override the default `<P>::TABLE`), `pivot_foreign_key`,
    // `pivot_related_key`.
    let _db = TestDatabase::sqlite_memory().await.unwrap();
    _db.execute_unprepared(
        "CREATE TABLE btm_custom_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    _db.execute_unprepared(
        "CREATE TABLE btm_perms (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    _db.execute_unprepared(
        "CREATE TABLE btm_user_perm (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            owner_id INTEGER NOT NULL, \
            perm_ref_id INTEGER NOT NULL, \
            UNIQUE(owner_id, perm_ref_id)\
         )",
    )
    .await
    .unwrap();

    let u = BtmCustomUser::create(attrs! { name: "u" }).await.unwrap();
    let p1 = BtmPerm::create(attrs! { name: "read" }).await.unwrap();
    let p2 = BtmPerm::create(attrs! { name: "write" }).await.unwrap();
    u.perms().attach(p1.id).await.unwrap();
    u.perms().attach(p2.id).await.unwrap();

    let perms = u.perms().get().await.unwrap();
    assert_eq!(perms.len(), 2);
    let names: Vec<String> = perms.iter().map(|p| p.name.clone()).collect();
    assert!(names.contains(&"read".to_string()));
    assert!(names.contains(&"write".to_string()));

    assert_eq!(u.perms().count().await.unwrap(), 2);
}

// ---- Custom related-PK column (`related_key = "..."`) -------------------
//
// The T4 review caught that `.get()`'s IN-filter and the aggregate-JOIN
// arm hardcoded `"id"` as the related-side PK column. For any related
// model declared with `#[model(primary_key = "uuid")]` (or similar),
// that produced `no such column: __sn_r.id` errors at runtime. The fix
// threads `related_key = "uuid"` from the macro options through to the
// runtime via `.related_pk(...)`. This test pins that wiring
// end-to-end.

#[suprnova::model(
    table = "btm_uuid_things",
    primary_key = "uuid",
    key_type = "String",
    auto_increment = false
)]
pub struct BtmUuidThing {
    pub uuid: String,
    pub weight: i64,
}

#[suprnova::model(table = "btm_uuid_pivot", primary_key = "id")]
pub struct BtmUuidPivot {
    pub id: i64,
    pub btm_uuid_user_id: i64,
    pub btm_uuid_thing_uuid: String,
}

#[suprnova::model(table = "btm_uuid_users", relations = {
    things: BelongsToMany<BtmUuidThing, BtmUuidPivot> {
        related_key = "uuid",
        pivot_related_key = "btm_uuid_thing_uuid",
    },
})]
pub struct BtmUuidUser {
    pub id: i64,
    pub name: String,
}

async fn migrate_uuid(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE btm_uuid_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE btm_uuid_things (uuid TEXT PRIMARY KEY, weight INTEGER NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "CREATE TABLE btm_uuid_pivot (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            btm_uuid_user_id INTEGER NOT NULL, \
            btm_uuid_thing_uuid TEXT NOT NULL, \
            UNIQUE(btm_uuid_user_id, btm_uuid_thing_uuid)\
         )",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn belongs_to_many_respects_custom_related_pk() {
    // Raw-SQL inserts for the UUID model bypass `create()` — the
    // `key_type = "String", auto_increment = false` round-trip is
    // orthogonal to T4, so we don't gate this test on it.
    let db = TestDatabase::sqlite_memory().await.unwrap();
    migrate_uuid(&db).await;
    let u = BtmUuidUser::create(attrs! { name: "alice" }).await.unwrap();
    db.execute_unprepared("INSERT INTO btm_uuid_things (uuid, weight) VALUES ('thing-1', 10)")
        .await
        .unwrap();
    db.execute_unprepared("INSERT INTO btm_uuid_things (uuid, weight) VALUES ('thing-2', 25)")
        .await
        .unwrap();
    db.execute_unprepared(&format!(
        "INSERT INTO btm_uuid_pivot (btm_uuid_user_id, btm_uuid_thing_uuid) \
         VALUES ({}, 'thing-1')",
        u.id,
    ))
    .await
    .unwrap();
    db.execute_unprepared(&format!(
        "INSERT INTO btm_uuid_pivot (btm_uuid_user_id, btm_uuid_thing_uuid) \
         VALUES ({}, 'thing-2')",
        u.id,
    ))
    .await
    .unwrap();

    // `.get()` must JOIN on btm_uuid_things.uuid (NOT the hardcoded
    // "id") — without the fix this errors with "no such column: id".
    let things = u
        .things()
        .get()
        .await
        .expect("get() must honour related_key when related PK is non-`id`");
    assert_eq!(things.len(), 2);

    // The aggregate dispatcher must use the same JOIN column. If the
    // hardcoded `"id"` slipped back in this errors at SQL prepare time.
    let mut parents = BtmUuidUser::all().await.unwrap();
    {
        let mut refs: Vec<&mut BtmUuidUser> = parents.iter_mut().collect();
        BtmUuidUser::__aggregate_relation(
            "things",
            "weight",
            AggregateKind::Sum,
            refs.as_mut_slice(),
            db.conn(),
        )
        .await
        .unwrap();
    }
    let u_loaded = parents.iter().find(|p| p.id == u.id).unwrap();
    let sum: &f64 = u_loaded
        .__eager
        .get_aggregate::<f64>("things_sum_weight")
        .unwrap();
    assert_eq!(*sum, 35.0);
}
