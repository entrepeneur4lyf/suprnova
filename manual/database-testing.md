# Database Tests

The DB-specific companion to [Testing](testing.md). Where that chapter
covers the test harness — `#[suprnova_test]`, `describe!` / `test!`,
`expect!`, and the in-process fakes — this one covers what changes
when your test needs a database: how `TestDatabase` builds one for
you, how isolation actually works, where factories and seeders plug
in, and when an in-memory SQLite is and isn't enough.

## The two constructors

Every database test starts by building a `TestDatabase`. Two
constructors, two intents.

### `TestDatabase::fresh::<Migrator>()`

Builds an in-memory SQLite database, runs your migrator end-to-end,
and registers the connection in the test container so any code
calling `DB::connection()` or `App::resolve::<DbConnection>()`
resolves to it. This is the right default for everything that touches
real schema.

```rust
use suprnova::testing::TestDatabase;
use crate::migrations::Migrator;

#[tokio::test]
async fn user_lifecycle_end_to_end() {
    let db = TestDatabase::fresh::<Migrator>().await.unwrap();

    let alice = User::create(attrs! {
        name: "Alice", email: "alice@example.com",
    })
    .await
    .unwrap();

    assert!(alice.id > 0);
    // Query directly when you want to bypass the model surface:
    let row = users::Entity::find_by_id(alice.id)
        .one(db.conn())
        .await
        .unwrap();
    assert!(row.is_some());
}
```

`Migrator` is your application's `MigratorTrait` implementation —
the same type the production `suprnova migrate` command runs. By
threading the real migrator through the test schema you make schema
drift impossible: a column the migrator forgot to add cannot be
silently present in the test DB.

The `test_database!()` macro is sugar for the common case (`crate::migrations::Migrator`):

```rust
use suprnova::test_database;

#[tokio::test]
async fn shortcut() {
    let db = test_database!();          // == TestDatabase::fresh::<crate::migrations::Migrator>()
    // ...
}

// Or with a custom migrator path:
let db = test_database!(my_crate::CustomMigrator);
```

### `TestDatabase::sqlite_memory()`

Same container and registry wiring, but **does not run any
migrator**. Use this when the test wants precise column-shape
control — typically cast round-trips, query-builder SQL surface
tests, or driver-level edge cases where a full migrator is overkill
or noise:

```rust
let db = TestDatabase::sqlite_memory().await.unwrap();
db.execute_unprepared(
    "CREATE TABLE casts_t (id INTEGER PRIMARY KEY, payload BLOB)",
)
.await
.unwrap();

// Then write directly and read back with the typed helpers:
let row = db.fetch_one(
    "INSERT INTO casts_t (payload) VALUES (?) RETURNING id, payload",
    vec![sea_orm::Value::Bytes(Some(Box::new(b"hello".to_vec())))],
).await.unwrap();
```

`sqlite_memory()` is the foundation `fresh()` is built on — `fresh`
calls it and then runs your migrator. Anything you can do with
`fresh` you can do here; you just bring your own DDL.

### `execute_unprepared`, `fetch_one`, `fetch_all`

`TestDatabase` re-exports the three SeaORM execution shapes you reach
for most in tests, so test files don't have to pull in
`ConnectionTrait`:

| Method | Use for |
| --- | --- |
| `execute_unprepared(sql)` | DDL or DML with no placeholders. Returns `Result<(), FrameworkError>` |
| `fetch_one(sql, bindings)` | One-row SELECT. Errors if zero rows |
| `fetch_all(sql, bindings)` | All-row SELECT |

The bindings are `Vec<sea_orm::Value>` — the same shape the
production query path uses. The connection's backend (SQLite for
both constructors) is supplied for you, so a `?` placeholder is
correct.

## How isolation actually works

The fresh-database-per-test model is the isolation mechanism. Each
call to `fresh()` or `sqlite_memory()` opens a new `sqlite::memory:`
connection, which under SQLite is an entirely separate database
instance — no shared schema, no shared rows, no other test can see
into it. There is no transaction wrapper, no `RefreshDatabase` trait
to opt into and no rollback to remember: the *next* test gets a
clean empty DB because it builds its own.

When the `TestDatabase` value drops, three things happen, in this
order:

1. The held `TestContainerGuard` clears the thread-local test
   container, so any subsequent `App::get::<DbConnection>()` no longer
   finds the test connection.
2. If this was the *last* live `TestContainerGuard` in the process,
   the named [`ConnectionRegistry`](database.md#named-connections)
   is wiped. (A refcount over `FAKE_GUARDS` guarantees an inner
   test's drop cannot erase a connection name a concurrent outer
   test still depends on — the standing trap that prompted the
   refcount.)
3. The SQLite connection itself drops, which destroys the in-memory
   database.

Because state is rebuilt rather than rolled back, the isolation is
stronger than `BEGIN`/`ROLLBACK` wrapping: there is no committed
state to mistakenly survive, no nested transaction quirks, no
sequence-counter drift between tests. The cost is that you pay for
running the migrator once per test (negligible for SQLite with most
schemas; if it becomes a real cost, see "Sharing a migrated database
across tests" below).

## Why the pool is pinned to one connection

Both constructors build the database with `max_connections(1)` and
`min_connections(1)`. This is load-bearing for `sqlite::memory:`,
not a generic policy.

`sqlite::memory:` is a per-connection database — each *new*
connection in the pool would be a separate, empty SQLite instance.
A pool of size 2 would mean half your queries see the migrated
database and half see an empty one. Pinning the pool to one
connection makes every query in the test land on the same in-memory
database that the migrator ran against.

The consequence: a test that exercises true connection concurrency
(two transactions racing, replica routing, a queue worker hitting
the DB while a request handler does) needs a real database. See
"When SQLite in-memory isn't enough" below.

## Factories in tests

Factories produce randomized model instances and (optionally) persist
them. The persistence path resolves the bound test connection
automatically — there is no factory-side wiring for tests.

```rust
use crate::factories::UserFactory;

#[tokio::test]
async fn factory_round_trip() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();

    // In-memory only: fastest, no DB round trip.
    let alice = UserFactory::new()
        .with(|u| u.email = "alice@example.com".into())
        .make();
    assert_eq!(alice.email, "alice@example.com");

    // Persist one + return the post-insert model (id assigned).
    let bob = UserFactory::new().create().await.unwrap();
    assert!(bob.id > 0);

    // Bulk: persist 50 in sequence.
    let many = UserFactory::times(50).create_many().await.unwrap();
    assert_eq!(many.len(), 50);
}
```

Two patterns worth knowing:

**Factory inserts bypass model events.** The `Persistable` impl that
backs `create()` / `create_many()` writes through SeaORM's
`ActiveModelTrait::insert` directly — it does *not* go through the
`Model::create` surface that dispatches `Creating` / `Created` /
`Saving` / `Saved`. A test that asserts "no observer fires while we
build the fixture" needs nothing special; a test that asserts "the
`Created` observer DID fire" must drive `Model::create(...)` (or
`save()`) instead of a factory.

**`create_many` does not transact.** Inserts are sequential. If a
later row fails the prior rows are not rolled back. Wrap the call
in your own `DB::transaction` if a test requires atomicity:

```rust
DB::transaction(|tx| async move {
    UserFactory::times(50).create_many().await?;
    PostFactory::times(200).create_many().await?;
    Ok::<_, FrameworkError>(())
}).await.unwrap();
```

See [Eloquent → Factories](eloquent-factories.md) for the full
factory surface (states, sequences, `with`-relations, `count`,
`times`, `make_one` / `create_one`).

## Seeders in tests

Seeders are functions you've registered with the framework's
seeder registry under a stable name. Two patterns for driving them
from tests, one for each axis of intent.

### Run a single seeder by name

```rust
use suprnova::seed;
use my_app::seeders::UsersSeeder;

#[tokio::test]
async fn users_seeder_populates_fixtures() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();

    seed::register::<UsersSeeder>();
    seed::run_one("UsersSeeder").await.unwrap();

    let count = User::query().count().await.unwrap();
    assert!(count > 0);
}
```

### Run the full bootstrap seeder set

```rust
use serial_test::serial;
use suprnova::seed;

#[tokio::test]
#[serial]
async fn full_seed_lands_expected_row_counts() {
    seed::clear();                              // start from a known-empty registry
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();

    seed::register::<my_app::seeders::UsersSeeder>();
    seed::register::<my_app::seeders::PostsSeeder>();
    seed::run_all().await.unwrap();

    let users = User::query().count().await.unwrap();
    let posts = Post::query().count().await.unwrap();
    assert_eq!(users, 50);
    assert_eq!(posts, 200);

    seed::clear();
}
```

Two important contract details:

**The seeder registry is process-global.** `seed::register::<S>()`
inserts into a `RwLock<IndexMap>` keyed by `S::name()`. A test that
mutates the registry should call `seed::clear()` at entry, register
the seeders it needs, run, and `clear()` again at exit — and the
test itself should be `#[serial_test::serial]` so two parallel tests
don't fight over the registry. `#[suprnova_test]` does **not** auto-
register seeders; only the explicit `seed::register::<>()` call in
your own `bootstrap.rs` or in the test body puts them in the
registry.

**Model-driven seeds vs factory-driven seeds.** A seeder that loops
`User::create(...)` in a `for` fires `Creating` / `Saving` /
`Created` / `Saved` per row and invokes every registered observer.
For bulk seeding where that fanout is unwanted, wrap the loop in
`seed::without_events`:

```rust
seed::without_events(async {
    for i in 0..50 {
        User::create(attrs! { name: format!("user{i}"), email: format!("user{i}@example.com") }).await?;
    }
    Ok::<_, FrameworkError>(())
}).await?;
```

The mute is **task-scoped** — only the work performed inside the
future is silenced; concurrent request handlers and queue workers
continue to fire events normally. Factories (`create_many`) already
bypass the event path, so `without_events` is unnecessary around
them.

See [Seeding](seeding.md) for the seeder authoring surface and
[Eloquent → Factories](eloquent-factories.md) for the relationship
between the two.

## Parallel-safe database tests

`cargo test` runs tests in parallel by thread. The default
`#[suprnova_test]` expansion (which is `#[tokio::test]`, i.e. a
`current_thread` runtime per test) interacts safely with this for
two reasons:

- **Each test gets its own `sqlite::memory:` connection.** Tests do
  not share DB state.
- **The bound connection lives in the thread-local
  `TestContainer`.** Tests do not share container bindings.

What you don't have to think about: `DB::connection()`, `App::resolve`,
factory persistence, model trait writes — these all transparently
land on the right per-test database.

What you *do* need to think about:

| Surface | Why it's process-global | Mitigation |
| --- | --- | --- |
| `ConnectionRegistry` (`DB::register_named`, `__read_replica__`) | Single `RwLock<HashMap>` shared by the process | `#[serial_test::serial]` for any test that registers or reads named connections |
| The seeder registry | Single `RwLock<IndexMap>` | `#[serial_test::serial]` + `seed::clear()` at entry and exit |
| The Eloquent observer / scope registries | Keyed by `TypeId::<M>()` | Each test should use a unique model struct, or be `#[serial]` and call the registry's `clear()` helper |
| The named query log (`DB::enable_query_log`) | Single process-global ring buffer | `#[serial]` if assertions read the log |

The connection-registry refcount makes this safer than it sounds: a
test holding a `TestContainerGuard` keeps the registry alive even
when a *sibling* test's guard drops. You still want `#[serial]` for
the tests that actually mutate the registry, so their reads and
writes can't interleave.

### Multi-thread runtime caveat

`#[suprnova_test]` expands to `#[tokio::test]` with the default
`current_thread` runtime, so the thread-local container path always
works. If you explicitly opt a test into the multi-thread runtime:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_io_test() {
    let _db = TestDatabase::fresh::<Migrator>().await.unwrap();
    // PROBLEM: tasks spawned with `tokio::spawn` may run on a
    // worker thread different from the one that built the
    // TestDatabase. They will not see the thread-local
    // TestContainer binding, and DB::connection() will return the
    // global (production) container's value or error.
}
```

Two fixes, depending on what the test does:

1. **Direct connection access** — `db.conn()` still returns the
   right `&DatabaseConnection` regardless of which worker thread
   reads it. If the test only ever talks to the DB through the
   `db` handle (not through `DB::connection()`), the multi-thread
   runtime is fine.

2. **`TestContainer::scope`** — wrap the test body in
   `TestContainer::scope(async { ... }).await` and bind your fakes
   (and the DB connection) inside it. The scope binds the container
   to the task-local layer, which is preserved across awaits even
   when the runtime hops the future between worker threads. For
   spawned sub-tasks, use `TestContainer::spawn` (not bare
   `tokio::spawn`) so the task-local container is captured and
   reinstalled inside the spawned future.

See [Service Container → Lookup order](container.md) for the full
task-local / thread-local / global layering.

## SQLite in-memory vs a real Postgres / MySQL / MariaDB

`TestDatabase` is intentionally SQLite-only. The driver is hardcoded
to `sqlite::memory:`; there is no `TestDatabase::postgres()`,
`fresh_with_url()`, or env-driven variant. For the overwhelming
majority of test surface — model CRUD, query builder shape, cast
round-trips, relationship loading, observer firing order, soft-delete
semantics — SQLite in-memory is the right tool: zero setup, zero
network, milliseconds per test, perfect isolation, no external
service to keep alive in CI.

There are four cases where SQLite in-memory isn't enough:

1. **Driver-specific SQL.** A query that uses Postgres `LATERAL`,
   `JSONB` operators, `ON CONFLICT ... WHERE`, MySQL window
   functions, or any other dialect-specific surface won't run on
   SQLite. The model+builder path tries to stay generic, but a
   raw-SQL test asserting Postgres-shaped output needs Postgres.
2. **Concurrency under real connection contention.** SQLite
   in-memory is single-connection (see "Why the pool is pinned to
   one connection"). Tests that race two transactions, exercise
   read-replica routing under load, or measure deadlock retry need
   a multi-connection server.
3. **Vector / NoSQL / temporal surfaces.** Suprnova's MariaDB
   `VECTOR` driver, Qdrant integration, Pinecone integration, and
   similar non-SQL drivers cannot be modelled in SQLite at all.
4. **Production parity smoke tests.** A handful of "does this
   actually work on the real DB we deploy to?" tests, gated to
   CI, are worth keeping even when the unit-test layer is SQLite.

For all four cases the pattern is the same: step outside
`TestDatabase` entirely, build a `DbConnection` against an
operator-supplied `DATABASE_URL`-style env var, env-gate the test
so it skips when the var is absent, and mark it `#[serial]` so two
of them don't fight over the shared real database. The
`MARIADB_URL` pattern in `framework/tests/vector_mariadb.rs` is the
canonical example:

```rust
use serial_test::serial;
use suprnova::database::{DatabaseConfig, DbConnection};

async fn maybe_real_db(test_name: &str) -> Option<DbConnection> {
    let url = match std::env::var("POSTGRES_TEST_URL") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            eprintln!("[{test_name}] skipping: POSTGRES_TEST_URL not set");
            return None;
        }
    };
    let config = DatabaseConfig::builder().url(&url).build();
    Some(DbConnection::connect(&config).await.expect("real DB connects"))
}

#[tokio::test]
#[serial]
async fn jsonb_operator_works_against_postgres() {
    let Some(conn) = maybe_real_db("jsonb_operator_works_against_postgres").await else {
        return;
    };
    // Drive Postgres-specific SQL directly against `conn`.
}
```

The standing convention: name the env var after the target driver
(`POSTGRES_TEST_URL`, `MYSQL_TEST_URL`, `MARIADB_URL`), print a
skip line so a developer running the suite locally sees the test
was skipped (not silently passed), and document the env var in the
test module's leading doc-comment so CI can wire it up.

## A worked example

The full app dogfood pattern, combining everything in this chapter:

```rust
use app::migrations::Migrator;
use app::models::posts::Post;
use app::models::users::User;
use serial_test::serial;
use suprnova::testing::TestDatabase;
use suprnova::{Model, attrs, seed, FrameworkError};

#[tokio::test]
#[serial]
async fn users_and_posts_full_seed_round_trip() {
    // 1. Empty seeder registry.
    seed::clear();

    // 2. Fresh in-memory DB with the app's migrator.
    let db = TestDatabase::fresh::<Migrator>().await.unwrap();

    // 3. Register the seeders the test cares about.
    seed::register::<app::seeders::UsersSeeder>();
    seed::register::<app::seeders::PostsSeeder>();

    // 4. Drive the seed inside without_events so observer fanout
    //    doesn't try to enqueue jobs (no queue is running here).
    seed::without_events(async {
        seed::run_all().await
    }).await.unwrap();

    // 5. Read back via the model surface and the raw connection.
    let user_count = User::query().count().await.unwrap();
    assert_eq!(user_count, 50);

    let raw_post_count = db.fetch_one(
        "SELECT COUNT(*) AS n FROM posts",
        vec![],
    ).await.unwrap();
    let n: i64 = raw_post_count.try_get("", "n").unwrap();
    assert_eq!(n, 200);

    // 6. Exercise the cancellable observer path on a fresh model.
    let alice = User::create(attrs! {
        name: "Alice", email: "alice@example.com",
    }).await.unwrap();
    assert!(alice.id > 0);

    seed::clear();
}
```

Step 5 is the part that proves the wiring: the model query and the
raw `fetch_one` are both reading the same in-memory database — the
model surface because the `DB::connection()` lookup found the
`TestContainer` binding, the raw `fetch_one` because `db.conn()`
returns that same connection directly.

## Cross-references

- [Testing](testing.md) — the test harness, `expect!`, `describe!`,
  `test!`, fakes.
- [Database](database.md#testing) — the surface-level testing
  section that introduces `TestDatabase`.
- [Eloquent → Factories](eloquent-factories.md) — factory definition
  syntax, states, sequences, relations.
- [Seeding](seeding.md) — seeder authoring, ordering, idempotency.
- [Service Container](container.md) — task-local vs thread-local
  vs global lookup, which decides what `DB::connection()` resolves
  to inside a test.
- [Mocking & Fakes](mocking.md) — `Storage::fake`, `Mail::fake`,
  `Queue::fake`, `Notification::fake`, and the trait-bind pattern
  for swapping in fake HTTP clients and other external surfaces.
- [HTTP Tests](http-tests.md) — driving handlers through the
  routing stack with a `TestDatabase` bound.
