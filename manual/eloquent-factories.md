# Eloquent Factories

Factories produce randomized model instances for tests and seeders. The
shape is Laravel's: `UserFactory::new().count(10).create_many().await?`.
The contract is one trait plus a fluent builder, with a `#[derive(Factory)]`
shortcut for the common case where the model already has a sensible
randomized representation.

This chapter covers defining factories by hand and by derive, composing
overrides into reusable "states", deterministic IDs via `Sequence`, the
`Persistable` seam that powers `create`, and the difference between
`make` (in-memory) and `create` (persisted). For the test-writing context
where factories are most useful, see [Testing](testing.md).

## The `Factory` trait

The trait has exactly one required method:

```rust
pub trait Factory {
    type Model;

    fn definition() -> Self::Model
    where
        Self: Sized;
}
```

`definition()` returns a fully populated model with every field
randomized to whatever default makes sense. The trait carries no
per-instance state — implementors are typically zero-sized markers
(`struct UserFactory;`) so a caller can reach the factory by name
without holding a handle.

The trait also provides two builder entrypoints with default
implementations:

```rust
fn new() -> FactoryBuilder<Self::Model>;       // count = 1, no overrides
fn times(n: usize) -> FactoryBuilder<Self::Model>;  // sugar for new().count(n)
```

Every other method you'll call (`with`, `count`, `make`, `create`,
`create_many`, …) lives on `FactoryBuilder<M>`.

## Defining a factory by hand

The minimal hand-written form pairs a marker struct with a `Factory`
impl that knows how to build one instance. You'll typically reach for
this when the model doesn't derive `fake::Dummy` — perhaps because some
fields need deterministic seeding (relation IDs in a known range) or
the randomized representation needs business-rule awareness:

```rust
use suprnova::Factory;
use crate::models::users::User;

pub struct UserFactory;

impl Factory for UserFactory {
    type Model = User;

    fn definition() -> User {
        let now = chrono::Utc::now();
        User {
            // `0` is a placeholder — `persist_via_seaorm` flips
            // primary-key columns to `NotSet` before inserting so
            // the database assigns the real id.
            id: 0,
            name: format!("Factory User #{}", next_seq()),
            email: format!("factory-{}@example.test", next_seq()),
            password: "factory-placeholder".into(),
            remember_token: None,
            active: true,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            __eager: Default::default(),
            __pivot: None,
        }
    }
}
```

The `__eager` and `__pivot` fields are the eager-load and pivot scratch
state that the `#[suprnova::model]` macro injects on every Eloquent
struct. Always default them — they get populated by the query builder,
not by factories.

`next_seq()` is whatever you want it to be — a `static AtomicU64`, a
`Sequence` (covered below), or a thread-local counter. The point is
that `definition()` runs fresh on every call inside `make_many` /
`create_many`, so any uniqueness you need has to come from a counter
the function can reach.

## `#[derive(Factory)]` for the common case

When the model itself implements `fake::Dummy` — either via
`#[derive(Dummy)]` or a hand-written `impl Dummy<Faker> for Model` —
the derive collapses the marker + impl into one line on the model:

```rust
use suprnova::{Dummy, Factory};

#[derive(Dummy, Factory)]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub author_id: i64,
    pub is_public: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
```

The derive emits `pub struct PostFactory;` as a sibling type and an
`impl Factory for PostFactory` whose `definition()` calls
`Faker.fake::<Post>()`. Visibility on the factory mirrors visibility
on the model — a `pub` model gets a `pub` factory, a `pub(crate)`
model gets a `pub(crate)` factory.

### Overriding the generated name

By default `#[derive(Factory)]` emits `<Model>Factory`. Override via
the `name` attribute:

```rust
#[derive(Dummy, Factory)]
#[factory(name = "AccountFactory")]
pub struct User { /* … */ }
```

The value must parse as a Rust identifier — `name = "User Factory"`
or `name = "user-factory"` fails to compile with a clear span-pointed
error. The macro emits `pub struct <Name>;` literally, so anything
that can't be a type name can't be a factory name.

### Hand-written `Dummy` for richer randomization

`#[derive(Dummy)]` works for primitive-typed structs but gives you no
control over distributions or cross-field invariants. For anything
non-trivial, write the `Dummy` impl by hand and pair it with
`#[derive(Factory)]`:

```rust
use suprnova::__fake::rand::Rng;
use suprnova::__fake::{Dummy, Fake, Faker, faker::lorem::en::{Paragraph, Sentence}};
use suprnova::Factory;

#[derive(Factory)]
pub struct Post { /* fields … */ }

impl Dummy<Faker> for Post {
    fn dummy_with_rng<R: Rng + ?Sized>(_: &Faker, rng: &mut R) -> Self {
        let title: String = Sentence(3..7).fake_with_rng(rng);
        let body: String = Paragraph(3..6).fake_with_rng(rng);
        let author_id: i64 = (1..=50i64).fake_with_rng(rng);
        let now = chrono::Utc::now();

        Post {
            id: 0,
            author_id,
            title,
            body,
            is_public: Faker.fake_with_rng::<bool, _>(rng),
            created_at: now,
            updated_at: now,
            __eager: Default::default(),
            __pivot: None,
        }
    }
}
```

The `fake` crate is re-exported as `suprnova::__fake` so consumers
don't need a separate `fake = "…"` line in `Cargo.toml`. Common types
are also re-exported under the crate root: `suprnova::{Dummy, Fake, Faker}`.

### Why `#[derive(Factory)]` only takes plain structs

The derive rejects enums, unions, and generic models with a clear
compile error. Enums and unions don't have a meaningful default
representation. Generics would force a decision about how the factory
type parameterizes its model — and there's no good default, so the
derive refuses to guess. Write the `impl Factory` by hand for those
cases.

## The fluent builder

`Factory::new()` / `Factory::times(n)` return a `FactoryBuilder<M>`.
Every operation is chainable; nothing happens until you call a
terminal method (`make`, `make_one`, `make_many`, `create`,
`create_one`, `create_many`).

### `count(n)` — how many instances

```rust
let user = UserFactory::new().make();             // 1 user
let users = UserFactory::new().count(10).make_many();  // 10 users
let same = UserFactory::times(10).make_many();   // identical
```

`count(n)` is ignored by `make` / `create` (always one) and honored by
`make_many` / `create_many`. `times(n)` is just sugar for
`Self::new().count(n)` and matches Laravel's `Factory::times($n)`.

### `with(|m| { … })` — per-call overrides

`with` registers a closure that runs against every produced instance
after `definition()`. Multiple `with` calls compose in registration
order, so a later override clobbers an earlier one on the same field:

```rust
let admin = UserFactory::new()
    .with(|u| u.active = true)
    .with(|u| u.role = "admin".into())
    .make();
```

Overrides are stored as `Box<dyn Fn(&mut M) + Send + Sync + 'static>`
so the builder stays `Send` — important for the async `create` /
`create_many` paths, which hold the builder across an `.await` on the
SeaORM insert.

### `prepend(|m| { … })` — defaults that callers can still override

`prepend` inserts a closure at the **front** of the override chain, so
it runs **before** any other `with(...)`. Use it inside a state method
when you want to provide a default the caller can still clobber with a
later `.with(...)`:

```rust
impl UserFactory {
    /// State method — admin defaults, caller can still customise.
    pub fn admin() -> suprnova::FactoryBuilder<User> {
        Self::new()
            .prepend(|u| u.role = "admin".into())
            .prepend(|u| u.active = true)
    }
}

// Caller wins on `role` because their .with() comes after the prepends.
let owner = UserFactory::admin()
    .with(|u| u.role = "owner".into())
    .make();
```

This is Suprnova's equivalent of Laravel's `Factory::prependState`. It's
the right primitive for state methods specifically — `with` would lose
to a caller's `.with(...)`, which is the opposite of what a default
should do.

### `when(cond, |b| { … })` — conditional chaining

`when` threads a flag through a chain without breaking the fluent
style. The closure receives the builder, returns the builder. When
`cond` is false, the builder passes through unchanged:

```rust
UserFactory::times(10)
    .with(|u| u.active = true)
    .when(seed_admins, |b| b.with(|u| u.role = "admin".into()))
    .create_many()
    .await?;
```

Mirrors Laravel's `Conditionable::when($cond, $cb)`. The
`FnOnce(Self) -> Self` signature means you can `await` inside the
closure as long as you `.await` before returning the builder.

### Terminal methods

| Method | Returns | Persisted? |
|---|---|---|
| `make()` | one `M` | no |
| `make_one()` | one `M` (forces count = 1) | no |
| `make_many()` | `Vec<M>` of `count` items | no |
| `create()` | `Result<M, FrameworkError>` | yes |
| `create_one()` | `Result<M, FrameworkError>` (forces count = 1) | yes |
| `create_many()` | `Result<Vec<M>, FrameworkError>` | yes |

`make_one` and `create_one` are useful when a state method has set
`count` internally to something else and the caller wants exactly one
result:

```rust
pub fn admins_in_org(org_id: i64) -> suprnova::FactoryBuilder<User> {
    UserFactory::times(5)               // sensible default for fixtures
        .with(move |u| u.org_id = org_id)
        .with(|u| u.role = "admin".into())
}

// Test only wants one — `create_one` discards the count(5).
let admin = admins_in_org(42).create_one().await?;
```

## States: reusable preset combinations

Suprnova doesn't ship a `state("name")` lookup table. Instead, states
are plain methods on your factory marker that return a pre-configured
`FactoryBuilder<M>`. The pattern composes by inheritance — every state
method returns the same `FactoryBuilder<M>` type, so you can chain more
methods onto the result:

```rust
use suprnova::FactoryBuilder;
use crate::models::users::User;

pub struct UserFactory;

impl suprnova::Factory for UserFactory {
    type Model = User;
    fn definition() -> User { /* … */ }
}

impl UserFactory {
    /// Inactive variant — overlays an `active: false` default.
    pub fn inactive() -> FactoryBuilder<User> {
        Self::new().prepend(|u| u.active = false)
    }

    /// Admin variant — overlays role + verified email.
    pub fn admin() -> FactoryBuilder<User> {
        Self::new()
            .prepend(|u| u.role = "admin".into())
            .prepend(|u| u.email_verified_at = Some(chrono::Utc::now()))
    }

    /// Composable: inactive admin.
    pub fn inactive_admin() -> FactoryBuilder<User> {
        Self::admin().prepend(|u| u.active = false)
    }
}
```

```rust
// Compose at the call site too — chain more overrides freely.
let user = UserFactory::admin()
    .with(|u| u.name = "Alice".into())
    .create()
    .await?;

let batch = UserFactory::inactive().count(20).create_many().await?;
```

The `prepend` choice is deliberate: a state's overrides are *defaults*
that the caller can still rewrite. If you want a state's setting to be
non-negotiable, use `with` instead — it goes to the end of the chain
and wins.

### Why no `state("name")` lookup

A name-keyed state registry would force runtime string matching for
something the compiler can check. State methods give you compile-time
verification (typo `UserFactor::admn()` is a hard error) and full IDE
autocomplete. The composability — chaining `Self::admin()` from inside
`inactive_admin()` — falls out for free.

## Deterministic IDs with `Sequence`

`Sequence` is a monotonic counter for seeding unique-per-call fields.
Each `next()` call returns 1, 2, 3, … atomically across threads:

```rust
use suprnova::{Fake, Sequence};

static ORDER_IDS: Sequence = Sequence::new();

pub struct OrderFactory;
impl suprnova::Factory for OrderFactory {
    type Model = Order;
    fn definition() -> Order {
        Order {
            id: 0,
            number: format!("ORD-{:06}", ORDER_IDS.next()),
            total_cents: (100..=10_000).fake(),
            created_at: chrono::Utc::now(),
            __eager: Default::default(),
            __pivot: None,
        }
    }
}
```

`Sequence::new()` is `const`, so it works as a `static` initializer.
The counter starts at 0 and increments to 1 on first call. Use
`reset()` between tests if you want a clean count — the
`#[suprnova_test]` macro doesn't do this for you because the framework
can't know which sequences are yours:

```rust
#[suprnova::suprnova_test]
async fn each_order_gets_a_unique_number(db: TestDatabase) {
    ORDER_IDS.reset();   // start at 1 for this test
    let orders = OrderFactory::new().count(5).create_many().await?;
    assert_eq!(orders[0].number, "ORD-000001");
    assert_eq!(orders[4].number, "ORD-000005");
}
```

`Sequence` uses `SeqCst` ordering — overkill for "give me a unique
id" but keeps reasoning trivial. If a Sequence ever shows up in a hot
path you can write your own with `Relaxed`.

## `Persistable`: the seam to your storage

The `create` family of methods is available whenever the model
implements `Persistable`:

```rust
#[async_trait]
pub trait Persistable: Sized + Send {
    async fn persist(self) -> Result<Self, FrameworkError>;
}
```

A blanket impl in `factory::persist` covers every SeaORM model that
can `IntoActiveModel<ActiveModel>` — which is every model the
`#[suprnova::model]` macro emits. No per-model boilerplate; if `User`
is a model, `UserFactory::new().create()` works.

The blanket pulls `DB::connection()` and inserts. The returned `Self`
is what SeaORM hands back from the insert — assigned id, defaulted
columns resolved, etc.

### Primary-key handling

A SeaORM `IntoActiveModel` impl marks every field — including the PK
— as `Set(value)`. For factory-produced models the PK is a placeholder
(`0` for `AUTO_INCREMENT i64`), so a straight insert collides on the
second call with a UNIQUE constraint failure.

`persist_via_seaorm` (the helper that backs the blanket) flips every
primary-key column to `NotSet` before inserting, which lets the
database assign its own id — the semantic factories actually need:

```rust
pub async fn persist_via_seaorm<M, E, C>(model: M, db: &C) -> Result<M, FrameworkError>
where
    M: ModelTrait<Entity = E> + IntoActiveModel<<E as EntityTrait>::ActiveModel> + Send,
    E: EntityTrait<Model = M>,
    /* … bounds … */
    C: ConnectionTrait,
{
    let mut active = model.into_active_model();
    for pk in <<E as EntityTrait>::PrimaryKey as Iterable>::iter() {
        active.not_set(pk.into_column());
    }
    active.insert(db).await.map_err(/* … */)
}
```

If you actually *want* to assign a specific id (replay test, restoring
a fixture by id), bypass the helper and call
`model.into_active_model().insert(db).await` directly.

### Persisting against an explicit connection

`persist_via_seaorm` takes the connection as an argument. Useful when
you want to drive persistence against a connection that isn't the
framework's bound `DB::connection()` — most often a specific
`sqlite::memory:` handle in an integration test:

```rust
use suprnova::factory::persist_via_seaorm;

let model = UserFactory::new().make();
let row = persist_via_seaorm(model, db.inner()).await?;
```

### Custom non-SeaORM backends

Because the blanket impl targets every `ModelTrait` type, you can't
write `impl Persistable for MyOrm::Model` from a downstream crate
without colliding. For non-SeaORM custom persistence (Redis, Surreal,
blob-only stores), wrap the model in a newtype and impl `Persistable`
on the wrapper:

```rust
use suprnova::{FrameworkError, Persistable};
use suprnova::async_trait;

pub struct RedisCached<T>(pub T);

#[async_trait]
impl Persistable for RedisCached<MyValue> {
    async fn persist(self) -> Result<Self, FrameworkError> {
        let client = suprnova::App::make::<RedisClient>()
            .ok_or_else(|| FrameworkError::internal("redis client not bound"))?;
        client.set(&self.0.key, &serde_json::to_vec(&self.0)?).await?;
        Ok(self)
    }
}
```

A `Factory<Model = RedisCached<MyValue>>` then gets `create` /
`create_many` for free.

## `make` vs `create`: when to use which

`make` returns the model without touching the database:

```rust
// Unit test for a pure function — no DB needed.
let draft = PostFactory::new().with(|p| p.is_public = false).make();
let snippet = my_lib::extract_summary(&draft);
assert!(snippet.len() < 200);
```

`create` persists and returns the post-insert version:

```rust
// Integration test — the action needs a real row.
let post = PostFactory::new().create().await?;
let action = App::resolve::<PublishPostAction>().unwrap();
let published = action.execute(post.id).await?;
assert!(published.is_public);
```

Reach for `make` whenever the test doesn't care that the row exists.
Reach for `create` when you'll query the row back, when a foreign key
needs a real id, or when you're populating fixtures for a sub-system
that reads the DB. Note that `create_many` persists sequentially — if a
later insert fails, the prior inserts are NOT rolled back. `create` /
`create_many` use the framework's bound `DB::connection()` directly,
so they do **not** participate in an ambient `DB::transaction(...)`
scope. If you need atomicity across a batch of factory inserts, drop
down to `make_many` + `persist_via_seaorm` against the transaction
handle:

```rust
use suprnova::{DB, factory::persist_via_seaorm};

DB::transaction(|tx| Box::pin(async move {
    for user in UserFactory::times(50).make_many() {
        persist_via_seaorm(user, tx).await?;
    }
    for post in PostFactory::times(200).make_many() {
        persist_via_seaorm(post, tx).await?;
    }
    Ok::<_, suprnova::FrameworkError>(())
})).await?;
```

## "After-creating" behaviour

Suprnova doesn't ship a named `after_creating(|m| { … })` callback. Two
patterns cover the use cases that callback exists for in Laravel:

**1. The chain — do the follow-up after `create`/`create_many`:**

```rust
let user = UserFactory::new().create().await?;
ProfileFactory::new()
    .with(move |p| p.user_id = user.id)
    .create()
    .await?;
```

This is the canonical pattern when one model's id needs to flow into a
follow-up insert. `create` returns the persisted row, so the id is
immediately available.

**2. Model observers — react on the model lifecycle, not the factory:**

Use [Model Observers](eloquent.md#observers) to wire post-insert
behaviour to the model itself rather than the factory. The observer
fires for `User::create(...)`, `UserFactory::new().create()`, and any
other persistence path — exactly what you want when the behaviour is
"every time this row lands, do X":

```rust
use suprnova::{FrameworkError, Observer, async_trait, observer};

#[observer(User)]
pub struct AuditUser;

#[async_trait]
impl Observer<User> for AuditUser {
    async fn created(&self, user: &User) -> Result<(), FrameworkError> {
        tracing::info!(user_id = user.id, "user created");
        Ok(())
    }
}
```

Factory-only callbacks would invite divergence between test inserts
and real inserts. Observers stay consistent across both.

## Seeders

Factories produce instances; seeders orchestrate them. A `Seeder` is a
zero-sized type with an async `run` that knows what to populate:

```rust
use suprnova::{Factory, FrameworkError, Seeder};
use suprnova::async_trait;

use crate::factories::{PostFactory, UserFactory};

pub struct BaseSeeder;

#[async_trait]
impl Seeder for BaseSeeder {
    fn name() -> &'static str { "BaseSeeder" }

    async fn run() -> Result<(), FrameworkError> {
        // Users first — posts reference user ids in 1..=50.
        UserFactory::new().count(50).create_many().await?;
        PostFactory::new().count(200).create_many().await?;
        Ok(())
    }
}
```

Register the seeder in `bootstrap.rs` so the per-project `console`
binary's `db:seed` command knows about it:

```rust
suprnova::seed::register::<crate::seeders::BaseSeeder>();
```

Run through the project's `console` binary (every scaffolded app
ships one at `src/bin/console.rs`):

```bash
cargo run --bin console -- db:seed
```

Seeders run in registration order. Idempotency is the seeder's
responsibility — `run` does not snapshot or roll back, so a seeder
that inserts unconditionally produces duplicates on re-run. Use
`migrate:fresh` followed by `db:seed` for a clean slate.

## Putting it together: a complete test fixture

```rust
use suprnova::{App, describe, test, expect};
use suprnova::events::{EventFacade, assert_dispatched_times};
use suprnova::testing::TestDatabase;
use crate::factories::{PostFactory, UserFactory};
use crate::actions::publish_post::PublishPostAction;

describe!("PublishPostAction", {
    test!("publishes a draft post", async fn(db: TestDatabase) {
        // Arrange — an author and one draft post owned by them.
        let author = UserFactory::new()
            .with(|u| u.active = true)
            .create()
            .await
            .unwrap();

        let draft = PostFactory::new()
            .with(move |p| p.author_id = author.id)
            .with(|p| p.is_public = false)
            .create()
            .await
            .unwrap();

        // Act.
        let action = App::resolve::<PublishPostAction>().unwrap();
        let published = action.execute(draft.id).await.unwrap();

        // Assert.
        expect!(published.is_public).to_equal(true);
        expect!(published.author_id).to_equal(author.id);
    });

    test!("publishing emits exactly one event", async fn(db: TestDatabase) {
        let _guard = EventFacade::fake();
        let post = PostFactory::new().create().await.unwrap();

        App::resolve::<PublishPostAction>().unwrap()
            .execute(post.id).await.unwrap();

        assert_dispatched_times::<crate::events::PostPublished>(1);
    });
});
```

Three patterns worth pointing at:

- The author's `id` flows into the post via a `move` closure inside
  `.with(...)`. Captures are explicit, which keeps the relation
  visible at the call site.
- `create().await.unwrap()` is the test idiom — the test is allowed to
  panic on setup failure because a broken fixture is a broken test,
  not a graceful failure mode.
- Factories compose with the rest of the testing surface
  (`EventFacade::fake`, `Storage::fake`, `Mail::fake`, …) — none of
  the fakes know about factories, but every test you write will use
  them together.

### Why Suprnova diverges

Laravel's factories ship with named states (`->state('admin')`),
runtime sequences (`->sequence(['name' => 'A'], ['name' => 'B'])`),
and an `afterCreating` callback registered on the factory itself.
Suprnova drops all three and replaces them with Rust-shaped
primitives:

- **States are methods, not strings.** Compile-time typo-checking and
  IDE autocomplete are both free; the only cost is "you write `pub fn
  admin()` instead of `protected function admin()`", which is no cost
  at all.
- **Sequences are a separate primitive.** `Sequence` does one thing
  (atomic counter) and is reusable outside the factory surface — you
  can drop one into a request id generator, a workflow step counter,
  or a test harness without explaining what it is.
- **After-creating is wired to the model, not the factory.** The
  framework already has [Model Observers](eloquent.md#observers) for
  exactly that purpose. Adding a parallel mechanism on the factory
  would make test-time behaviour and production-time behaviour diverge
  by construction.

The fluent surface — `count(10)`, `times(10)`, `with`, `prepend`,
`when`, `make`, `create`, `create_many`, `make_one`, `create_one` —
mirrors Laravel's directly, so the muscle memory ports without a
glossary.

## Next

- [Testing](testing.md) — `#[suprnova_test]`, `TestDatabase`, the fake
  facades that pair with factory-built fixtures.
- [Eloquent](eloquent.md) — model derivation, observers, the cast
  pipeline that runs when `create` persists your factory output.
- [Migrations](migrations.md) — the schema your factories need to
  exist against; use `migrate:fresh && db:seed` for a clean fixture
  slate.
- [Database](database.md) — `DB::transaction`, multi-connection
  routing, savepoints — what to reach for when `create_many` needs
  atomicity.
- [Service Container](container.md) — how `App::resolve` and
  `App::make` find the action and service types your tests call into
  alongside factories.
