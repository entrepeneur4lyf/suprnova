//! Phase 10C T10 — DB facade + DynamicRow integration tests.
//!
//! Covers the runtime-shape escape hatch: `DynamicRow` (typed accessors
//! over `serde_json::Map`), `DB::table(name)` (model-less query
//! builder), and `DB::select` / `update` / `delete` / `statement` /
//! `affecting_statement` (raw-SQL escape hatches).

use serde_json::json;
use suprnova::testing::TestDatabase;
use suprnova::{DynamicRow, DB};

// ---------- DynamicRow ---------------------------------------------------

#[test]
fn dynamic_row_typed_accessors() {
    let mut m = serde_json::Map::new();
    m.insert("id".into(), json!(42));
    m.insert("name".into(), json!("alice"));
    m.insert("active".into(), json!(true));
    m.insert("score".into(), serde_json::Value::Null);
    m.insert("prefs".into(), json!({"theme": "dark"}));
    let row = DynamicRow::from_map(m);

    assert_eq!(row.get_int("id").unwrap(), 42);
    assert_eq!(row.get_string("name").unwrap(), "alice");
    assert!(row.get_bool("active").unwrap());
    assert_eq!(row.get_optional_string("score").unwrap(), None);
    assert_eq!(row.get_optional_int("score").unwrap(), None);

    let prefs = row.get_value("prefs").unwrap();
    assert_eq!(prefs.get("theme").unwrap(), &json!("dark"));

    // get_as<T>
    let prefs_struct: serde_json::Value = row.get_as("prefs").unwrap();
    assert_eq!(prefs_struct.get("theme").unwrap(), &json!("dark"));

    // Deref to Map exposes iteration
    let count = row.iter().count();
    assert_eq!(count, 5);
}

#[test]
fn dynamic_row_missing_key_returns_error() {
    let m = serde_json::Map::new();
    let row = DynamicRow::from_map(m);

    let err = row.get_int("missing").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("missing") || msg.contains("not found"),
        "expected missing-key error, got: {msg}"
    );

    // Optional variants ALSO surface missing-key as error (distinct
    // from null-value, which returns Ok(None)).
    let err = row.get_optional_string("missing").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("missing") || msg.contains("not found"),
        "expected missing-key error, got: {msg}"
    );
}

#[test]
fn dynamic_row_type_mismatch_returns_error() {
    let mut m = serde_json::Map::new();
    m.insert("name".into(), json!("alice"));
    let row = DynamicRow::from_map(m);

    let err = row.get_int("name").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("not an int") || msg.contains("type"),
        "expected type-mismatch error, got: {msg}"
    );

    // Bool mismatch on a string value.
    let err = row.get_bool("name").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("not a bool") || msg.contains("type"),
        "expected type-mismatch error, got: {msg}"
    );
}

// ---------- DB::table query builder --------------------------------------

async fn setup_audit_table() -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE audit_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            event TEXT NOT NULL,
            actor_id INTEGER NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .await
    .unwrap();
    db
}

#[tokio::test]
async fn db_table_get_returns_dynamic_rows() {
    let _db = setup_audit_table().await;

    DB::table("audit_log")
        .insert(suprnova::attrs! { event: "user.created", actor_id: 1 })
        .await
        .unwrap();
    DB::table("audit_log")
        .insert(suprnova::attrs! { event: "user.updated", actor_id: 2 })
        .await
        .unwrap();

    let rows = DB::table("audit_log")
        .filter_op("actor_id", ">", 0)
        .order_by_desc("id")
        .limit(10)
        .get()
        .await
        .unwrap();

    assert_eq!(rows.len(), 2);
    let first = rows.first().unwrap();
    let event = first.get_string("event").unwrap();
    assert!(event == "user.created" || event == "user.updated");
}

#[tokio::test]
async fn db_table_update_returns_affected_count() {
    let _db = setup_audit_table().await;

    DB::table("audit_log")
        .insert(suprnova::attrs! { event: "x", actor_id: 5 })
        .await
        .unwrap();
    DB::table("audit_log")
        .insert(suprnova::attrs! { event: "y", actor_id: 5 })
        .await
        .unwrap();

    let affected = DB::table("audit_log")
        .filter("actor_id", 5)
        .update(suprnova::attrs! { event: "redacted" })
        .await
        .unwrap();

    assert_eq!(affected, 2);

    let after = DB::table("audit_log")
        .filter("event", "redacted")
        .get()
        .await
        .unwrap();
    assert_eq!(after.len(), 2);
}

#[tokio::test]
async fn db_table_delete_returns_affected_count() {
    let _db = setup_audit_table().await;

    DB::table("audit_log")
        .insert(suprnova::attrs! { event: "a", actor_id: 1 })
        .await
        .unwrap();
    DB::table("audit_log")
        .insert(suprnova::attrs! { event: "b", actor_id: 2 })
        .await
        .unwrap();

    let affected = DB::table("audit_log")
        .filter("event", "a")
        .delete()
        .await
        .unwrap();
    assert_eq!(affected, 1);

    let remaining = DB::table("audit_log").get().await.unwrap();
    assert_eq!(remaining.len(), 1);
}

#[tokio::test]
async fn db_table_first_returns_option() {
    let _db = setup_audit_table().await;

    // Empty table → None
    let none = DB::table("audit_log")
        .order_by_desc("id")
        .first()
        .await
        .unwrap();
    assert!(none.is_none());

    // After insert → Some
    DB::table("audit_log")
        .insert(suprnova::attrs! { event: "only", actor_id: 7 })
        .await
        .unwrap();

    let some = DB::table("audit_log")
        .order_by_desc("id")
        .first()
        .await
        .unwrap();
    let row = some.unwrap();
    assert_eq!(row.get_string("event").unwrap(), "only");
    assert_eq!(row.get_int("actor_id").unwrap(), 7);
}

#[tokio::test]
async fn db_table_count_returns_total() {
    let _db = setup_audit_table().await;

    assert_eq!(DB::table("audit_log").count().await.unwrap(), 0);

    for i in 0..5 {
        DB::table("audit_log")
            .insert(suprnova::attrs! { event: "x", actor_id: i })
            .await
            .unwrap();
    }

    assert_eq!(DB::table("audit_log").count().await.unwrap(), 5);

    // count() respects WHERE
    let count = DB::table("audit_log")
        .filter_op("actor_id", ">=", 3)
        .count()
        .await
        .unwrap();
    assert_eq!(count, 2);
}

// ---------- Raw SQL escape hatches ---------------------------------------

#[tokio::test]
async fn db_select_runs_raw_sql() {
    let _db = setup_audit_table().await;
    DB::table("audit_log")
        .insert(suprnova::attrs! { event: "a", actor_id: 1 })
        .await
        .unwrap();

    let rows = DB::select(
        "SELECT * FROM audit_log WHERE actor_id = ?",
        vec![1i64.into()],
    )
    .await
    .unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_string("event").unwrap(), "a");
}

#[tokio::test]
async fn db_select_projects_named_columns() {
    let _db = setup_audit_table().await;
    DB::table("audit_log")
        .insert(suprnova::attrs! { event: "a", actor_id: 7 })
        .await
        .unwrap();
    DB::table("audit_log")
        .insert(suprnova::attrs! { event: "b", actor_id: 7 })
        .await
        .unwrap();

    // Named columns (no aggregates) — sqlx has type info, so
    // JsonValue::find_by_statement keeps them.
    let rows = DB::select(
        "SELECT event, actor_id FROM audit_log WHERE actor_id = ? ORDER BY id",
        vec![7i64.into()],
    )
    .await
    .unwrap();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_string("event").unwrap(), "a");
    assert_eq!(rows[0].get_int("actor_id").unwrap(), 7);
    assert_eq!(rows[1].get_string("event").unwrap(), "b");
}

#[tokio::test]
async fn db_statement_runs_ddl() {
    let _db = setup_audit_table().await;
    DB::statement("CREATE INDEX idx_audit_actor ON audit_log(actor_id)")
        .await
        .unwrap();

    // Sanity: the index now exists in sqlite_master.
    let rows = DB::select(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name = ?",
        vec!["idx_audit_actor".into()],
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
}

#[tokio::test]
async fn db_affecting_statement_returns_rows_affected() {
    let _db = setup_audit_table().await;
    DB::table("audit_log")
        .insert(suprnova::attrs! { event: "x", actor_id: 1 })
        .await
        .unwrap();
    DB::table("audit_log")
        .insert(suprnova::attrs! { event: "y", actor_id: 2 })
        .await
        .unwrap();

    let affected = DB::affecting_statement(
        "UPDATE audit_log SET event = ? WHERE actor_id = ?",
        vec!["redacted".into(), 1i64.into()],
    )
    .await
    .unwrap();

    assert_eq!(affected, 1);

    // DB::update / DB::delete share the affecting_statement plumbing.
    let updated = DB::update(
        "UPDATE audit_log SET event = ? WHERE actor_id = ?",
        vec!["other".into(), 2i64.into()],
    )
    .await
    .unwrap();
    assert_eq!(updated, 1);

    let deleted = DB::delete(
        "DELETE FROM audit_log WHERE actor_id = ?",
        vec![1i64.into()],
    )
    .await
    .unwrap();
    assert_eq!(deleted, 1);
}
