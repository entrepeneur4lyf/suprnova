# Query Builder

When you want to query a table without modelling it as a typed
`#[suprnova::model]` struct, reach for `DB::table(name)`. It returns a
chainable builder shaped like the typed Eloquent `Builder<M>`, but
materialises rows as `DynamicRow` — a `serde_json::Map` newtype with
typed accessors. This is the chapter for audit logs, ad-hoc reports,
dashboard aggregates, and any table you haven't bothered to model. For
the typed equivalent, see [Eloquent](eloquent.md). For raw `DB::select`
inside transactions or with `DB::listen` observation, see
[Database](database.md).

```rust
use suprnova::DB;

let rows = DB::table("audit_log")
    .select(["id", "event", "actor_id"])
    .filter("actor_id", 42i64)
    .filter_op("created_at", ">=", "2026-01-01")
    .order_by_desc("id")
    .limit(50)
    .get()
    .await?;

for row in rows.iter() {
    let id: i64 = row.get_int("id")?;
    let event: String = row.get_string("event")?;
    println!("{id}: {event}");
}
```

## When to use which surface

Three query surfaces overlap; pick the right one for the table.

| Table is… | Use | Returns |
|---|---|---|
| Modeled with `#[suprnova::model]` | `Model::query()` → `Builder<M>` | typed `M` values |
| Unmodeled but you want a chainable WHERE/ORDER/LIMIT shape | `DB::table(name)` → `DbTableBuilder` | `DynamicRow` |
| Anything the builders can't express — CTEs, window functions, backend DDL | `DB::select` / `DB::statement` / `DB::affecting_statement` | `DynamicRow` / `bool` / `u64` |

`DbTableBuilder` exists for the middle case. You get the WHERE / ORDER /
LIMIT chain without committing to a `#[suprnova::model]` struct and
without dropping all the way to raw SQL strings.

## The chainable surface

`DB::table(name)` returns a `DbTableBuilder`. Build it up, then call a
terminal method to execute.

### Filtering

```rust
// Equality.
DB::table("users").filter("email", "alice@example.com").get().await?;

// Arbitrary operator. Allowlist: =, <>, <, <=, >, >=, LIKE, NOT LIKE,
// ILIKE, NOT ILIKE, IS, IS NOT.
DB::table("orders").filter_op("total", ">=", 100i64).get().await?;
DB::table("posts").filter_op("title", "LIKE", "%rust%").get().await?;

// Multiple filters AND together.
DB::table("audit_log")
    .filter("actor_id", 42i64)
    .filter_op("event", "<>", "noop")
    .get()
    .await?;
```

`filter` and `filter_op` both accept any `Into<SeaValue>` for the
right-hand side, which covers `i64`, `String`, `&str`, `bool`, `f64`,
`Option<T>`, `chrono::*`, `uuid::Uuid`, and `serde_json::Value` — every
column type the backend understands.

### Selecting columns

```rust
// Default is SELECT *.
DB::table("users").get().await?;

// Restrict columns when you only need some.
DB::table("users").select(["id", "email"]).get().await?;
```

### Ordering and windowing

```rust
DB::table("posts")
    .order_by_desc("created_at")
    .order_by_asc("title")
    .limit(20)
    .offset(40)
    .get()
    .await?;
```

`order_by_desc` and `order_by_asc` chain in insertion order; the
generated SQL preserves it.

### Terminals

```rust
// All matching rows.
let rows: Collection<DynamicRow> = DB::table("audit_log")
    .filter("actor_id", 42i64)
    .get()
    .await?;

// First row or None.
let first: Option<DynamicRow> = DB::table("audit_log")
    .filter("event", "user.deleted")
    .first()
    .await?;

// Just the count (clears any select/order/limit/offset before
// rendering — count semantics don't care about those).
let n: u64 = DB::table("audit_log")
    .filter("actor_id", 42i64)
    .count()
    .await?;
```

`get()` returns `Collection<DynamicRow>` — the same collection wrapper
typed models use, with the same `.iter()`, `.len()`, `.into_vec()`
surface. See [Eloquent Collections](eloquent-collections.md).

### Inserts, updates, deletes

```rust
use suprnova::attrs;

// INSERT, returns the new row's auto-increment id.
let id: i64 = DB::table("audit_log")
    .insert(attrs! { event: "user.created", actor_id: 42 })
    .await?;

// UPDATE, returns rows affected.
let updated: u64 = DB::table("audit_log")
    .filter("id", id)
    .update(attrs! { event: "user.created.v2" })
    .await?;

// DELETE, returns rows affected.
let deleted: u64 = DB::table("audit_log")
    .filter("actor_id", 42i64)
    .delete()
    .await?;
```

The `attrs!` macro builds the column-to-value map at the call site.
Keys are SQL identifiers (validated) and values are bound as
parameters.

#### `update_all` and `delete_all` aliases

`update` and `delete` are the Laravel-faithful names. The
`Builder<M>`-style aliases — `update_all` and `delete_all` — call the
same implementation. Prefer the `_all` form when the table-wide intent
is the point of the call site; it makes a missing `filter` visible to
reviewers:

```rust
// Same behaviour as DB::table("rate_limits").delete().await? but the
// _all suffix tells reviewers "yes, I meant to truncate the table".
DB::table("rate_limits").delete_all().await?;

// Mass update with a WHERE — the _all suffix here matches the typed
// Builder<M> convention for the same operation.
DB::table("sessions")
    .filter_op("expires_at", "<", chrono::Utc::now())
    .update_all(attrs! { status: "expired" })
    .await?;
```

#### Empty WHERE on update or delete operates on every row

`DB::table("x").delete().await?` removes every row in the table. That
is supported by design — sometimes you really do want to truncate —
but it's rarely correct. Always look at a `delete()` / `delete_all()`
call and check whether there's a `filter` in front of it. The same is
true of `update` / `update_all`.

#### Insert backend split

`RETURNING id` is used on Postgres and SQLite. MySQL doesn't support
`RETURNING`, so the builder runs the INSERT and reads the driver's
per-connection `last_insert_id()` from the result. The model-less
builder assumes a standard `id` auto-increment primary key. UUID,
composite, renamed, or non-integer primary keys aren't supported on
this surface — use the typed [Eloquent](eloquent.md) `Model` interface
instead, which consults the model definition for primary-key shape.

## `DynamicRow` — typed accessors over a JSON map

Every row returned by `DB::table` or `DB::select` materialises as
`DynamicRow`, a `serde_json::Map<String, Value>` newtype with typed
accessors. Each getter returns `Result<T, FrameworkError>` with a
clear error message on missing key or type mismatch.

```rust
for row in rows.iter() {
    let id: i64                 = row.get_int("id")?;
    let event: String           = row.get_string("event")?;
    let active: bool            = row.get_bool("active")?;
    let weight: f64             = row.get_float("weight")?;
    let payload: serde_json::Value = row.get_value("payload")?;
}
```

For nullable columns, use `get_optional_*`. These distinguish "column
missing" (error — schema mismatch) from "column present, value SQL
NULL" (`Ok(None)`):

```rust
let title: Option<String> = row.get_optional_string("title")?;
let score: Option<i64>    = row.get_optional_int("score")?;
```

Today the optional family covers `String` and `i64`. For other
nullable types, use `get_value` and match on `serde_json::Value::Null`
yourself, or read the column through `get_as::<Option<T>>` (any
`T: DeserializeOwned`).

To deserialise a column into any struct or container type, use
`get_as`. The full `serde_json` deserialisation surface is available:

```rust
#[derive(serde::Deserialize)]
struct UserPrefs {
    theme: String,
    notifications: bool,
}

let prefs: UserPrefs    = row.get_as("prefs")?;
let tags: Vec<String>   = row.get_as("tags")?;
let when: chrono::DateTime<chrono::Utc> = row.get_as("created_at")?;
```

`DynamicRow` derefs to `Map<String, Value>`, so iteration and
key-existence checks work directly:

```rust
for (key, value) in row.iter() {
    println!("{key} = {value}");
}

if row.contains_key("deleted_at") { /* … */ }
```

## Identifier trust boundary

Table names, column names, ORDER BY directions, and SQL operators are
interpolated into the SQL string verbatim — they are NOT bound as
parameters (SQL doesn't allow placeholder-bound identifiers). Treat
every `impl Into<String>` argument as a trusted, compile-time literal.

```rust
// Safe — the column name is a constant; the value is bound.
DB::table("users").filter("email", request.email()).get().await?;

// UNSAFE — never splice user input into a column name.
DB::table("users")
    .filter(request.user_supplied_column(), value)
    .get()
    .await?;
```

The framework enforces a strict allowlist at the I/O boundary —
identifiers must match `[A-Za-z_][A-Za-z0-9_]*` with one optional
`schema.` prefix, and operators must come from a fixed list. Violations
fail closed with a `FrameworkError::Database` before any SQL is
rendered. That's a safety net, not a license: keep identifiers literal
in your code.

Values on the right-hand side of `filter` / `filter_op` are always
bound as parameters and safe to splice through from request data.

## Raw queries

When the builder can't express what you need — recursive CTEs, window
functions, backend-specific DDL, `INSERT … ON CONFLICT DO UPDATE` —
drop to a raw string. Placeholders match the active backend (`$1, $2,
…` for Postgres, `?` for MySQL and SQLite); the framework auto-detects
from `DatabaseConfig::url`.

```rust
use suprnova::DB;
use sea_orm::Value;

// SELECT — every row as DynamicRow.
let rows = DB::select(
    "SELECT u.name, COUNT(p.id) AS post_count
     FROM users u LEFT JOIN posts p ON p.user_id = u.id
     GROUP BY u.id
     HAVING COUNT(p.id) > ?",
    vec![Value::from(5i64)],
).await?;

// SELECT — first row only, mirrors Laravel's DB::selectOne.
let alice = DB::select_one(
    "SELECT * FROM users WHERE email = ?",
    vec![Value::from("alice@example.com")],
).await?;

// SELECT — first column of first row as a typed scalar.
let total: i64 = DB::scalar(
    "SELECT COUNT(*) FROM users WHERE active = ?",
    vec![Value::from(true)],
).await?;

// INSERT — true when at least one row was affected.
DB::insert(
    "INSERT INTO users (name, active) VALUES (?, ?)",
    vec![Value::from("bob"), Value::from(true)],
).await?;

// UPDATE / DELETE — return the rows-affected count.
let updated: u64 = DB::update(
    "UPDATE users SET active = ? WHERE id = ?",
    vec![Value::from(false), Value::from(1i64)],
).await?;

let deleted: u64 = DB::delete(
    "DELETE FROM users WHERE active = ?",
    vec![Value::from(false)],
).await?;

// Any prepared statement with bindings.
DB::statement(
    "UPDATE users SET votes = votes + ? WHERE id = ?",
    vec![Value::from(1i64), Value::from(42i64)],
).await?;

// DDL or other no-binding statements that reject placeholder binding.
DB::unprepared("CREATE INDEX idx_users_name ON users(name)").await?;

// Generic "rows affected" path — for upserts and operations that
// don't fit the named helpers.
let n: u64 = DB::affecting_statement(
    "INSERT INTO counters (k, n) VALUES ($1, 1)
     ON CONFLICT (k) DO UPDATE SET n = counters.n + 1",
    vec![Value::from("page_views")],
).await?;
```

### Aggregate-column gotcha

Untyped aggregates like `SELECT COUNT(*) AS n FROM t` work through the
builder's `.count()` helper but may come back silently dropped from
raw `DB::select` rows on SQLite. The underlying row materialiser walks
sqlx's per-column type info, and a bare aggregate carries none. If you
need raw `DB::select` with aggregates on SQLite, either wrap the
expression in `CAST(… AS BIGINT)` to give it a type tag, or use
`DB::scalar::<i64>` which goes through `query_one` + `try_get` and
doesn't depend on the per-column type detection.

## Bridge to typed Eloquent

When the table is worth a `#[suprnova::model]` struct, the chainable
shape carries over. `Model::query()` returns `Builder<M>`, which
ships the same `filter` / `filter_op` / `order_by_*` / `limit` /
`offset` / `get` / `first` / `count` surface — plus a much wider WHERE
vocabulary (`filter_in`, `filter_between`, `filter_null`, `filter_has`,
`filter_raw`, …) and Laravel-shape aliases (`db_where`, `where_in`,
`where_between`, `where_null`, `where_has`, `where_raw`, …).

```rust
use suprnova::Model;

let admins = User::query()
    .filter("role", "admin")
    .filter_op("created_at", ">=", since)
    .order_by_desc("created_at")
    .limit(20)
    .get()
    .await?;     // Collection<User> — typed, not DynamicRow

let alice = User::query().filter("email", &email).first().await?;
let total = User::query().filter("active", true).count().await?;
// Note: Builder<M>::count returns i64 (matches Laravel's Eloquent),
// whereas DbTableBuilder::count returns u64. Both surfaces give you a
// non-negative SQL COUNT — they only differ in their wire type.
```

The full `Builder<M>` surface — every WHERE shape, aggregates,
relations, eager loading, scopes, paginators, chunk iteration — is in
[Eloquent](eloquent.md). The chainable shape you learned above is the
same shape; the differences are typing and reach.

## Routing to a named connection

`DB::table` and the raw helpers default to the primary connection. To
target a read replica, shard, or warehouse pool, pin the call:

```rust
// Builder pinned to a named connection.
let rows = DB::table("audit_log").on("warehouse").get().await?;

// Equivalent shorthand.
let rows = DB::table_on("warehouse", "audit_log").get().await?;

// Raw escapes have _on variants too.
let rows = DB::select_on("warehouse", "SELECT …", vec![]).await?;
let n    = DB::affecting_statement_on(
    "warehouse",
    "UPDATE …",
    vec![],
).await?;
```

When `__read_replica__` is registered, every read-shape terminal
auto-routes through it; writes (`insert` / `update` / `delete` /
`update_all` / `delete_all`) always target the primary. Inside a
`DB::transaction` closure the active transaction's connection wins
absolutely — `on(name)` is silently ignored to preserve atomicity. See
[Database — Named connections](database.md) for the full precedence
chain.

### Why Suprnova diverges

Laravel's `DB::table(...)` is its model-less query builder; under the
hood it returns a `stdClass` per row (a PHP object whose properties
are the columns). Suprnova returns `DynamicRow` instead — a
`serde_json::Map` newtype with typed accessors. The accessor shape
catches missing-column and wrong-type errors at the boundary instead
of panicking deep in user code with a property-access exception.

The dual `update`/`update_all` and `delete`/`delete_all` names exist
because the typed Eloquent `Builder<M>` surface uses the `_all` suffix
to make table-wide intent explicit at the call site. Rather than pick
a side, the model-less builder ships both — `update` and `delete`
match Laravel's `DB::table($t)->update(...)` and `->delete()` letter
for letter; `update_all` and `delete_all` match the convention `M`
users will already have in their muscle memory.

## Next

- [Database](database.md) — `DB` facade, transactions with savepoints,
  `DB::listen` observability, named connections
- [Eloquent](eloquent.md) — typed `#[suprnova::model]` structs and the
  full `Builder<M>` surface
- [Pagination](pagination.md) — `paginate` / `simple_paginate` /
  `cursor_paginate` on typed builders
- [Eloquent Collections](eloquent-collections.md) — the `Collection<T>`
  returned by `get()` on both surfaces
- [Migrations](migrations.md) — defining the schema the builders query
