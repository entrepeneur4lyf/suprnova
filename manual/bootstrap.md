# Application Bootstrap

`bootstrap.rs` is the one place where your application wires itself up
at startup. Container bindings, event listeners, observers, supervisors,
global middleware — anything that should exist before the first request
hits the server (or the first job pops off the queue) is registered
inside a single async `bootstrap` function. There is no
service-provider scaffold to assemble; one function, run once, is the
whole API.

## The shape

A scaffolded app's entry point builds an [`Application`](lifecycle.md)
fluently and runs it. The `bootstrap` step is one method on the
builder:

```rust
// cmd/main.rs
use app::{bootstrap, config, migrations, routes};
use suprnova::Application;

#[tokio::main]
async fn main() {
    Application::new()
        .config(config::register_all)
        .bootstrap(bootstrap::register)
        .routes(routes::register)
        .migrations::<migrations::Migrator>()
        .run()
        .await;
}
```

The framework calls your `bootstrap_fn` once during the boot sequence,
after `Config::init` and after the runtime drivers (Cache, Queue,
RateLimit, Mail) are up but before the router is built. The same call
runs for background workers (`queue:work`, `workflow:work`,
`schedule:work`) so an observer or listener registered here fires
identically for an insert from a queue job and an insert from an HTTP
handler. [Lifecycle](lifecycle.md) walks the full sequence.

The function's signature is fixed by `Application::bootstrap`:

```rust
// src/bootstrap.rs
pub async fn register() {
    // bindings, observers, listeners, supervisors, global middleware
}
```

It returns `()`. Fallible setup uses `.expect("…")` with a message that
explains the remediation — boot is the right time to fail loudly. The
example app's call is `DB::init().await.expect("Failed to connect to
database");` so a missing `DATABASE_URL` aborts the process at boot
with the actual error printed, instead of surfacing as a confusing
"connection refused" on the first request.

## What goes in bootstrap

A real `bootstrap` function does a small number of distinct things.
Each subsection below is one of them. The example app's
`app/src/bootstrap.rs` exercises all of them and is the working
reference.

### Database connection

```rust
use suprnova::DB;

pub async fn register() {
    DB::init().await.expect("Failed to connect to database");
}
```

`DB::init` reads `DatabaseConfig` (registered by your `config_fn`) and
opens the pool. The connection is stored in the [container](container.md)
as a singleton — `DB::connection()` / `DB::get()` resolves it
anywhere. `DB::init_with(config)` is the test-and-tooling escape
hatch when you want to point at something other than the env-derived
URL.

### Global middleware

```rust
use suprnova::{global_middleware, SessionMiddleware, SessionConfig, TimeoutMiddleware};
use crate::middleware;

pub async fn register() {
    global_middleware!(middleware::LoggingMiddleware);
    global_middleware!(TimeoutMiddleware::default());
    global_middleware!(SessionMiddleware::new(SessionConfig::from_env()));
}
```

`global_middleware!` registers a layer that runs on every request,
including unrouted ones (404s, OPTIONS preflight). The order you
register in is the order the chain runs — outside-in. The framework
slots its own `RequestIdMiddleware` outermost; everything you add sits
inside it. [Middleware](middleware.md) explains the full chain shape,
including the per-route layer.

### Container bindings

The container takes whatever you put in it; the macros are sugar over
the [`App`](container.md) facade.

```rust
use std::sync::Arc;
use suprnova::{App, bind, singleton, factory};
use crate::providers::DatabaseUserProvider;

pub async fn register() {
    // Trait → singleton (wraps in Arc):
    bind!(dyn UserProvider, DatabaseUserProvider);

    // Concrete singleton:
    singleton!(MyConfig { max_uploads_per_user: 100 });

    // Factory (constructed per resolve):
    factory!(|| RequestLogger::new());

    // Or call the facade directly for finer control:
    let hub: Arc<dyn BroadcastHub> = Arc::new(InMemoryBroadcastHub::new());
    App::bind::<dyn BroadcastHub>(hub);
}
```

Trait-object bindings are the most common shape — bind an interface,
let handlers and tests substitute the implementation. The
[Container](container.md) chapter has the full binding API including
`bind_factory!`, the `_if_absent` variants, and the three-layer
lookup model.

### Event listeners and observers

The dispatcher is alive as soon as bootstrap runs — listeners
registered here see every subsequent dispatch.

```rust
use std::sync::Arc;
use suprnova::EventFacade;
use crate::events::UserRegistered;
use crate::listeners::SendWelcomeEmailListener;

pub async fn register() {
    EventFacade::listen::<UserRegistered, _>(
        Arc::new(SendWelcomeEmailListener),
    ).await;
}
```

Eloquent observers (`#[suprnova::observer(M)]`) collect themselves via
`inventory::submit!` at compile time. One call drains the inventory
into the dispatcher:

```rust
suprnova::eloquent::observers::bootstrap_observers()
    .await
    .expect("observer install failed");
```

The call is idempotent — re-running bootstrap (a worker that boots a
second time) does not double-register the listener adapters.
[Events](events.md) covers dispatch and listener authoring;
[Eloquent](eloquent.md) covers observers.

### Supervisors

Long-running background tasks declared via the `Supervisor` trait and
`inventory::submit!` start through one call:

```rust
use suprnova::SupervisorRegistry;

pub async fn register() {
    SupervisorRegistry::start_all().await;
}
```

Each supervisor runs in its own restart-loop task with a panic
boundary; a panicked supervisor is logged and restarted, not allowed
to take the process down. See [Supervisors](supervisors.md) for the
trait and the restart policy.

### Worker job registration

Queue jobs and mailables that workers need to dispatch by name register
themselves at boot:

```rust
use suprnova::queue::worker::register_job;

pub async fn register() {
    register_job::<crate::jobs::welcome_log::WelcomeLog>();

    suprnova::mail::register_mailable_factory::<crate::mail::welcome::WelcomeEmail>()
        .expect("register at boot");
    register_job::<suprnova::mail::send_job::SendMailJob>();
}
```

Without this, the worker has no way to map a queued envelope back to
the type that handles it.

## The post-boot hook: `booted()`

Bootstrap *registers*; `booted()` *resolves*. The builder takes a
second callback that fires after the server has finished its own
service boot but before it begins accepting connections. Use it when
you need to read something the framework itself bound during boot:

```rust
Application::new()
    .config(config::register_all)
    .bootstrap(bootstrap::register)
    .routes(routes::register)
    .booted(|| {
        let cfg: MyConfig = suprnova::App::get().unwrap();
        tracing::info!(?cfg, "services booted");
    })
    .run()
    .await;
```

`booted` is synchronous and runs after `Server::from_config` — drivers
are up, encryption keys are loaded, your bindings exist. Most apps do
not need this hook; reach for it when a one-shot post-boot side effect
needs to see a fully-constructed container.

## A complete `bootstrap.rs`

A trimmed but representative shape, drawn from the example app:

```rust
//! Application bootstrap — register services, listeners, and
//! global middleware.

use std::sync::Arc;
use std::time::Duration;

use suprnova::broadcasting::{BroadcastHub, ChannelRegistry, InMemoryBroadcastHub};
use suprnova::features::{FeatureMiddleware, bootstrap_database_cached};
use suprnova::queue::worker::register_job;
use suprnova::{
    App, DB, EventFacade, FrameworkError, Inertia, InertiaConfig,
    SessionConfig, SessionMiddleware, Storage, SupervisorRegistry,
    UserProvider, bind, global_middleware,
};

use crate::broadcasting::ChatChannel;
use crate::events::UserRegistered;
use crate::listeners::SendWelcomeEmailListener;
use crate::middleware;
use crate::providers::DatabaseUserProvider;

pub async fn register() {
    // ── Database
    DB::init().await.expect("Failed to connect to database");

    // ── Global middleware (outside-in in registration order)
    global_middleware!(middleware::LoggingMiddleware);
    global_middleware!(suprnova::TimeoutMiddleware::default());
    global_middleware!(SessionMiddleware::new(SessionConfig::from_env()));

    // ── Auth provider
    bind!(dyn UserProvider, DatabaseUserProvider);

    // ── Inertia protocol layer
    Inertia::install(&InertiaConfig::new().version("1.0"));

    // ── Broadcasting hub + channel registry
    let hub: Arc<dyn BroadcastHub> = Arc::new(InMemoryBroadcastHub::new());
    App::bind::<dyn BroadcastHub>(Arc::clone(&hub));

    let mut registry = ChannelRegistry::new();
    registry.register(ChatChannel);
    App::singleton(Arc::new(registry));

    // ── Event listeners + bridges
    EventFacade::listen::<UserRegistered, _>(
        Arc::new(SendWelcomeEmailListener),
    ).await;
    EventFacade::broadcast::<UserRegistered>(Arc::clone(&hub)).await;

    // ── Storage disks (env-gated S3 in production)
    Storage::register_fs("public", "./storage/public")
        .expect("register public disk");

    // ── Worker job registration
    register_job::<crate::jobs::welcome_log::WelcomeLog>();
    suprnova::mail::register_mailable_factory::<crate::mail::welcome::WelcomeEmail>()
        .expect("register at boot");
    register_job::<suprnova::mail::send_job::SendMailJob>();

    // ── Observers + supervisors
    suprnova::eloquent::observers::bootstrap_observers()
        .await
        .expect("observer install failed");
    SupervisorRegistry::start_all().await;

    // ── Feature flags
    bootstrap_database_cached(Duration::from_secs(60))
        .await
        .expect("feature-flag chain wired");
    global_middleware!(FeatureMiddleware::new());
}
```

Notice the rhythm: each block does one thing, calls one or two APIs,
and either succeeds or fails with a clear message. Nothing here is
clever; the function is long because the app has a lot of moving
parts, not because the bootstrap pattern is complicated.

## When to bootstrap vs `#[injectable]`

`#[injectable]` is a macro that auto-registers a singleton in the
container's `inventory` at compile time. It is the right choice for
services that need nothing more than their `#[inject]` dependencies to
construct:

```rust
use suprnova::injectable;

#[injectable]
pub struct UserService;

#[injectable]
pub struct OrderService {
    #[inject]
    user_service: UserService,
}
```

These resolve themselves; bootstrap does not need to touch them.

Bootstrap is the right place when construction needs anything else —
an environment variable, a constructed config struct, a `dyn Trait`
binding, a runtime decision, an async setup call, or registration of
something that is not itself a service (a listener, an observer, a
queue job mapping, a global middleware layer).

| Use `#[injectable]` for | Use `bootstrap` for |
|---|---|
| Concrete singletons with no runtime config | Anything `dyn Trait` |
| Services constructed from other injectables | Anything async at boot |
| Default DI graph | Environment-driven values |
| | Event listeners, observers, supervisors |
| | Global middleware |
| | Worker job + mailable registration |

You can mix freely. `#[injectable]` services are visible in the
container by the time `bootstrap` runs, so a binding in bootstrap can
read them.

## Where bootstrap sits in the boot order

The full sequence (excerpted from [Lifecycle](lifecycle.md)):

1. `Config::init(".")` — load `.env`, detect environment
2. `init_policies()` — drain the `#[policy]` inventory
3. Your `config_fn` runs (typed config registration)
4. Migrations run (auto-migrate on `serve`)
5. **Your `bootstrap_fn` runs** ← `bootstrap::register`
6. Routes assembled from your `routes_fn`
7. `Server::from_config` boots drivers + container
8. Your `booted_fn`s fire
9. Server begins accepting connections

Background workers (`queue:work`, `workflow:work`, `schedule:work`)
share steps 1–5 and 7 so a listener or observer you register reaches
worker code paths exactly as it reaches HTTP handlers.

### Why Suprnova diverges

Laravel splits boot across multiple service providers: each provider
implements `register()` and `boot()`, they're collected in
`config/app.php`, and Laravel walks them in two passes (all `register`,
then all `boot`) so a service can depend on another provider's
bindings without ordering ceremony in user code. The provider class
gives you a unit of organisation when an app accumulates dozens of
distinct subsystems.

Suprnova collapses that to one function. The reasons:

- **The two-pass `register`/`boot` split solves an ordering problem
  Rust does not have.** `#[injectable]` and the container's
  `bootstrap_singletons` already resolve dependency graphs without
  user-visible ordering. Bindings register inline; the lookup machinery
  handles the rest.
- **One function is easier to read than ten.** A new contributor
  opens `bootstrap.rs` and sees every binding, every listener, every
  observer, every middleware layer in one place. Provider-style
  fragmentation hides what the app actually does.
- **Inventory-style auto-registration covers the rest.** Observers,
  supervisors, scheduled tasks, policies, and queue handlers all
  collect themselves at compile time via `inventory::submit!`.
  Bootstrap drains the inventories with single calls
  (`bootstrap_observers`, `SupervisorRegistry::start_all`) rather than
  enumerating each.

Where Laravel earns the provider split is library distribution: a
crate that ships its own bindings would want a registration entry
point that an app can opt into without editing its own bootstrap.
Suprnova's analogue is a public `pub async fn register()` in the
crate's root and a one-line call from the app's `bootstrap`. The
ergonomic cost is one line; the readability gain is everything in
one place.

## Next

- [Lifecycle](lifecycle.md) — full boot order and where `bootstrap_fn` fires
- [Container](container.md) — `App::bind` / `App::singleton` /
  `App::factory` and the three-layer lookup
- [Configuration](configuration.md) — typed config registration that
  runs before bootstrap
- [Middleware](middleware.md) — chain composition for layers
  registered with `global_middleware!`
- [Events](events.md) — the dispatcher that listeners and observers
  plug into
