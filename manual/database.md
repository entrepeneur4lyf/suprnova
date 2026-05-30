# Database

Suprnova's database layer wraps SeaORM with a Laravel-shaped `DB` facade:
raw query escapes, a model-less query builder, transactions with
savepoints and retry-on-deadlock, connection registry for read replicas
and shards, and a full observability surface that mirrors Laravel 13's
`DB::listen` / `QueryExecuted` / query log API.

The Eloquent ORM (`use suprnova::eloquent::*`) builds on top of this
layer and lives in [eloquent.md](eloquent.md.md). When you want a typed
model, go there; when you want a raw query against an unmodeled table
or want to observe every query the framework runs, this is the page.

## Configuration

```rust
use suprnova::{Config, DB, DatabaseConfig};

// In bootstrap.rs
Config::register(DatabaseConfig::from_env());
DB::init().await.expect("DB::init failed");
```

`DatabaseConfig::from_env` reads `DATABASE_URL` and (optionally) the
pool tunables `DB_MAX_CONNECTIONS`, `DB_MIN_CONNECTIONS`,
`DB_CONNECT_TIMEOUT`, `DB_LOGGING`. When `DATABASE_URL` is unset the
config falls back to `sqlite://./database.db` — convenient for
zero-setup development; production boots refuse the fallback via
`validate_for_environment` so you can't accidentally ship a SQLite
file in `APP_ENV=production`.

URL → driver detection:

```text
postgres://user:pass@host/db       → DatabaseType::Postgres
postgresql://user:pass@host/db     → DatabaseType::Postgres
mysql://user:pass@host/db          → DatabaseType::Mysql
sqlite://./file.db                 → DatabaseType::Sqlite
sqlite::memory:                    → DatabaseType::Sqlite
```

## Raw queries

The `DB` facade ships the full Laravel 13 raw escape surface. Every
helper goes through the same instrumented executor — every call fires
`QueryExecuted` (see [Observability](#observability)).

```rust
use suprnova::DB;
use sea_orm::Value;

// SELECT — all rows as DynamicRow.
let users = DB::select(
    "SELECT * FROM users WHERE active = ?",
    vec![Value::from(true)],
).await?;

// SELECT — first row only.
let alice = DB::select_one(
    "SELECT * FROM users WHERE name = ?",
    vec![Value::from("alice")],
).await?;

// SELECT — first column of first row as a typed value.
let count: i64 = DB::scalar(
    "SELECT COUNT(*) FROM users",
    vec![],
).await?;

// INSERT — returns bool (true when at least one row was affected).
DB::insert(
    "INSERT INTO users (name, active) VALUES (?, ?)",
    vec![Value::from("bob"), Value::from(true)],
).await?;

// UPDATE / DELETE — return the rows-affected count.
let updated = DB::update(
    "UPDATE users SET active = ? WHERE id = ?",
    vec![Value::from(false), Value::from(1)],
).await?;
let deleted = DB::delete(
    "DELETE FROM users WHERE active = ?",
    vec![Value::from(false)],
).await?;

// Any prepared statement with bindings.
DB::statement(
    "UPDATE users SET votes = votes + ? WHERE id = ?",
    vec![Value::from(1), Value::from(42)],
).await?;

// DDL with no bindings — `unprepared` mirrors Laravel's
// `DB::unprepared` for statements (CREATE INDEX, ALTER TABLE, VACUUM)
// that reject placeholder binding.
DB::unprepared("CREATE INDEX idx_users_name ON users(name)").await?;

// affecting_statement is the explicit form used by update/delete
// internally — drop to it directly for ops that don't fit either name
// (e.g. INSERT...ON CONFLICT DO UPDATE).
let affected = DB::affecting_statement(
    "INSERT INTO users (id, name) VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET name = excluded.name",
    vec![Value::from(1), Value::from("alice")],
).await?;
```

### Placeholder syntax

`?` for SQLite + MySQL. `$1`, `$2`, ... for Postgres. The active
backend is auto-detected from `DatabaseConfig::url`.

### DynamicRow

Untyped rows materialise as `DynamicRow` — a `serde_json::Map` newtype
with typed accessors:

```rust
for row in users {
    let id: i64 = row.get_int("id")?;
    let name: String = row.get_string("name")?;
    let active: Option<bool> = row.get_optional_bool("active")?;
    // Deserialise an arbitrary T:
    let prefs: UserPrefs = row.get_as("prefs")?;
}
```

`get_*` errors when the column is absent OR null. `get_optional_*`
errors only when absent and returns `Ok(None)` for SQL NULL. See
[DynamicRow](dynamic_row.rs.md) for the full
accessor surface.

## Model-less query builder — `DB::table`

For ad-hoc queries against tables you haven't bothered to model with
`#[suprnova::model]`, `DB::table(...)` returns a chainable builder
shaped like the Eloquent `Builder<M>` but materialising rows as
`DynamicRow`:

```rust
use suprnova::{DB, attrs};

let rows = DB::table("audit_log")
    .select(["id", "event", "actor_id"])
    .filter("actor_id", 42i64)
    .filter_op("created_at", ">=", "2025-01-01")
    .order_by_desc("id")
    .limit(50)
    .get()
    .await?;

let first = DB::table("audit_log")
    .filter("event", "user.deleted")
    .first()
    .await?;

let count = DB::table("audit_log")
    .filter("actor_id", 42i64)
    .count()
    .await?;

let id = DB::table("audit_log")
    .insert(attrs! { event: "user.created", actor_id: 42 })
    .await?;

let updated = DB::table("audit_log")
    .filter("id", id)
    .update(attrs! { event: "user.created.v2" })
    .await?;

let deleted = DB::table("audit_log")
    .filter("actor_id", 42i64)
    .delete()
    .await?;
```

### Trust boundary on identifiers

Table names, column names, ORDER BY directions, and SQL operators are
interpolated INTO the SQL string verbatim — they are NOT bound as
parameters (SQL doesn't allow placeholder-bound identifiers). Treat
every `impl Into<String>` argument as a TRUSTED literal:

```rust
// Safe — the column name is a constant.
DB::table("users").filter("email", request.email()).get().await?;

// UNSAFE — never splice user input into a column name.
DB::table("users").filter(&request.column_name(), value).get().await?;
```

Values (the right-hand side of `filter` / `filter_op`) ARE bound as
parameters and safe for user input.

The framework enforces a strict allowlist on identifiers
(`[A-Za-z_][A-Za-z0-9_]*` with one optional `schema.` prefix) and
operators (`=`, `<>`, `<`, `<=`, `>`, `>=`, `LIKE`, `NOT LIKE`,
`ILIKE`, `NOT ILIKE`, `IS`, `IS NOT`). Violations error at the I/O
boundary before the SQL string is rendered.

## Transactions

Three entry points, each with the `QueryExecuted` /
`TransactionBeginning` / `TransactionCommitted` /
`TransactionRolledBack` observation hooks wired in.

### Closure form

```rust
use suprnova::DB;

DB::transaction(|_tx| {
    Box::pin(async move {
        let mut alice = User::query().filter("name", "alice").first_or_fail().await?;
        alice.balance -= 30;
        alice.save().await?;

        let mut bob = User::query().filter("name", "bob").first_or_fail().await?;
        bob.balance += 30;
        bob.save().await?;
        Ok::<(), suprnova::FrameworkError>(())
    })
}).await?;
```

Commit on `Ok(_)`. Rollback + propagate the error on `Err(_)`.

Operations inside the closure automatically pick up the active
transaction via a `tokio::task_local` — you do NOT have to thread a
`&tx` handle through every model call. Nested `DB::transaction`
returns a database error; use `tx.savepoint(...)` for nested-rollback
behaviour.

### Retry on deadlock

```rust
DB::transaction_with_attempts(5, |_tx| {
    Box::pin(async move {
        // Same closure body as above. Re-runs from scratch on
        // SQLSTATE 40001 / 40P01 / any error containing "deadlock"
        // (case-insensitive).
        Ok::<(), suprnova::FrameworkError>(())
    })
}).await?;
```

### Manual form

```rust
let tx = DB::begin_transaction().await?;

User::create(/* ... */, &tx).await?;
Order::create(/* ... */, &tx).await?;

if some_condition() {
    tx.rollback().await?;
} else {
    tx.commit().await?;
}
```

Manual mode does NOT install the task-local — scope individual
operations through the transaction with `Builder::with_tx(&tx)` or
the `Model::*_with_tx` shims. Holding a `Transaction` handle pins one
pool connection for its lifetime; pre-load any rows you need to read
BEFORE the `begin_transaction()` call, especially on SQLite.

### Savepoints

```rust
DB::transaction(|tx| {
    Box::pin(async move {
        Order::create(/* ... */).await?;

        tx.savepoint("after_order").await?;
        if let Err(e) = Payment::charge().await {
            // Drop the payment attempt but keep the order.
            tx.rollback_to("after_order").await?;
        }
        Ok::<(), suprnova::FrameworkError>(())
    })
}).await?;
```

All three first-class backends support `SAVEPOINT` / `ROLLBACK TO
SAVEPOINT` — SQLite included.

## Observability

Laravel 13's `DB::listen` / `QueryExecuted` / query log surface, ported
to Rust through Suprnova's event dispatcher.

### `DB::listen` — direct callback

```rust
use suprnova::{DB, QueryExecuted};

// In bootstrap.rs (or a service provider).
DB::listen(|event: &QueryExecuted| {
    tracing::debug!(
        sql = %event.sql,
        bindings = ?event.bindings,
        time_ms = event.time.as_millis(),
        connection = %event.connection_name,
        "query executed",
    );
})?;
```

Listeners run **synchronously inside the executor helper**. A slow
listener slows the query — keep direct callbacks light. For anything
that can fail, prefer the `EventFacade` path below; it runs through
`dispatch_best_effort` and tolerates errors.

### `EventFacade` dispatch path

`QueryExecuted` is a real `suprnova::Event` — listen through the
dispatcher to get queued, fakeable, fail-tolerant delivery:

```rust
use suprnova::{EventFacade, Listener, QueryExecuted, FrameworkError};
use std::sync::Arc;

struct LogToDatabase;

#[suprnova::async_trait]
impl Listener<QueryExecuted> for LogToDatabase {
    async fn handle(&self, event: &QueryExecuted) -> Result<(), FrameworkError> {
        // Even if THIS listener queries the database, the re-entrancy
        // guard prevents infinite recursion.
        DB::statement(
            "INSERT INTO query_log (sql, time_ms) VALUES (?, ?)",
            vec![event.sql.clone().into(), (event.time.as_millis() as i64).into()],
        ).await?;
        Ok(())
    }
}

// In bootstrap.rs.
EventFacade::listen::<QueryExecuted, _>(Arc::new(LogToDatabase)).await;
```

Listeners on this path:

- Run through `dispatch_best_effort` — a failing listener does NOT
  fail the query.
- Are short-circuited when they themselves issue a query (re-entrancy
  guard).
- Can use `Event::fake()` in tests to assert dispatch without
  actually running listeners.

### In-memory query log

```rust
DB::enable_query_log()?;

User::query().filter("active", true).get().await?;
Order::query().count().await?;

let log = DB::get_query_log()?;
for query in &log {
    println!("{} ({}ms)", query.sql, query.time.as_millis());
}

DB::flush_query_log()?;     // drop entries, keep enabled
DB::disable_query_log()?;   // stop capturing
let still_capturing = DB::logging();
```

The log is **unbounded** — every captured query grows it until the
process exits, `flush_query_log()` runs, or `disable_query_log()` is
called. Use it for development, not as a long-running production
profiler.

### Transaction lifecycle events

`TransactionBeginning`, `TransactionCommitted`, and
`TransactionRolledBack` are real `suprnova::Event` types — listen for
them through `EventFacade::listen` to drive auditing, distributed
locks, or compensation logic.

```rust
EventFacade::listen::<TransactionCommitted, _>(Arc::new(AuditCommit)).await;
EventFacade::listen::<TransactionRolledBack, _>(Arc::new(MetricRollback)).await;
```

All three transaction entry points
(`DB::transaction` / `DB::transaction_with_attempts` /
`DB::begin_transaction` + `Transaction::commit`/`rollback`) fire the
events. A leaked manual `Transaction` handle that gets dropped without
explicit commit/rollback emits no event — SeaORM's `Drop` impl is
synchronous and can't reach the async dispatcher.

### `QueryExecuted` payload

```rust
pub struct QueryExecuted {
    pub sql: String,
    pub bindings: Vec<String>,         // debug-rendered (`{:?}`)
    pub time: std::time::Duration,
    pub connection_name: String,
    pub read_write_type: Option<ReadWriteType>,
    pub result: Result<(), String>,    // Err on driver error
}
```

`to_raw_sql()` substitutes the captured bindings into the SQL for
display:

```rust
let query = /* captured from a listener */;
println!("{}", query.to_raw_sql());
// SELECT * FROM users WHERE id = 42 AND active = true
```

The substitution is **debug-format** (not SQL-safe escaping) and is
intended for log output only. Never feed the result back into a query.

### Coverage scope

Today, `QueryExecuted` fires for every query that goes through the
instrumented `ExecutorChoice` helpers:

- Every raw helper on `DB` (`select` / `select_one` / `scalar` /
  `insert` / `update` / `delete` / `statement` / `affecting_statement` /
  `unprepared`).
- Every terminal method on `DbTableBuilder` (the model-less builder).
- `DB::transaction` / `DB::begin_transaction` BEGIN / COMMIT / ROLLBACK
  fire transaction events.
- `DbConnection::connect` fires `ConnectionEstablished`.

The Eloquent ORM (`Builder<M>::get` / `first` / `count`, model CRUD)
matches the `ExecutorChoice` `Tx` / `Pool` arms directly today rather
than calling through the instrumented helpers — adopting the helpers
(and therefore the observation hook) lands in the Eloquent module.

## Connection metadata

```rust
let name = DB::database_name()?;        // "myapp" for postgres://.../myapp
let driver = DB::driver_name()?;        // "postgres" | "mysql" | "sqlite"
let title = DB::driver_title()?;        // "Postgres" | "MySQL" | "SQLite"
let version = DB::server_version().await?;  // "15.5" | "8.0.36" | "3.42.0"
```

`server_version` issues a backend-specific introspection query
(`SELECT VERSION()` for Postgres + MySQL, `SELECT sqlite_version()`
for SQLite). Cache the result if you call it often — every call is a
round trip.

## Named connections

For read replicas, sharded shards, or per-model warehouse pools:

```rust
// In bootstrap.rs
DB::register_named("__read_replica__", read_config).await?;
DB::register_named("warehouse", warehouse_config).await?;

// Per-query routing:
let rows = User::query().on("__read_replica__").get().await?;
let warehouse_rows = DB::table("audit_log").on("warehouse").get().await?;
let raw = DB::select_on("warehouse", "SELECT ...", vec![]).await?;
```

The `__read_replica__` name is well-known: when registered, every
read-shape terminal method auto-routes through it. Writes ignore the
replica and target the primary. Use `Builder::on_write_connection`
(per query) or `#[model(connection = "...")]` (per model default) to
opt back to the primary for specific operations.

Reserved names:

- `__primary__` — the default pool. Cannot be registered (it's the
  return value of `DB::connection()`).
- `__read_replica__` — well-known read replica. ANY connection
  registered under this name takes over read routing.

See [eloquent.md → Multi-connection routing](eloquent.md.md) for the
full precedence chain (builder tx override → ambient tx → builder
`on(name)` → model default → `__read_replica__` → primary).

## Testing

`TestDatabase` builds an in-memory SQLite database, registers it in
the test container so `DB::connection()` resolves to it, and runs your
migrations:

```rust
use suprnova::testing::TestDatabase;
use crate::migrations::Migrator;

#[tokio::test]
async fn test_user_creation() {
    let db = TestDatabase::fresh::<Migrator>().await.unwrap();
    // Any code calling DB::connection() now gets this in-memory DB.
    let _ = CreateUser::run("alice@example.com").await.unwrap();
}

// `test_database!()` is the macro shortcut.
let db = test_database!();
```

For tests that build their own ad-hoc schema:

```rust
let db = TestDatabase::sqlite_memory().await.unwrap();
db.execute_unprepared("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)").await.unwrap();
```

When a `TestDatabase` is dropped, the test container is cleared and
the connection registry is wiped — no cross-test leakage. Tests that
mutate process-wide state (the registry, the listener registry, the
query log) should be annotated `#[serial_test::serial]` so they don't
collide.

## Surface index

| Surface | Laravel analogue |
| --- | --- |
| `DB::init` / `DB::init_with` / `DB::connection` / `DB::is_connected` / `DB::get` | `DB::connection()` |
| `DB::table(name)` → `DbTableBuilder` | `DB::table($name)` |
| `DB::select` / `select_one` / `scalar` / `insert` / `update` / `delete` / `statement` / `affecting_statement` / `unprepared` | `DB::select` / `selectOne` / `scalar` / `insert` / `update` / `delete` / `statement` / `affectingStatement` / `unprepared` |
| `DB::transaction` / `transaction_with_attempts` / `begin_transaction` | `DB::transaction($cb, $attempts)` / `DB::beginTransaction` |
| `Transaction::commit` / `rollback` / `savepoint` / `rollback_to` | `DB::commit` / `rollBack` / savepoint helpers |
| `DB::listen(callback)` | `DB::listen` |
| `DB::enable_query_log` / `disable_query_log` / `get_query_log` / `flush_query_log` / `logging` | `DB::enableQueryLog` / `disableQueryLog` / `getQueryLog` / `flushQueryLog` / `logging` |
| `DB::database_name` / `driver_name` / `driver_title` / `server_version` | `getDatabaseName` / `getDriverName` / `getDriverTitle` / `getServerVersion` |
| `DB::register_named` / `named` / `select_on` / `table_on` / `statement_on` / `affecting_statement_on` | multi-connection `DB::connection($name)` |
| `QueryExecuted` / `TransactionBeginning` / `TransactionCommitted` / `TransactionRolledBack` / `ConnectionEstablished` / `DatabaseBusy` | `Illuminate\Database\Events\*` |
| `DatabaseConfig::builder()` / `from_env` / `validate_for_environment` | `config/database.php` |
| `TestDatabase::fresh::<M>` / `sqlite_memory` / `execute_unprepared` / `fetch_one` / `fetch_all` | `RefreshDatabase` testing trait |
