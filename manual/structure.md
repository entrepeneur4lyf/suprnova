# Directory Structure

When you run `suprnova new my-app --frontend svelte`, the scaffolder
gives you this:

```
my-app/
├── Cargo.toml                      # workspace dependencies + crate metadata
├── .env                            # local config — DB URL, app key, ports
├── .env.example                    # template for ops/CI
├── .gitignore                      # excludes target/, .env, node_modules/, public/assets/
├── cmd/
│   └── main.rs                     # the binary entry; calls Application::new().run()
├── src/
│   ├── lib.rs                      # module wiring (`pub mod controllers;` etc.)
│   ├── bootstrap.rs                # registers services, observers, listeners — the
│   │                               # Suprnova analogue of Laravel's service providers
│   ├── routes.rs                   # the `routes!` macro tree — every URL the app serves
│   ├── bin/
│   │   └── console.rs              # `cargo run --bin console <subcommand>` entry —
│   │                               # the Suprnova analogue of `php artisan`
│   ├── actions/
│   │   ├── mod.rs
│   │   └── example_action.rs       # one-method invokable controllers
│   ├── commands/
│   │   └── mod.rs                  # `#[command]`-annotated handlers register here
│   ├── config/
│   │   ├── mod.rs
│   │   ├── database.rs             # typed DB config (driver, URL, pool)
│   │   └── mail.rs                 # typed mail config
│   ├── controllers/
│   │   ├── mod.rs
│   │   ├── home.rs                 # GET / handler
│   │   ├── auth.rs                 # login / register / logout
│   │   └── dashboard.rs            # requires auth; example protected route
│   ├── middleware/
│   │   ├── mod.rs
│   │   ├── logging.rs              # request/response logging
│   │   └── authenticate.rs         # session-based auth guard
│   ├── migrations/
│   │   ├── mod.rs
│   │   ├── m_*_create_users_table.rs
│   │   ├── m_*_create_sessions_table.rs
│   │   ├── m_*_create_remember_tokens_table.rs
│   │   ├── m_*_create_workflows_table.rs
│   │   └── m_*_create_workflow_steps_table.rs
│   └── models/
│       ├── mod.rs
│       └── user.rs                 # `#[suprnova::model]` User model
├── frontend/
│   ├── package.json
│   ├── vite.config.ts
│   ├── tsconfig.json
│   ├── index.html                  # Vite entry; mounts the SPA
│   └── src/
│       ├── main.{tsx,ts}           # Inertia client setup (per-framework)
│       ├── app.css                 # global styles + Tailwind
│       ├── pages/
│       │   ├── Home.{tsx,svelte,vue}
│       │   ├── Dashboard.{tsx,svelte,vue}
│       │   └── auth/
│       │       ├── Login.{tsx,svelte,vue}
│       │       └── Register.{tsx,svelte,vue}
│       └── types/
│           └── inertia-props.ts    # auto-generated from #[derive(InertiaProps)]
└── public/
    └── assets/                     # Vite production build output lands here
```

Svelte adds `frontend/svelte.config.js` and `frontend/src/app.d.ts`.
Vue adds `frontend/src/shims-vue.d.ts`.

The API starter (`suprnova new my-api --api`) is slimmer: no
`frontend/`, no auth controllers, and `cmd/main.rs` is replaced by
`src/main.rs` (single-crate layout instead of workspace).

## What each directory is for

### `cmd/main.rs`

The binary entry point. A short file — typically 10–20 lines — that
calls the standard boot pipeline:

```rust
use suprnova::Application;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    Application::new()
        .config(my_app::config::register)
        .bootstrap(my_app::bootstrap::bootstrap)
        .routes(my_app::routes::register)
        .migrations::<my_app::migrations::Migrator>()
        .run()
        .await
}
```

`Application::run()` parses the binary's CLI (`serve` / `web:run` /
`migrate*` / `schedule:*` / `workflow:work` / `queue:work`), loads
`.env`, runs your config function, then dispatches the subcommand. The
serve path also runs your bootstrap function and starts the HTTP
server.

You almost never edit `cmd/main.rs` after the initial scaffold.

### `src/lib.rs`

A flat module declaration file:

```rust
pub mod actions;
pub mod bootstrap;
pub mod commands;
pub mod config;
pub mod controllers;
pub mod middleware;
pub mod migrations;
pub mod models;
pub mod routes;
```

This is what makes `crate::controllers::home::index` reachable from
`routes.rs`.

### `src/bootstrap.rs`

The single function that wires your app. You register service container
bindings, observers, event listeners, custom middleware, and any other
boot-time setup here. It's the analogue of Laravel's `AppServiceProvider`,
`EventServiceProvider`, `BroadcastServiceProvider`, etc., all in one
file:

```rust
use std::sync::Arc;
use suprnova::{App, FrameworkError};

pub async fn bootstrap() -> Result<(), FrameworkError> {
    // Bind a service into the container
    App::bind(Arc::new(MyService::new()));

    // Register an Eloquent observer
    crate::models::user::register_observer();

    // Listen for events
    suprnova::Event::listen::<OrderShipped, _>(SendShipmentNotification);

    Ok(())
}
```

`bootstrap()` runs once per process, after the config loader but
before `serve` accepts the first request. Workers (`queue:work`,
`schedule:run`, `workflow:work`) reuse the same bootstrap so they see
the same services. See [Application Bootstrap](bootstrap.md).

### `src/routes.rs`

Your URL surface. One `routes!` macro tree:

```rust
use suprnova::{get, post, put, delete, routes};
use crate::controllers;

pub fn register() -> suprnova::Router {
    routes! {
        get!("/", controllers::home::index).name("home"),

        // Auth (registered + protected)
        get!("/login", controllers::auth::show_login).name("login.show"),
        post!("/login", controllers::auth::login).name("login.attempt"),
        post!("/logout", controllers::auth::logout).name("logout"),
        get!("/register", controllers::auth::show_register).name("register.show"),
        post!("/register", controllers::auth::register).name("register"),

        // Dashboard requires authenticate middleware
        get!("/dashboard", controllers::dashboard::index)
            .middleware(crate::middleware::authenticate())
            .name("dashboard"),
    }
}
```

The macro returns a `Router` you hand to `Application::routes(…)`. See
[Routing](routing.md).

### `src/bin/console.rs`

Your per-project console binary. Runs as `cargo run --bin console
<subcommand>`. The default scaffolds with stubs for `db:seed`,
`model:prune`, etc.; your own `#[command]`-annotated handlers in
`src/commands/` get picked up automatically via inventory:

```bash
cargo run --bin console queue:work        # built-in
cargo run --bin console schedule:run      # built-in
cargo run --bin console workflow:work     # built-in
cargo run --bin console db:seed           # built-in
cargo run --bin console make:something    # your custom command
```

See [Console](console.md).

### `src/commands/`

Where your `#[command]`-annotated console handlers live:

```rust
use suprnova::{Command, command};

#[command(name = "report:daily")]
pub struct DailyReport;

#[async_trait::async_trait]
impl Command for DailyReport {
    async fn run(&self) -> Result<(), suprnova::FrameworkError> {
        // …
        Ok(())
    }
}
```

`suprnova make:command report-daily` scaffolds the file and adds it to
`src/commands/mod.rs`. See [Console](console.md) for the typed-args
variant.

### `src/config/`

Typed configuration structs. The scaffold ships `database.rs` and
`mail.rs`; add your own for any subsystem your app cares about:

```rust
use suprnova::Config;

#[derive(Clone, serde::Deserialize)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
}

pub fn register() -> Result<(), suprnova::FrameworkError> {
    Config::section("database", |env| DatabaseConfig {
        url: env.required("DATABASE_URL")?,
        max_connections: env.optional("DB_MAX_CONNECTIONS").unwrap_or(10),
    })?;
    Ok(())
}
```

See [Configuration](configuration.md).

### `src/controllers/`

HTTP handler functions. One module per resource. Each `pub async fn`
that takes a `Request` and returns a `Response` is callable from a
route.

### `src/middleware/`

Middleware implementations. The scaffold ships `logging` and
`authenticate`; you add your own here as `pub struct Foo` with
`impl Middleware for Foo`. Register them globally in `bootstrap.rs`
or apply per-route via `.middleware(…)` in the `routes!` tree. See
[Middleware](middleware.md).

### `src/migrations/`

SeaORM migrators. The scaffold ships a handful for the auth + workflow
tables. `suprnova make:migration <name>` adds a new one. `suprnova
migrate`, `migrate:rollback`, `migrate:status`, `migrate:fresh`,
`db:sync` all operate on this directory. See [Migrations](migrations.md).

### `src/models/`

Your Eloquent models. One file per model, each a `#[suprnova::model]`
struct. The scaffold ships `user.rs`; everything else you add via
`suprnova make:model <Name>`. See [Eloquent](eloquent.md).

### `src/actions/`

Single-method invokable controllers. Optional pattern — use them when
a controller would have exactly one method and you'd rather call it
"Action" than wrap it. The scaffold ships an example you can delete or
adapt. See [Actions](actions.md).

### `frontend/`

The Vite + Inertia SPA. This is a normal frontend project — `package.json`,
`vite.config.ts`, `tsconfig.json`, an `index.html` Vite entry, source
under `src/`. The Inertia client setup lives in `src/main.{tsx,ts}` and
the page components in `src/pages/`. TypeScript types for your Rust
`#[derive(InertiaProps)]` props are regenerated into
`src/types/inertia-props.ts` by `suprnova generate-types`.

See [Frontend](frontend.md).

### `public/assets/`

Where Vite drops the production build (`npm run build`). The Suprnova
server serves this directory as static assets at `/assets/*` in
production.

## Directories you'll add as the app grows

The scaffold gives you the minimum — enough to ship the welcome flow
and a protected dashboard. Real apps grow more subsystems. Common
additions:

| Directory | When you add it |
|---|---|
| `src/jobs/` | First time you `Queue::dispatch(SomeJob)`. See [Queues](queues.md). |
| `src/listeners/` | First time you `Event::listen`. See [Events](events.md). |
| `src/observers/` | First time you implement `Observer<MyModel>`. See [Eloquent](eloquent.md#observers). |
| `src/notifications/` | First time you implement a `Notification`. See [Notifications](notifications.md). |
| `src/mail/` | First time you implement a `Mailable`. See [Mail](mail.md). |
| `src/policies/` | First time you write a `#[policy]`. See [Authorization](authorization.md). |
| `src/factories/` | First time you write a `Factory<Model>` for tests. See [Eloquent Factories](eloquent-factories.md). |
| `src/seeders/` | First time you write a `Seeder` for `db:seed`. See [Seeding](seeding.md). |
| `src/events/` | First time you `#[derive(Event)]` your own event type. See [Events](events.md). |
| `src/broadcasting/` | First time you define a private/presence `Channel`. See [Broadcasting](broadcasting.md). |
| `src/ws/` | First time you write a `ws!()` handler. See [WebSockets](websockets.md). |
| `src/supervisors/` | First time you implement a long-running `Supervisor`. See [Supervisors](supervisors.md). |
| `src/payments/` | First time you wire up Stripe/Paddle for your app. See [Payments](payments.md). |
| `src/props/` | When you want to keep `#[derive(InertiaProps)]` structs separate from controllers. |
| `resources/views/` | First time you add a Tera template for mail bodies. |
| `storage/` | First time you write files to the local filesystem disk (see [File Storage](filesystem.md)). |
| `tests/` | First time you write an integration test. |

You don't have to ask permission — `mkdir src/jobs` and add
`pub mod jobs;` to `src/lib.rs`, and you're done. The framework
doesn't enforce the directory names; the conventions exist so other
Suprnova developers can find things quickly.

## The dogfood `app/` in this repo

If you're reading this from inside the Suprnova repo itself, you'll
see an `app/` directory at the root that uses every framework feature
together. That's our internal test bed — it exercises payments,
broadcasting, web push, workflows, supervisors, etc. all at once. It's
NOT a clean reference for a new app; the scaffold output above is
deliberately smaller and easier to learn from. Read `app/` once you
want to see a maximal example of how the pieces compose.

## Next

- [Configuration](configuration.md) — how `.env` becomes typed config
- [Application Bootstrap](bootstrap.md) — what `bootstrap.rs` actually
  does
- [Routing](routing.md) — your first route
- [Service Container](container.md) — how `App::bind` and `App::get`
  work
