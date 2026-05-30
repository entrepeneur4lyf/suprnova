# Eloquent Casts, Accessors & Mutators

A cast mediates the boundary between what a column holds on disk and
what your model carries in memory. An accessor invents a virtual
attribute from the columns you already have. A mutator routes writes
to a field through your own transform. Together with auto-managed
timestamps, they are the four moving parts that turn a flat row into
a typed Rust value.

This chapter covers the full cast surface (every built-in type, the
`casts!` runtime override, encryption and hashing), the
`#[accessor]` and `#[mutator]` attribute macros, the
auto-timestamp contract including `touch()` and `without_touching`,
and the `Replicating` lifecycle event that fires when you clone a
model with `replicate()`.

For the broader model surface (`#[suprnova::model]`, query builder,
relationships, observers) see the [Eloquent API](eloquent.md) chapter.
For lifecycle events end-to-end see [Events & Listeners](events.md).
For the crypto facade the encrypted casts use see
[Encryption](encryption.md).

## How casts work

Every cast is a struct that implements the `Cast` trait:

```rust
pub trait Cast: Send + Sync {
    type Runtime;
    type Storage;

    fn to_storage(value: &Self::Runtime) -> Result<Self::Storage, FrameworkError>;
    fn from_storage(stored: &Self::Storage) -> Result<Self::Runtime, FrameworkError>;
}
```

`Runtime` is the Rust type you write in your model struct
(`bool`, `chrono::NaiveDate`, `rust_decimal::Decimal`, your own
enum). `Storage` is the type SeaORM sees on the column
(`i64` for an SQLite boolean column, `String` for a TEXT date).
Both directions are fallible — temporal and decimal parsing can
reject malformed input — so the macro propagates the `Result`
through `From<inner::Model>` and the `ActiveModel` write path.

Casts are explicit. A `Vec<String>` field does not implicitly become
`AsArray<String>` because field-type inspection at macro time would
break the moment you renamed an alias or imported a different `Vec`.
You declare casts on the macro attribute:

```rust
use suprnova::{model, AsArray, AsBool, AsJson};

#[model(
    table = "posts",
    casts = {
        tags = AsArray<String>,
        published = AsBool,
        metadata = AsJson<serde_json::Value>,
    },
)]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub tags: Vec<String>,
    pub published: bool,
    pub metadata: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
```

The macro expands each `field = CastType` entry into calls into the
`Cast::to_storage` and `Cast::from_storage` on every read and write.
You never invoke the cast yourself — you write the runtime type,
the cast wires the column shape.

### Why Suprnova diverges

Laravel declares casts as `protected $casts = ['tags' => 'array']`.
The string `'array'` resolves to a class via a runtime lookup, which
means cast names live as untyped strings until they run. Suprnova
takes the type directly — `AsArray<String>` is a real Rust type
that the macro checks at compile time. A typo in the cast name is a
compile error, not a runtime exception three weeks after deploy.

## The primitive casts

Five casts cover the SQL scalar types.

### `AsBool`

`bool` ↔ `INTEGER` (0 / 1). SQLite has no native boolean column;
Postgres and MySQL both round-trip `i64` cleanly through SeaORM's
`Value::Int` boundary. A single storage shape lets you use the same
cast against every backend.

```rust
#[model(table = "settings", casts = { dark_mode = AsBool })]
pub struct Settings {
    pub id: i64,
    pub dark_mode: bool,
}
```

### `AsInt<I>`

A narrower integer (`i32`, `u32`, `i16`) ↔ `i64`. SeaORM stores
integers as `i64` on the column; the cast narrows on read and
widens on write. Out-of-range values produce a validation error at
read time rather than silently truncating.

```rust
#[model(table = "counters", casts = { age = AsInt<u32> })]
pub struct Counter {
    pub id: i64,
    pub age: u32,
}
```

Use `AsInt<i64>` (or omit the cast) when the runtime type already
matches storage.

### `AsFloat`

`f64` ↔ `REAL`. Pass-through both directions — the cast exists for
naming parity with Laravel's `'float'` cast; backends round-trip
floats natively.

### `AsString`

`String` ↔ `TEXT`. Also pass-through; the cast exists so the
`Builder::with_casts(...)` runtime override can erase it to a
`DynCast` like every other cast.

### `AsDecimal<P>`

`rust_decimal::Decimal` ↔ `TEXT`. `P` is the precision (number of
decimal places); values are rounded to `P` places on the way to
storage. Default is `P = 4`. Storage is a fixed-format string so
round-trips are backend-agnostic — SeaORM's native `Decimal` column
type has different precision semantics on each driver, and the
string round-trip avoids that.

```rust
use rust_decimal::Decimal;
use suprnova::AsDecimal;

#[model(
    table = "ledger",
    casts = { amount = AsDecimal<2> },  // currency, 2 dp
)]
pub struct LedgerEntry {
    pub id: i64,
    pub amount: Decimal,
}
```

## The temporal casts

Six casts cover dates, datetimes, immutable variants, and Unix
timestamps. All non-timestamp casts store as `TEXT` (ISO-8601 /
RFC-3339) so the round-trip works on every driver — SQLite stores
datetimes as strings natively, and Postgres / MySQL accept them
through SeaORM's `Value::String` boundary.

### `AsDate`

`chrono::NaiveDate` ↔ `TEXT` (`YYYY-MM-DD`).

```rust
use chrono::NaiveDate;
use suprnova::AsDate;

#[model(table = "people", casts = { birthday = AsDate })]
pub struct Person {
    pub id: i64,
    pub birthday: NaiveDate,
}
```

### `AsDateTime`

`chrono::DateTime<Utc>` ↔ `TEXT` (RFC-3339). The default cast for
arbitrary timestamps when you want a wall-clock representation.

### `AsImmutableDate` and `AsImmutableDateTime`

Same storage shape as `AsDate` / `AsDateTime`. Rust's borrow checker
already enforces immutability through `&` references, so these casts
share the underlying types — they exist for parity with Laravel's
`immutable_date` / `immutable_datetime` and to document intent at
the model declaration site.

### `AsOptionalDateTime`

`Option<DateTime<Utc>>` ↔ `Option<String>`. Auto-injected by the
`#[model(soft_deletes)]` flag for the nullable tombstone column
(`deleted_at` by default — see [Soft deletes](eloquent.md#deleting-and-soft-deletes)).
The wrapped option keeps the storage column nullable so soft-deleted
vs alive rows discriminate on `IS NULL` without a sentinel value.

Use the cast directly on any other nullable datetime column you want
to round-trip as RFC-3339 text:

```rust
#[model(
    table = "subscriptions",
    casts = { cancelled_at = AsOptionalDateTime },
)]
pub struct Subscription {
    pub id: i64,
    pub cancelled_at: Option<chrono::DateTime<chrono::Utc>>,
}
```

### `AsTimestamp`

Unix-epoch `i64` ↔ `INTEGER`. Use when the column is queried as a
numeric range or used in arithmetic. Distinct from `AsDateTime` —
pick `AsTimestamp` when you want `WHERE created_unix > 1700000000`
and `AsDateTime` when you want RFC-3339 strings in your logs.

## The structured casts

Five casts cover collections, structs, and arbitrary JSON. All
serialise the runtime value to JSON text and store it in a `TEXT`
column. Postgres native `JSON` / `JSONB` and MySQL `JSON` columns
accept the same string payload — if you want a native JSON column
type for indexing, declare it manually in a migration; the cast
layer doesn't constrain the column type.

### `AsArray<T>`

`Vec<T>` ↔ JSON-encoded `TEXT`. Element type must be
`Serialize + DeserializeOwned`.

```rust
use suprnova::AsArray;

#[model(table = "posts", casts = { tags = AsArray<String> })]
pub struct Post {
    pub id: i64,
    pub tags: Vec<String>,
}
```

### `AsObject<T>`

A `Serialize + DeserializeOwned` struct ↔ JSON-encoded `TEXT`. Use
when the runtime shape is a fixed record with statically-known keys.

```rust
use serde::{Deserialize, Serialize};
use suprnova::AsObject;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prefs {
    pub theme: String,
    pub notifications: bool,
}

#[model(table = "users", casts = { prefs = AsObject<Prefs> })]
pub struct User {
    pub id: i64,
    pub prefs: Prefs,
}
```

### `AsCollection<T>`

`Collection<T>` ↔ JSON-encoded `TEXT`. Thin wrapper over `AsArray`
that round-trips through Suprnova's `Collection<T>` (a `Vec<T>`
newtype with the Laravel-style slice surface — see
[Collections](eloquent.md#collections)).

### `AsJson<T>`

Any `Serialize + DeserializeOwned` type ↔ JSON-encoded `TEXT`. Use
when the field is a `serde_json::Value` or a user-defined struct
that's already fully describable in serde terms but doesn't fit the
fixed-shape `AsObject` pattern (e.g. enum payloads, untyped maps).

### `AsArrayObject<T>`

`IndexMap<String, T>` ↔ JSON-encoded `TEXT`. Use when the runtime
shape is a dynamic-key map and the order of keys matters (the UI
ordering of labels, the canonical order of a config block). `IndexMap`
over `HashMap` is intentional: serde preserves insertion order
through `IndexMap`, and Suprnova's `serde_json` is already configured
with `preserve_order` for the same reason.

For fixed-shape records use `AsObject`; for arrays use `AsArray`.

## The enum cast

### `AsEnum<E>`

`E: FromStr + AsRef<str>` ↔ `TEXT`. The enum's variant name (or its
`AsRefStr`-customised string) is what hits the column. There is no
framework lock-in on `strum`, but it's the most ergonomic way to get
the two bounds without hand-rolling them:

```rust
use suprnova::AsEnum;

#[derive(Debug, Clone, Copy, strum::EnumString, strum::AsRefStr)]
pub enum Role {
    Admin,
    Editor,
    Viewer,
}

#[model(
    table = "users",
    casts = { role = AsEnum<Role> },
)]
pub struct User {
    pub id: i64,
    pub role: Role,
}
```

Integer-discriminant storage is intentionally not the default. A
`Role::Admin = 0` that later becomes `Role::Admin = 2` after a
re-order would silently swap every admin in the database. Variant
names are self-describing in a DB browser and stable across
re-orders.

## Encryption and hashing

Five casts mediate cryptographic transforms on the storage boundary.
All four `AsEncrypted*` casts share the [`Crypt`](encryption.md)
facade — the facade must be initialised before any of them run.
Production apps get this through `Server::from_config` (which reads
`APP_KEY` from the environment); tests call
`suprnova::testing::install_test_encryption_key()` once at startup.

### `AsEncrypted`

`String` ↔ AES-256-GCM-encrypted `String`. The on-disk column holds
URL-safe base64 of `nonce || ciphertext_with_tag`. Each write uses a
fresh random nonce, so two writes of the same plaintext produce
distinct ciphertexts — your DB admin cannot identify duplicate
secrets at rest.

```rust
use suprnova::AsEncrypted;

#[model(
    table = "secrets",
    casts = { api_key = AsEncrypted },
)]
pub struct Secret {
    pub id: i64,
    pub api_key: String,  // runtime is plain UTF-8
}
```

The runtime value is the decrypted UTF-8 string; you read and write
it like any other `String`.

### `AsEncryptedArray<T>` / `AsEncryptedObject<T>` / `AsEncryptedCollection<T>`

`Vec<T>` / `T` / `Collection<T>` ↔ AES-256-GCM-encrypted JSON. Pipeline
is: serialise to JSON → encrypt → base64 → store; reverse on read.
Element / value type must be `Serialize + DeserializeOwned`.

```rust
use suprnova::AsEncryptedObject;
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize)]
pub struct CardOnFile {
    pub last4: String,
    pub exp_month: u8,
    pub exp_year: u16,
}

#[model(
    table = "billing",
    casts = { card = AsEncryptedObject<CardOnFile> },
)]
pub struct Billing {
    pub id: i64,
    pub card: CardOnFile,
}
```

### Key rotation

The `Crypt` facade supports rotation through `APP_KEY_PREVIOUS`:
encryption always uses `APP_KEY`, but decryption tries `APP_KEY`
first and falls back to `APP_KEY_PREVIOUS` if the primary key fails.
A rolling re-encryption strategy is: set `APP_KEY` to the new key,
move the old key to `APP_KEY_PREVIOUS`, then `save()` every encrypted
row to rewrite ciphertexts under the new key. The cast layer does
not have to know about rotation — it round-trips through `Crypt` on
every read and write, so a `User::all().await?` followed by saving
each row migrates the column in place. See [Encryption](encryption.md)
for the full rotation protocol.

### `AsHashed`

`String` ↔ a hashed string on write, using the active hash driver
(`HASH_DRIVER` env var — bcrypt by default, argon2i and argon2id
also supported). The runtime value IS the hashed string; there is
no reverse direction. Mirrors Laravel's `hashed` cast.

```rust
use suprnova::AsHashed;

#[model(
    table = "users",
    casts = { password = AsHashed },
)]
pub struct User {
    pub id: i64,
    pub password: String,
}
```

`AsHashed::to_storage` is **idempotent**: a value that already looks
like ANY recognised hash (bcrypt `$2*$`, argon2i / argon2id PHC)
passes through unchanged. Without this guard,
`User::find(id).await?.save().await?` would re-hash the existing
hash into a hash-of-hash, breaking `Hash::check(plain, stored)` and
invalidating every existing password.

Pair `AsHashed` with the `#[mutator]` pattern (below) when you need
to apply more than a hash on write — e.g. normalise whitespace or
reject blank passwords before hashing.

## Runtime cast override — `casts!` macro

The casts declared in `#[model(casts = { ... })]` are static — they
fire on every read of that model. When you need a different cast on a
single query (a debug tool wants the raw stored shape, an export
script wants a different JSON representation), use
`Builder::with_casts(...)`:

```rust
use suprnova::{casts, AsDate, AsJson, User};

let map = casts! {
    birthday = AsDate,
    metadata = AsJson<serde_json::Value>,
};
let rows = User::query().with_casts(map).get().await?;
```

The `casts!` macro builds a `HashMap<&'static str, Arc<dyn DynCast>>`.
Each entry is `field_name = CastType`; every built-in cast implements
`IntoDynCast`, so the type-erased `DynCast` shadow is automatic. The
runtime-override map only applies for the duration of the chained
query — the model's static cast pipeline is unchanged.

Use this surface sparingly. The model attribute is the right place
for the casts you want every read to apply; the runtime override is
the escape hatch for one-off queries.

## Accessors — virtual attributes from real columns

An accessor is an `impl` method on the model annotated with the
`#[accessor]` macro. When you list the method's name in
`#[model(appends = [...])]`, the model's `to_json()` calls the
method and inserts the result under that key.

```rust
use suprnova::{accessor, model, Model};

#[model(
    table = "users",
    appends = ["full_name"],
)]
pub struct User {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
}

impl User {
    #[accessor]
    pub fn full_name(&self) -> String {
        format!("{} {}", self.first_name, self.last_name)
    }
}
```

A `serde_json::to_value(&user)` (or `user.to_json()`) now contains:

```json
{
  "id": 1,
  "first_name": "Alice",
  "last_name": "Xu",
  "full_name": "Alice Xu"
}
```

The method is also callable directly (`user.full_name()`) — the
`#[accessor]` macro is mostly a marker so the struct-level
`#[suprnova::model]` macro can wire the `to_json()` dispatch. There
is no cost to calling it from your own code.

Each name in `appends` must match a real `#[accessor]` method by
identifier. A typo (`appends = ["fullName"]` when the method is
`full_name`) is caught at compile time with a pointed error message.

### Returning non-`String` values

Accessors can return any `Serialize` type. The macro converts the
returned value through `serde_json::to_value` before insertion, so:

```rust
impl Post {
    #[accessor]
    pub fn word_count(&self) -> usize {
        self.body.split_whitespace().count()
    }
}
```

renders as `"word_count": 42` in the JSON output.

### Hiding the source columns

When the accessor's value is what the consumer should see and the
underlying columns are noise, pair `appends` with `hidden`:

```rust
#[model(
    table = "users",
    appends = ["full_name"],
    hidden = ["first_name", "last_name"],
)]
```

`hidden` strips the named columns from the serialised output;
`appends` then inserts the accessor's value. The order is fixed —
filters run first, accessor injection runs after. See
[Hidden, visible, and appends](eloquent.md#mass-assignment) for the
complete surface.

## Mutators — routed writes through your transform

A mutator is the write-side counterpart. When the field's name appears
in `#[model(mutators = [...])]`, every fill / create / update path
routes the value through `self.set_<field>(value)?` instead of
assigning the field directly.

```rust
use serde_json::Value;
use suprnova::{model, mutator, FrameworkError, Model};

#[model(
    table = "users",
    fillable = ["password"],
    mutators = ["password"],
)]
pub struct User {
    pub id: i64,
    pub password: String,
}

impl User {
    #[mutator]
    pub fn set_password(&mut self, value: Value) -> Result<(), FrameworkError> {
        let raw: String = serde_json::from_value(value).map_err(|e| {
            FrameworkError::validation("password", format!("{e}"))
        })?;
        // Normalise + hash; AsHashed would do the hash on its own,
        // but the mutator is where you can also enforce policy.
        let trimmed = raw.trim().to_string();
        if trimmed.len() < 12 {
            return Err(FrameworkError::validation(
                "password",
                "must be at least 12 characters",
            ));
        }
        self.password = suprnova::hashing::hash(&trimmed)?;
        Ok(())
    }
}
```

`set_password` receives a `serde_json::Value`. The body owns the
deserialise + transform — the field type on the struct can stay
`String`, and your validation runs before the column is touched.
A returned error propagates through `create()` / `update()` /
`fill()` as a `bad_request`.

Direct field assignment bypasses the mutator:

```rust
user.password = "raw".to_string();  // skips set_password
user.save().await?;                 // saves "raw"
```

This matches Laravel's `$user->password = ...` vs `$user->fill(...)`
behaviour. When you want the mutator to be the only path, route
all writes through `attrs!` + `fill` / `create` / `update`.

### Combining mutators with casts

A mutator and a cast can coexist on the same field; the mutator runs
on the write path (when fill / create / update is called), the cast
runs on the read path (when the column is materialised from a SELECT).
A common pattern is to use `AsHashed` for the read-side idempotence
guarantee and the mutator for write-side validation — the mutator
hashes, `AsHashed` sees an already-hashed value and passes through.

## Auto-managed timestamps

When a model carries both `created_at` and `updated_at` fields
(typed `chrono::DateTime<chrono::Utc>`), the macro:

- Sets both to `Utc::now()` on `create()`.
- Bumps `updated_at` on every `save()` and `update(attrs)`.
- Emits an `impl Touchable for YourStruct` so you can call
  `.touch().await` to bump `updated_at` without changing any other
  column.

```rust
use chrono::{DateTime, Utc};
use suprnova::{model, Model, Touchable};

#[model(table = "posts")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// Bump updated_at without other changes:
let post = Post::find_or_fail(1).await?;
post.touch().await?;
```

Storage uses the `AsDateTime` cast that the macro auto-injects for
timestamp columns. The cast lets the same `DateTime<Utc>` value
round-trip across all three SeaORM drivers (SQLite, MySQL,
PostgreSQL) without forcing you to pick a database-specific
timestamp type.

### Opt-out and custom column names

`#[model(timestamps = false)]` disables the auto-management entirely
— you control the timestamps yourself.

`#[model(created_at = "creado_en", updated_at = "actualizado_en")]`
keeps the auto-management but renames the columns. The macro
detects the renamed fields and wires the same logic against them.

When the struct has only ONE of the two timestamp fields, the macro
emits a `compile_error!` — almost always a typo (`craeted_at`)
that you want surfaced loudly rather than silently swallowed.

### `without_touching` — task-scoped suppression

Sometimes you want to update a row without bumping `updated_at` —
running a backfill, fixing a typo, recording an internal sync that
shouldn't reset cache TTLs keyed on `updated_at`. Wrap the work in
`without_touching`:

```rust
use suprnova::eloquent::without_touching;

without_touching(async {
    for post in Post::query().get().await? {
        post.touch().await?;  // no-op inside the scope
    }
    Ok::<_, suprnova::FrameworkError>(())
}).await?;
```

The flag is a `tokio::task_local!` so it doesn't leak across
`tokio::spawn` boundaries — concurrent requests on other tasks
continue to honour their own scope (or its absence). This is the
Suprnova analogue of Laravel's `Model::withoutTouching(closure)`.

### Why Suprnova diverges

Laravel uses a static `$timestamps = false` property and a global
`Model::withoutTouching` static method backed by an instance counter.
Both approaches assume request-per-process isolation. Suprnova runs
many requests on one Tokio runtime, so a process-global flag would
let one request silently suppress timestamps on another. The
`tokio::task_local!` scope is async-aware: it follows futures
across `.await` points within the same task and goes out of scope
when the future drops, no matter how the request ends.

## The `Replicating` lifecycle event

Of the 16 model lifecycle events (see [Observers and lifecycle
events](eloquent.md#observers-and-lifecycle-events)), `Replicating`
is the one that fires when you clone an existing row into an unsaved
in-memory copy via `replicate()`:

```rust
let original = Post::find_or_fail(1).await?;
let copy = original.replicate().await?;  // unsaved
copy.title = format!("{} (copy)", original.title);
copy.save().await?;  // now persisted with a new PK
```

The `Replicating` event fires AFTER the in-memory clone is built but
BEFORE you've had a chance to mutate it. Listeners receive
`(&Self, Arc<Mutex<Self>>)` — the original and the freshly-built
replica behind a `Mutex`, so you can mutate the replica from the
listener before the user sees it:

```rust
use suprnova::{Listener, FrameworkError};

pub struct ResetReplicatedFlags;

#[async_trait::async_trait]
impl Listener<post::events::Replicating> for ResetReplicatedFlags {
    async fn handle(&self, event: &post::events::Replicating) -> Result<(), FrameworkError> {
        let mut replica = event.replica.lock().await;
        replica.published = false;       // copies start unpublished
        replica.view_count = 0;          // counters reset
        Ok(())
    }
}
```

The replica's PK is already cleared by the time the listener runs
— `replicate()` calls `reset_primary_key()` before firing the
event, so you can't accidentally re-save under the original ID.
Timestamps are also reset; `created_at` / `updated_at` fire on the
subsequent `save()` like any new row.

### `replicate_into<T>` — cross-type replication

When the replica is a different type (`Post` → `Draft`, say), use
`replicate_into::<Draft>()`. The `Replicating` event does NOT fire on
this path because the event struct is per-source-type and a listener
registered for `post::events::Replicating` would receive an
`Arc<Mutex<Post>>`, not an `Arc<Mutex<Draft>>`. The cross-type path
is for when you want a fresh target type without observer interference;
register a normal `Creating` listener on the target type if you want a
hook at construction.

See [Replication](eloquent.md#replication) for the rest of the
replicate surface (`replicate_except`, the replica's relation
handling, the rules for nullable PKs).

## Putting it together

A model with every surface from this chapter:

```rust
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use suprnova::{
    accessor, hashing, model, mutator, AsBool, AsDateTime,
    AsDecimal, AsEncryptedObject, AsEnum, AsHashed, AsJson,
    AsOptionalDateTime, FrameworkError, Model,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardOnFile {
    pub last4: String,
    pub exp_month: u8,
    pub exp_year: u16,
}

#[derive(Debug, Clone, Copy, strum::EnumString, strum::AsRefStr)]
pub enum Role {
    Admin,
    Editor,
    Viewer,
}

#[model(
    table = "users",
    soft_deletes,
    appends = ["display_name"],
    hidden = ["password", "card"],
    fillable = ["name", "email", "password", "role", "credit"],
    mutators = ["password"],
    casts = {
        role = AsEnum<Role>,
        verified = AsBool,
        credit = AsDecimal<2>,
        card = AsEncryptedObject<CardOnFile>,
        metadata = AsJson<serde_json::Value>,
        password = AsHashed,
        last_login_at = AsOptionalDateTime,
    },
)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub password: String,
    pub role: Role,
    pub verified: bool,
    pub credit: Decimal,
    pub card: CardOnFile,
    pub metadata: serde_json::Value,
    pub last_login_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    // deleted_at is auto-injected by soft_deletes (AsOptionalDateTime)
}

impl User {
    #[accessor]
    pub fn display_name(&self) -> String {
        if self.name.is_empty() { self.email.clone() } else { self.name.clone() }
    }

    #[mutator]
    pub fn set_password(&mut self, value: Value) -> Result<(), FrameworkError> {
        let raw: String = serde_json::from_value(value).map_err(|e| {
            FrameworkError::validation("password", format!("{e}"))
        })?;
        let trimmed = raw.trim().to_string();
        if trimmed.len() < 12 {
            return Err(FrameworkError::validation(
                "password",
                "must be at least 12 characters",
            ));
        }
        // The mutator hashes; AsHashed sees an already-hashed value
        // on subsequent saves and passes through unchanged.
        self.password = hashing::hash(&trimmed)?;
        Ok(())
    }
}
```

This single declaration gives you:

- Eight typed casts wiring the storage / runtime boundary.
- An accessor that synthesises `display_name` from existing columns.
- A mutator that validates and hashes the password.
- Auto-managed `created_at` / `updated_at`.
- Soft deletes with an auto-injected `deleted_at` column.
- Encrypted card-on-file storage with key-rotation support.

Every cast is checked at compile time. The dual-API query builder
(see [Eloquent — query builder](eloquent.md#query-builder--dual-api))
runs against the typed columns; serialisation to Inertia / JSON
applies the hidden / appends rules; and a `User::find(id).await?`
materialises the row through eight `Cast::from_storage` calls
without you writing a single line of conversion code.

## Next

- [Eloquent API](eloquent.md) — the rest of the model surface: query
  builder, relationships, observers, pagination, transactions.
- [Encryption](encryption.md) — the `Crypt` facade the encrypted
  casts share, key rotation protocol, and the wider crypto surface.
- [Events & Listeners](events.md) — the dispatcher behind
  `Replicating` and the other 15 model lifecycle events.
- [Authentication](authentication.md) — the `Authenticatable` trait
  and where `AsHashed` fits into the password flow.
- [Validation](validation.md) — `FrameworkError::validation` and the
  pattern mutators use to surface per-field errors.
