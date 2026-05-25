//! Regression: HIGH audit finding `database` #2 — `DbTableBuilder`
//! interpolates table names, column names, and operators directly
//! into SQL. SeaORM parameterises *values* but not identifiers (SQL
//! itself doesn't), so the validator is the only thing standing
//! between user input and the SQL string.
//!
//! These tests run real queries through `TestDatabase::sqlite_memory`
//! to prove:
//!   1. Sensible identifiers still work end-to-end (no regression).
//!   2. Injection payloads in `table` / `select` / `filter` columns /
//!      operators / `order_by` / insert/update columns are rejected
//!      at the boundary with a clear `FrameworkError::Domain` / param
//!      error, BEFORE the SQL ever reaches SeaORM.
//!
//! Coverage matches every entry point that lands an identifier or
//! operator in rendered SQL: `get`, `count`, `insert`, `update`,
//! `delete`.

use suprnova::eloquent::attrs::Attrs;
use suprnova::testing::TestDatabase;
use suprnova::DB;

async fn fresh_db_with_table() -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared("CREATE TABLE audit_log (id INTEGER PRIMARY KEY, event TEXT)")
        .await
        .unwrap();
    db
}

// ---- happy path: legitimate identifiers still work ----------------------

#[tokio::test]
async fn legitimate_identifiers_still_work_end_to_end() {
    let _db = fresh_db_with_table().await;
    // Insert via the model-less builder
    let mut attrs = Attrs::new();
    attrs.insert("event", serde_json::json!("user.signin"));
    let id = DB::table("audit_log").insert(attrs).await.unwrap();
    assert!(id > 0);

    // Select with filter
    let rows = DB::table("audit_log")
        .select(["id", "event"])
        .filter("event", "user.signin")
        .filter_op("id", ">=", 1)
        .order_by_desc("id")
        .limit(10)
        .get()
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);

    // Count
    let n = DB::table("audit_log").count().await.unwrap();
    assert_eq!(n, 1);
}

// ---- table-name injection -----------------------------------------------

#[tokio::test]
async fn malicious_table_name_is_rejected_on_get() {
    let _db = fresh_db_with_table().await;
    let err = DB::table("audit_log; DROP TABLE audit_log")
        .get()
        .await
        .expect_err("attacker-controlled table name must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("SQL identifier"),
        "error must call out identifier validation; got: {msg}"
    );
}

#[tokio::test]
async fn malicious_table_name_is_rejected_on_insert() {
    let _db = fresh_db_with_table().await;
    let mut attrs = Attrs::new();
    attrs.insert("event", serde_json::json!("x"));
    let err = DB::table("audit_log)--")
        .insert(attrs)
        .await
        .expect_err("attacker-controlled table name must be rejected at insert");
    assert!(format!("{err}").contains("SQL identifier"));
}

#[tokio::test]
async fn malicious_table_name_is_rejected_on_update() {
    let _db = fresh_db_with_table().await;
    let mut attrs = Attrs::new();
    attrs.insert("event", serde_json::json!("y"));
    let err = DB::table("audit_log WHERE 1=1")
        .filter("id", 1)
        .update(attrs)
        .await
        .expect_err("attacker-controlled table name must be rejected at update");
    assert!(format!("{err}").contains("SQL identifier"));
}

#[tokio::test]
async fn malicious_table_name_is_rejected_on_delete() {
    let _db = fresh_db_with_table().await;
    let err = DB::table("audit_log; TRUNCATE users")
        .delete()
        .await
        .expect_err("attacker-controlled table name must be rejected at delete");
    assert!(format!("{err}").contains("SQL identifier"));
}

#[tokio::test]
async fn malicious_table_name_is_rejected_on_count() {
    let _db = fresh_db_with_table().await;
    let err = DB::table("audit_log) UNION SELECT")
        .count()
        .await
        .expect_err("attacker-controlled table name must be rejected at count");
    assert!(format!("{err}").contains("SQL identifier"));
}

// ---- column-name injection ----------------------------------------------

#[tokio::test]
async fn malicious_select_column_is_rejected() {
    let _db = fresh_db_with_table().await;
    let err = DB::table("audit_log")
        .select(["id, (SELECT password FROM users) AS leak"])
        .get()
        .await
        .expect_err("attacker-controlled select column must be rejected");
    assert!(format!("{err}").contains("SQL identifier"));
}

#[tokio::test]
async fn malicious_filter_column_is_rejected() {
    let _db = fresh_db_with_table().await;
    let err = DB::table("audit_log")
        .filter("id) OR (1=1", 1)
        .get()
        .await
        .expect_err("attacker-controlled filter column must be rejected");
    assert!(format!("{err}").contains("SQL identifier"));
}

#[tokio::test]
async fn malicious_order_by_column_is_rejected() {
    let _db = fresh_db_with_table().await;
    let err = DB::table("audit_log")
        .order_by_desc("id; DROP TABLE audit_log")
        .get()
        .await
        .expect_err("attacker-controlled order-by column must be rejected");
    assert!(format!("{err}").contains("SQL identifier"));
}

#[tokio::test]
async fn malicious_insert_column_is_rejected() {
    let _db = fresh_db_with_table().await;
    let mut attrs = Attrs::new();
    attrs.insert("event), (1, (SELECT", serde_json::json!("x"));
    let err = DB::table("audit_log")
        .insert(attrs)
        .await
        .expect_err("attacker-controlled attrs key must be rejected");
    assert!(format!("{err}").contains("SQL identifier"));
}

#[tokio::test]
async fn malicious_update_column_is_rejected() {
    let _db = fresh_db_with_table().await;
    let mut attrs = Attrs::new();
    attrs.insert("event = 'pwned' WHERE 1=1; --", serde_json::json!("x"));
    let err = DB::table("audit_log")
        .filter("id", 1)
        .update(attrs)
        .await
        .expect_err("attacker-controlled attrs key must be rejected on update");
    assert!(format!("{err}").contains("SQL identifier"));
}

// ---- operator injection -------------------------------------------------

#[tokio::test]
async fn malicious_operator_is_rejected() {
    let _db = fresh_db_with_table().await;
    let err = DB::table("audit_log")
        .filter_op("id", "= 1 OR 1=1 --", 0)
        .get()
        .await
        .expect_err("attacker-controlled operator must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.contains("operator"),
        "error must call out operator validation; got: {msg}"
    );
}

#[tokio::test]
async fn operator_allowlist_accepts_canonical_comparisons() {
    let _db = fresh_db_with_table().await;
    // Each of these is on the allowlist.
    for op in ["=", "<>", "!=", "<", "<=", ">", ">=", "LIKE", "like"] {
        let result = DB::table("audit_log")
            .filter_op("id", op, 1)
            .get()
            .await;
        assert!(
            result.is_ok(),
            "operator {op:?} should pass the allowlist; got: {:?}",
            result.err()
        );
    }
}
