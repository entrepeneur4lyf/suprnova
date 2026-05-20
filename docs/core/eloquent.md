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
- [Finding rows](#finding-rows)
- [Creating and updating](#creating-and-updating)
- [Deleting and soft deletes](#deleting-and-soft-deletes)
- [Query builder — dual API](#query-builder--dual-api)
- [Relationships](#relationships)
- [Eager loading](#eager-loading)
- [Mass assignment](#mass-assignment)
- [Casts](#casts)
- [Accessors and mutators](#accessors-and-mutators)
- [Timestamps](#timestamps)
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
  accessor. The model's `to_json()` calls it when `name` is listed
  in `appends = [...]`.
- `#[mutator]` on a `fn set_name(&mut self, value: serde_json::Value)`
  makes it an Eloquent mutator. The model's JSON-fill path routes
  through it when `name` is listed in `mutators = [...]`.
- `#[scope]` (Phase 10C) on a `fn(query: Builder<Self>) -> Builder<Self>`
  registers a local scope.
- `#[global_scope]` (Phase 10C) registers a global scope.
- `#[prunable]` on `impl Prunable for T { ... }` registers the
  pruner via inventory so `model:prune` finds it.

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
- **`load_missing` is collection-wide.** When any row in a collection
  already has the relation cached, `load_missing` skips the eager-load
  for the whole collection. Laravel's per-row skip is v2.
- **No nested eager loading through `MorphTo`.** The per-family enum
  erases the child type, so a dotted path like
  `with(["commentable.user"])` can't tail-recurse — the dispatcher
  returns a typed error. Resolve per-family by matching on the enum
  and calling `with(["user"])` on each variant individually.
- **`with_where`'s closure names the target type explicitly.** Rust
  can't infer the relation's target from the relation name — write
  `with_where(("posts", |q: Builder<Post>| q.filter(...)))`.

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

// Filter the eager-loaded children:
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

`with_where(("posts", |q: Builder<Post>| q.filter("published", true)))`
applies a closure to the inner `Builder<Post>` BEFORE the
`filter_in(<fk>, parent_ids)` IN-query is issued, so only matching
child rows reach the cache.

The closure's signature must name the relation's target type
explicitly (Rust can't infer it from the relation name alone). For
the polymorphic kinds, the predicate runs against the related-table
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

`load_missing(["posts"])` skips the eager-load when AT LEAST one row
in the collection already has `posts` cached. The v1 contract is
collection-wide ("does any row have it? then skip"); Laravel's
per-row skip is v2.

> **v1 semantics:** `load_missing` is collection-wide, not per-row. It checks if ANY row in the collection has the relation cached, then skips the whole load. This differs from Laravel's per-row semantics where each row is checked independently. Laravel-style per-row skip is deferred to v2.

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

When `user.to_json()` runs, the `full_name` accessor is called and
its return value is inserted into the JSON output. Calling
`user.full_name()` from Rust is just a regular method call.

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

- **Serialization (`to_json` / `to_array`)** runs accessors. Every
  field name listed in `appends = [...]` becomes a call to
  `self.<name>()`; the return value is inserted into the JSON
  output.
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
   `user::ActiveModel`. The macro emits these for every model;
   they're SeaORM types you can use directly.
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
