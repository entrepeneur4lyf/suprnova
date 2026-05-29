//! Laravel-13 parity — raw helper completeness.
//!
//! Covers `DB::select_one`, `DB::scalar`, `DB::insert` (raw),
//! `DB::statement` (with bindings), `DB::unprepared`, plus the
//! connection metadata surface (`database_name`, `driver_name`,
//! `driver_title`, `server_version`).

use serial_test::serial;
use suprnova::DB;
use suprnova::testing::TestDatabase;

async fn setup() -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, \
         name TEXT NOT NULL, active INTEGER NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_unprepared(
        "INSERT INTO users (name, active) VALUES ('alice', 1), ('bob', 1), ('carol', 0)",
    )
    .await
    .unwrap();
    db
}

// ---- DB::select_one -----------------------------------------------------

#[tokio::test]
#[serial]
async fn select_one_returns_first_row() {
    let _db = setup().await;
    let row = DB::select_one(
        "SELECT * FROM users ORDER BY id LIMIT 1",
        Vec::<sea_orm::Value>::new(),
    )
    .await
    .unwrap();
    let row = row.expect("must return a row");
    assert_eq!(row.get_string("name").unwrap(), "alice");
}

#[tokio::test]
#[serial]
async fn select_one_returns_none_on_empty_result() {
    let _db = setup().await;
    let row = DB::select_one(
        "SELECT * FROM users WHERE name = 'never'",
        Vec::<sea_orm::Value>::new(),
    )
    .await
    .unwrap();
    assert!(row.is_none());
}

// ---- DB::scalar ---------------------------------------------------------

#[tokio::test]
#[serial]
async fn scalar_returns_first_column_of_first_row_i64() {
    let _db = setup().await;
    let count: i64 = DB::scalar("SELECT COUNT(*) FROM users", Vec::<sea_orm::Value>::new())
        .await
        .unwrap();
    assert_eq!(count, 3);
}

#[tokio::test]
#[serial]
async fn scalar_returns_first_column_of_first_row_string() {
    let _db = setup().await;
    let name: String = DB::scalar(
        "SELECT name FROM users ORDER BY id LIMIT 1",
        Vec::<sea_orm::Value>::new(),
    )
    .await
    .unwrap();
    assert_eq!(name, "alice");
}

#[tokio::test]
#[serial]
async fn scalar_errors_on_empty_result() {
    let _db = setup().await;
    let res: Result<i64, _> = DB::scalar(
        "SELECT id FROM users WHERE name = 'never'",
        Vec::<sea_orm::Value>::new(),
    )
    .await;
    assert!(res.is_err(), "scalar must error on no rows");
}

// ---- DB::insert (raw) ---------------------------------------------------

#[tokio::test]
#[serial]
async fn insert_raw_returns_true_on_success() {
    let _db = setup().await;
    let inserted = DB::insert(
        "INSERT INTO users (name, active) VALUES (?, ?)",
        vec![sea_orm::Value::from("dave"), sea_orm::Value::from(1)],
    )
    .await
    .unwrap();
    assert!(inserted);
    let count: i64 = DB::scalar("SELECT COUNT(*) FROM users", Vec::<sea_orm::Value>::new())
        .await
        .unwrap();
    assert_eq!(count, 4);
}

// ---- DB::statement (with bindings) --------------------------------------

#[tokio::test]
#[serial]
async fn statement_runs_dml_with_bindings() {
    let _db = setup().await;
    let ok = DB::statement(
        "UPDATE users SET active = 0 WHERE id = ?",
        vec![sea_orm::Value::from(1)],
    )
    .await
    .unwrap();
    assert!(ok);
    let active_count: i64 = DB::scalar(
        "SELECT COUNT(*) FROM users WHERE active = 1",
        Vec::<sea_orm::Value>::new(),
    )
    .await
    .unwrap();
    assert_eq!(active_count, 1, "one of the two active users went inactive");
}

// ---- DB::unprepared -----------------------------------------------------

#[tokio::test]
#[serial]
async fn unprepared_runs_ddl_without_bindings() {
    let _db = setup().await;
    let ok = DB::unprepared("CREATE INDEX idx_users_name ON users(name)")
        .await
        .unwrap();
    assert!(ok);
    let row = DB::select_one(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'idx_users_name'",
        Vec::<sea_orm::Value>::new(),
    )
    .await
    .unwrap();
    assert!(row.is_some(), "index must be visible to sqlite_master");
}

// ---- Connection metadata ------------------------------------------------

#[test]
fn database_type_classification() {
    use suprnova::{DatabaseConfig, DatabaseType};
    let pg = DatabaseConfig::builder()
        .url("postgres://u:p@localhost/db")
        .build();
    assert_eq!(pg.database_type(), DatabaseType::Postgres);
    let pg2 = DatabaseConfig::builder()
        .url("postgresql://u:p@localhost/db")
        .build();
    assert_eq!(pg2.database_type(), DatabaseType::Postgres);
    let my = DatabaseConfig::builder()
        .url("mysql://u:p@localhost/db")
        .build();
    assert_eq!(my.database_type(), DatabaseType::Mysql);
    let sl = DatabaseConfig::builder()
        .url("sqlite://./db.sqlite")
        .build();
    assert_eq!(sl.database_type(), DatabaseType::Sqlite);
    let unk = DatabaseConfig::builder()
        .url("oracle://localhost/db")
        .build();
    assert_eq!(unk.database_type(), DatabaseType::Unknown);
}

#[tokio::test]
#[serial]
async fn server_version_returns_sqlite_version_string() {
    let _db = setup().await;
    let version = DB::server_version().await.unwrap();
    assert!(
        !version.is_empty(),
        "server_version must return a non-empty string: {version}",
    );
    // SQLite version format: X.Y.Z (e.g. 3.42.0). Verify it parses
    // at least as a major version number.
    let major = version.split('.').next().unwrap();
    assert!(
        major.parse::<u32>().is_ok(),
        "server_version major component must be numeric: {version}",
    );
}
