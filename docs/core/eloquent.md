# Eloquent API

Suprnova's Eloquent layer gives Laravel developers the API they know,
implemented as a thin shim over SeaORM. Copy code from the Laravel
docs, swap PHP syntax for Rust, add `.await?`, and it runs.

The whole layer is a struct attribute (`#[suprnova::model]`), a trait
(`Model`), and a chainable query builder (`Builder<M>`) — that's it.
Behind the scenes the macro generates a SeaORM `Entity`, `Model`,
`ActiveModel`, and `Column` enum, plus every Eloquent trait impl. The
SeaORM types stay reachable for the rare case the Eloquent surface
doesn't cover (see the [SeaORM escape hatches](#dropping-to-seaorm)).

## Table of contents

- [Quick start](#quick-start)
- [The `#[suprnova::model]` attribute](#the-suprnovamodel-attribute)
- [Model module layout](#model-module-layout)
- [Finding rows](#finding-rows)
- [Creating and updating](#creating-and-updating)
- [Deleting and soft deletes](#deleting-and-soft-deletes)
- [Query builder — dual API](#query-builder--dual-api)
- [Row locking](#row-locking)
- [Scopes](#scopes)
- [Relationships](#relationships)
- [Eager loading](#eager-loading)
- [Pagination](#pagination)
- [Chunking and lazy iteration](#chunking-and-lazy-iteration)
- [Mass assignment](#mass-assignment)
- [Casts](#casts)
- [Accessors and mutators](#accessors-and-mutators)
- [Timestamps](#timestamps)
- [Observers and lifecycle events](#observers-and-lifecycle-events)
- [Prunable](#prunable)
- [Testing models](#testing-models)
- [Dropping to SeaORM](#dropping-to-seaorm)
- [Migrating from `database::Model`](#migrating-from-databasemodel)

## Quick start

One attribute on a struct turns it into a fully-featured Eloquent
model:

```rust
use chrono::{DateTime, Utc};
use suprnova::{model, Model};

#[model(table = "users")]
pub struct User {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

Once declared, you can write:

- `User::query()` — start a fluent query builder.
- `User::find(id).await?` — fetch by primary key.
- `User::find_or_fail(id).await?` — same, but errors with `ModelNotFound` on miss.
- `User::all().await?` — every row.
- `User::create(attrs!{ name: "Alice", email: "alice@example.com" }).await?` —
  insert with mass-assignment filtering.
- `User::filter("email", "alice@example.com").first().await?` —
  one row that matches.
- `user.update(attrs!{ name: "Alice B" }).await?` — partial update.
- `user.save().await?` — persist in-memory changes.
- `user.delete().await?` — remove the row.
- `user.refresh().await?` / `user.fresh().await?` / `user.replicate()` —
  the rest of the Laravel lifecycle.

The user-facing struct (here `User`) IS the type your handlers and
controllers carry. The macro emits a per-model inner module (`user::`)
with the SeaORM `Entity`, `Column`, `ActiveModel`, and `Model` types
for the cases where you want to drop down to SeaORM directly. The
struct is also registered in an inventory-backed `ModelEntry` so
admin and tooling code can enumerate every model at boot.

## The `#[suprnova::model]` attribute

The single entry point for declaring a model. Every attribute is
optional; the defaults are tuned so a struct with `id` +
`created_at` + `updated_at` works as a Suprnova model with zero
configuration.

### Macro attribute reference

| Attribute | Type | Default | Notes |
|-----------|------|---------|-------|
| `table` | string | snake_case-plural of struct name | Override the table name |
| `primary_key` | string | `"id"` | Override the PK column name |
| `key_type` | type | `i64` | PK type — `String` for UUID, `i32` for legacy schemas |
| `auto_increment` | bool | `true` | Disable for UUID PKs |
| `connection` | string | `"default"` | Multi-connection apps name a non-default connection |
| `fillable` | list of strings | (default = `guarded = ["id"]`) | Mass-assignment allowlist |
| `guarded` | list of strings | `["id"]` when neither set | Mass-assignment denylist (mutually exclusive with `fillable`) |
| `casts` | map of `field = CastType` | `{}` | Per-column casts |
| `hidden` | list of strings | `[]` | Excluded from `to_json` / `to_array` |
| `visible` | list of strings | (all) | Inclusive variant of `hidden` (mutually exclusive) |
| `appends` | list of strings | `[]` | Accessors to include in serialization |
| `soft_deletes` | flag | `false` | Enable `deleted_at` column + tombstone semantics |
| `soft_deletes_column` | string | `"deleted_at"` | Override the soft-delete column name |
| `timestamps` | flag / bool | `true` when both `created_at` and `updated_at` exist | Disable auto-managed timestamps |
| `created_at` | string | `"created_at"` | Override the column name |
| `updated_at` | string | `"updated_at"` | Override the column name |
| `touches` | list of relation names | `[]` | Bump parent `updated_at` when this model saves (activates in Phase 10B) |
| `mutators` | list of strings | `[]` | Field names whose JSON-fill path routes through a `set_<field>(value)` mutator method |

### Full example

```rust
use chrono::{DateTime, Utc};
use serde_json::Value as Json;
use suprnova::{model, AsBool, AsEncrypted, AsJson};

#[model(
    table = "users",
    fillable = ["name", "email", "preferences"],
    casts = {
        active = AsBool,
        preferences = AsJson<Json>,
        api_token = AsEncrypted,
    },
    hidden = ["password", "remember_token", "api_token"],
    appends = ["full_name"],
    soft_deletes,
    timestamps,
)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub password: String,
    pub remember_token: Option<String>,
    pub api_token: Option<String>,
    pub active: bool,
    pub preferences: Json,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}
```

### Function-level macros

Function-level macros work alongside the struct attribute:

- `#[accessor]` on a `fn name(&self) -> T` makes it an Eloquent
  accessor. The model's `to_array()` calls it when `name` is listed
  in `appends = [...]` (and `to_json()` picks it up via the
  `to_array` → string delegation).
- `#[mutator]` on a `fn set_name(&mut self, value: serde_json::Value)`
  makes it an Eloquent mutator. The model's JSON-fill path routes
  through it when `name` is listed in `mutators = [...]`.
- `#[scope]` (Phase 10C) on a `fn(query: Builder<Self>) -> Builder<Self>`
  registers a local scope.
- `#[global_scope]` (Phase 10C) registers a global scope.
- `#[prunable]` on `impl Prunable for T { ... }` registers the
  pruner via inventory so `model:prune` finds it.

## Model module layout

`#[suprnova::model]` keeps your user-facing struct (e.g. `Post`) at
parent scope and emits a sibling `pub mod` named after the struct in
snake_case (`post`). That inner module is where the SeaORM types live.

For a model declared at `app/src/models/posts.rs`:

```rust
use chrono::{DateTime, Utc};
use suprnova::model;

#[model(table = "posts", fillable = ["title", "body"], timestamps)]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// Convention: re-export the SeaORM types the macro emits inside the
// inner module so call sites can use the unprefixed names. Suprnova's
// own dogfood models all carry this line (see `app/src/models/users.rs`,
// `app/src/models/posts.rs`, etc.).
pub use post::{ActiveModel, Column, Entity};
```

You now have these items reachable from `crate::models::posts`:

| Path | What it is |
|------|-----------|
| `crate::models::posts::Post` | Your user-facing struct — the Eloquent model |
| `crate::models::posts::post::Entity` | SeaORM `EntityTrait` impl for the `posts` table |
| `crate::models::posts::post::Column` | SeaORM `Column` enum (one variant per column) |
| `crate::models::posts::post::ActiveModel` | SeaORM `ActiveModel` for insert/update |
| `crate::models::posts::post::Model` | SeaORM-shape row (storage-typed columns) |
| `crate::models::posts::{Entity, Column, ActiveModel}` | The `pub use` convention above; not auto-emitted |

Two things to know about the inner module's `Model`:

1. It is the **SeaORM-shape** row, not your `Post` struct. Cast columns
   carry their `Storage` type here (e.g. `bool` becomes the underlying
   integer), and the `__eager` / `__pivot` runtime slots from your
   struct are absent.
2. `From<post::Model> for Post` and `From<Post> for post::Model` bridge
   the two shapes. See [Dropping to SeaORM](#dropping-to-seaorm) for the
   round-trip pattern.

`Model` is intentionally **not** part of the conventional parent
re-export — the user-facing `Post` already occupies the `Post` name at
parent scope, and `post::Model` is a separate type that callers reach
through `post::Model` (or `From` conversion) when they need the inner
shape.

### When to reach into the inner module

The Eloquent surface (`Model` trait + `Builder<M>`) covers the vast
majority of queries. Reach into `post::*` when you need SeaORM-only
features:

- **Raw query construction** with SeaORM's `EntityTrait::find()` chain
  when Eloquent doesn't expose the helper you want.
- **Custom join logic** — building `JoinType::*` joins explicitly via
  `QuerySelect::join()` for a relation Eloquent's `with(...)` doesn't
  model.
- **SeaORM-native subqueries** through `Entity::find().select_only()`.
- **Plain `ActiveModel` mutation** for the rare case you want to bypass
  the Eloquent lifecycle (no observers, no auto-timestamps).

```rust
// Common case — Column re-exported at parent module level via the
// `pub use post::{...}` convention above.
use crate::models::posts::Column;

let drafts = Post::query()
    .db_where(Column::Status, "draft")
    .get()
    .await?;

// Power-user case — reach into the inner module for the SeaORM Entity
// directly. This is what the parent `pub use` does not surface.
use crate::models::posts::post;
use suprnova::sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

let db = suprnova::DB::connection()?;
let rows: Vec<post::Model> = post::Entity::find()
    .filter(post::Column::Status.eq("published"))
    .all(db.inner())
    .await?;

// Bridge back to the Eloquent shape when the caller wants it.
let posts: Vec<Post> = rows.into_iter().map(Post::from).collect();
```

If you find yourself reaching into the inner module routinely for the
same operation, that's a signal Eloquent is missing a helper — open an
issue, or add the helper to the `Model` / `Builder` surface.

## Finding rows

```php
// Laravel
$user = User::find(1);
$user = User::findOrFail(1);          // throws on missing
$users = User::findMany([1, 2, 3]);
```

```rust
// Suprnova
let user: Option<User> = User::find(1).await?;
let user: User = User::find_or_fail(1).await?;
let users: Vec<User> = User::find_many([1, 2, 3]).await?;
```

`find_or_fail` returns `FrameworkError::ModelNotFound` (HTTP 404 when
bubbled to a controller).

### `first_or_create` / `update_or_create` / `first_or_new` / `first_or`

```php
// Laravel
$user = User::firstOrCreate(
    ['email' => 'alice@example.com'],
    ['name' => 'Alice'],
);
$user = User::updateOrCreate(
    ['email' => 'alice@example.com'],
    ['name' => 'Alice Updated'],
);
$user = User::firstOrNew(['email' => 'alice@example.com']);  // unsaved
```

```rust
// Suprnova
let user = User::first_or_create(
    attrs! { email: "alice@example.com" },          // lookup keys
    attrs! { name: "Alice" },                       // extras on create
).await?;

let user = User::update_or_create(
    attrs! { email: "alice@example.com" },
    attrs! { name: "Alice Updated" },
).await?;

let user = User::first_or_new(
    attrs! { email: "alice@example.com" },
).await?;   // returns an unsaved User; caller saves explicitly
```

Lookup keys go in the first map; extra fields applied on the
create-path go in the second map. Returning an unsaved model via
`first_or_new` lets the caller mutate it further before
`save().await?`.

## Creating and updating

### Create

```php
// Laravel
$user = User::create([
    'name' => 'Alice',
    'email' => 'alice@example.com',
]);
```

```rust
// Suprnova
let user = User::create(attrs! {
    name: "Alice",
    email: "alice@example.com",
}).await?;
```

`attrs!` is a macro that produces an `Attrs` value (a typed JSON
map). Pure JSON also works —
`User::create(serde_json::json!({"name": "Alice", "email": "..."}))`.
The `Fillable` filter runs inside `create`; non-fillable fields are
silently dropped, matching Laravel's behaviour.

### Save / update

```php
// Laravel
$user->name = 'Alice B';
$user->save();

$user->update(['name' => 'Alice B']);
```

```rust
// Suprnova
user.name = "Alice B".into();
user.save().await?;

user.update(attrs! { name: "Alice B" }).await?;
```

`save()` walks every non-PK field, sets them on the ActiveModel via
`Set(...)`, calls SeaORM's `update()`, and returns the canonical row.
`update(attrs)` is the same flow but applies a partial attribute
map first (running the Fillable filter and any declared mutators).

### Increment / decrement

```php
// Laravel
$user->increment('login_count');
$user->increment('login_count', 5);
$user->decrement('credits', 10);
User::where('plan', 'free')->increment('quota_reset_count');
```

```rust
// Suprnova
user.increment("login_count", 1).await?;
user.increment("login_count", 5).await?;
user.decrement("credits", 10).await?;
User::filter("plan", "free").increment("quota_reset_count", 1).await?;
```

`increment` / `decrement` emit `UPDATE table SET col = col + N WHERE
...` SQL — atomic against concurrent updates, no read-modify-write
race. Available both on a fetched model instance (uses the row's PK
in the WHERE clause) and as a Builder terminal (uses the chain's
WHERE clauses).

### Fresh / refresh / replicate

```php
// Laravel
$user->refresh();                          // reload from DB
$copy = $user->fresh();                    // fetch + return copy
$replica = $user->replicate();             // unsaved clone with fresh PK
$replica = $user->replicate(['email']);    // skip a field
```

```rust
// Suprnova
user.refresh().await?;
let copy: User = user.fresh().await?;
let replica: User = user.replicate();
let replica: User = user.replicate_except(["email"]);
```

`refresh` mutates in place; `fresh` returns a separately-fetched
copy. `replicate` builds an in-memory clone with the PK reset
(`Default::default()` for the key type). Caller saves explicitly.

### Cross-type replication

```rust
let replica: UserDraft = user.replicate_into()?;  // cross-type clone
```

A Suprnova divergence — Laravel can't do this because PHP doesn't
have types. Useful when promoting a draft model into a final one
or vice-versa.

## Deleting and soft deletes

### Soft deletes flag

Add `soft_deletes` to the macro attribute and a
`deleted_at: Option<DateTime<Utc>>` column to the struct:

```rust
#[model(table = "users", soft_deletes, timestamps)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub deleted_at: Option<DateTime<Utc>>,
    // ...
}
```

### Lifecycle

```rust
user.delete().await?;             // UPDATE: sets deleted_at = NOW()
user.trashed();                   // -> true
let trashed = User::with_trashed().find(user.id).await?.unwrap();
trashed.restore().await?;         // UPDATE: sets deleted_at = NULL

let only_dead = User::only_trashed().get().await?;
let all_including_dead = User::with_trashed().get().await?;

user.force_delete().await?;       // actual DELETE
```

### Default scope

When `soft_deletes` is set, the macro overrides `Model::query()` so
default reads filter out trashed rows automatically. `with_trashed()`
and `only_trashed()` opt back in. Concretely: `User::find(id)`
skips trashed rows; `User::with_trashed().find(id)` finds them.

## Query builder — dual API

`Builder<M>` is the chainable query type returned by `User::query()`,
`User::filter(...)`, `User::db_where(...)`, and every other static
method that doesn't terminate the chain.

### Naming note: dual API

`where` is a Rust keyword, so the bare-equality where method can't
share Laravel's name. Rather than pick a winner, every where-shape
method ships under **both** a Rust-idiomatic name (`filter`,
`filter_in`, `filter_null`, …) and a Laravel-shape name (`db_where`,
`where_in`, `where_null`, …). They're aliases over one canonical
implementation — pick whichever your muscle memory matches.

```rust
// Rust dev:
User::query().filter("active", true).filter_in("role", ["admin"]).get().await?;

// Laravel dev:
User::db_where("active", true).where_in("role", ["admin"]).get().await?;

// Same query. Same result. Different muscle memory.
```

### Where shortcuts

```php
// Laravel
$users = User::where('email', $email)->get();
$users = User::where('age', '>=', 18)->get();
$users = User::where('email', 'like', '%@example.com')->get();
```

```rust
// Suprnova — pick either family; both compile, both documented.

// Rust-shape (filter family):
let users = User::query().filter("email", &email).get().await?;
let users = User::query().filter_op("age", ">=", 18).get().await?;
let users = User::query().filter_like("email", "%@example.com").get().await?;

// Laravel-shape (db_where / where_* family):
let users = User::db_where("email", &email).get().await?;
let users = User::query().db_where_op("age", ">=", 18).get().await?;
let users = User::query().where_like("email", "%@example.com").get().await?;
```

### Where variants

Every row has two equivalent Suprnova forms — Rust-shape (`filter*`)
and Laravel-shape (`db_where` / `where_*`). Both call the same
canonical implementation; both are tagged with `#[doc(alias = "...")]`
so rustdoc search finds either.

| Laravel | Suprnova (Rust-shape) | Suprnova (Laravel-shape) | Notes |
|---------|----------------------|--------------------------|-------|
| `->where(col, val)` | `.filter(col, val)` | `.db_where(col, val)` | Equality |
| `->where(col, op, val)` | `.filter_op(col, op, val)` | `.db_where_op(col, op, val)` | Arbitrary operator |
| `->orWhere(...)` | `.or_filter(...)` | `.or_where(...)` | |
| `->whereNot(col, val)` | `.filter_not(col, val)` | `.where_not(col, val)` | |
| `->whereIn(col, vals)` | `.filter_in(col, vals)` | `.where_in(col, vals)` | |
| `->whereNotIn(col, vals)` | `.filter_not_in(col, vals)` | `.where_not_in(col, vals)` | |
| `->whereBetween(col, [a, b])` | `.filter_between(col, a..=b)` | `.where_between(col, a..=b)` | Rust range |
| `->whereNotBetween(col, [a, b])` | `.filter_not_between(col, a..=b)` | `.where_not_between(col, a..=b)` | |
| `->whereNull(col)` | `.filter_null(col)` | `.where_null(col)` | |
| `->whereNotNull(col)` | `.filter_not_null(col)` | `.where_not_null(col)` | |
| `->whereDate(col, '2026-05-19')` | `.filter_date(col, NaiveDate)` | `.where_date(col, NaiveDate)` | |
| `->whereMonth(col, 5)` | `.filter_month(col, 5)` | `.where_month(col, 5)` | |
| `->whereDay(col, 19)` | `.filter_day(col, 19)` | `.where_day(col, 19)` | |
| `->whereYear(col, 2026)` | `.filter_year(col, 2026)` | `.where_year(col, 2026)` | |
| `->whereTime(col, '12:30')` | `.filter_time(col, NaiveTime)` | `.where_time(col, NaiveTime)` | |
| `->whereLike(col, pattern)` | `.filter_like(col, pattern)` | `.where_like(col, pattern)` | |
| `->whereNotLike(col, pattern)` | `.filter_not_like(col, pattern)` | `.where_not_like(col, pattern)` | |
| `->whereJsonContains(col, v)` | `.filter_json_contains(col, v)` | `.where_json_contains(col, v)` | Backend-dispatched |
| `->whereJsonLength(col, op, n)` | `.filter_json_length(col, op, n)` | `.where_json_length(col, op, n)` | |
| `->whereColumn(a, b)` | `.filter_column(a, b)` | `.where_column(a, b)` | Column-to-column compare |
| `->whereExists(closure)` | `.filter_exists(builder)` | `.where_exists(builder)` | Subquery |
| `->whereHas(rel, closure)` | `.filter_has(rel, fn)` | `.where_has(rel, fn)` | Relation predicate (10B) |
| `->whereDoesntHave(rel)` | `.filter_doesnt_have(rel)` | `.where_doesnt_have(rel)` | (10B) |
| `->whereRelation(rel, col, op, v)` | `.filter_relation(...)` | `.where_relation(...)` | (10B) |
| `->whereRaw(sql, bindings)` | `.filter_raw(sql, bindings)` | `.where_raw(sql, bindings)` | |

### Ordering

```php
$users = User::orderBy('name', 'asc')->get();
$users = User::orderByDesc('created_at')->get();
$users = User::latest()->get();        // shortcut: orderBy(created_at, desc)
$users = User::oldest()->get();        // shortcut: orderBy(created_at, asc)
$users = User::inRandomOrder()->get();
```

```rust
let users = User::query().order_by("name", Direction::Asc).get().await?;
let users = User::query().order_by_desc("created_at").get().await?;
let users = User::latest().get().await?;
let users = User::oldest().get().await?;
let users = User::query().in_random_order().get().await?;
```

`Direction::Asc` / `Direction::Desc` is the Suprnova enum
re-exported from SeaORM.

### Grouping + having

```php
$rows = User::groupBy('role')->having('count(*)', '>', 5)->get();
```

```rust
let rows = User::query()
    .group_by("role")
    .having_op("count(*)", ">", 5)
    .get()
    .await?;
```

### Limit / offset

```php
$users = User::limit(10)->offset(20)->get();
$users = User::take(10)->skip(20)->get();   // aliases
```

```rust
let users = User::query().limit(10).offset(20).get().await?;
let users = User::query().take(10).skip(20).get().await?;
```

### Select / add_select / select_raw

```rust
let users = User::query().select(["id", "name", "email"]).get().await?;
let users = User::query().select("name").add_select("email").get().await?;
let rows  = User::query().select_raw("count(*) as total, role")
    .group_by("role")
    .get_raw()
    .await?;
```

`get_raw()` returns the raw column-shape result for `select_raw`
cases where the columns don't match the model schema; `get()`
returns `Vec<User>` and requires the selected columns to fill the
model struct.

### Distinct

```rust
let emails: Vec<String> = User::query().distinct().pluck("email").await?;
```

### Aggregates

```rust
let count   = User::count().await?;
let count   = User::filter("active", true).count().await?;
let sum     = User::sum::<f64>("balance").await?;
let avg     = Order::avg::<f64>("total").await?;
let min     = Order::min::<DateTime<Utc>>("created_at").await?;
let max     = Order::max::<DateTime<Utc>>("created_at").await?;
let exists  = User::filter("email", &email).exists().await?;
let missing = User::filter("email", &email).doesnt_exist().await?;
```

Aggregates are generic over the return type because SeaORM needs to
know what to coerce the DB scalar to. Type defaults:
`count -> i64`; `sum`/`avg` carry an explicit type parameter.

### Terminals

```rust
let users:  Vec<User>          = User::all().await?;
let first:  Option<User>       = User::first().await?;
let user:   User               = User::first_or_fail().await?;
let value:  Option<String>     = User::filter("...").value("email").await?;
let emails: Vec<String>        = User::pluck::<String>("email").await?;
let keyed:  HashMap<i64, String> = User::pluck_keyed::<i64, String>("id", "name").await?;
let sql:    String             = User::filter("...").to_sql();
```

`to_sql` returns the parameterised SQL the next terminal would emit
— useful for debugging or building views. The bindings are
accessible via `.to_sql_with_bindings() -> (String, Vec<Value>)`.

### Unions

```rust
let first  = User::filter("active", true);
let second = User::filter("role", "admin");
let users  = first.union(second).get().await?;
let users  = first.union_all(second).get().await?;
```

## Row locking

Two builder methods request a per-row database lock at SELECT time:

```rust
// Exclusive write lock — blocks other transactions trying to lock
// or write the same rows until this transaction commits.
let order = Order::query()
    .filter("id", 42)
    .lock_for_update()
    .first_or_fail()
    .await?;

// Shared read lock — allows other shared readers, blocks writers.
let inventory = Inventory::query()
    .filter("sku", sku)
    .shared_lock()
    .first_or_fail()
    .await?;
```

Per-backend SQL emitted:

| Backend  | `lock_for_update()` | `shared_lock()`        |
|----------|---------------------|------------------------|
| Postgres | `FOR UPDATE`        | `FOR SHARE`            |
| MySQL    | `FOR UPDATE`        | `LOCK IN SHARE MODE`   |
| SQLite   | (no SQL, see below) | (no SQL, see below)    |

The lock clause is appended at the very end of the compound
statement — after every `UNION` arm, every `ORDER BY`, every
`LIMIT` / `OFFSET`. A `union(...)` of two builders followed by
`.lock_for_update()` emits exactly **one** `FOR UPDATE` at the
outer scope, not one per arm.

### Use inside a transaction

The lock only does useful work **inside a transaction** — without
one, the SQL still emits but the lock releases at statement end.
Pair with `DB::transaction(...)`:

```rust
DB::transaction(|tx| async move {
    let order = Order::query()
        .filter("id", 42)
        .lock_for_update()
        .first_or_fail()
        .with_tx(&tx)
        .await?;
    // Other transactions trying to lock id=42 block here until commit.
    order.status = "processed".into();
    order.save_with_tx(&tx).await?;
    Ok(())
}).await?;
```

### `lock_for_update` vs `shared_lock`

Most "read then write" flows want `lock_for_update`. A shared
lock still lets another `shared_lock` reader race you to a
following `UPDATE` — only `FOR UPDATE` is mutually exclusive.

`shared_lock` is right for consistent snapshot reads where you
read a row, derive a decision from it, and don't write back —
e.g. an inventory check that doesn't itself decrement stock.

### SQLite

SQLite has no row-level locking. It uses file-level transaction
locking only (`BEGIN IMMEDIATE` / `BEGIN EXCLUSIVE`). The lock
methods are **kept** in the SQLite path so cross-backend code
compiles, but they emit no SQL.

The first time per process that `lock_for_update` / `shared_lock`
runs against a SQLite backend, the framework logs a single
`warn!` on the `suprnova::eloquent::lock` tracing target. This
surfaces the no-op without spamming high-volume code paths.

If you need cross-row contention guarantees on SQLite, wrap the
critical section in an explicit `BEGIN IMMEDIATE` transaction — at
the file level that blocks every other writer.

### What's not in v1

- **`NOWAIT` / `SKIP LOCKED`** — useful for job-queue claim
  workflows but they add API surface. Deferred until a real
  consumer needs them.

## Scopes

Phase 10C ships two flavours of scope, mirroring Laravel:

- **Local scopes** — extension methods on the builder, declared per
  model with `#[suprnova::scopes(Model)]`. Each free function in the
  annotated `impl` block becomes both `Model::name()` (a static
  starter) and `Builder::name()` (a chainable method).
- **Global scopes** — implementations of `GlobalScope<M>` registered
  at boot via `ScopeRegistry::register::<M, _>(scope)`. Every
  `Model::query()` call layers them on automatically.

### Local scopes

Declare local scopes by giving them the shape
`fn(query: Builder<Self>, args...) -> Builder<Self>`:

```rust
#[suprnova::scopes(User)]
impl User {
    pub fn active(query: Builder<Self>) -> Builder<Self> {
        query.filter("active", true)
    }

    pub fn popular(query: Builder<Self>, threshold: i64) -> Builder<Self> {
        query.filter_op("followers_count", ">", threshold)
    }
}

// Use as either a starter or a chainable method:
let active_users  = User::active().get().await?;
let popular_users = User::query().active().popular(500).get().await?;
```

Non-scope methods declared in the same `impl` block (anything whose
first parameter isn't `query: Builder<Self>`) pass through unchanged.

### Global scopes

Global scopes apply on every `Model::query()` call. The classic use
case is multi-tenancy — every read is scoped to the current tenant
without each caller threading the filter through.

```rust
use suprnova::eloquent::scopes::{GlobalScope, ScopeRegistry};

pub struct TenantScope;

impl GlobalScope<Article> for TenantScope {
    fn apply(&self, query: Builder<Article>) -> Builder<Article> {
        // Reads the current tenant from a task-local /
        // AtomicI64 / wherever per-request state lives.
        query.filter("tenant_id", current_tenant_id())
    }
}

// At boot — typically inside your provider/bootstrap module:
ScopeRegistry::register::<Article, _>(TenantScope);

// Every read is auto-scoped to the active tenant:
let scoped = Article::query().get().await?;
```

Multiple scopes per model compose in registration order — first
registered runs first, so its filter clauses appear first in the
WHERE chain. AND-combined filters don't care about order, but
left-to-right matters for any clause whose side-effect order is
visible (e.g. ordering, having, raw fragments).

### Opting out of a global scope

Each model the `#[suprnova::model]` macro touches gets two static
helpers emitted on it:

```rust
// Bypass exactly one registered scope by type. Other scopes still apply.
let all_tenants = Article::without_global_scope::<TenantScope>().get().await?;

// Bypass every registered scope. Admin tooling pattern.
let everything = Article::without_global_scopes().get().await?;
```

**Important:** the opt-out helpers must be the entry point. Chaining
`.without_global_scope::<S>()` onto a builder already returned by
`Model::query()` doesn't undo scopes that have already run —
`Model::query()` applies scopes eagerly at construction time, so the
mask is set too late. Use the per-model static helpers (above) for
correct semantics.

### Where global scopes apply

| Path | Global scopes apply? |
|------|----------------------|
| `Model::query()` | Yes — the canonical scoped entry point |
| `Model::without_global_scope::<S>()` | Yes, minus `S` |
| `Model::without_global_scopes()` | No |
| `Model::find(id)` | No — PK lookup goes through SeaORM directly |
| `Model::find_many([...])` | No — same reason |
| `Model::all()` | No — same reason |

This mirrors Laravel: `Eloquent\Model::find` doesn't trigger
`addGlobalScopes`. Callers that want scoped PK lookups use
`Self::query().filter("id", pk).first().await?`.

### Soft deletes and global scopes coexist

`#[suprnova::model(soft_deletes)]` installs the
`deleted_at IS NULL` filter via a separate string-tag mechanism, not
through the typed scope registry. Both layers compose:

- `Model::query()` filters out trashed rows AND runs every registered
  scope.
- `Model::without_global_scopes()` drops registered scopes but
  preserves the soft-delete filter — admin tooling that wants to read
  every column-set still excludes trashed rows by default.
- `Model::with_trashed()` and `Model::only_trashed()` skip soft-delete
  filtering and also bypass the registry (they build a fresh unscoped
  builder). Pair with `.without_global_scope::<S>()` if you need
  scope-aware reads over trashed rows.

## Relationships

Suprnova ships every Eloquent relation flavour. They're declared in
the `relations = { ... }` block on `#[suprnova::model]`, and the
macro emits — per declared relation — a method on the struct, a
loaded-accessor (`<name>_loaded()`), a count-accessor
(`<name>_count()`), and the dispatcher arm the eager loader calls
into. The relation kinds shipped today:

| Kind                | One/many | Across families | Backed by |
|---------------------|----------|-----------------|-----------|
| `HasOne<R>`         | one      | no              | `IN` query on `<parent>_id` |
| `BelongsTo<R>`      | one      | no              | `IN` query on FK on this row |
| `HasMany<R>`        | many     | no              | same as `HasOne`, returns `Vec<R>` |
| `BelongsToMany<R, P>` | many   | no              | pivot table `P`, INNER JOIN + `pivot::<P>()` |
| `HasOneThrough<B, R>`  | one   | no              | two-query JOIN `parent → B → R` |
| `HasManyThrough<B, R>` | many  | no              | same as above, returns `Vec<R>` |
| `MorphOne<R>`       | one      | yes             | `IN` + `<name>_type = "<self>"` filter |
| `MorphMany<R>`      | many     | yes             | same as `MorphOne`, returns `Vec<R>` |
| `MorphTo`           | one      | yes (children → many families) | per-family enum emitted at the declaration site |
| `MorphToMany<R, P>` | many     | yes             | polymorphic m2m pivot `P` |
| `MorphedByMany<R, P>` | many   | yes (inverse)   | same pivot, scanned the other way |

### `relations = { ... }` syntax

Every relation declaration carries the same outer shape: the relation
name, the kind, the related type (and pivot/intermediate types where
applicable), and a `{ ... }` block of options.

```rust
use suprnova::model;

#[model(
    table = "users",
    relations = {
        // HasMany<R>
        posts: HasMany<crate::models::Post> {
            fk = "author_id",         // override default `user_id`
        },
        // BelongsToMany<R, Pivot>
        roles: BelongsToMany<crate::models::Role, crate::models::RoleUser> {
            with_pivot = ["assigned_at"],
            with_timestamps,
        },
    },
)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
```

Common options:

| Option                     | Relation kinds                | Purpose |
|----------------------------|-------------------------------|---------|
| `fk = "..."`               | every kind with a child FK    | Column on the CHILD pointing at the parent. Default = `<snake(parent_struct)>_id`. |
| `lk = "..."`               | one/many kinds                | Column on the PARENT used as the join key. Default = `"id"`. |
| `related_key = "..."`      | `BelongsToMany`, `MorphToMany` | The related-side PK COLUMN name. Default = `"id"`. Required when the related model uses a non-`id` PK. |
| `with_pivot = ["...", ...]` | `BelongsToMany`, `MorphToMany` | Extra columns on the pivot to surface in the join. |
| `with_timestamps`          | `BelongsToMany`, `MorphToMany` | Stamp `created_at` / `updated_at` on attach/sync. |
| `with_default = \|\| { ... }` | `BelongsTo`                 | Closure producing a default when the FK is null OR the parent is missing. |
| `first_key`, `second_key`, `local_key`, `second_local_key` | `HasOneThrough`, `HasManyThrough` | JOIN key overrides — see the Through section below. |
| `name = "..."`             | every morph kind              | Morph family name (e.g. `"commentable"`, `"taggable"`). Drives the `<name>_id` / `<name>_type` columns on the child/pivot. |
| `targets = [T1, T2, ...]`  | `MorphTo`                     | The list of concrete morph targets. The macro emits a `<Name>Morph` enum at the declaration site with one variant per target plus `Unknown(String, i64)`. |
| `target_morph_type = "..."` | `MorphedByMany`              | The morph-type string identifying the target family on the pivot. |
| `pivot_table`, `pivot_foreign_key`, `pivot_related_key` | `BelongsToMany`, `MorphToMany` | Pivot-side column / table overrides when the defaults don't fit. |

### `HasOne<R>` and `BelongsTo<R>`

One-to-one in both directions. `HasOne` lives on the parent side and
calls `R::query().filter(<fk>, <self.id>).first()`. `BelongsTo` lives
on the child side and reads the FK off `self`, then calls
`R::query().filter(<owner_key>, <fk_value>).first()`.

```rust
#[model(table = "users", relations = {
    profile: HasOne<crate::models::Profile>,
})]
pub struct User { /* ... */ }

#[model(table = "profiles", relations = {
    user: BelongsTo<crate::models::User>,
})]
pub struct Profile {
    pub id: i64,
    pub user_id: i64,
    pub bio: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

let user = User::find(1).await?.unwrap();
let profile: Option<Profile> = user.profile().first().await?;

let profile = Profile::find(42).await?.unwrap();
let owner: Option<User> = profile.user().first().await?;
```

`BelongsTo` supports `with_default = || R { ... }`, which fires
either when the FK is null OR when the parent row is missing. The
default closure runs per call (and per eager-loaded row) — perfect
for an empty stand-in when a deleted user still has comments:

```rust
#[model(table = "comments", relations = {
    author: BelongsTo<crate::models::User> {
        with_default = || User {
            name: "[deleted]".into(),
            ..Default::default()
        },
    },
})]
pub struct Comment { /* ... */ }

let c = Comment::find(99).await?.unwrap();
// Always Some — the default fires when the user row is missing.
let author = c.author().first().await?.unwrap();
```

### `HasMany<R>`

One-to-many on the parent side. Returns a fluent builder; chain
filter / order / latest / take / get / count and terminate.

```rust
#[model(table = "users", relations = {
    posts: HasMany<crate::models::Post> {
        fk = "author_id",
    },
})]
pub struct User { /* ... */ }

let u = User::find(1).await?.unwrap();

// Every post by this user, default ordering:
let posts: Vec<Post> = u.posts().get().await?;

// Filtered + ordered + paged:
let recent = u.posts()
    .filter("published", true)
    .latest()                          // ORDER BY created_at DESC
    .take(10)
    .get()
    .await?;

// COUNT alone — no row fetching:
let total: i64 = u.posts().count().await?;
```

Available terminal methods: `.first()`, `.get()`, `.count()`. Available
chainable filters: `.filter` / `.db_where`, `.filter_in` / `.where_in`,
`.order_by`, `.latest`, `.oldest`, `.limit`, `.take`.

### `BelongsToMany<R, P>` — first-class Pivot

Many-to-many through a `#[suprnova::model]`-declared pivot. The pivot
is a first-class model with its own row identity — not a tuple, not a
hidden hash map. Two key benefits over Laravel's anonymous-pivot
shape:

1. The pivot row is type-safe. Read `with_pivot` columns via
   `r.pivot::<P>().<column>`, never via `r.pivot.get("...")`.
2. The pivot model is reachable from the rest of the framework
   (factories, scopes, casts, hooks) the same way every model is.

```rust
#[model(table = "role_user", fillable = ["user_id", "role_id", "assigned_at"])]
pub struct RoleUser {
    pub id: i64,
    pub user_id: i64,
    pub role_id: i64,
    pub assigned_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[model(table = "users", relations = {
    roles: BelongsToMany<crate::models::Role, RoleUser> {
        with_pivot = ["assigned_at"],
        with_timestamps,
    },
})]
pub struct User { /* ... */ }

let u = User::find(1).await?.unwrap();
let admin = Role::create(attrs! { name: "admin" }).await?;

// Attach + sync mutators
u.roles().attach(admin.id).await?;
u.roles().attach_with(admin.id, attrs! { assigned_at: chrono::Utc::now() }).await?;
u.roles().sync([role_a.id, role_b.id, role_c.id]).await?;
u.roles().detach(admin.id).await?;

// Read pivot data through the per-row downcast accessor:
let roles = u.roles().get().await?;
for r in &roles {
    let p: &RoleUser = r.pivot::<RoleUser>();
    println!("user {} got role {} at {:?}", p.user_id, p.role_id, p.assigned_at);
}
```

- `.attach(id)` — INSERT a single pivot row. Errors on duplicate
  unless your pivot allows it (the framework doesn't dedupe at the
  Rust layer; use `.sync` for idempotency).
- `.attach_with(id, attrs! { ... })` — INSERT with extra pivot
  columns. Stamps timestamps when `with_timestamps` is on.
- `.detach(id)` — DELETE the pivot row(s) linking parent → id.
- `.sync([ids...])` — diff-and-apply: attach what's new, detach what's
  missing, leave the intersection alone. Wrapped in a transaction.

`.get()` returns `Vec<R>` with the pivot stamped on each row's
internal `__pivot` field. The `.pivot::<P>()` accessor downcasts the
`Arc<dyn Any>` to the pivot type you declared. Calling it with the
wrong type panics — match the type to the declared pivot.

### `HasOneThrough<B, R>` and `HasManyThrough<B, R>`

Reach a final target `R` through an intermediate `B`. Useful when the
relation traverses two tables but you don't need to expose the
intermediate (`A → B → R`).

```rust
#[model(table = "countries", relations = {
    posts: HasManyThrough<crate::models::User, crate::models::Post>,
})]
pub struct Country {
    pub id: i64,
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

let c = Country::find(1).await?.unwrap();
let posts: Vec<Post> = c.posts().get().await?;
```

The dispatcher infers JOIN keys from struct names. Overrides:

| Option              | Default                          | Description |
|---------------------|----------------------------------|-------------|
| `first_key`         | `<snake(parent_struct)>_id`      | Column on intermediate `B` pointing at parent `A`. |
| `second_key`        | `<snake(intermediate_struct)>_id` | Column on final `R` pointing at intermediate `B`. |
| `local_key`         | `"id"`                           | Column on parent `A` matched by `first_key`. |
| `second_local_key`  | `"id"`                           | Column on intermediate `B` matched by `second_key`. Required when `B` uses a non-`id` PK. |

```rust
#[model(table = "countries", relations = {
    posts: HasManyThrough<crate::models::User, crate::models::Post> {
        first_key = "country_uuid",
        second_key = "author_id",
        local_key = "uuid",
    },
})]
pub struct Country { /* ... */ }
```

### `MorphTo` with `targets = [...]` and per-family enum

Polymorphic relations point a child row at one of several parent
families. The child carries a `(<name>_id, <name>_type)` pair; the
`*_type` column holds the morph-type string each parent declares.

`MorphTo` lives on the child. Its declaration lists every parent
family it can point at via `targets = [...]`. The macro emits a
per-family enum named `<RelationName>Morph` (matching the relation
name's PascalCase form, suffixed with `Morph`) with one variant per
target type plus `Unknown(String, i64)` for legacy rows whose
`<name>_type` value doesn't match any registered target.

```rust
#[model(table = "posts", morph_type = "post")]
pub struct Post { /* ... */ }

#[model(table = "videos", morph_type = "video")]
pub struct Video { /* ... */ }

#[model(table = "comments", relations = {
    commentable: MorphTo {
        name = "commentable",
        targets = [
            crate::models::Post,
            crate::models::Video,
        ],
    },
})]
pub struct Comment {
    pub id: i64,
    pub commentable_id: i64,
    pub commentable_type: String,
    pub body: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

let c = Comment::find(1).await?.unwrap();
match c.commentable().get().await? {
    CommentableMorph::Post(post)   => println!("comment on post {}", post.title),
    CommentableMorph::Video(video) => println!("comment on video {}", video.url),
    // Legacy / dangling rows — `<name>_type` doesn't match any target,
    // OR the morph_type matched but the row at `<name>_id` is gone.
    CommentableMorph::Unknown(ty, id) => {
        eprintln!("comment {} points at unknown {ty}#{id}", c.id);
    }
}
```

The `morph_type = "..."` attribute on each target struct is what the
loader writes into the child's `<name>_type` column on insert and
filters by on read. Without `morph_type`, the framework derives the
type-string from `to_snake(struct_name)`.

`MorphTo` dispatch — how the per-family enum picks the right variant
— consults the runtime morph registry (the inventory populated by
every `#[suprnova::model(morph_type = "...")]` declaration). For each
declared target, the fetch helper looks up the target's `TypeId`,
reads the registered `morph_type` string, and compares it against the
stored `<name>_type` value on the child row. First match wins, in
declaration order. Targets without an explicit `morph_type` attribute
fall back to `to_snake(target_type_name)` — the same default the
parent-side `MorphMany` / `MorphOne` uses to stamp the type-string at
write time, so the two sides stay aligned. This means custom
`morph_type` values (e.g. `morph_type = "blog_post"` on a struct
named `Post`, or any non-conventional string) dispatch correctly
without changes to the declaration site.

### `MorphOne<R>` and `MorphMany<R>` — parent side

The inverse direction of `MorphTo`: a parent type declares the
polymorphic one-or-many it owns. `MorphOne` returns `Option<R>` from
`.first()`; `MorphMany` returns `Vec<R>` from `.get()`. Both filter
the child's `(<name>_id, <name>_type)` pair by `self.id` and the
parent's `morph_type`.

```rust
#[model(table = "posts", morph_type = "post", relations = {
    comments: MorphMany<crate::models::Comment> {
        name = "commentable",
    },
    cover: MorphOne<crate::models::Image> {
        name = "imageable",
    },
})]
pub struct Post { /* ... */ }

#[model(table = "videos", morph_type = "video", relations = {
    comments: MorphMany<crate::models::Comment> {
        name = "commentable",
    },
})]
pub struct Video { /* ... */ }

let post = Post::find(1).await?.unwrap();
let post_comments: Vec<Comment> = post.comments().get().await?;
let post_cover:    Option<Image> = post.cover().first().await?;

let video = Video::find(1).await?.unwrap();
let video_comments: Vec<Comment> = video.comments().get().await?;
// post.comments() returns only `commentable_type = "post"` rows;
// video.comments() returns only `commentable_type = "video"`.
```

The same chainable surface as `HasMany` / `HasOne`: `.filter` /
`.db_where`, `.order_by` / `.latest` / `.oldest`, `.limit` / `.take`,
`.first` / `.get` / `.count`.

### `MorphToMany<R, P>` and `MorphedByMany<R, P>`

Polymorphic many-to-many. The shared pivot `P` carries the FK pair
PLUS a `<name>_type` discriminator column. One end declares
`MorphToMany` (e.g. `Post.tags()`, `Video.tags()`), the other end
declares one `MorphedByMany` per target family (e.g. `Tag.posts()`,
`Tag.videos()`).

```rust
#[model(table = "taggables", fillable = ["tag_id", "taggable_id", "taggable_type"])]
pub struct Taggable {
    pub id: i64,
    pub tag_id: i64,
    pub taggable_id: i64,
    pub taggable_type: String,
}

#[model(table = "posts", morph_type = "post", relations = {
    tags: MorphToMany<crate::models::Tag, Taggable> {
        name = "taggable",
    },
})]
pub struct Post { /* ... */ }

#[model(table = "videos", morph_type = "video", relations = {
    tags: MorphToMany<crate::models::Tag, Taggable> {
        name = "taggable",
    },
})]
pub struct Video { /* ... */ }

// Inverse: Tag declares one MorphedByMany per target family.
#[model(table = "tags", relations = {
    posts: MorphedByMany<crate::models::Post, Taggable> {
        name = "taggable",
        target_morph_type = "post",
    },
    videos: MorphedByMany<crate::models::Video, Taggable> {
        name = "taggable",
        target_morph_type = "video",
    },
})]
pub struct Tag { /* ... */ }

let post  = Post::find(1).await?.unwrap();
let video = Video::find(1).await?.unwrap();
let tag   = Tag::create(attrs! { name: "rust" }).await?;

// `attach` / `attach_with` / `detach` / `sync` work the same way as
// BelongsToMany. The `<name>_type` column lands automatically from
// the calling parent's `morph_type`.
post.tags().attach(tag.id).await?;
video.tags().attach(tag.id).await?;          // independent attachment
post.tags().sync([tag_a.id, tag_b.id]).await?;

// Inverse direction — Tag splits by family:
let posts_with_tag:  Vec<Post>  = tag.posts().get().await?;   // typed "post"
let videos_with_tag: Vec<Video> = tag.videos().get().await?;  // typed "video"
```

`MorphedByMany`'s `target_morph_type` is required because the macro
at `Tag`'s declaration site can't introspect the target's
`morph_type = "..."` attribute (it lives in a separate
`#[suprnova::model]` invocation). Setting it explicitly keeps each
`MorphedByMany` arm honest about which family it scans.

### Escape hatch: hand-written relation methods

The relations declared in `relations = { ... }` are the only ones the
eager-load dispatcher (and `with`, `with_count`, etc.) knows about.
If a relation is too unusual for the macro shape — for example a
query that aggregates across two pivots, or a typed view of a
denormalised cache table — you can omit it from `relations = { ... }`
and write a plain inherent impl:

```rust
impl User {
    /// Posts this user authored OR is tagged in. Crosses two relations
    /// and is therefore not expressible as a single `relations = { ... }`
    /// declaration — written by hand.
    pub async fn posts_touched(&self) -> Result<Vec<Post>, FrameworkError> {
        let authored: Vec<Post> = self.posts().get().await?;
        let tagged:   Vec<Post> = /* ...custom query... */;
        // ...merge + dedupe...
        Ok(/* ... */)
    }
}
```

Such methods lose eager-load support — `User::with(["posts_touched"])`
will error because the dispatcher has no arm for `posts_touched`. The
in-macro declarations remain the path the framework knows how to
eager-load, count, aggregate, and predicate-filter.

### v1 restrictions

A handful of things the v1 surface holds off on. Each is documented at
its declaration site too — collected here for visibility:

- **Morph IDs are `i64`-only.** `MorphTo::morph_id` is hardcoded to
  `i64`, so any model used as a `MorphTo` target must declare an `i64`
  primary key, and the child table's `<name>_id` column must also be
  `i64`. String / UUID-as-string morph FKs are v2.
- **No nested eager loading through `MorphTo`.** The per-family enum
  erases the child type, so a dotted path like
  `with(["commentable.user"])` can't tail-recurse — the dispatcher
  returns a typed error. Resolve per-family by matching on the enum
  and calling `with(["user"])` on each variant individually.
## Eager loading

Eager loading avoids N+1 queries. Instead of `posts.len()` queries to
fetch every user's posts, Suprnova issues ONE query per top-level
relation regardless of how many parent rows are loaded.

The full surface — flat list, nested paths, count, aggregates, and
predicate-filtered eager loads — is reached through the
`#[suprnova::model]`-emitted helpers on each model:

```rust
// Single relation:
let users = User::with(["posts"]).get().await?;
for u in &users {
    for p in u.posts_loaded() { /* ... */ }
}

// Multiple relations:
let users = User::with(["posts", "profile"]).get().await?;

// Nested paths — three queries (users + posts + comments), no N+1:
let users = User::with(["posts.comments"]).get().await?;
let p1 = users[0].posts_loaded()[0];
let comments = p1.comments_loaded();

// Deeper nesting works as expected:
let users = User::with(["posts.comments.author"]).get().await?;

// Count alongside the parent rows:
let users = User::with_count(["posts"]).get().await?;
for u in &users {
    println!("{} has {} posts", u.name, u.posts_count());
}

// Aggregates — Sum / Avg / Min / Max over a relation column. The
// ergonomic read is the macro-emitted `<rel>_sum_of(col)` accessor.
let users = User::with_sum(("posts", "views")).get().await?;
let sum: f64 = users[0]
    .posts_sum_of("views")
    .expect("with_sum populated the cache");

// Multiple aggregates on the same relation compose — the cache key
// is the wide `<rel>_<kind>_<col>` form, so distinct kinds and
// distinct columns don't collide:
let users = User::with_sum(("posts", "views"))
    .with_avg(("posts", "views"))
    .with_min(("posts", "id"))
    .get()
    .await?;
let u = &users[0];
let sum = u.posts_sum_of("views").unwrap();   // Some(_)  — sum of views
let avg = u.posts_avg_of("views").unwrap();   // Some(_)  — avg of views
let min = u.posts_min_of("id").unwrap();      // Some(Some(_)) — non-empty group
let max = u.posts_max_of("id");               // None  — with_max was not called

// Filter the eager-loaded children. The macro emits a typed
// `with_where_<rel>(closure)` static helper per relation so the closure
// parameter type is inferred — no need to spell out `Builder<Post>`:
let users = User::with_where_posts(|q| q.filter("published", true))
    .get()
    .await?;
// The returned `Builder<User>` chains with any other base-query
// builder method:
let users = User::with_where_posts(|q| q.filter("published", true))
    .filter("active", true)
    .get()
    .await?;
// The generic form is still available — useful when the relation name
// is computed at runtime — but you'll need to name the target type on
// the closure:
let users = User::query()
    .with_where(("posts", |q: Builder<Post>| q.filter("published", true)))
    .get()
    .await?;
// Each u.posts_loaded() contains only published posts.
```

### Cache layout

The per-row `__eager` cache cells are keyed by:

- `<rel>` (relation NAME alone) for `with` and `with_count`.
- `<rel>_<kind>_<col>` (e.g. `posts_sum_views`) for the four
  aggregate kinds — `with_sum` / `with_avg` / `with_min` / `with_max`.
  This wide key lets multiple aggregates on the same relation coexist
  on the same row without overwriting each other.

| Method                              | Cache key            | Cache cell type   | Empty-group value |
|-------------------------------------|----------------------|-------------------|-------------------|
| `with(["posts"])`                   | `posts`              | `Vec<Post>`       | `Vec::new()`      |
| `with(["profile"])`                 | `profile`            | `Option<Profile>` | `None`            |
| `with_count(["posts"])`             | `posts`              | `u64`             | `0`               |
| `with_sum(("posts","views"))`       | `posts_sum_views`    | `f64`             | `0.0`             |
| `with_avg(("posts","views"))`       | `posts_avg_views`    | `f64`             | `0.0`             |
| `with_min(("posts","id"))`          | `posts_min_id`       | `Option<f64>`     | `None`            |
| `with_max(("posts","id"))`          | `posts_max_id`       | `Option<f64>`     | `None`            |

The macro emits matching accessors on each model:

- `<rel>_loaded()` — for collection relations: `&[Post]` (panics if
  the relation wasn't eager-loaded). For single-value relations:
  `Option<&Profile>`.
- `<rel>_count()` — `u64`. Panics if `with_count(["..."])` wasn't
  called.
- `<rel>_sum_of(col)` / `<rel>_avg_of(col)` — return `Option<f64>`
  (`None` if the matching `with_sum` / `with_avg` was not called).
- `<rel>_min_of(col)` / `<rel>_max_of(col)` — return
  `Option<Option<f64>>`: outer `Option` is "was `with_min` /
  `with_max` called?", inner `Option` is "did SQL return NULL because
  the group was empty?".

The accessors are the ergonomic surface — read through them rather
than reaching into `__eager.get_aggregate::<T>(...)` directly. They
build the same cache key under the hood via
`eloquent::relations::aggregate_cache_key`.

### Composing aggregates on the same relation

The wide cache key means you can stack as many `with_*` calls on the
same relation in one query as you want — no collisions:

```rust
let users = User::with_sum(("posts", "views"))
    .with_avg(("posts", "views"))
    .with_min(("posts", "id"))
    .with_max(("posts", "id"))
    .get()
    .await?;

let u = &users[0];
let total_views: f64 = u.posts_sum_of("views").unwrap();
let avg_views:   f64 = u.posts_avg_of("views").unwrap();

// Min/Max are double-Option because SQL min/max NULLs on empty:
match u.posts_min_of("id") {
    None              => panic!("with_min not called"),
    Some(None)        => println!("no posts yet"),
    Some(Some(min))   => println!("smallest post id: {min}"),
}

// Accessor returns `None` when the matching `with_*` was skipped:
assert!(u.posts_avg_of("score").is_none()); // never called with col="score"
```

### Aggregates and INTEGER columns

SUM over an INTEGER column lands in the cache as `f64`. The
dispatcher arms try `try_get::<Option<f64>>` first, then fall back to
`try_get::<Option<i64>>().map(|n| n as f64)` so SQLite's INTEGER-
preserving COUNT/SUM types don't silently coerce to `0.0`. Read via
the macro-emitted accessors regardless of the source column type.

### `with_where` predicate routing

`User::with_where_posts(|q| q.filter("published", true))` applies a
closure to the inner `Builder<Post>` BEFORE the
`filter_in(<fk>, parent_ids)` IN-query is issued, so only matching
child rows reach the cache. The macro emits one typed
`with_where_<rel>` static helper per declared relation, so the closure's
parameter type is inferred from the method signature.

The generic
`with_where(("posts", |q: Builder<Post>| q.filter("published", true)))`
is still available — useful when the relation name is computed at
runtime, or when you already hold a `Builder<User>` and want to attach
a predicate. It requires naming the target type on the closure because
the predicate goes through a `Box<dyn Any>` and Rust can't infer the
type from the relation name alone. (Rust's orphan rules forbid the
macro from adding a typed method directly on `Builder<User>`, so the
typed shorthand is offered only on the model — `User::with_where_<rel>`
— not as a builder-chain method.)

For the polymorphic kinds, the predicate runs against the related-table
query — not the pivot scan.

`with_where` is supported on every relation kind EXCEPT `MorphTo`.
MorphTo's per-family enum erases the child type, so no single
`Builder<R>` covers all variants. Nested eager loading through
MorphTo is also not supported in v1 — `with(["commentable.user"])`
where `commentable` is a `MorphTo` returns an error from the
recurse-eager-load dispatcher.

### `Collection::load` / `load_missing`

When you've already fetched rows and want to eager-load relations
after the fact:

```rust
use suprnova::Collection;

let mut users: Collection<User> = User::all().await?.into();
users.load(["posts.comments"]).await?;
```

`load_missing` is per-row: each row in the collection is partitioned
independently. Rows that already have the named relation cached stay
untouched; rows that don't get the relation loaded. Mirrors Laravel's
`$collection->loadMissing(...)` semantics.

For nested paths the partition repeats at every level. Given
`load_missing(["posts.comments"])`:

- Rows without `posts` cached get the FULL path loaded — `posts` plus
  their `comments`.
- Rows WITH `posts` already cached recurse into the cached posts and
  load `comments` only on the posts that don't already have comments
  cached.

The same per-row partition repeats at every further segment of a
longer dotted path (`"posts.comments.author"` etc.) — at each step
only the rows missing that segment get the bulk-load.

## Pagination

Three paginator types compose on top of `Builder<M>`:

| Method | Returns | Queries per page | Use when |
|--------|---------|------------------|----------|
| `paginate(per_page)` | `LengthAwarePaginator<M>` | 2 (COUNT + LIMIT) | UI needs total page count |
| `simple_paginate(per_page)` | `Paginator<M>` | 1 (LIMIT + 1) | Large tables; "Next" button only |
| `cursor_paginate(per_page)` | `CursorPaginator<M>` | 1 (LIMIT + 1) | Infinite scroll; deep pagination |

All three implement `Serialize` with the Laravel-standard JSON shape,
so they ship directly to Inertia / JSON consumers without reshaping.

### Length-aware

```rust
use suprnova::LengthAwarePaginator;

let page: LengthAwarePaginator<User> = User::query()
    .filter("active", true)
    .order_by_desc("created_at")
    .paginate(20)
    .await?;

// page.data: Vec<User>
// page.total: u64 — total row count across all pages
// page.last_page: u64 — 1-based last page index
// page.current_page: u64
// page.per_page: u64
// page.from / page.to: Option<u64> — 1-based window bounds
// page.path: Option<String> — optional base URL for link generation
```

Page-param parsing reads `?page=N` from the active request via
`Context::query_param`. To paginate multiple lists on the same page
with their own query keys, use `paginate_using`:

```rust
let posts = Post::query().paginate_using("posts_page", 10).await?;
let comments = Comment::query().paginate_using("comments_page", 25).await?;
```

**JSON shape:**

```json
{
  "data": [...],
  "current_page": 1,
  "last_page": 3,
  "per_page": 10,
  "total": 25,
  "from": 1,
  "to": 10,
  "path": "/api/users"
}
```

`path` is omitted from JSON when unset.

### Simple paginate (no count)

`paginate` always runs two queries — a `COUNT(*)` plus the page
fetch. On large tables the count alone can dominate request time.
`simple_paginate` skips the count entirely; instead it fetches
`per_page + 1` rows and reports whether a next page exists via the
`has_more` flag:

```rust
use suprnova::Paginator;

let page: Paginator<User> = User::query()
    .order_by_desc("id")
    .simple_paginate(20)
    .await?;

// page.has_more: bool — was there an extra row past per_page?
// page.current_page, page.per_page, page.data, page.path: as above.
```

**JSON shape:**

```json
{
  "data": [...],
  "current_page": 1,
  "per_page": 10,
  "has_more": true
}
```

### Cursor paginate (keyset)

Cursor paginate is the choice for infinite scroll, deep pagination,
or anywhere a stable row order with cheap O(1)-per-page seeking is
worth more than a numeric page UI. Forward-only by default — the
returned `prev_cursor` is always `None`.

```rust
use suprnova::CursorPaginator;

let page: CursorPaginator<User> = User::query()
    .cursor_paginate(20)
    .await?;

// page.data: Vec<User>
// page.per_page: u64
// page.next_cursor: Option<String> — opaque cursor for the next page
// page.prev_cursor: Option<String> — None for forward-only iteration
// page.path: Option<String>
```

Cursors are **encrypted and authenticated** via `CursorPaginator::encode_value`
— they encode the keyset boundary (the model's primary key) plus a
direction tag, AES-256-GCM-sealed with the framework's `APP_KEY`.
Tampering produces a 400 ParamParse error; the cursor is opaque to
the client and unforgeable without the key.

The next request passes the cursor through `?cursor=<opaque>`:

```
GET /api/users?cursor=eyJ0IjoiQmlnSW50IiwidiI6MTAwLCJkIjoibmV4dCJ9...
```

Cursor pagination **replaces** any existing `ORDER BY` on the
builder — a stable PK ASC order is required for `gt(boundary)` to
slice deterministically.

**JSON shape:**

```json
{
  "data": [...],
  "per_page": 10,
  "next_cursor": "...",
  "prev_cursor": null,
  "path": "/api/users"
}
```

`next_cursor` and `prev_cursor` are always present as JSON keys
(emitted as `null` when absent) so client schemas can rely on the
field's presence; `path` is omitted when unset.

### Errors

| Condition | Variant | HTTP |
|-----------|---------|------|
| `per_page == 0` | `FrameworkError::ParamError { param_name: "per_page" }` | 400 |
| Invalid cursor (bad base64, JSON, or HMAC fails) | `FrameworkError::Internal` from `Crypt::decrypt_string` | 500 |
| Underlying DB failure | `FrameworkError::Database` | 500 |

Cursor authentication failure surfaces as `Internal` (not
`ParamParse`) so a tampered cursor doesn't leak protocol-level
information to the client; the response body still carries a
human-readable reason.

### Reading query params outside a real request

Tests, console commands, and background workers don't run inside a
hyper request — so `Context::query_param("page")` returns `None` and
`paginate` falls back to page 1. Tests that need to exercise a
specific page can install a per-thread override:

```rust
use suprnova::context::Context;

#[tokio::test]
async fn paginate_page_2() {
    Context::test_clear_query();
    Context::test_set_query("page", "2");

    let page = User::query().paginate(10).await.unwrap();
    assert_eq!(page.current_page, 2);

    Context::test_clear_query();
}
```

`test_set_query` / `test_clear_query` are gated behind the
`testing` feature (default-enabled in `framework/Cargo.toml`) so
release builds never see this surface.

## Chunking and lazy iteration

Seven streaming entry points on `Builder<M>` let you process large
result sets in bounded memory. Pick by trade-off:

| Method | Pagination | Concurrent-safe? | Returns |
|--------|-----------|------------------|---------|
| `chunk(n, async \|batch\| { ... })` | OFFSET | No | `Result<(), _>` |
| `chunk_by_id(n, async \|batch\| { ... })` | PK cursor | **Yes** | `Result<(), _>` |
| `chunk_map(n, async \|batch\| { ... })` | OFFSET | No | `Collection<U>` |
| `each(async \|row\| { ... })` | OFFSET, size 1 | No | `Result<(), _>` |
| `lazy()` | PK cursor, batch 1000 | **Yes** | `LazyCollection<M>` |
| `lazy_by_id(batch_size)` | PK cursor, custom batch | **Yes** | `LazyCollection<M>` |
| `cursor()` | Alias for `lazy()` | **Yes** | `LazyCollection<M>` |

### chunk — OFFSET-paginated batches

```rust
use suprnova::{Collection, Model};

User::query().chunk(100, |batch: Collection<User>| async move {
    for user in &batch {
        send_welcome_email(user).await?;
    }
    Ok(())
}).await?;
```

The closure receives a `Collection<M>` per batch — slice-shape access
(`.iter()`, indexing) works directly via `Deref`.

`chunk` is OFFSET-paginated and **not safe under concurrent inserts**:
rows inserted before the next batch's offset get skipped; rows deleted
before the offset get processed twice (whatever shifted into their
slot). Use `chunk_by_id` for production-grade bulk processing against
tables under write load.

### chunk_by_id — PK-cursor batches, concurrent-safe

```rust
User::query().chunk_by_id(500, |batch| async move {
    for user in &batch {
        reindex_user(user).await?;
    }
    Ok(())
}).await?;
```

Each batch filters on `WHERE id > last_id ORDER BY id ASC LIMIT n`,
so rows inserted mid-iteration with PKs above the cursor land in a
later batch (or are picked up by a subsequent run) — they never cause
an original row to skip or duplicate.

`chunk_by_id` requires an `i64` primary key. Models with `String` /
`Uuid` PKs use `chunk` with the OFFSET caveat. (Generalising the
cursor shape to non-`i64` keys is on the follow-up list.)

### chunk_map — chunk + per-chunk map

```rust
let totals: Collection<i64> = Order::query()
    .chunk_map(1000, |batch| async move {
        let sum: i64 = batch.iter().map(|o| o.amount).sum();
        Ok(Collection::from_vec(vec![sum]))
    })
    .await?;
```

Maps each batch through `f`, concatenates the mapped output, and
returns a single `Collection<U>`. Memory-bounded only when `U` is
strictly smaller than `M` — pick this when you're producing summaries
(per-batch totals, ids, aggregates) rather than transformed rows.

### each — one row at a time, OFFSET

```rust
User::query().each(|user| async move {
    send_welcome_email(&user).await?;
    Ok(())
}).await?;
```

Sugar for `chunk(1, ...)` — one query per row. For large datasets,
switch to `lazy()` which batches internally (default 1000 rows per
fetch) while still surfacing one row at a time to the consumer.

### lazy / lazy_by_id / cursor — streams

```rust
let mut stream = User::query().lazy();
while let Some(row) = stream.next().await {
    let user = row?;
    println!("{}", user.email);
}
```

`lazy()` returns a `LazyCollection<M>` — a `Send` stream wrapper
that yields `Result<M, FrameworkError>` per row. Backpressure works
naturally: a slow consumer parks at the `await` point and the next
batch only fetches when the in-memory buffer drains.

`lazy()` batches via PK cursor with a default size of 1000 rows.
Override the batch size with `lazy_by_id(500)`. `cursor()` is the
Laravel name and is a zero-cost alias for `lazy()`.

Same `i64`-PK constraint as `chunk_by_id`.

### Eager loads inside chunks

All seven entry points **reject `.with(...)` up front** with a loud
`FrameworkError::internal`. The Builder's cross-batch clone drops
the type-erased eager-load plan (its boxed-`dyn Any` predicate isn't
clonable without tightening the public API), so honouring the plan
would be silently inconsistent across batches. Re-apply `.with(...)`
inside the per-chunk closure when needed — each batch's
`Collection<M>` composes with `load(...)` / `load_missing(...)`:

```rust
User::query().chunk(100, |batch| async move {
    let mut batch = batch;
    batch.load("posts").await?;
    for u in &batch {
        let posts = u.posts_loaded();
        // ...
    }
    Ok(())
}).await?;
```

## Mass assignment

### Fillable allowlist

```rust
#[model(
    table = "users",
    fillable = ["name", "email"],
)]
pub struct User { /* ... */ }

User::create(attrs! {
    name: "Alice",
    email: "alice@example.com",
    admin: true,    // silently dropped at runtime — not in fillable
}).await?;
```

### Guarded denylist

`guarded` is the inverse — every field is fillable EXCEPT the
guarded ones. Mutually exclusive with `fillable`; using both at
once is a compile-time error from the macro.

```rust
#[model(
    table = "posts",
    guarded = ["id", "user_id"],   // everything else is fillable
)]
pub struct Post { /* ... */ }
```

### Default policy

When neither `fillable` nor `guarded` is set, the default policy is
`guarded = ["id"]` (or whatever `primary_key = "..."` resolves to)
— every field is fillable except the primary key. This matches
Laravel's "all fields fillable except the PK" default.

### `unguarded(closure)` escape hatch

`unguarded(closure)` turns off the filter for a block:

```rust
use suprnova::eloquent::unguarded;

// Bypass the filter for a one-shot data-migration script:
unguarded(|| async {
    User::create(attrs! {
        name: "Bootstrap",
        email: "boot@example.com",
        admin: true,    // assignable inside the closure
    }).await
}).await?;
```

Implementation: a `tokio::task_local!` boolean the `Fillable::apply`
filter checks before running. Task-local means concurrent requests
aren't affected by another task's `unguarded` scope.

## Casts

Casts run at the boundary between storage (column value) and runtime
(model field). Each cast type implements the `Cast` trait. Built-in
casts cover Laravel's full set; users register custom casts via the
trait.

### Explicit-only

Casts are declared in `#[model(casts = { ... })]` — there is no
auto-detection from field types. A `prefs: Json` field doesn't
implicitly become `AsJson`; you write `casts = { prefs = AsJson }`.
Rationale: you should be able to read the model and know exactly
what runs at storage boundaries. No magic.

### Example

```rust
use suprnova::{model, AsArray, AsBool, AsCollection, AsDate, AsDateTime,
    AsEncrypted, AsEnum, AsObject, AsTimestamp};

#[model(
    table = "users",
    casts = {
        active        = AsBool,
        preferences   = AsArray<String>,
        options       = AsObject<UserOptions>,
        profile       = AsCollection<ProfileField>,
        birthday      = AsDate,
        last_seen_at  = AsDateTime,
        role          = AsEnum<UserRole>,
        api_token     = AsEncrypted,
    },
)]
pub struct User { /* ... */ }
```

### Full Laravel cast list and Suprnova mapping

| Laravel cast | Suprnova cast | Runtime type |
|--------------|---------------|--------------|
| `bool`, `boolean` | `AsBool` | `bool` |
| `int`, `integer` | `AsInt<I>` | `I: PrimInt` |
| `float`, `double`, `real` | `AsFloat` | `f64` |
| `decimal:N` | `AsDecimal<N>` | `rust_decimal::Decimal` |
| `string` | `AsString` | `String` |
| `array` | `AsArray<T>` | `Vec<T>` (JSON-encoded) |
| `object` | `AsObject<T>` | `T: Serialize + DeserializeOwned` |
| `collection` | `AsCollection<T>` | `Collection<T>` |
| `json` | `AsJson<T>` | `T` (raw JSON column) |
| `date`, `date:format` | `AsDate` | `chrono::NaiveDate` |
| `datetime`, `datetime:format` | `AsDateTime` | `chrono::DateTime<Utc>` |
| `immutable_date` | `AsImmutableDate` | `chrono::NaiveDate` |
| `immutable_datetime` | `AsImmutableDateTime` | `chrono::DateTime<Utc>` |
| `timestamp` | `AsTimestamp` | `i64` (unix epoch) |
| `encrypted` | `AsEncrypted` | `String` (encrypted via `Crypt`) |
| `encrypted:array` | `AsEncryptedArray<T>` | `Vec<T>` (JSON + encrypted) |
| `encrypted:object` | `AsEncryptedObject<T>` | `T` (JSON + encrypted) |
| `encrypted:collection` | `AsEncryptedCollection<T>` | `Collection<T>` |
| `EnumClass::class` | `AsEnum<E>` | `E: EnumString + AsRefStr` |
| `AsArrayObject::class` | `AsArrayObject<T>` | `IndexMap<String, T>` |
| `hashed` | `AsHashed` | `String` (`Hash::make` on write; never decrypts) |

21 casts total. Most map one-to-one with Laravel; the
`AsOptionalDateTime` (used by `soft_deletes`) is auto-injected by
the macro when the soft-delete column is `Option<DateTime<Utc>>`.

### Encrypted cast failure modes

The four `AsEncrypted*` casts route every encrypt/decrypt through the
`Crypt` facade (keyed by `APP_KEY`). When decryption fails — wrong
key, truncated ciphertext, tampered bytes, AEAD tag mismatch — the
cast surfaces a clear `FrameworkError::Internal` from
`Cast::from_storage`. There is no silent fallback to garbage:

- Loading a row through `Model::find` / `Model::query()` propagates
  the decrypt error and (per the macro-generated `From<inner::Model>`)
  panics with `cast from_storage failed — corrupt data in database
  column`. Operators see the failure in logs immediately; the model
  never carries plausible-but-wrong plaintext.
- The `AsHashed` cast is one-way; it never decrypts so this failure
  mode does not apply.

This matches Laravel's `encrypted` cast: a wrong `APP_KEY` against an
existing encrypted column is a hard error, never a quiet
`null`/empty string.

### Rotating `APP_KEY`

Suprnova supports zero-downtime key rotation via a key *ring*: the
current `APP_KEY` encrypts; an optional `APP_KEY_PREVIOUS` env var
(comma-separated, oldest-to-newest) supplies decrypt fallbacks for
data written under older keys. Encryption *always* uses the current
key — previous keys participate only on decrypt.

Each decrypt that falls through to a previous key emits a
`tracing::warn!` line containing the previous-key index. The log
payload deliberately excludes plaintext and ciphertext; just the
fact-of-rotation plus an actionable re-encrypt hint.

**Rotation procedure** (zero-downtime, safe for production):

1. Mint a new key: `suprnova key:generate` (writes to stdout).
2. Move the old key to `APP_KEY_PREVIOUS` and set `APP_KEY` to the
   new value:
   ```
   APP_KEY_PREVIOUS=<old_key>
   APP_KEY=<new_key>
   ```
3. Deploy. New writes use the new key; existing rows continue to
   decrypt via the previous-key fallback. Warnings in logs identify
   columns that still depend on `APP_KEY_PREVIOUS`.
4. Run a re-encrypt pass. For each model with encrypted casts:
   ```rust
   for chunk in User::query().chunk(500).await? {
       for user in chunk {
           // Touch + save rewrites every cast column under the
           // current key. `Cast::to_storage` always reaches for
           // the current ring entry.
           user.save().await?;
       }
   }
   ```
   This is idempotent — rows already on the new key just no-op.
5. Once logs show no more `APP_KEY_PREVIOUS` warnings (give the
   batch + any soft-deleted / archived data a generous window),
   remove `APP_KEY_PREVIOUS` from the environment and redeploy.

**Multi-step rotation.** If you rotate again before completing the
previous pass, append: `APP_KEY_PREVIOUS=<oldest>,<previous>`. The
ring tries every previous key in order. There is no upper bound, but
each fallback adds a single AEAD trial-decrypt — keep the list short
in steady state.

**Constraints.**

- A malformed entry in `APP_KEY_PREVIOUS` fails boot loudly (same as
  a malformed `APP_KEY`) — a half-rotated secret should never
  silently degrade.
- Empty entries in the list (e.g. trailing commas from templated
  config) are tolerated as "no key in this slot" — not an error.
- The wire format is unchanged from the pre-rotation single-key
  layout: no key identifier is embedded in the ciphertext. The ring
  trial-decrypts each key in order until one succeeds.

### Runtime cast override — `with_casts`

```rust
let users = User::query()
    .with_casts(suprnova::casts! { birthdate = AsDateTime })
    .get()
    .await?;
```

`with_casts` overrides the model's declared casts for the duration
of a single query — useful when a raw column comes back from a
join / view / `select_raw` and needs a different type coercion
than the model's default.

### Custom casts

Custom casts implement `Cast`:

```rust
use suprnova::eloquent::casts::Cast;
use suprnova::FrameworkError;

pub struct AsAesGcmJson<T>(std::marker::PhantomData<T>);

impl<T: serde::Serialize + serde::de::DeserializeOwned + Send + Sync> Cast
    for AsAesGcmJson<T>
{
    type Runtime = T;
    type Storage = String;
    fn to_storage(value: &T) -> Result<String, FrameworkError> { /* ... */ }
    fn from_storage(stored: &String) -> Result<T, FrameworkError> { /* ... */ }
}

#[model(casts = { secret = AsAesGcmJson<SecretBundle> })]
pub struct Vault { /* ... */ }
```

The `Cast` trait is shipped alongside the primitive casts. Custom
casts can use either `String` storage (when JSON-encoding) or any
of the SeaORM-supported scalar types (`i64`, `f64`, `bool`,
`Vec<u8>`).

## Accessors and mutators

### Accessors

```rust
#[model(
    table = "users",
    appends = ["full_name"],
)]
pub struct User {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
    // ...
}

impl User {
    #[accessor]
    pub fn full_name(&self) -> String {
        format!("{} {}", self.first_name, self.last_name)
    }
}
```

When `user.to_array()` runs (or `user.to_json()`, which delegates
to it), the `full_name` accessor is called and its return value
is inserted into the JSON output. Calling `user.full_name()` from
Rust is just a regular method call.

### Mutators

Mutators run before storage:

```rust
#[model(
    table = "users",
    fillable = ["first_name", "last_name", "password"],
    mutators = ["password"],
)]
pub struct User { /* ... */ }

impl User {
    #[mutator]
    pub fn set_password(
        &mut self,
        value: serde_json::Value,
    ) -> Result<(), suprnova::FrameworkError> {
        let raw: String = serde_json::from_value(value).map_err(|e| {
            suprnova::FrameworkError::validation("password", format!("{e}"))
        })?;
        self.password = hash::make(&raw);
        Ok(())
    }
}
```

Calling `user.password = "secret".into()` directly assigns the raw
value without running the mutator. To run the mutator path, call
`user.set_password(json!("secret"))` or use the JSON path
(`user.fill(attrs!{password: "secret"})`), which routes through
the mutator automatically because `"password"` is listed in
`mutators = [...]`.

### How routing works

- **Serialization (`to_array` → `Value`, `to_json` → `String`)**
  runs accessors. Every field name listed in `appends = [...]`
  becomes a call to `self.<name>()`; the return value is inserted
  into the JSON output. `to_json()` is a thin wrapper:
  `serde_json::to_string(&self.to_array())`.
- **Fill-style writes (`fill`, `create`, `update`)** route through
  mutators. Every field name listed in `mutators = [...]` becomes a
  call to `self.set_<field>(value)` instead of direct assignment.

The function-level `#[accessor]` and `#[mutator]` macros emit
registry entries the macro's serialization / fill paths walk.

### Hidden / visible

```rust
#[model(
    table = "users",
    hidden = ["password", "remember_token"],
)]
pub struct User { /* ... */ }
```

`hidden = [...]` is a denylist — every column except the listed
ones serialises. `visible = [...]` is the inclusive form — only the
listed ones serialise. Mutually exclusive at compile time.

## Timestamps

When both `created_at` and `updated_at` columns exist, the macro
auto-detects them and enables timestamp tracking:

- `created_at` is set to `Utc::now()` on `save()` for new rows.
- `updated_at` is set to `Utc::now()` on every `save()`.

The auto-detect is conservative: if the struct has only one of the
two columns, the macro errors out so a typo
(`craeted_at`) doesn't silently disable timestamps. Set
`timestamps = false` to opt out entirely.

### Disabling auto-timestamps

```rust
#[model(table = "audit_logs", timestamps = false)]
pub struct AuditLog {
    pub id: i64,
    pub event: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    // No updated_at field — but timestamps = false silences the
    // macro's `only one column found` error too.
}
```

### `touch()` — bump updated_at without other changes

```rust
user.touch().await?;
```

`touch()` issues `UPDATE table SET updated_at = ? WHERE pk = ?` —
atomic, no read-modify-write. The macro emits a `Touchable` impl on
every timestamped model.

### Parent touching

```rust
#[model(
    table = "comments",
    touches = ["post"],
    timestamps,
)]
pub struct Comment {
    pub id: i64,
    pub post_id: i64,
    // ...
}
```

In Phase 10A the `touches = [...]` list is parsed and stored on the
model as a `TOUCHES` const. The post-save hook that actually fires
`self.post().touch().await?` lands in Phase 10B alongside the
relation API — the metadata is already in place.

### Format

Always ISO 8601 with UTC. No `Model::$timestampsFormat` override
(per the divergence-from-Eloquent table — frontend interop comes
first; locale formatting belongs in the i18n layer).

## Observers and lifecycle events

Every model goes through a fixed 16-event lifecycle as it moves
through `create` / `save` / `update` / `delete` / `restore` /
`replicate` / Builder query paths. Listeners can hook each event
to log, audit, side-effect, validate, or cancel the in-flight
operation.

### The 16 lifecycle events

Events split into two groups by cancellability:

**Cancellable (5)** — fire BEFORE the database write. A listener
returning `EventResult::cancel("reason")` aborts the operation with
`FrameworkError::bad_request(reason)`.

| Event       | When                                      | Payload                                                 |
|-------------|-------------------------------------------|---------------------------------------------------------|
| `Saving`    | Before both `create` and `save`           | `Arc<Mutex<Attrs>>` + `is_creating: bool`               |
| `Creating`  | Before `create`                           | `Arc<Mutex<Attrs>>`                                     |
| `Updating`  | Before `save` / `update` on existing row  | Pre-update model snapshot + `Arc<Mutex<Attrs>>`         |
| `Deleting`  | Before `delete` (soft or hard)            | Model + `is_force: bool` (force-delete on soft-delete)  |
| `Restoring` | Before `restore` on soft-delete model     | Model                                                   |

**Non-cancellable (11)** — fire AFTER the operation. Listener errors
propagate but cannot stop a write that already landed.

| Event           | When                                              | Payload                          |
|-----------------|---------------------------------------------------|----------------------------------|
| `Retrieving`    | Once per Builder query, before the DB call        | None                             |
| `Retrieved`     | Once per row returned by a Builder query          | Model                            |
| `Created`       | After successful `create`                         | Model                            |
| `Updated`       | After successful `save` / `update`                | Previous + current snapshots     |
| `Saved`         | After both `create` and `save`                    | Model                            |
| `Deleted`       | After successful `delete`                         | Model + `is_force: bool`         |
| `Trashed`       | After soft-delete (NOT force-delete)              | Model                            |
| `Restored`      | After successful `restore`                        | Model                            |
| `Replicating`   | During `replicate` / `replicate_into`, before return | Source + `Arc<Mutex<replica>>` (mutable) |
| `ForceDeleting` | Before `force_delete` on soft-delete model        | Model                            |
| `ForceDeleted`  | After successful `force_delete`                   | Model                            |

The cancellable / non-cancellable split mirrors Laravel's `creating`
vs `created` hook pair. `Saving` fires for both insert and update —
override that one when the behaviour is identical across both paths
and discriminate via `is_creating`.

`Replicating` is the one non-cancellable hook that hands a mutable
reference (the replica is `Arc<Mutex<M>>`). Use it to clear
timestamps, regenerate UUIDs, reset auto-increments, etc. before the
clone is returned to the caller.

### Observers vs raw listeners

Two ways to hook lifecycle events:

1. **Raw listeners** — call `EventFacade::listen::<Created, _>(Arc::new(MyListener))`
   for each event you want, one impl per event. This is the
   underlying mechanism; observers ride on top of it.

2. **Observers** — bundle all 16 hooks under one trait. The macro
   sees which methods the user overrode and registers exactly those.
   This is the recommended path for any non-trivial set of hooks.

```rust
use async_trait::async_trait;
use suprnova::eloquent::attrs::Attrs;
use suprnova::eloquent::events::EventResult;
use suprnova::eloquent::observers::Observer;
use suprnova::FrameworkError;

pub struct AuditObserver;

#[suprnova::observer(User)]   // <- MUST precede #[async_trait]
#[async_trait]
impl Observer<User> for AuditObserver {
    async fn creating(&self, attrs: &mut Attrs) -> EventResult {
        if attrs.get("email").is_none() {
            return EventResult::cancel("email is required");
        }
        EventResult::ok()
    }

    async fn created(&self, user: &User) -> Result<(), FrameworkError> {
        tracing::info!(user.id = user.id, "user created");
        Ok(())
    }
}
```

Every trait method has a default no-op, so the impl block contains
only the events you care about. The macro identifies overrides by
name match against the closed 16-method set; methods you don't
override register no listeners.

### Required attribute ordering

`#[suprnova::observer(M)]` MUST appear ABOVE `#[async_trait]`:

```rust
#[suprnova::observer(User)]   // outer — runs first, sees raw async fns
#[async_trait]                // inner — rewrites async fn signatures
impl Observer<User> for AuditObserver { /* ... */ }
```

Attribute macros expand outside-in. `async_trait` rewrites every
`async fn` into a desugared `Pin<Box<dyn Future>>` poll-fn shape;
if `#[async_trait]` ran first, the observer macro's name-match
against the 16 trait method names would find nothing and silently
emit zero listeners.

### Four registration paths

| Path                                         | When to use                                         |
|----------------------------------------------|-----------------------------------------------------|
| `#[suprnova::observer(M)]` (inventory)       | Static observer known at compile time. Auto-installs on boot. |
| `#[model(observers = [Foo, Bar])]`           | Documentation + compile-time validation that the listed types resolve. Does NOT itself register. |
| `Model::observe(MyObs).await`                | Runtime registration. Hand-driven; useful when registration depends on config. |
| `EventFacade::listen::<events::Created, _>(...)` | Lowest level — one event at a time. Use when an observer feels heavy. |

The `observers = [...]` attribute on `#[model]` is a documentation
marker. It compiles to a `const _: fn() = || { let _ =
::std::any::type_name::<T>; ... };` block that proves each listed
type resolves to a real Rust item; typos surface at the model
declaration site. Actual install is via the inventory pathway —
the `#[observer(M)]` attribute on `Foo` is what enrolls `Foo` for
auto-install.

### Bootstrap

Call `bootstrap_observers()` once at startup to drain the inventory
and install every `#[observer(M)]`-registered observer:

```rust
suprnova::eloquent::observers::bootstrap_observers().await?;
```

The drain is idempotent for the inventory pathway — each observer's
install closure is gated by a per-type `AtomicBool` (T2b's macro
emission), so calling `bootstrap_observers()` twice does not
double-register.

The runtime `Model::observe(MyObs)` shim is NOT gated. Calling it
twice registers two listener sets, matching Laravel's manual
`Model::observe(MyObs::class)` semantics. If a hand-driven observer
also has `#[observer]`, the inventory adapter fires in addition to
the manually-installed ones.

### Cancelling from an observer

The five cancellable hooks return `EventResult`. To abort the
operation, return `EventResult::cancel("reason")`:

```rust
#[suprnova::observer(Subscription)]
#[async_trait]
impl Observer<Subscription> for PolicyObserver {
    async fn creating(&self, attrs: &mut Attrs) -> EventResult {
        if let Some(plan) = attrs.get("plan") {
            if plan == "blocked" {
                return EventResult::cancel("plan is blocked");
            }
        }
        EventResult::ok()
    }
}
```

The cancel reason surfaces as `FrameworkError::bad_request(reason)`
from `Subscription::create`. The row never lands in the database —
cancel is a true abort, not a "delete after the fact".

Multiple observers may register cancellable hooks on the same model;
any one of them returning `Cancel` stops the operation. Order is the
inventory enrolment order (link order in practice).

### Multiple observers on one model

Multiple `Observer<M>` impls all fire for the same event —
EventFacade dispatch fans out to every registered listener rather
than picking one:

```rust
#[suprnova::observer(Comment)]
#[async_trait]
impl Observer<Comment> for AuditObserver { /* ... */ }

#[suprnova::observer(Comment)]
#[async_trait]
impl Observer<Comment> for NotifyObserver { /* ... */ }

// Comment::create(...) fires AuditObserver::created AND NotifyObserver::created.
```

This matches Laravel's fan-out semantics and is the load-bearing
property behind the "decompose hooks by concern" pattern: an
`AuditObserver` only knows about audit, a `NotifyObserver` only
knows about notifications, and the model declaration doesn't care
how many observers attach.

### Manual `Model::observe()`

Every `#[suprnova::model]` struct gets a per-model `observe<O>()`
shim. Call it at boot for dynamic registration:

```rust
#[derive(Clone)]
struct MyObs;

#[async_trait]
impl Observer<User> for MyObs { /* ... */ }

// At runtime:
User::observe(MyObs).await;
```

The shim's `O: Clone + 'static` bound is what lets the framework
hand a fresh observer clone to each of the 16 internal adapter
listeners. All 16 listener adapters install on every call — the
trait defaults make non-overridden methods cheap no-ops.

### Constraints

- **The macro version requires the impl block use plain method
  names matching the trait's 16 hooks.** Renamed methods,
  `#[allow]`-suppressed defaults, and `#[cfg]`-gated bodies fall
  outside the name-match and don't register listeners.

- **Observer structs the macro inspects must be zero-sized** (no
  fields) in v1. The macro constructs the observer via `let obs =
  MyObserver;` inside each adapter. Stateful observers (carrying
  `Arc<Inner>`) need the runtime `Model::observe()` path, which
  takes the observer by value and clones it into each adapter.

- **Test isolation: use unique model types per scenario.** The
  process-global EventDispatcher means listeners installed for
  `User` are visible to every test in the same binary. Per-test
  unique model types (`T2Comment`, `T2Subscription`, …) keep
  cross-test bleed out of the counter assertions. The
  `eloquent_observers.rs` integration tests exercise this pattern.

## Prunable

Laravel ships a `Prunable` trait that lets a model declare a scope
of rows to delete on a schedule. Suprnova mirrors that with two
traits and a console command.

### Declaring a pruner

```rust
use async_trait::async_trait;
use chrono::{Duration, Utc};
use suprnova::eloquent::Prunable;

#[suprnova::prunable]
#[async_trait]
impl Prunable for ExpiredSession {
    fn prunable() -> suprnova::Builder<Self> {
        Self::query().filter_op(
            "expires_at",
            "<",
            (Utc::now() - Duration::days(30)).to_rfc3339(),
        )
    }
}
```

### `MassPrunable` — bulk-delete variant

For high-volume tables (audit logs, request logs, expired cache
entries) `MassPrunable` skips per-row events and runs a single
`DELETE WHERE …` statement:

```rust
use suprnova::eloquent::MassPrunable;

#[suprnova::prunable]
#[async_trait]
impl MassPrunable for AuditLog {
    fn prunable() -> suprnova::Builder<Self> {
        Self::query().filter_op(
            "created_at",
            "<",
            (Utc::now() - Duration::days(365)).to_rfc3339(),
        )
    }
}
```

### Triggering pruning

Run via the per-project console (which `app/cmd/main.rs` calls
`suprnova::console::dispatch_argv` for, after `db:seed` and the
other built-ins):

```bash
suprnova model:prune                          # prune every registered type
suprnova model:prune --model=ExpiredSession   # filter to one model
suprnova model:prune --pretend                # dry run; logs what would delete
```

Programmatically the runners are at
`suprnova::eloquent::{prune_all, prune_all_dry, prune_one}`.

### Pruning hook

`Prunable::pruning(&self)` fires before each row delete so the user
can run side-effects (cleaning up associated files, fanning out
events, etc.). The default impl is empty. `MassPrunable` skips this
hook by definition — bulk deletes don't enumerate rows.

### Cascade behavior

**Pruning does NOT auto-cascade to related rows.** A `Prunable` or
`MassPrunable` impl on `User` deletes user rows; their `posts`,
`role_user` pivot entries, polymorphic `comments`, etc. are LEFT
ORPHANED with FK columns pointing at the now-deleted user.

This matches Laravel's contract: relation cleanup is the user's job.
Two clean ways to handle it:

1. **Database-level FK cascade** — declare `ON DELETE CASCADE` (or
   `ON DELETE SET NULL`) in the foreign-key constraint when you write
   the migration. The DB engine handles cascade for free, with no
   per-row Rust code.

2. **Per-row hook** — implement `Prunable::pruning(&self)` to delete
   children before the parent row is dropped. The hook fires inside
   the same logical operation as the parent delete, so consistent
   ordering is guaranteed:

   ```rust
   #[async_trait]
   impl Prunable for User {
       fn prunable() -> Builder<Self> {
           Self::query().filter_op("deleted_at", "<", thirty_days_ago())
       }

       async fn pruning(&self) -> Result<(), FrameworkError> {
           // Delete posts.
           Post::query().filter("user_id", self.id).get().await?
               .into_iter()
               .map(|p| p.delete());
           // Detach role pivots.
           self.roles().sync(Vec::<i64>::new()).await?;
           Ok(())
       }
   }
   ```

`MassPrunable` is set-based — `pruning()` does not fire. Use plain
`Prunable` whenever you need cascade. The framework will not silently
issue a per-row DELETE when you opt into `MassPrunable`; the trade-off
is documented loudly.

### Registry mechanism

Pruner registration uses the same inventory pattern as observers,
commands, and supervisors. The `#[suprnova::prunable]` attribute on
the `impl Prunable for T { ... }` block auto-registers via
`inventory::submit!` at compile time. No central config file; adding
a new prunable type is one attribute.

## Testing models

Tests instantiate a real database via `TestDatabase`, which
registers the connection in the per-test container so anything
calling `DB::connection()` inside the SUT resolves to the test DB.

### Two entry points

- **`TestDatabase::fresh::<MyMigrator>().await`** — runs every
  migration the production migrator runs. Use this for app-level
  dogfood tests where you want the test schema to exactly match
  what `suprnova migrate` produces.
- **`TestDatabase::sqlite_memory().await`** — opens an in-memory
  SQLite database WITHOUT applying any migrations. Use this for
  framework-level unit tests where you want precise column-shape
  control via per-test `db.execute_unprepared("CREATE TABLE …")`.

### App-level dogfood pattern

```rust
use app::migrations::Migrator;
use app::models::users::User;
use suprnova::testing::TestDatabase;
use suprnova::{attrs, Model};

#[tokio::test]
async fn user_lifecycle() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();

    let alice = User::create(attrs! {
        name: "Alice",
        email: "alice@example.com",
        password: "hashed",
    }).await.unwrap();

    assert!(alice.id > 0);

    alice.delete().await.unwrap();
    assert!(User::find(alice.id).await.unwrap().is_none(),
        "default scope hides soft-deleted rows");
}
```

The `_db` binding holds the `TestDatabase` for the whole test —
dropping it tears the container down and releases the in-memory
SQLite connection. Don't shadow it to `_` or the connection
disappears before the SUT runs.

### Framework-level shape pattern

```rust
use suprnova::testing::TestDatabase;
use suprnova::{attrs, model, Model};

#[model(table = "t_users", timestamps = false)]
pub struct TUser { pub id: i64, pub name: String }

#[tokio::test]
async fn shape_test() {
    let db = TestDatabase::sqlite_memory().await.unwrap();
    db.execute_unprepared(
        "CREATE TABLE t_users (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)"
    ).await.unwrap();

    let u = TUser::create(attrs! { name: "Alice" }).await.unwrap();
    assert_eq!(u.name, "Alice");
}
```

### Key patterns

- `TestDatabase::fresh::<MyMigrator>()` for app-level tests with the
  production schema. `TestDatabase::sqlite_memory()` for unit-level
  shape tests.
- Use `TestContainer::bind` (NOT `App::bind`) for any singletons
  the test mutates — global registry overrides race in parallel
  runs. The `TestDatabase` constructor handles the DB binding for
  you.
- Keep model declarations at module scope, not inside test fns.
  The macro emits an inner `mod` whose `use super::*;` only sees
  the file's top-level imports — declaring a model inside a test
  function breaks SeaORM type resolution.

## Dropping to SeaORM

Three escape hatches keep SeaORM reachable from inside the Eloquent
layer:

1. **The inner module** — `user::Entity`, `user::Column`,
   `user::ActiveModel`, `user::Model`. The macro emits these for every
   model; they're SeaORM types you can use directly. See
   [Model module layout](#model-module-layout) for the full layout and
   when to reach in.
2. **`From` conversions** — `From<user::Model> for User` and
   `From<User> for user::Model` bridge between SeaORM-shape rows
   (storage-typed columns) and Eloquent-shape rows (runtime-typed
   columns). Useful when you want to issue a SeaORM query and
   convert the result to the Eloquent shape, or vice-versa.
3. **The Suprnova-aliased SeaORM types** — every SeaORM type a
   consumer would touch is re-exported under `suprnova::*`. You
   shouldn't need `use sea_orm::*` in app code.

```rust
use suprnova::sea_orm::{ColumnTrait, EntityTrait};

// Drop to SeaORM mid-query — Eloquent doesn't have a method for
// this, but SeaORM does:
let db = suprnova::DB::connection()?;
let users = user::Entity::find()
    .filter(user::Column::Email.like("%@example.com"))
    .all(db.inner())
    .await?;

// Convert to Eloquent shape:
let eloquent: Vec<User> = users.into_iter().map(User::from).collect();
```

Three escape hatches and the From bridge means the Eloquent layer
never blocks you from reaching the underlying ORM.

## Migrating from `database::Model`

Pre-Phase-10A code may carry `impl suprnova::database::Model for
Entity {}` on a hand-rolled SeaORM entity. The trait was renamed to
`EntityExt` during Phase 10A T1 to make room for the new
`Model` trait (which sits on the user-facing struct, not the
SeaORM entity).

The recommended migration path is to switch the type to
`#[suprnova::model]`, which gives you the full Eloquent surface
plus the renamed `EntityExt` traits as a bonus. For the rare case
where you want to keep the old SeaORM-Entity-extension shape, the
`EntityExt` / `EntityExtMut` trait names are still available under
`suprnova::database::*`. They behave exactly like the old
`database::Model` did.

## DB facade — model-less queries

Some tables don't belong on a `#[suprnova::model]` struct: short-lived
audit logs, ad-hoc reporting joins, dashboard aggregates. For those,
reach for the `DB` facade. Two surfaces sit under it:

### `DB::table(name)` — chainable query builder

`DbTableBuilder` mirrors the where / order / limit shape of
`Builder<M>` but returns rows as `DynamicRow` (a typed-accessor
newtype over `serde_json::Map<String, Value>`):

```rust
use suprnova::DB;

let rows = DB::table("audit_log")
    .filter("actor_id", 42)
    .filter_op("created_at", ">=", "2026-01-01")
    .order_by_desc("id")
    .limit(50)
    .get()
    .await?;

for row in rows.iter() {
    let event: String = row.get_string("event")?;
    let actor_id: i64 = row.get_int("actor_id")?;
    println!("{actor_id}: {event}");
}
```

The full surface:

| Method | Returns | Purpose |
|--------|---------|---------|
| `.select(["id", "event"])` | `DbTableBuilder` | Restrict columns (default `*`) |
| `.filter(col, val)` | `DbTableBuilder` | `WHERE col = ?` |
| `.filter_op(col, op, val)` | `DbTableBuilder` | `WHERE col <op> ?` |
| `.order_by_asc(col) / _desc(col)` | `DbTableBuilder` | Ordering |
| `.limit(n) / .offset(n)` | `DbTableBuilder` | Window |
| `.get()` | `Collection<DynamicRow>` | All matching rows |
| `.first()` | `Option<DynamicRow>` | First row or `None` |
| `.count()` | `u64` | `SELECT COUNT(*) ...` |
| `.insert(attrs)` | `i64` | New row's `id` |
| `.update(attrs)` | `u64` | Rows affected |
| `.delete()` | `u64` | Rows affected |

**Identifier trust boundary.** Table names, column names, SQL
operators, and ORDER BY directions are interpolated into the SQL
string verbatim — they are NOT bound as parameters. Pass only
trusted, compile-time literals to these arguments. Values (the
right-hand side of `filter` / `filter_op`) ARE bound and safe to pass
through from request data.

**Empty WHERE on `update` / `delete` operates on every row.**
`DB::table("audit_log").delete().await?` truncates the table by
design — add a `filter` if you don't mean that.

**Insert backend split.** `RETURNING id` is used on Postgres and
SQLite; MySQL runs the INSERT then issues
`SELECT LAST_INSERT_ID() as id` to recover the auto-increment.

### `DynamicRow` — typed accessors over JSON map

`DynamicRow` wraps a `serde_json::Map<String, Value>` and exposes
typed getters. Each returns `Result<T, FrameworkError>` with a clear
error message on missing key or type mismatch:

```rust
let event: String     = row.get_string("event")?;
let actor_id: i64     = row.get_int("actor_id")?;
let active: bool      = row.get_bool("active")?;
let prefs: Prefs      = row.get_as("prefs")?;  // any DeserializeOwned
let raw: serde_json::Value = row.get_value("meta")?;
```

Nullable columns: use `get_optional_*`. These distinguish "column
missing" (error — schema mismatch) from "column present, value null"
(`Ok(None)`):

```rust
let score: Option<i64>      = row.get_optional_int("score")?;
let title: Option<String>   = row.get_optional_string("title")?;
```

`DynamicRow` derefs to `Map<String, Value>`, so iteration and
key-existence checks work naturally:

```rust
for (key, value) in row.iter() {
    println!("{key} = {value}");
}
```

### Raw-SQL escapes

When the builder isn't enough — window functions, recursive CTEs,
backend-specific DDL — drop to a raw string. Placeholders match the
active backend (`$1, $2, ...` for Postgres, `?` for MySQL + SQLite):

```rust
// Raw SELECT, materialised as DynamicRow.
let rows = DB::select(
    "SELECT u.name, COUNT(p.id) as post_count
     FROM users u LEFT JOIN posts p ON p.user_id = u.id
     GROUP BY u.id
     HAVING post_count > ?",
    vec![5i64.into()],
).await?;

// Raw UPDATE / DELETE — return rows-affected.
let updated = DB::update(
    "UPDATE users SET verified_at = NOW() WHERE id = ANY($1)",
    vec![ids.into()],
).await?;

let deleted = DB::delete(
    "DELETE FROM stale_sessions WHERE expires_at < ?",
    vec![now.into()],
).await?;

// Raw DDL or no-binding statements.
DB::statement("CREATE INDEX CONCURRENTLY idx_users_email ON users(email)")
    .await?;

// Generic affecting statement — for INSERT ... ON CONFLICT etc.
let rows = DB::affecting_statement(
    "INSERT INTO counters (k, n) VALUES ($1, 1) ON CONFLICT (k) DO UPDATE SET n = counters.n + 1",
    vec!["page_views".into()],
).await?;
```

Use these escape hatches sparingly — the typed builder catches more
errors at compile time and reads cleaner in business logic. But when
you need them, they're here.

**Aggregate-column gotcha.** Untyped aggregates like
`SELECT COUNT(*) AS n FROM t` work through the builder's `.count()`
helper but may be silently dropped from raw `DB::select` rows on
SQLite — the underlying `JsonValue::from_query_result` walks sqlx's
per-column type info, and a bare aggregate carries none. If you need
the raw select path with aggregates, give the expression a typed
context: either use a `CAST(... AS BIGINT)` wrapper or read the
column with a typed `DB::table(...).count()` / `.max(...)` helper
that uses `query_one` + `try_get` under the hood.
