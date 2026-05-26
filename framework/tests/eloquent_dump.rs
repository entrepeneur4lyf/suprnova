//! Phase 10C T14 — `Builder::dump()` + `Builder::dd()` interactive
//! debugging helpers.
//!
//! Mirrors Laravel's `Builder::dump()` (logs SQL and returns the
//! builder so the call stays in the chain) and `Builder::dd()`
//! ("dump-and-die" — logs then panics with the SQL in the message).
//!
//! Both fall back to SQLite when no DB connection is initialised so
//! they remain useful at REPL or in a test without `TestDatabase` —
//! exactly the moments interactive debugging happens.

use suprnova::Model;
use suprnova::testing::TestDatabase;

#[suprnova::model(table = "t14_dump_x")]
pub struct T14X {
    pub id: i64,
    pub name: String,
    pub active: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[tokio::test]
async fn dump_returns_builder_for_chaining() {
    // With an active connection: dump compiles, logs, and the
    // builder is still usable for further chaining.
    let _db = TestDatabase::sqlite_memory().await.unwrap();

    let b = T14X::query()
        .filter("active", true)
        .dump() // logs via tracing, returns self
        .order_by_desc("id")
        .limit(10);

    // Confirm the post-dump chain still renders SQL correctly.
    let sql = b.to_sql();
    assert!(
        sql.contains("FROM \"t14_dump_x\"") || sql.contains("FROM t14_dump_x"),
        "post-dump builder still renders SQL: {sql}"
    );
    assert!(
        sql.contains("ORDER BY"),
        "order_by survives after dump: {sql}"
    );
}

#[test]
fn dump_without_connection_falls_back_to_sqlite() {
    // No `TestDatabase` here — dump should NOT panic. It logs at
    // info!, picks SQLite as the fallback dialect, and returns self.
    let _b = T14X::query().filter("active", true).dump();
}

#[test]
#[should_panic(expected = "eloquent dd")]
fn dd_panics_with_sql_in_message() {
    // No DB connection needed — dd renders with the SQLite fallback
    // backend and panics with the rendered SQL embedded in the
    // panic message.
    T14X::query().filter("id", 1).dd();
}
