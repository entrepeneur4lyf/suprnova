# Service Container

The container is where Suprnova holds your application's services —
the DB connection pool, the mail driver, your `Arc<MyService>`. You
bind values into it at boot time and resolve them in handlers and
workers. It's the Suprnova equivalent of Laravel's service container,
with one important difference: lookup is task-local first, so tests
running concurrently don't see each other's bindings.

## The two pieces

| Type | Role |
|---|---|
| `Container` | The underlying registry: holds bindings, factories, and singletons |
| `App` | The global facade you actually call — `App::bind`, `App::get`, etc. |

You almost always call `App::*` rather than constructing a
`Container` directly. The container is plumbing; the `App` facade
is the API.

## Lookup order

Every `App::get` / `App::make` call checks **three layers** in order:

```
        task-local
            │
            ▼  (miss)
       thread-local
            │
            ▼  (miss)
          global
            │
            ▼  (miss)
          None
```

This matters because:

- **Per-request state goes through task-local** — Inertia shared data,
  flash bag, request id. Each request gets its own layer, transparently.
- **Tests use thread-local** — `TestContainer::fake(|tc| { tc.bind(…); })`
  binds inside one thread without touching the global container,
  so parallel tests don't bleed services into each other.
- **App-wide services go through global** — bound once at boot,
  resolved everywhere.

You rarely think about which layer a binding lives in — `App::bind`
puts it where it makes sense, and `App::get` finds it wherever it
lives. The model only matters when something behaves unexpectedly
under concurrency, and then the [Testing](testing.md) chapter has the
detail.

## Binding a value

Five ways to put something into the container, depending on what you
have:

### `App::singleton(value)` — owned, cloned at lookup

For `T: Clone` values that should live forever:

```rust
use suprnova::App;

App::singleton(MyConfig {
    timeout_secs: 30,
    retries: 3,
});

let cfg = App::get::<MyConfig>().expect("registered at boot");
println!("{}", cfg.timeout_secs);
```

The value is stored once; `App::get::<MyConfig>()` returns a clone.
Use this for plain config-shaped data that's cheap to clone.

### `App::bind(Arc<T>)` — for traits and shared services

For trait objects or anything you want behind an `Arc`:

```rust
use std::sync::Arc;
use suprnova::App;

let store: Arc<dyn KeyValueStore> = Arc::new(RedisStore::connect(url)?);
App::bind(store);

let store = App::make::<dyn KeyValueStore>().expect("bound at boot");
store.put("hello", b"world").await?;
```

`App::make::<T>()` returns the `Arc<T>` clone (cheap atomic refcount
bump). Use this for any service shared across threads, especially
trait objects.

### `App::factory(|| { … })` — built on demand

When constructing the value should happen at first use (or every time):

```rust
App::factory(|| -> Result<HttpClient, FrameworkError> {
    HttpClient::builder()
        .timeout(Duration::from_secs(30))
        .build()
});
```

`App::factory` registers a *concrete-type* factory; `App::bind_factory`
registers a *trait-object* factory. Both invoke the closure outside
any container lock, so a factory that re-enters the container won't
deadlock and an expensive constructor won't block other bindings.

### `App::*_if_absent(value)` — boot-order-friendly registration

Sometimes a default service is registered by a service crate, and the
app wants to override it only when present. The `_if_absent` variants
let you register a default that won't clobber an existing binding:

```rust
// Inside a starter or library crate:
App::singleton_if_absent(DefaultMailDriver::new());

// In your app's bootstrap.rs:
App::singleton(MyCustomMailDriver::new());  // wins because it ran later
```

`bind_if_absent`, `singleton_if_absent`, and the factory variants all
return `bool` — `true` if they actually inserted, `false` if there
was already a binding.

## Resolving a value

Two read methods, plus their `Result`-returning siblings:

```rust
// Clone the bound value out:
let cfg: MyConfig = App::get::<MyConfig>().expect("bound at boot");

// Clone the Arc:
let store: Arc<dyn KeyValueStore> = App::make().expect("bound at boot");

// Same but Result, for the `?` idiom in fallible paths:
let cfg = App::resolve::<MyConfig>()?;
let store = App::resolve_make::<dyn KeyValueStore>()?;
```

`resolve` and `resolve_make` return
`Result<T, FrameworkError::ServiceUnresolved(_)>` — useful in handler
paths where a missing service should surface as a 500 with a proper
log, not a panic.

Membership checks (rarely needed):

```rust
if App::has::<MyConfig>() { … }
if App::has_binding::<dyn KeyValueStore>() { … }
```

## Where binding happens

The standard place is `src/bootstrap.rs` — one function that runs
once at boot:

```rust
use std::sync::Arc;
use suprnova::{App, FrameworkError};
use crate::services::{MyService, RealEmailGateway};

pub async fn bootstrap() -> Result<(), FrameworkError> {
    // Plain singletons
    App::singleton(MyAppConfig {
        max_uploads_per_user: 100,
    });

    // Trait-object services
    let gateway: Arc<dyn EmailGateway> = Arc::new(RealEmailGateway::new());
    App::bind(gateway);

    // Lazy services (built on first use)
    App::bind_factory::<dyn HttpClient, _>(|| {
        Ok(Arc::new(ReqwestClient::with_timeout(30)))
    });

    Ok(())
}
```

The framework also calls into the container itself during boot:

- `App::init()` runs first, initialising the registry
- `App::boot_services()` resolves boot-time dependencies (drivers,
  encryption keys, etc.) — your services see a fully-booted framework
- Your `bootstrap_fn` runs after that, so it can rely on the framework's
  services being available

See [Application Bootstrap](bootstrap.md) for the full boot order.

## Inertia shared data

The container is also where Inertia shared data lives. Two convenience
APIs make that explicit:

```rust
use suprnova::App;

App::inertia_share("flash", || {
    let flash = some_flash_function();
    serde_json::json!({ "message": flash })
});

App::flash("Saved!");  // shorthand for the common case
```

These read from `Container::inertia()` which returns
`Arc<InertiaRegistry>` — you can interact with it directly if you
need lower-level access. See [Inertia / Frontend](frontend.md) for
how the shared data ends up in the page response.

## Why three layers?

The task-local → thread-local → global cascade exists for one
reason: **isolation under concurrency**. Three things benefit:

**Per-request isolation.** Inertia's flash bag is bound per-request
via the task-local layer. Two concurrent requests don't see each
other's flash because their task-local containers don't overlap. The
binding evaporates when the request's task ends.

**Per-test isolation.** A test that binds a fake mail driver should
not see a fake bound by a sibling test. `TestContainer::fake(|tc|
{ tc.bind(...); })` binds inside one thread, so parallel tests stay
hermetic:

```rust
#[suprnova::test]
async fn one_test_binds_a_fake() {
    suprnova::container::testing::TestContainer::fake(|tc| {
        tc.bind::<dyn Mailer>(Arc::new(FakeMailer::new()));
    });

    // … this test uses FakeMailer
    // a sibling test running in parallel doesn't see it
}
```

**Override-at-boot.** Application code can override defaults registered
by library crates. The `_if_absent` variants and the layered lookup
combine to give library crates clean default-registration without
fighting application overrides.

## Common patterns

### Bind a struct holding the DB pool

You almost never do this directly — the framework binds the DB pool
itself. But if you have your own subsystem with an expensive
shared resource:

```rust
let pool = MyResourcePool::connect(url).await?;
App::bind(Arc::new(pool));

// later:
let pool = App::make::<MyResourcePool>()?;
let conn = pool.checkout().await?;
```

### Swap a default for a fake in tests

```rust
#[suprnova::test]
async fn order_dispatches_email() {
    use std::sync::Arc;
    use suprnova::container::testing::TestContainer;

    let fake = Arc::new(FakeEmailGateway::new());
    let fake_for_assert = Arc::clone(&fake);

    TestContainer::fake(|tc| {
        tc.bind::<dyn EmailGateway>(fake);
    });

    place_order(123).await?;

    assert_eq!(fake_for_assert.sent_count(), 1);
}
```

### Lazy expensive construction

```rust
// Builds the embedding model on first request, not at boot.
App::bind_factory::<dyn EmbeddingModel, _>(|| {
    Ok(Arc::new(OnnxEmbedding::load_from_disk("/models/all-mini-lm.onnx")?))
});
```

## Why Suprnova diverges

Laravel's container has one global scope — bindings are global, and
isolating between tests requires `setUp` / `tearDown` discipline plus
the framework's per-test database transaction. PHP's request-per-process
model makes this safe-by-accident: a fresh process per request means
the container is reset every time.

Rust's process model is the opposite — one process serves many
concurrent requests on many threads. A global-only container would
mean a test in one thread can see a fake bound by another, or a
request could see another request's per-request data. That's why
Suprnova has the three-layer cascade: task-local for per-request,
thread-local for per-test, global for app-wide.

The container API is the same as Laravel's; the lookup machinery
is different because the runtime is different.

## Next

- [Application Bootstrap](bootstrap.md) — where the binding code goes
- [Configuration](configuration.md) — typed config registration
  alongside services
- [Testing](testing.md) — `TestContainer::fake` and `#[suprnova::test]`
- [Lock Policy](lock-policy.md) — why poisoned-lock recovery matters
  in a container-backed application
