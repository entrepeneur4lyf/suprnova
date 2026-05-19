//! Locks the TestDatabase no-migration constructor + raw-SQL helpers
//! that every Phase 10A test from T4 onwards depends on.

use suprnova::testing::TestDatabase;
use sea_orm::Value;

#[tokio::test]
async fn sqlite_memory_connects_and_registers_in_container() {
    let _db = TestDatabase::sqlite_memory().await.expect("connect");
    // DB::connection() must resolve to the test DB — proves container reg.
    let _resolved = suprnova::DB::connection().expect("resolved from container");
}

#[tokio::test]
async fn execute_unprepared_and_fetch_round_trip() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared("CREATE TABLE probe (id INTEGER PRIMARY KEY, name TEXT NOT NULL)").await.unwrap();
    db.execute_unprepared("INSERT INTO probe (id, name) VALUES (1, 'alpha'), (2, 'beta')").await.unwrap();

    let row = db.fetch_one("SELECT name FROM probe WHERE id = ?", vec![Value::from(1i64)])
        .await.unwrap();
    let name: String = row.try_get("", "name").unwrap();
    assert_eq!(name, "alpha");

    let rows = db.fetch_all("SELECT id FROM probe ORDER BY id", vec![]).await.unwrap();
    assert_eq!(rows.len(), 2);
}

#[tokio::test]
async fn fetch_one_errors_when_no_rows() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared("CREATE TABLE empty (id INTEGER)").await.unwrap();
    let err = db.fetch_one("SELECT id FROM empty", vec![]).await.unwrap_err();
    assert!(err.to_string().contains("no rows"), "got: {err}");
}
