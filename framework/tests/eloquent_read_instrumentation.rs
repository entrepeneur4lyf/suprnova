//! Regression — Eloquent typed-model READ terminals must emit
//! `QueryExecuted`, so model SELECTs surface in `DB::listen` / the query
//! log exactly like Laravel's. Before this fix `Builder::get`,
//! `Model::find`, `Model::find_many`, and `Model::all` executed SeaORM
//! `.all()` / `.one()` straight at the connection, bypassing the
//! `ExecutorChoice` instrumentation that every write — and the
//! model-less `DB::table(...).get()` path — already flowed through. That
//! left ORM reads invisible to query logging and `DB::listen`.
//!
//! Each test installs the recorder AFTER seeding, so only the single
//! read under test is captured. `Builder::get` is the terminal that
//! `first` / pagination / chunking / lazy cursors / relation `.get()`
//! and the eager-load IN-queries all delegate to, so instrumenting it
//! covers that whole family transitively.

use serial_test::serial;
use std::sync::{Arc, Mutex};
use suprnova::testing::TestDatabase;
use suprnova::{DB, Model, QueryExecuted, ReadWriteType, attrs, model};

#[model(table = "ri_users")]
pub struct RiUser {
    pub id: i64,
    pub name: String,
}

/// Fresh in-memory DB with the table created and three rows seeded.
/// Listeners are flushed first so a prior test can't leak a recorder
/// into the seeding writes.
async fn fresh() -> TestDatabase {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    DB::flush_listeners().unwrap();
    db.execute_unprepared(
        "CREATE TABLE ri_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
    )
    .await
    .unwrap();
    for name in ["alice", "bob", "carol"] {
        RiUser::create(attrs! { name: name.to_string() })
            .await
            .unwrap();
    }
    db
}

/// Register a `DB::listen` recorder. Called AFTER seeding so the sink
/// only ever sees the read terminal under test.
fn record() -> Arc<Mutex<Vec<QueryExecuted>>> {
    let sink = Arc::new(Mutex::new(Vec::new()));
    let sink_cb = sink.clone();
    DB::listen(move |event: &QueryExecuted| {
        sink_cb.lock().unwrap().push(event.clone());
    })
    .unwrap();
    sink
}

fn assert_single_read(events: &[QueryExecuted], table: &str) {
    assert_eq!(
        events.len(),
        1,
        "expected exactly one QueryExecuted; got {}:\n{}",
        events.len(),
        events
            .iter()
            .map(|e| e.sql.clone())
            .collect::<Vec<_>>()
            .join("\n"),
    );
    assert!(
        events[0].sql.contains(table),
        "SELECT must target {table}; got: {}",
        events[0].sql,
    );
    assert!(
        matches!(events[0].read_write_type, Some(ReadWriteType::Read)),
        "read terminal must be classified as Read; got {:?}",
        events[0].read_write_type,
    );
}

#[tokio::test]
#[serial]
async fn builder_get_emits_query_executed() {
    let _db = fresh().await;
    let sink = record();

    let rows = RiUser::query().get().await.unwrap();
    assert_eq!(rows.len(), 3);

    assert_single_read(&sink.lock().unwrap(), "ri_users");
    DB::flush_listeners().unwrap();
}

#[tokio::test]
#[serial]
async fn model_find_emits_query_executed() {
    let _db = fresh().await;
    let sink = record();

    let row = RiUser::find(1i64).await.unwrap();
    assert!(row.is_some());

    assert_single_read(&sink.lock().unwrap(), "ri_users");
    DB::flush_listeners().unwrap();
}

#[tokio::test]
#[serial]
async fn model_find_many_emits_query_executed() {
    let _db = fresh().await;
    let sink = record();

    let rows = RiUser::find_many([1i64, 2, 3]).await.unwrap();
    assert_eq!(rows.len(), 3);

    assert_single_read(&sink.lock().unwrap(), "ri_users");
    DB::flush_listeners().unwrap();
}

#[tokio::test]
#[serial]
async fn model_all_emits_query_executed() {
    let _db = fresh().await;
    let sink = record();

    let rows = RiUser::all().await.unwrap();
    assert_eq!(rows.len(), 3);

    assert_single_read(&sink.lock().unwrap(), "ri_users");
    DB::flush_listeners().unwrap();
}
