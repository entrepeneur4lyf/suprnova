# Suprnova

**A Laravel-inspired web framework for Rust.**

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![CHANGELOG](https://img.shields.io/badge/CHANGELOG-keep%20a%20changelog-orange)](./CHANGELOG.md)

![Suprnova - A Laravel-inspired web framework for Rust](manual/suprnova_header.jpg)

Suprnova is a full-stack Rust web framework with Laravel 13's developer
experience and Tokio's runtime model. Familiar API surfaces — `Auth::login`,
`Cache::remember`, `Mail::to`, `Event::dispatch`, Eloquent-style models,
`#[handler]`, `#[command]`, `routes!` — sit on top of a hyper / SeaORM /
async-trait stack designed for long-lived connections, in-process workers,
and concurrent IO. No request-per-process compromise.

```bash
cargo install --git https://github.com/entrepeneur4lyf/suprnova.git suprnova-cli
suprnova new myapp --frontend svelte
cd myapp
suprnova serve
```

Your app is now serving at `http://localhost:8765`, with a Vite dev server
proxied for the frontend.

## Quick taste

If you've used Laravel, this should feel like home — only typed.

```rust
use std::time::Duration;
use suprnova::{handler, routes, attrs, inertia_response, json_response, Response};
use suprnova::{Auth, Cache, Event, RouteParam};
use crate::models::Post;
use crate::events::PostCreated;
use crate::requests::CreatePostRequest;

routes! {
    get!("/",             home),
    get!("/posts/{post}", show),
    post!("/posts",       store).middleware(Authenticate),
}

#[handler]
async fn home() -> Response {
    let popular = Cache::remember(
        "posts.popular",
        Some(Duration::from_secs(60)),
        || async { Post::query().db_where_op("views", ">", 1000).get().await },
    )
    .await?;
    Ok(inertia_response!("Home", { "posts": popular }))
}

#[handler]
async fn show(RouteParam(post): RouteParam<Post>) -> Response {
    Ok(json_response!({ "post": post }))
}

#[handler]
async fn store(req: CreatePostRequest) -> Response {
    let post = Post::create(attrs! {
        user_id: Auth::id().ok_or_else(|| FrameworkError::Unauthorized)?,
        title:   req.title,
        body:    req.body,
    })
    .await?;
    Event::dispatch(PostCreated { post: post.clone() }).await?;
    Ok(inertia_response!("Posts/Created", { "post": post }))
}
```

`RouteParam<Post>` applies the model's global scopes and soft-delete
filter automatically. `Post::query()` is the Eloquent builder
(`db_where_op` is the Laravel-side alias of `filter_op` for arbitrary
SQL operators). The `#[handler]` macro pulls FormRequests out of the
body, route params out of the URI, and authenticated users out of the
session — all type-checked.

## What's in the box

The Laravel-13 parity surface plus the Rust-native wins:

| Layer | What ships |
|---|---|
| **HTTP & routing** | `Router`, named routes, route groups & prefixes, parameter binding, resource routing, signed URLs, redirect helpers (`Redirect::to`/`back`/`route`/`with_errors`/…), `#[handler]` macro, 100% type-checked |
| **Middleware** | CORS, CSRF, session, request-timeout, request-id, throttle / login-throttle, signed-URL verify, authenticated, email-verified, brute-force, custom global/group/per-route |
| **Inertia 3 bridge** | `InertiaProps` derive + TypeScript codegen, partial reloads, deferred / lazy props, version mismatch handling, SSR loopback, `#[handler]` integration, three starters: **Svelte 5 / React 19 / Vue 3.5** |
| **Eloquent ORM** | `#[suprnova::model]` macro, 11 relation kinds (hasMany / belongsToMany / morph / polymorphic / through), eager loading, soft deletes, observers, global & local scopes, casts, 16 lifecycle events, factories + seeders, `Collection<M>`, 3 paginators, chunk/lazy/cursor iteration, multi-connection R/W split, transactions + savepoints + retry-on-deadlock |
| **Auth** | `Auth::user`/`login`/`once`/`check`, named guards via `AuthManager`, remember-me, 2FA TOTP with recovery codes, email verification, password reset, brute-force lockout, login throttle, role/permission gates, `#[policy]` registration |
| **Database** | SeaORM-backed migrations + entity codegen, four databases: **SQLite / Postgres / MySQL / MariaDB** (MariaDB rides the MySQL driver and adds a native vector driver), `DB::transaction` with savepoints, query logging, multi-connection registry |
| **Cache** | Memory, file, Redis — `Cache::remember` + lock-based `Cache::lock` + tags + atomic Redis Retry-After |
| **Queues & jobs** | Memory, sync, Redis, database — middleware pipeline, batches, chains, retry schedules, failed-job store, unique jobs, `#[job]` macro |
| **Events & bus** | `Event::dispatch`, `Listener`/`Subscriber`, queued listeners, panic-isolated dispatcher, command/query bus |
| **Notifications** | Mail / database / broadcast / Web Push channels, anonymous notifications, deferred dispatch |
| **Mail** | SMTP, Mailgun, Postmark, SendGrid, Resend, SES, log + in-memory transports, Markdown templates via Tera, fake() helper, queued mail |
| **Broadcasting & WebSocket** | Channels (public / private / presence), `BroadcastHub` trait, sea-streamer fanout adapter, JSON-envelope protocol, supervised heartbeats with auto-restart |
| **Filesystem** | Local + S3 (R2 / B2 / MinIO compatible) via OpenDAL, path-traversal guard, atomic copy |
| **Vector** | Memory, **Qdrant**, **Pinecone**, **MariaDB native `VECTOR(N)`** (HNSW + cosine/euclid/L1/L2) — first-class trait + drivers, no Postgres-only gatekeeping |
| **Payments** | Generic `Payment` / `Subscription` / `CustomerStore` / `WebhookHandler` traits + DB mirror; **Stripe** and **Paddle** reference adapters; webhook UNIQUE idempotency |
| **Validation** | `Required`, `Email`, `Min`/`Max`/`Between`, `RequiredIf`/`With`/`WithAll`/`Unless`, `Unique` (async), `Confirmed`, custom rules via traits, `validator` derive integration |
| **Scheduling** | `Schedule::call` / `command` / `job`, cron expressions, `runInBackground`, `withoutOverlapping`, supervised execution |
| **Workflows** | Durable steps via `#[workflow_step]`, `#[workflow]` orchestration, panic-recovery on the queue, exponential backoff with strict caps |
| **Console** | Per-project `console` binary (the Rust analogue of `php artisan`), `#[command]` + `#[derive(Command)]` typed args, `make:*` generators, `db:seed` |
| **Observability** | Structured `tracing` everywhere, request IDs end-to-end, OpenTelemetry support, DB query logging via `QueryExecuted` events |
| **Testing** | `#[suprnova_test]`, in-memory SQLite via `TestDatabase`, fakes (`Mail::fake()`, `Queue::fake()`, `Event::fake()`, `BroadcastHub` recorder), `expect!` macro, `handle_request` in-process driver |
| **Feature flags** | `DatabaseEvaluator` + `CachedEvaluator` + admin CRUD + sub-second propagation via `FeatureSync` |
| **Idempotency, rate-limit, CORS, CSRF, sessions, hashing, crypto** | First-class; each subsystem ships fail-open vs fail-closed as an explicit policy choice |

## Starter kits

Don't start from an empty scaffold — fork a kit:

- **[Nebula](https://github.com/entrepeneur4lyf/Nebula)** — authentication
  (Breeze-tier): register, email verification, login with remember-me, password
  reset, and profile management, on Inertia 3 + Svelte 5.
- **[Pulsar](https://github.com/entrepeneur4lyf/Pulsar)** — a full product site
  and community on Vue 3.5 + Vuetify: everything in Nebula plus a marketing
  landing, dashboard, a Markdown docs pipeline, a blog with RSS, member
  profiles, taxonomy, role-based access control, and admin/moderation surfaces.

See **[Starter Kits](./manual/starter-kits.md)** for the full rundown, or run
`suprnova new` for the plain scaffold on any of the three frontends.

## End-to-end type safety

Define props in Rust once; use them in TypeScript with full autocomplete.

```rust
use suprnova::{handler, InertiaProps, inertia_response, Response};

#[derive(InertiaProps)]
pub struct HomeProps {
    pub title: String,
    pub user: UserDto,
}

#[derive(InertiaProps)]
pub struct UserDto {
    pub name: String,
    pub email: String,
}

#[handler]
pub async fn index() -> Response {
    Ok(inertia_response!("Home", HomeProps {
        title: "Welcome!".into(),
        user: UserDto {
            name: "Ada".into(),
            email: "ada@example.com".into(),
        },
    }))
}
```

Run `suprnova generate-types` and your `frontend/src/types/inertia-props.ts`
mirrors the Rust shape exactly. Change a field, regenerate, the compiler
points at every component that needs to update.

## Durable workflows

Workflow steps survive process restarts and retry with exponential backoff
+ jitter (strict cap, no doubling past it):

```rust
use suprnova::{workflow, workflow_step, start_workflow, FrameworkError};

#[workflow_step]
async fn fetch_user(user_id: i64) -> Result<String, FrameworkError> { ... }

#[workflow_step]
async fn send_welcome_email(user: String) -> Result<(), FrameworkError> { ... }

#[workflow]
async fn welcome_flow(user_id: i64) -> Result<(), FrameworkError> {
    let user = fetch_user(user_id).await?;
    send_welcome_email(user).await?;
    Ok(())
}

// Enqueue & run the worker:
let handle = start_workflow!(welcome_flow, 123).await?;
```

```bash
suprnova workflow:work
```

## Documentation

- **[Manual](./manual/README.md)** — 100+ chapters, every public subsystem.
  Pick a reading path: [From Laravel](./manual/from-laravel.md) (if you
  know `Auth::user()` / Eloquent / Blade) or
  [From Rust Web](./manual/from-rust-web.md) (if you know Axum / Actix / Rocket).
- **[Quickstart](./manual/quickstart.md)** — small app end-to-end.
- **[CHANGELOG.md](./CHANGELOG.md)** — keep-a-changelog format.
- **[ROADMAP.md](./ROADMAP.md)** — design principles, what's shipped,
  what's next. The working agreement: **full implementations only, well
  tested, production-ready.** A track ships when it's done, not when it
  has a prototype.

## Distribution model

Suprnova distributes via git, not crates.io. Generated apps depend on
`suprnova = { git = "https://github.com/entrepeneur4lyf/suprnova.git" }`;
the CLI installs via `cargo install --git`. Adapter crates
(`suprnova-payments-stripe`, `suprnova-payments-paddle`,
`suprnova-web-push`) follow the same model. This keeps the framework's
internal API churn pre-1.0 from costing downstream a constant stream of
SemVer bumps.

## License

MIT, © 2025 Suprnova contributors. Forked from [Kit](https://github.com/dayemsiddiqui/kit) (MIT, © Dayem Siddiqui) — see ROADMAP.md for the relationship to upstream.
