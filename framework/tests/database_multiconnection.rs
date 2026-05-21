//! Phase 10C T12 — Multi-Connection + Read-Write Split integration tests.
//!
//! Exercises:
//!
//! - [`DB::register_named`] / [`DB::named`] + [`ConnectionRegistry`].
//! - [`Builder::on(name)`](suprnova::Builder::on) for per-query routing.
//! - [`Builder::on_write_connection`] override back to primary.
//! - `__read_replica__` auto-routing of read terminals.
//! - Writes (`Model::create`) ignoring the replica.
//! - `#[model(connection = "warehouse")]` per-model default routing.
//! - In-transaction safety: `on(name)` is ignored when CURRENT_TX is
//!   set so atomicity isn't split across connections.
//! - `__primary__` registration rejection (reserved name).
//! - `DB::table_on` / `DB::select_on` / `DB::statement_on` raw escapes.
//!
//! ## Test isolation
//!
//! The connection registry is process-global. Tests in this file run
//! `#[serial_test::serial]` because they all touch the well-known
//! `__read_replica__` / `warehouse` names. The
//! [`TestDatabase`](suprnova::testing::TestDatabase) teardown calls
//! [`ConnectionRegistry::clear`] via the `TestContainerGuard::drop`
//! hook, so the next test starts with an empty registry — but
//! `#[serial]` is still required to prevent two tests racing to
//! register under the same name within the SAME process slot.

use chrono::{DateTime, Utc};
use serial_test::serial;
use suprnova::database::ConnectionRegistry;
use suprnova::testing::TestDatabase;
use suprnova::{attrs, model, Model, DB};

#[model(
    table = "t12_users",
    fillable = ["email"],
)]
pub struct T12User {
    pub id: i64,
    pub email: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[model(
    table = "t12_events",
    connection = "warehouse",
    fillable = ["event_name"],
)]
pub struct AnalyticsEvent {
    pub id: i64,
    pub event_name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

async fn fresh_users_table(db: &TestDatabase) {
    db.execute_unprepared(
        "CREATE TABLE t12_users (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, \
            email TEXT NOT NULL, \
            created_at TEXT NOT NULL, \
            updated_at TEXT NOT NULL\
         )",
    )
    .await
    .unwrap();
}

/// Step 1 of the plan — `DB::register_named` adds a named connection,
/// `Model::on(name)` runs a query against it.
#[tokio::test]
#[serial]
async fn register_named_connection_and_query_against_it() {
    let primary = TestDatabase::sqlite_memory().await.unwrap();
    fresh_users_table(&primary).await;
    primary
        .execute_unprepared(
            "INSERT INTO t12_users (email, created_at, updated_at) \
             VALUES ('alice@x.com', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
        )
        .await
        .unwrap();

    // The "named" connection points at the same in-memory DB for the
    // round-trip assertion. (SQLite in-memory connections are
    // process-private — connecting twice would yield two empty
    // databases. Sharing one connection across the registry slot is
    // the right shape for routing-correctness tests.)
    ConnectionRegistry::register_existing("analytics_read", primary.db().clone())
        .await
        .unwrap();

    let users = T12User::on("analytics_read").get().await.unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].email, "alice@x.com");
}

/// Step 2 — read-write split routing pin. When `__read_replica__` is
/// registered, default reads land on it; `on_write_connection` opts
/// back to the primary; writes never touch the replica.
#[tokio::test]
#[serial]
async fn read_replica_routes_read_queries() {
    let primary = TestDatabase::sqlite_memory().await.unwrap();
    fresh_users_table(&primary).await;
    primary
        .execute_unprepared(
            "INSERT INTO t12_users (email, created_at, updated_at) \
             VALUES ('primary@x.com', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
        )
        .await
        .unwrap();

    // The replica is a SEPARATE in-memory DB with its own seed row.
    // Routing-correctness: if the query lands on the primary we see
    // `primary@x.com`; if it lands on the replica we see
    // `replica@x.com`. The two DBs share no rows.
    let replica_conn = sea_orm::Database::connect("sqlite::memory:?mode=rwc")
        .await
        .expect("replica in-memory connection");
    let replica = suprnova::DbConnection::from_raw(replica_conn);
    use sea_orm::ConnectionTrait;
    replica
        .inner()
        .execute_unprepared(
            "CREATE TABLE t12_users (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                email TEXT NOT NULL, \
                created_at TEXT NOT NULL, \
                updated_at TEXT NOT NULL\
             )",
        )
        .await
        .unwrap();
    replica
        .inner()
        .execute_unprepared(
            "INSERT INTO t12_users (email, created_at, updated_at) \
             VALUES ('replica@x.com', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
        )
        .await
        .unwrap();

    ConnectionRegistry::register_existing("__read_replica__", replica.clone())
        .await
        .unwrap();

    // Default read — routes to replica via auto-routing step 5.
    let users = T12User::query().get().await.unwrap();
    assert_eq!(users.len(), 1, "default reads route to replica");
    assert_eq!(users[0].email, "replica@x.com");

    // `on_write_connection` opts back to primary even though replica
    // is registered.
    let users = T12User::on_write_connection().get().await.unwrap();
    assert_eq!(
        users.len(),
        1,
        "on_write_connection routes back to primary"
    );
    assert_eq!(users[0].email, "primary@x.com");
}

/// Step 3 — writes always go to primary. Even with the replica
/// registered, `Model::create` lands on the default pool; the replica
/// stays untouched.
#[tokio::test]
#[serial]
async fn writes_always_go_to_primary() {
    let primary = TestDatabase::sqlite_memory().await.unwrap();
    fresh_users_table(&primary).await;

    let replica_conn = sea_orm::Database::connect("sqlite::memory:?mode=rwc")
        .await
        .expect("replica in-memory connection");
    let replica = suprnova::DbConnection::from_raw(replica_conn);
    use sea_orm::ConnectionTrait;
    replica
        .inner()
        .execute_unprepared(
            "CREATE TABLE t12_users (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                email TEXT NOT NULL, \
                created_at TEXT NOT NULL, \
                updated_at TEXT NOT NULL\
             )",
        )
        .await
        .unwrap();

    ConnectionRegistry::register_existing("__read_replica__", replica.clone())
        .await
        .unwrap();

    // Create — write goes to primary because resolve_write skips the
    // replica auto-routing step.
    let _ = T12User::create(attrs! { email: "fresh@x.com" })
        .await
        .unwrap();

    // The new row exists on the primary. We read it back through
    // `on_write_connection` to be explicit.
    let on_primary = T12User::on_write_connection()
        .filter("email", "fresh@x.com")
        .get()
        .await
        .unwrap();
    assert_eq!(on_primary.len(), 1, "write landed on primary");

    // The replica is untouched.
    let on_replica = T12User::on("__read_replica__")
        .filter("email", "fresh@x.com")
        .get()
        .await
        .unwrap();
    assert_eq!(on_replica.len(), 0, "write did not leak to replica");
}

/// Step 4 — `#[model(connection = "warehouse")]` routes the model's
/// default reads + writes through the named connection without any
/// per-query `on(...)`.
#[tokio::test]
#[serial]
async fn per_model_connection_attribute_routes_default() {
    // Primary is empty; we expect every event-related query to land
    // on the warehouse connection. Set up a TestDatabase as the
    // primary so `DB::connection()` is wired but unused.
    let _primary = TestDatabase::sqlite_memory().await.unwrap();

    let warehouse_conn = sea_orm::Database::connect("sqlite::memory:?mode=rwc")
        .await
        .expect("warehouse in-memory connection");
    let warehouse = suprnova::DbConnection::from_raw(warehouse_conn);
    use sea_orm::ConnectionTrait;
    warehouse
        .inner()
        .execute_unprepared(
            "CREATE TABLE t12_events (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                event_name TEXT NOT NULL, \
                created_at TEXT NOT NULL, \
                updated_at TEXT NOT NULL\
             )",
        )
        .await
        .unwrap();
    warehouse
        .inner()
        .execute_unprepared(
            "INSERT INTO t12_events (event_name, created_at, updated_at) \
             VALUES ('click', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
        )
        .await
        .unwrap();

    ConnectionRegistry::register_existing("warehouse", warehouse.clone())
        .await
        .unwrap();

    // Default read — routes to warehouse via the per-model attribute
    // (step 4 of the precedence chain).
    let events = AnalyticsEvent::query().get().await.unwrap();
    assert_eq!(events.len(), 1, "per-model connection routes default read");
    assert_eq!(events[0].event_name, "click");

    // Default write — also lands on warehouse (no replica-skip
    // applies, the per-model default takes precedence over the empty
    // primary).
    AnalyticsEvent::create(attrs! { event_name: "pageview" })
        .await
        .unwrap();

    // Verify the row landed on warehouse and NOT on primary.
    let on_warehouse = AnalyticsEvent::query()
        .filter("event_name", "pageview")
        .get()
        .await
        .unwrap();
    assert_eq!(on_warehouse.len(), 1, "write landed on warehouse");

    // Primary doesn't even have the t12_events table — opting to
    // primary should fail because no such table exists there.
    let primary_attempt = AnalyticsEvent::on_write_connection()
        .filter("event_name", "pageview")
        .get()
        .await;
    assert!(
        primary_attempt.is_err(),
        "primary has no t12_events table; on_write_connection should error"
    );
}

/// Step 5 — inside a transaction, `on(name)` is silently ignored.
/// Every operation runs through the tx's connection because
/// atomicity must not split across connections.
#[tokio::test]
#[serial]
async fn transaction_ignores_on_name_routing() {
    let primary = TestDatabase::sqlite_memory().await.unwrap();
    fresh_users_table(&primary).await;

    // Register a separate replica with its own table so we can
    // detect routing.
    let replica_conn = sea_orm::Database::connect("sqlite::memory:?mode=rwc")
        .await
        .expect("replica in-memory connection");
    let replica = suprnova::DbConnection::from_raw(replica_conn);
    use sea_orm::ConnectionTrait;
    replica
        .inner()
        .execute_unprepared(
            "CREATE TABLE t12_users (\
                id INTEGER PRIMARY KEY AUTOINCREMENT, \
                email TEXT NOT NULL, \
                created_at TEXT NOT NULL, \
                updated_at TEXT NOT NULL\
             )",
        )
        .await
        .unwrap();
    replica
        .inner()
        .execute_unprepared(
            "INSERT INTO t12_users (email, created_at, updated_at) \
             VALUES ('replica@x.com', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
        )
        .await
        .unwrap();

    ConnectionRegistry::register_existing("isolated_alt", replica.clone())
        .await
        .unwrap();

    // Inside DB::transaction, `on("isolated_alt")` is silently
    // ignored — the read runs through the transaction's connection
    // (which is on the primary), so it sees an empty table.
    let result = DB::transaction(|_tx| {
        Box::pin(async move {
            let users = T12User::on("isolated_alt").get().await?;
            Ok::<_, suprnova::FrameworkError>(users.len())
        })
    })
    .await
    .unwrap();
    assert_eq!(
        result, 0,
        "transaction body sees primary (empty), not isolated_alt"
    );

    // Outside the transaction, `on("isolated_alt")` routes correctly.
    let users = T12User::on("isolated_alt").get().await.unwrap();
    assert_eq!(users.len(), 1, "outside-tx routing works");
    assert_eq!(users[0].email, "replica@x.com");
}

/// Registering under `__primary__` is rejected — that name is
/// reserved for the default pool.
#[tokio::test]
#[serial]
async fn register_named_rejects_reserved_primary() {
    let primary = TestDatabase::sqlite_memory().await.unwrap();

    let result = ConnectionRegistry::register_existing(
        suprnova::PRIMARY_CONNECTION_NAME,
        primary.db().clone(),
    )
    .await;
    assert!(
        result.is_err(),
        "registering under __primary__ must be rejected"
    );
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("reserved"),
        "error must mention 'reserved'; got: {msg}"
    );
}

/// `DB::table_on` + `DB::select_on` + `DB::statement_on` raw escapes.
/// Build a row on a named connection through the dynamic-row builder.
#[tokio::test]
#[serial]
async fn db_facade_named_connection_escapes() {
    let _primary = TestDatabase::sqlite_memory().await.unwrap();

    let warehouse_conn = sea_orm::Database::connect("sqlite::memory:?mode=rwc")
        .await
        .expect("warehouse in-memory connection");
    let warehouse = suprnova::DbConnection::from_raw(warehouse_conn);
    ConnectionRegistry::register_existing("aux_warehouse", warehouse.clone())
        .await
        .unwrap();

    // statement_on — DDL against the warehouse.
    DB::statement_on(
        "aux_warehouse",
        "CREATE TABLE t12_aux (id INTEGER PRIMARY KEY, label TEXT NOT NULL)",
    )
    .await
    .unwrap();

    // table_on — chainable builder pinned to the warehouse.
    let id = DB::table_on("aux_warehouse", "t12_aux")
        .insert(attrs! { label: "hello" })
        .await
        .unwrap();
    assert!(id > 0, "insert returned a positive id");

    // select_on — raw select against the warehouse.
    let rows = DB::select_on(
        "aux_warehouse",
        "SELECT id, label FROM t12_aux WHERE label = ?",
        vec![sea_orm::Value::String(Some(Box::new("hello".to_string())))],
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "select_on returned one row");
    assert_eq!(
        rows[0].get("label").and_then(|v| v.as_str()),
        Some("hello")
    );

    // Primary doesn't have the t12_aux table.
    let primary_attempt = DB::select(
        "SELECT id FROM t12_aux",
        Vec::<sea_orm::Value>::new(),
    )
    .await;
    assert!(
        primary_attempt.is_err(),
        "primary has no t12_aux table; raw select should error"
    );
}
