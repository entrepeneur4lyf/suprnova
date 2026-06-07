//! Soft-delete `restore()` must honour `#[model(connection = "...")]`
//! per-model routing, the same way every other write-side lifecycle
//! method does.
//!
//! Reproduces the original bug: the macro-emitted `restore()` body
//! called `ExecutorChoice::resolve()`, which only consults
//! `CURRENT_TX` and then falls back to `DB::connection()?`. A model
//! tagged `#[model(connection = "alt")]` had every write-side
//! method route through `alt` — except `restore()`, which landed
//! on the primary pool instead. The fix routes restore through
//! `ExecutorChoice::resolve_write(None, None,
//! Self::default_connection_name())`, exactly matching `create` /
//! `save` / `update` / `delete`, so the full 5-step precedence
//! chain applies.

use chrono::{DateTime, Utc};
use serial_test::serial;
use suprnova::DbConnection;
use suprnova::database::ConnectionRegistry;
use suprnova::model;
use suprnova::testing::TestDatabase;

#[model(table = "rnc_users", connection = "alt", soft_deletes)]
pub struct RncUser {
    pub id: i64,
    pub name: String,
    pub deleted_at: Option<DateTime<Utc>>,
}

#[tokio::test]
#[serial]
async fn restore_routes_through_per_model_named_connection() {
    // Primary pool is a fresh empty in-memory SQLite — it does NOT
    // have an `rnc_users` table. If `restore()` were still routing
    // through `DB::connection()?` (the buggy resolve() path) it
    // would crash with "no such table: rnc_users" on the primary.
    let _primary = TestDatabase::sqlite_memory().await.unwrap();

    // Register `alt` as a separate in-memory pool with the table
    // present + a pre-seeded soft-deleted row. The full 5-step
    // precedence chain — driven by `#[model(connection = "alt")]` —
    // must steer the restore here.
    let alt_conn = sea_orm::Database::connect("sqlite::memory:?mode=rwc")
        .await
        .expect("alt in-memory connection");
    let alt = DbConnection::from_raw(alt_conn);
    use sea_orm::ConnectionTrait;
    alt.inner()
        .execute_unprepared(
            "CREATE TABLE rnc_users (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                name TEXT NOT NULL, \
                deleted_at TEXT\
             )",
        )
        .await
        .unwrap();
    // Pre-seed a trashed row directly on `alt` — bypassing the model
    // so the test setup never touches the (table-less) primary. The
    // assertion below targets restore() specifically.
    alt.inner()
        .execute_unprepared(
            "INSERT INTO rnc_users (id, name, deleted_at) \
             VALUES (1, 'Alice', '2025-01-01T00:00:00Z')",
        )
        .await
        .unwrap();
    ConnectionRegistry::register_existing("alt", alt.clone())
        .await
        .unwrap();

    // Pull the trashed row through with_trashed (reads route via
    // `alt` because the model declares `connection = "alt"`).
    let trashed = RncUser::with_trashed()
        .filter("id", 1i64)
        .first()
        .await
        .unwrap()
        .expect("seeded trashed row exists on alt");
    assert!(
        trashed.deleted_at.is_some(),
        "row is trashed before restore"
    );

    // The actual regression assertion: restore must succeed by
    // landing on `alt`. Before the fix this failed with "no such
    // table: rnc_users" because resolve() fell back to the primary,
    // which has no schema.
    trashed
        .restore()
        .await
        .expect("restore must route through `alt`, not primary");

    // Confirm the restore actually landed on `alt`:
    // - `deleted_at` is now NULL on the alt row.
    // - The primary pool still has no `rnc_users` table at all (the
    //   restore did not silently create or write to it).
    let alive = RncUser::find(1i64)
        .await
        .unwrap()
        .expect("re-alive after restore on alt");
    assert!(
        alive.deleted_at.is_none(),
        "deleted_at must be cleared on alt"
    );

    // Directly inspect `alt` to confirm the UPDATE landed there.
    use sea_orm::FromQueryResult;
    let stmt = sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Sqlite,
        "SELECT deleted_at FROM rnc_users WHERE id = 1".to_string(),
    );
    #[derive(FromQueryResult)]
    struct Row {
        deleted_at: Option<String>,
    }
    let row = Row::find_by_statement(stmt)
        .one(alt.inner())
        .await
        .unwrap()
        .expect("row still present on alt");
    assert!(
        row.deleted_at.is_none(),
        "UPDATE deleted_at = NULL must have landed on alt; row = {:?}",
        row.deleted_at,
    );

    ConnectionRegistry::clear();
}
