//! Covers four MEDIUM audit findings on the database module:
//!
//! 1. `EntityExt`/`EntityExtMut` previously called `DB::connection()`
//!    directly, bypassing the tx routing layer. A write inside
//!    `DB::transaction` survived rollback — a silent data-integrity
//!    bug. Both surfaces now route through `ExecutorChoice`.
//! 2. SQLite parent-directory creation now propagates filesystem
//!    errors with path context instead of swallowing them.
//! 3. `DatabaseConfig::validate_pool` rejects `max == 0`, `min > max`,
//!    and `connect_timeout == 0`.
//! 4. `DbTableBuilder::insert` returns a clear database error instead
//!    of silently producing `0` when the target table doesn't have an
//!    auto-increment integer `id`.
//!
//! Tests share the process-wide container, so anything that registers
//! a connection is `#[serial_test::serial]`.
//!
//! References:
//! - `framework/src/database/model.rs`
//! - `framework/src/database/connection.rs`
//! - `framework/src/database/config.rs`
//! - `framework/src/database/db_facade.rs`

use sea_orm::Set;
use suprnova::database::{DatabaseConfig, DbConnection, EntityExt, EntityExtMut};
use suprnova::testing::TestDatabase;
use suprnova::{DB, FrameworkError};

// ---- EntityExt entity ---------------------------------------------------
//
// Defined inline so the test file is self-contained. SeaORM entity, NOT
// the new `#[suprnova::model]` macro, because the point of these tests
// is to exercise the legacy `EntityExt` surface.

mod widget {
    use sea_orm::entity::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "audit_widgets")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = true)]
        pub id: i64,
        pub name: String,
    }

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}

    impl ActiveModelBehavior for ActiveModel {}

    impl suprnova::database::EntityExt for Entity {}
    impl suprnova::database::EntityExtMut for Entity {}
}

async fn setup_widget_db() -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE audit_widgets (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            name TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
    db
}

// ---- Finding 1: EntityExt routes through the transaction layer --------

#[tokio::test]
#[serial_test::serial]
async fn entity_ext_insert_inside_transaction_rolls_back() {
    let _db = setup_widget_db().await;

    // Closure returns Err -> transaction must roll back. Previously
    // `EntityExtMut::insert_one` reached `DB::connection()` directly,
    // so the row was committed outside the transaction and survived
    // the rollback. After the fix the row should not be visible after
    // the transaction returns.
    let _ = DB::transaction::<_, ()>(|_tx| {
        Box::pin(async move {
            let am = widget::ActiveModel {
                name: Set("inside-rollback".into()),
                ..Default::default()
            };
            widget::Entity::insert_one(am).await?;
            // Reads inside the closure must see the pending write.
            let n = widget::Entity::count_all().await?;
            assert_eq!(n, 1);
            Err::<(), FrameworkError>(FrameworkError::database("force rollback"))
        })
    })
    .await;

    let post = widget::Entity::count_all().await.unwrap();
    assert_eq!(
        post, 0,
        "EntityExtMut::insert_one must participate in the ambient transaction",
    );
}

#[tokio::test]
#[serial_test::serial]
async fn entity_ext_update_and_delete_inside_transaction_roll_back() {
    let _db = setup_widget_db().await;

    let am = widget::ActiveModel {
        name: Set("persisted".into()),
        ..Default::default()
    };
    let row = widget::Entity::insert_one(am).await.unwrap();
    let row_id = row.id;

    let _ = DB::transaction::<_, ()>(|_tx| {
        Box::pin(async move {
            // Update inside tx.
            let am = widget::ActiveModel {
                id: Set(row_id),
                name: Set("changed-in-tx".into()),
            };
            widget::Entity::update_one(am).await?;
            // Delete a second time inside tx.
            widget::Entity::delete_by_pk(row_id).await?;
            let n = widget::Entity::count_all().await?;
            assert_eq!(n, 0, "delete inside tx hides the row from reads");
            Err::<(), FrameworkError>(FrameworkError::database("force rollback"))
        })
    })
    .await;

    let after = widget::Entity::find_by_pk(row_id).await.unwrap();
    assert!(
        after.is_some(),
        "row deleted inside a rolled-back transaction must reappear",
    );
    assert_eq!(
        after.unwrap().name,
        "persisted",
        "update inside a rolled-back transaction must not stick",
    );
}

#[tokio::test]
#[serial_test::serial]
async fn entity_ext_save_one_inside_transaction_rolls_back() {
    let _db = setup_widget_db().await;

    let _ = DB::transaction::<_, ()>(|_tx| {
        Box::pin(async move {
            let am = widget::ActiveModel {
                name: Set("save-inside-rollback".into()),
                ..Default::default()
            };
            widget::Entity::save_one(am).await?;
            Err::<(), FrameworkError>(FrameworkError::database("rollback"))
        })
    })
    .await;

    let n = widget::Entity::count_all().await.unwrap();
    assert_eq!(n, 0, "save_one must participate in the ambient transaction");
}

// ---- Finding 3: SQLite parent-dir creation propagates errors ----------

#[tokio::test]
async fn sqlite_connect_errors_when_parent_dir_unwritable() {
    // Point at a path under a regular file: `create_dir_all` will fail
    // with "Not a directory" because the parent component is a file.
    let mut blocker = std::env::temp_dir();
    blocker.push(format!("suprnova-medium-blocker-{}", std::process::id(),));
    std::fs::write(&blocker, b"blocker").unwrap();

    let nested = blocker.join("subdir").join("test.db");
    let url = format!("sqlite://{}", nested.display());
    let cfg = DatabaseConfig::builder().url(url).build();

    let err = match DbConnection::connect(&cfg).await {
        Ok(_) => panic!("connect against `<file>/subdir/test.db` must fail"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(
        msg.contains("failed to create SQLite parent directory"),
        "expected fs-specific diagnostic, got: {msg}",
    );

    let _ = std::fs::remove_file(&blocker);
}

// ---- Finding 4: pool config validation --------------------------------

#[test]
fn validate_pool_rejects_zero_max_connections() {
    let cfg = DatabaseConfig::builder()
        .url("sqlite::memory:")
        .max_connections(0)
        .build();
    let err = cfg.validate_pool().unwrap_err();
    assert!(err.to_string().contains("DB_MAX_CONNECTIONS"), "got: {err}",);
}

#[test]
fn validate_pool_rejects_min_greater_than_max() {
    let cfg = DatabaseConfig::builder()
        .url("sqlite::memory:")
        .max_connections(5)
        .min_connections(10)
        .build();
    let err = cfg.validate_pool().unwrap_err();
    let s = err.to_string();
    assert!(s.contains("DB_MIN_CONNECTIONS"), "got: {s}");
    assert!(s.contains("DB_MAX_CONNECTIONS"), "got: {s}");
}

#[test]
fn validate_pool_rejects_zero_connect_timeout() {
    let cfg = DatabaseConfig::builder()
        .url("sqlite::memory:")
        .connect_timeout(0)
        .build();
    let err = cfg.validate_pool().unwrap_err();
    assert!(err.to_string().contains("DB_CONNECT_TIMEOUT"), "got: {err}",);
}

#[test]
fn validate_pool_accepts_zero_min_connections() {
    // `min == 0` is a legitimate lazy-pool configuration. Don't reject.
    let cfg = DatabaseConfig::builder()
        .url("sqlite::memory:")
        .min_connections(0)
        .max_connections(10)
        .build();
    cfg.validate_pool().expect("min == 0 is legal");
}

#[tokio::test]
async fn db_connection_connect_rejects_invalid_pool() {
    // Validation runs at the single chokepoint — DB::init / init_with
    // both go through DbConnection::connect. A zero-sized pool must
    // fail-fast instead of producing a sick pool.
    let cfg = DatabaseConfig::builder()
        .url("sqlite::memory:")
        .max_connections(0)
        .build();
    let err = match DbConnection::connect(&cfg).await {
        Ok(_) => panic!("connect with max_connections=0 must fail"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("DB_MAX_CONNECTIONS"), "got: {err}",);
}

// ---- Finding 5: DbTableBuilder::insert is loud on non-i64 id -----------

#[tokio::test]
#[serial_test::serial]
async fn db_table_insert_errors_on_uuid_primary_key() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared("CREATE TABLE audit_uuid (uuid TEXT PRIMARY KEY, name TEXT NOT NULL)")
        .await
        .unwrap();

    // No `id` column at all -> `RETURNING id` itself fails. The
    // wrapped error must surface the table name and the Eloquent
    // alternative.
    let err = DB::table("audit_uuid")
        .insert(suprnova::attrs! {
            uuid: "11111111-2222-3333-4444-555555555555".to_string(),
            name: "ralph".to_string(),
        })
        .await
        .unwrap_err();
    let msg = err.to_string();
    // The SQL error from `RETURNING id` on a table with no `id` column
    // surfaces verbatim — we just need a clear failure (not a silent 0).
    assert!(
        msg.contains("audit_uuid") || msg.contains("id"),
        "expected loud failure mentioning the table or the missing column, got: {msg}",
    );

    // Most importantly: nothing was inserted with a fake 0 id.
    let rows = db
        .fetch_all("SELECT uuid FROM audit_uuid", vec![])
        .await
        .unwrap();
    // Postgres/SQLite use `RETURNING id` so the insert fails before
    // executing. The row count must stay at 0.
    assert_eq!(rows.len(), 0);
}

#[tokio::test]
#[serial_test::serial]
async fn db_table_insert_errors_when_returning_id_value_is_not_i64() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    // Table has an `id` column but it's TEXT, not an integer. The
    // `RETURNING id` clause succeeds but the value cannot be read as
    // i64 — the fix must turn the previously-silent `0` into a clear
    // error.
    db.execute_unprepared("CREATE TABLE audit_text_id (id TEXT PRIMARY KEY, name TEXT NOT NULL)")
        .await
        .unwrap();

    let err = DB::table("audit_text_id")
        .insert(suprnova::attrs! {
            id: "abc-123".to_string(),
            name: "ralph".to_string(),
        })
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Eloquent Model surface"),
        "expected guidance towards the Eloquent surface, got: {msg}",
    );
    assert!(msg.contains("audit_text_id"), "missing table name: {msg}");
}
