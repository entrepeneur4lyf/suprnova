# Introduction

Suprnova is a web framework for Rust that gives you Laravel's developer
experience on top of Tokio. You write controllers and Eloquent-style models;
the framework gives you concurrency, type safety, and a single-binary deploy.

```rust
use suprnova::{Request, Response, json_response};

pub async fn show(req: Request) -> Response {
    let id = req.param("id").unwrap_or("0");
    json_response!({ "id": id, "name": "Alice" })
}
```

```rust
use suprnova::{model, Model};

#[model(table = "users")]
pub struct User {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

// Then anywhere:
let user = User::find(42).await?;
let admins = User::query().db_where("role", "admin").get().await?;
let alice = User::create(attrs!{ name: "Alice", email: "alice@x.com" }).await?;
```

If you wrote that in Laravel last week, the Rust version above will feel
identical — same chain shape, same method names, same defaults. The
difference is what happens underneath: Tokio instead of FPM, one binary
instead of a PHP runtime, compile-time type checks on every column.

## Why Suprnova exists

Laravel solved the productivity problem for backend web development. The
patterns work. After ten years of refinement, very little gets in your way
when you're building a real product. But PHP's request-per-process model
keeps two things out of reach: cheap long-lived connections (WebSockets,
SSE, server-pushed notifications without polling) and trivial concurrent
I/O inside one request handler.

Rust gives you both for free with Tokio. The problem is that the Rust web
ecosystem makes you build the productivity layer yourself: pick an HTTP
crate, pick an ORM, pick a migration tool, pick a queue, wire it all
together, design your own conventions. Each app reinvents what Laravel
already standardised.

Suprnova is what happens when you copy Laravel's conventions onto Tokio.
You get:

- **Same surface** — `routes!`, `Auth::user()`, `Cache::remember`,
  `Mail::send`, `Queue::push`, `Storage::disk("s3")`, `Notify::send`,
  `Schedule::call`, `Gate::allows`, the Eloquent query builder, soft deletes,
  factories, observers, broadcasting, all of it
- **Different engine** — async-everywhere, long-lived connections as
  first-class citizens, single statically-linked binary, no preforking, no
  opcache, no FPM
- **Type safety** — your models, routes, and event payloads are checked at
  compile time; broken refactors don't reach staging
- **A real frontend story** — Inertia.js bridges to Svelte 5, React 19, or
  Vue 3.5 starters, no separate API to maintain

## Design principles

These are the principles the framework's authors hold themselves to.
They explain why a chapter says what it says.

**1. Parity comes from the Laravel changelog.** When Laravel ships a
feature, Suprnova tracks it. Today's baseline is Laravel 13.x and every
shipped subsystem has been audited against it. The
[Laravel Parity Map](parity.md) is the explicit feature-by-feature table.

**2. Diverge intentionally where Rust makes things better.** Where Laravel
made a PHP-shaped choice we don't have to make in Rust, Suprnova picks
the Rust-shaped one and says so. The biggest example is concurrency:
WebSockets, broadcasting, background workers, and HTTP/2 server-push
are first-class, not bolted on. Where you'll see this called out in a
chapter, look for **"Why Suprnova diverges"** boxes.

**3. No gatekeeping.** Laravel restricts some features to one backend
(e.g. vector search via Postgres `pgvector`). Suprnova treats backends
as drivers — `Vector::driver("qdrant")`, `Vector::driver("pinecone")`,
`Vector::driver("mariadb")`, `Cache::driver("redis")`, `Mail::driver("ses")`.
You pick the right tool; we don't pick for you.

**4. Suprnova is the API surface.** Internally we use SeaORM, hyper, Tokio,
serde, sqlx, validator, lettre, and dozens more. None of that should
appear in your code. You depend on `suprnova::*`. We re-export everything
you'll touch — including SeaORM's `Entity`, `Column`, `ActiveModel`,
`QueryFilter`, etc. — under the framework root. The escape hatch
(`use suprnova::sea_orm;`) exists for the rare case the curated surface
doesn't cover, but you should almost never need it.

## What's in the box

A non-exhaustive map. The full list is in [`documentation.md`](documentation.md).

| Area | What ships |
|---|---|
| **HTTP** | `routes!` macro, controllers, middleware, requests, responses, route model binding, signed URLs, resource routing, redirect helpers, CORS, CSRF, idempotency keys, timeout, rate limiting, structured errors with panic recovery |
| **Database** | SeaORM under the hood, multi-driver (Postgres, MySQL, MariaDB, SQLite), migrations, seeders, query builder, transactions with savepoints, multi-connection read/write split |
| **Eloquent** | `#[suprnova::model]` macro, all 11 relation kinds, eager loading, soft deletes, prunable, scopes (local + global), 16 lifecycle events, observers, 22 built-in casts, accessors/mutators, three paginators, chunk/lazy/cursor iteration, collections, replication |
| **Auth** | Stateful sessions, opaque user IDs, multiple guards, Eloquent + database providers, password hashing (bcrypt + argon2), policy macros, gates, email verification, password reset, brute-force throttling, TOTP 2FA, remember-me, OAuth via torii integration |
| **Frontend** | Inertia v3 bridge, Svelte 5 / React 19 / Vue 3.5 starter templates, typed `#[derive(InertiaProps)]`, partial reloads, automatic TypeScript type generation |
| **Background** | Queue with memory/sync/redis/database/null drivers, batches, chains, job middleware, failed-job store, `#[command]`/`#[derive(Command)]` console binary, `Task` trait scheduler, `#[workflow]` long-running stateful work, `Supervisor` trait with panic-catch auto-restart, command bus, event dispatcher |
| **Realtime** | `ws!()` macro for typed WebSocket handlers, broadcasting channels (public, private, presence), sea-streamer fanout, server-sent events, web push (VAPID) |
| **Cache & Storage** | Memory, Redis, Database cache drivers; atomic operations; tagged cache; cache locks; filesystem with fs/memory/s3/azblob/gcs drivers; path-traversal protection; vector storage with multiple backends |
| **Mail & Notify** | `Mailable` trait, drivers for SMTP/SES/Mailgun/Postmark/SendGrid/Resend (plus in-memory & log for tests), `Notifiable` with mail/database/broadcast/webpush channels |
| **Validation & Data** | `#[derive(Validate)]`, form requests, async validation, `#[derive(Data)]` for partial-reload include sets, `#[derive(Resource)]` for JSON:API |
| **Payments** | Generic provider surface (gateway/MoR/redirect-flow), reference adapters for Stripe and Paddle, mirror tables with webhook idempotency, Inertia checkout components |
| **Feature flags** | Database evaluator, cached evaluator with TTL, feature middleware, sub-second propagation via sync trait |
| **Testing** | `#[suprnova_test]`, `expect!`, `TestDatabase`, fakes for every external surface (Mail, Notify, Queue, Bus, Events, Storage, Http) |
| **CLI** | `suprnova new` scaffolder (Svelte/React/Vue), `serve` dev runner, `migrate*`, `db:sync`, `db:seed`, `make:*` generators, `model:prune`, console binary per project |

## Production readiness

The framework is production-grade in scope and tested. As of the
current HEAD:

- Every Laravel 13.x surface across the 30 documented domains is shipped
- Every issue raised by independent code review has been resolved
- The workspace test suite passes on every change
- Every public API in `framework/src/lib.rs` is feature-stable for v0.1

The framework is in active **0.x development**, distributed through git
(no crates.io, no release tags). The public API is largely settled but
may still shift during the 0.x line as real consumer apps dogfood it.
See the [Release Notes](releases.md) and [CHANGELOG](../CHANGELOG.md) for
the per-version history.

## Pick a reading path

| You are… | Start with |
|---|---|
| A Laravel developer | [From Laravel](from-laravel.md) |
| A Rust developer who's used Axum/Actix/Rocket | [From Rust Web](from-rust-web.md) |
| Both, or neither, and just want to build | [Installation](installation.md) → [Quickstart](quickstart.md) |
| Looking for a specific feature | [`documentation.md`](documentation.md) (the master TOC) |
| Wondering "does Suprnova have X?" | [Laravel Parity Map](parity.md) |
