# From Laravel

If you've shipped Laravel apps, you already know 80% of Suprnova. This
chapter maps your habits to the Rust equivalent so you can get productive
fast. We'll show the patterns you reach for daily, the patterns that change
shape, and the few things Rust gives you for free that PHP can't.

## TL;DR side-by-side

| You wrote in Laravel | You write in Suprnova |
|---|---|
| `composer create laravel/laravel my-app` | `suprnova new my-app --frontend svelte` |
| `php artisan serve` | `suprnova serve` |
| `php artisan migrate` | `suprnova migrate` |
| `php artisan make:controller PostController` | `suprnova make:controller post` |
| `Route::get('/posts/{id}', [PostController::class, 'show'])` | `get!("/posts/{id}", controllers::post::show)` (in `routes!`) |
| `class Post extends Model` | `#[suprnova::model] struct Post { … }` |
| `Post::find($id)` | `Post::find(id).await?` |
| `Post::where('status', 'published')->get()` | `Post::query().db_where("status", "published").get().await?` |
| `Auth::user()` | `Auth::user().await?` |
| `Cache::remember('key', 60, fn() => …)` | `Cache::remember("key", Some(Duration::from_secs(60)), \|\| async { … }).await?` |
| `Queue::push(new SendEmail($user))` | `Queue::push(SendEmail { user_id }).await?` |
| `Mail::to($u)->send(new Welcome($u))` | `Mail::to(&u.email).send(WelcomeMail { user: u }).await?` |
| `Storage::disk('s3')->put($path, $bytes)` | `Storage::disk("s3")?.put(&path, bytes).await?` |
| `Notification::send($u, new Invoice($i))` | `Notify::send(&u, &InvoiceNotification { invoice }).await?` |
| `Gate::allows('update', $post)` | `Gate::allows::<PostPolicy, _>("update", &user, &post).await?` |
| `request()->validate([...])` | `#[handler]` extracts an `#[derive(Data, Validate)]` arg directly |
| `event(new OrderShipped($order))` | `Event::dispatch(OrderShipped { order }).await?` |
| `Bus::dispatch(new ProcessFoo($x))` | `Bus::dispatch(ProcessFoo { x }).await?` |
| `php artisan schedule:list` | `suprnova schedule:list` |
| `php artisan tinker` | (no REPL — write a one-off `cargo run` script or test) |
| `composer require league/csv` | `cargo add csv` |

## The mental model shift

### Async, everywhere

The biggest change: every database call, HTTP call, file I/O, cache call,
queue push — anything that crosses a boundary — is `async` and you call
it with `.await?`. Once you've done it for a few hours, it disappears
into the rhythm. Until then, the compiler will point at every spot you
forgot.

```rust
// Laravel
$user = User::find($id);
$user->subscribe($plan);
Mail::to($user)->send(new Welcome($user));

// Suprnova
let user = User::find(id).await?;
user.subscribe(&plan).await?;
Mail::to(&user.email).send(WelcomeMail { user }).await?;
```

`?` is Rust's "early return on error". A handler returns
`Result<HttpResponse, HttpResponse>` (aliased as `Response`), so a `?`
on a DB error short-circuits into your error converter and the client
gets a proper 500 (or 4xx, depending on the error kind). You almost
never have to write a `try/catch` — `?` does it.

### Compile-time models

Where Eloquent reads your DB schema at runtime, Suprnova reads it at
compile time:

```rust
#[suprnova::model(table = "posts")]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
```

That's it — that struct IS the Eloquent model. You get
`Post::find`, `Post::query()`, `Post::create`, `post.update(...)`,
`post.delete()`, soft deletes (with `#[model(soft_deletes)]`),
timestamps, observers, the works. The macro generates a SeaORM
`Entity`, `Model`, `ActiveModel`, and `Column` enum, and impls the
Suprnova `Model` trait — but you depend on `Post`, not any of those.

If you rename a column in a migration, the struct doesn't match the
DB schema anymore — and depending on your config, either the compiler
catches it at build time or the type-coerced cast fails on first
query. Either way you find out before staging, not after.

### Single binary

There's no PHP-FPM, no nginx config reading `index.php`, no `composer
install` on deploy. `cargo build --release` gives you one statically
linked binary. `scp` it to a server, `systemd` it, done. Or build a
container — `FROM scratch` works.

We have [deployment recipes](deployment.md) for Railway, Digital
Ocean, and Hetzner. The common shape: build the binary, ship the
binary, set env vars, run.

## Mapping the framework

### Routes

`routes!` plays the role of `routes/web.php` and `routes/api.php`
combined.

```rust
use suprnova::{routes, get, post, put, delete};
use crate::controllers;

routes! {
    get!("/", controllers::home::index).name("home"),

    // Route group with shared prefix + middleware
    group("/admin")
        .middleware(crate::middleware::admin())
        .routes(routes! {
            get!("/users", controllers::admin::users::index).name("admin.users"),
            post!("/users", controllers::admin::users::store),
            put!("/users/{id}", controllers::admin::users::update),
            delete!("/users/{id}", controllers::admin::users::destroy),
        }),

    // Resource routing (Laravel's Route::resource)
    resource!("posts", controllers::post),
}
```

Full reference: [Routing](routing.md). Differences worth knowing:

- Group middleware is **flattened** into each route's middleware list
  at register time (not run as a separate chain layer) — this means
  there's no extra runtime cost for grouping.
- Both Laravel's `{id}` and Rails-style `:id` syntax work; they're
  normalised internally.
- Named routes resolve via `route("posts.show", &[("id", "42")])` and
  there's a signed-URL variant for time-limited links.

### Controllers

A controller is just a free function returning `Response`:

```rust
use suprnova::{Request, Response, json_response, HttpResponse};
use crate::models::Post;

pub async fn show(req: Request) -> Response {
    let id = req.param("id").unwrap_or("0").parse::<i64>()?;
    let post = Post::find_or_fail(id).await?;
    json_response!({ "post": post })
}
```

You can also use the `#[handler]` macro to extract typed args (route
params, query, body, the request itself, container services) at the
signature:

```rust
use suprnova::handler;

#[handler]
pub async fn show(post: post::Model) -> Response {
    // Route model binding ran automatically; `post` is the loaded row.
    json_response!({ "post": post })
}
```

The `post::Model` type comes from the model's generated module — that's
the signal `#[handler]` uses to pick route model binding over the
default form-request extraction. If the row doesn't exist, the binding
returns a 404 before your code runs — same behaviour as Laravel's
implicit binding.

Action structs (single-method "invokable" controllers, Laravel-style) are
supported too: see [Actions](actions.md).

### Eloquent

The dual-API query builder takes either Laravel names or Rust-idiomatic
names — both work, pick whichever reads cleanly at the call site.

```rust
// Laravel surface
let active = User::query()
    .db_where("status", "active")
    .order_by_desc("created_at")
    .limit(20)
    .get()
    .await?;

// Rust surface (identical result)
let active = User::query()
    .filter("status", "active")
    .order_by_desc("created_at")
    .take(20)
    .get()
    .await?;
```

`db_where` is the Laravel-side name (the bare `where` collides with the
Rust keyword). `filter` is the Rust-idiomatic alias. Both exist; both
do the same thing. For non-equality operators, reach for `db_where_op`
(or its `filter_op` alias): `.db_where_op("status", "!=", "archived")`.
See the [Eloquent reference](eloquent.md) — it's the longest chapter
for a reason, the surface is wide.

### Auth

```rust
use suprnova::{Auth, Credentials};

// In a handler:
let user = Auth::user().await?;   // Option<Arc<dyn Authenticatable>>
let id = user.as_ref().map(|u| u.get_auth_identifier());

// Logging in (e.g. inside your login controller):
let creds = Credentials::password("alice@x.com", "secret");
Auth::attempt(&creds, false).await?;

// Logging out:
Auth::logout().await?;
```

Guards, providers, sessions, remember-me, email verification, password
reset, brute-force throttling, TOTP 2FA, and OAuth are all here. The
auth-flows surface mirrors Laravel Fortify. See
[Authentication](authentication.md) and [Auth Flows](auth-flows.md).

### Migrations

You write SeaORM migrators. The shape will look familiar even if the
syntax is new:

```rust
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.create_table(
            Table::create()
                .table(Alias::new("posts"))
                .if_not_exists()
                .col(ColumnDef::new(Alias::new("id")).big_integer().primary_key().auto_increment())
                .col(ColumnDef::new(Alias::new("title")).string().not_null())
                .col(ColumnDef::new(Alias::new("body")).text().not_null())
                .to_owned()
        ).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.drop_table(Table::drop().table(Alias::new("posts")).to_owned()).await
    }
}
```

`suprnova make:migration create_posts_table` scaffolds the file.
`suprnova migrate`, `migrate:rollback`, `migrate:status`, `migrate:fresh`
all do what you'd expect. `suprnova db:sync` runs migrations and
regenerates the SeaORM entities the macro layer compiles against.
See [Migrations](migrations.md).

### Queues and scheduling

```rust
use suprnova::{FrameworkError, Job, Queue, async_trait};
use serde::{Deserialize, Serialize};

// Define a job — the data lives on the struct, the contract lives on
// `impl Job`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SendWelcomeEmail {
    pub user_id: i64,
}

#[async_trait]
impl Job for SendWelcomeEmail {
    fn job_name() -> &'static str {
        "SendWelcomeEmail"
    }

    async fn handle(self) -> Result<(), FrameworkError> {
        let user = User::find_or_fail(self.user_id).await?;
        Mail::to(&user.email).send(WelcomeMail { user }).await?;
        Ok(())
    }
}

// Push it onto the queue:
Queue::push(SendWelcomeEmail { user_id: user.id }).await?;

// Or with a delay:
Queue::later(
    std::time::Duration::from_secs(60),
    SendWelcomeEmail { user_id },
).await?;
```

Workers run with `cargo run --bin console queue:work`. Drivers include
memory and sync (in-process, for tests), database, redis, and null.
Batches, chains, unique jobs, retries, backoff, middleware, failed-job
store — all there. See [Queues](queues.md).

Scheduling uses the `Task` trait and the per-project scheduler binary:

```rust
use suprnova::{Task, TaskResult, async_trait};

pub struct DailyDigest;

#[async_trait]
impl Task for DailyDigest {
    async fn handle(&self) -> TaskResult {
        // …
        Ok(())
    }
}

// Register inside bootstrap (e.g. via Schedule::call / .task / .add):
//   schedule.add(schedule.task(DailyDigest).daily().at("03:00").name("daily-digest"));
```

See [Task Scheduling](scheduling.md).

### Mail, notifications, broadcasting

These follow Laravel one-to-one. `Mailable` is a derive macro;
`Notifiable` is a trait on your User model; channels are
`mail`/`database`/`broadcast`/`webpush`; broadcasting supports
public, private, and presence channels. See [Mail](mail.md),
[Notifications](notifications.md), [Broadcasting](broadcasting.md).

### Frontend

There's no Blade. Instead, the frontend is a real SPA via Inertia.js,
and you pass typed props from Rust:

```rust
use suprnova::{inertia_response, InertiaProps, Request, Response};

#[derive(InertiaProps, serde::Serialize)]
pub struct ShowProps {
    pub post: Post,
    pub comments: Vec<Comment>,
}

pub async fn show(req: Request) -> Response {
    let id: i64 = req.param("id").unwrap_or("0").parse().unwrap_or(0);
    let post = Post::find_or_fail(id).await?;
    let comments = post.comments().get().await?;
    inertia_response!(&req, "Posts/Show", ShowProps { post, comments })
}
```

`Posts/Show` is a Svelte component (or React, or Vue — your starter
picks). TypeScript types for the props are generated automatically from
the `InertiaProps` derive — run `suprnova generate-types` after adding a
new prop struct and the frontend gets typed bindings.

If you've used Inertia in Laravel via `inertia()`, this is the same
thing — just typed end-to-end. See the [Frontend overview](frontend.md).

## Things that change shape

A few things move differently in Suprnova. None of them are blockers,
but they're worth knowing up front.

### No service providers

Laravel has dozens of service providers registering bindings, observers,
view composers, etc. Suprnova has **one** bootstrap function in your
app's `bootstrap.rs`. You register everything there, in order. It's not
elegant but it's transparent — you can see in 30 lines exactly what
your app boots.

```rust
// bootstrap.rs
use std::sync::Arc;

pub async fn bootstrap() -> Result<(), suprnova::FrameworkError> {
    suprnova::App::bind::<dyn MyService>(Arc::new(MyServiceImpl::new()));
    suprnova::Event::listen::<OrderShipped, _>(Arc::new(SendShipmentNotification)).await;
    crate::observers::register();
    Ok(())
}
```

The [Container](container.md) and [Bootstrap](bootstrap.md) chapters
have the detail.

### Configuration is typed

Where Laravel uses `config('app.timezone')` returning whatever-the-array-says,
Suprnova has typed config structs:

```rust
let cfg = suprnova::Config::get::<AppConfig>()?;
let tz = &cfg.timezone;   // &str, not mixed
```

You can register your own typed config sections. See [Configuration](configuration.md).

### No facades-as-aliases

Laravel facades like `DB::` are class-aliases configured in `config/app.php`.
Suprnova facades are real modules at the crate root:

```rust
use suprnova::{Auth, Cache, DB, Event, Gate, Mail, Notify, Queue, Schedule, Storage};
```

Same surface, no global aliasing needed.

### Compile times are real

Rust compile times are not PHP. A clean build of a fresh Suprnova app
takes 1–2 minutes; incremental builds during development are a few
seconds. The dev workflow is the same — `suprnova serve` watches for
changes and rebuilds — but you'll feel it the first time you change a
macro and recompile a downstream crate. Caching pays for itself fast.

### The borrow checker exists

Most controllers and handlers never touch a lifetime annotation — the
framework's signatures hide them. When the borrow checker yells at you,
it's usually because you tried to hold a reference across an `.await`
that crossed a mutex or held a DB transaction across an awaited call
that needed exclusive access. The errors are clear and the fixes are
usually `.clone()` or restructure-into-smaller-scopes.

### No `tinker` REPL

There isn't a REPL. The closest equivalent is a one-off `cargo run`
script in `examples/`, or a `#[suprnova_test]` test that exercises the
thing you're debugging. Most of what you'd do in tinker (poke at a
model, fire a notification, dispatch a job) is a 5-line test.

## Where Laravel chapters land

Quick lookup if you know what you're after but not where it lives:

| Laravel topic | Suprnova chapter |
|---|---|
| Lifecycle | [Request Lifecycle](lifecycle.md) |
| Service Container | [Service Container](container.md) |
| Service Providers | [Application Bootstrap](bootstrap.md) |
| Facades | [Service Container](container.md) |
| Routing | [Routing](routing.md) |
| Middleware | [Middleware](middleware.md) |
| CSRF Protection | [CSRF Protection](csrf.md) |
| Controllers | [Controllers](controllers.md) |
| Requests | [Requests](requests.md) |
| Responses | [Responses](responses.md) |
| URL Generation | [URL Generation](urls.md) |
| Session | [Session](session.md) |
| Validation | [Validation](validation.md) |
| Error Handling | [Error Handling](errors.md) |
| Logging | [Logging](logging.md) |
| Artisan Console | [Console](console.md) + [CLI Reference](cli.md) |
| Broadcasting | [Broadcasting](broadcasting.md) |
| Cache | [Cache](cache.md) |
| Events | [Events](events.md) |
| File Storage | [File Storage](filesystem.md) |
| HTTP Client | [HTTP Client](http-client.md) |
| Mail | [Mail](mail.md) |
| Notifications | [Notifications](notifications.md) |
| Queues | [Queues](queues.md) |
| Rate Limiting | [Rate Limiting](rate-limiting.md) |
| Task Scheduling | [Task Scheduling](scheduling.md) |
| Authentication | [Authentication](authentication.md) |
| Authorization | [Authorization](authorization.md) |
| Email Verification | [Auth Flows](auth-flows.md) |
| Password Reset | [Auth Flows](auth-flows.md) |
| Encryption | [Encryption](encryption.md) |
| Hashing | [Hashing](hashing.md) |
| Database | [Database](database.md) |
| Query Builder | [Query Builder](queries.md) |
| Pagination | [Pagination](pagination.md) |
| Migrations | [Migrations](migrations.md) |
| Seeding | [Seeding](seeding.md) |
| Eloquent | [Eloquent](eloquent.md) |
| Eloquent: Relationships | [Relationships](eloquent-relationships.md) |
| Eloquent: Collections | [Collections](eloquent-collections.md) |
| Eloquent: Mutators / Casts | [Mutators & Casts](eloquent-mutators.md) |
| Eloquent: API Resources | [API Resources](eloquent-resources.md) |
| Eloquent: Serialization | [Serialization](eloquent-serialization.md) |
| Eloquent: Factories | [Factories](eloquent-factories.md) |
| Testing | [Testing](testing.md) |
| HTTP Tests | [HTTP Tests](http-tests.md) |
| Database Testing | [Database Tests](database-testing.md) |
| Mocking | [Mocking & Fakes](mocking.md) |
| Cashier (Stripe) | [Payments: Stripe](payments-stripe.md) |
| Cashier (Paddle) | [Payments: Paddle](payments-paddle.md) |
| Sanctum / Passport | (not yet — token auth via torii integration) |
| Horizon | (not yet — queue introspection is built-in) |
| Telescope / Pulse | (deferred to v2+) |

Things Laravel has that Suprnova doesn't (yet):

- Telescope / Pulse (observability surface) — basic [observability](observability.md) ships, the dashboards don't
- Sanctum / Passport token auth — torii integration covers OAuth and session auth, token auth is on the roadmap
- Horizon — queue introspection is built into the framework, no separate dashboard
- Starter kits (Breeze / Jetstream / Spark) — coming after v0.1.0
- Blade — by design; Inertia is the frontend story
- Localization helpers — on the roadmap; English-only today

## Next

- [Installation](installation.md) — get a project running
- [Quickstart](quickstart.md) — build a tiny app in 5 minutes
- [Routing](routing.md) — the natural next chapter from here

Or jump anywhere via [`documentation.md`](documentation.md).
