# From Rust Web

You've shipped Rust services on Axum, Actix, Rocket, or hand-rolled hyper.
You know the language and the runtime. What does Suprnova actually buy
you?

**The productivity layer.** Routing, controllers, an ORM, migrations,
queues, scheduling, auth, mail, notifications, broadcasting, cache,
storage, validation, and a typed frontend bridge — all wired together,
all using the same conventions, all production-ready. You write
controllers and models; you don't pick the layout.

If you've already built one or two real apps in Axum, you know how much
of that effort was wiring rather than features. Suprnova is the wiring,
done once, opinionated where opinion matters, pluggable where it
doesn't.

## The 30-second TL;DR

```bash
suprnova new myapp --frontend svelte    # scaffolds backend + SPA + Vite
cd myapp
suprnova db:sync                        # runs migrations, regenerates entities
suprnova serve                          # backend + Vite dev server
```

You now have:

- A hyper server with HTTP/1.1 and HTTP/2, WebSocket upgrade, graceful shutdown
- A SeaORM-backed Eloquent layer with relations, eager loading, soft deletes
- Inertia.js bridging Rust → Svelte 5 with typed `#[derive(InertiaProps)]`
- Auth (sessions, password hashing, email verification, 2FA, OAuth via torii)
- A queue with memory/sync/redis/database/null drivers
- A cron scheduler driven by the `Task` trait
- A console binary per project for `cargo run --bin console <cmd>`
- Cache, storage (fs/s3/azblob/gcs), mail (SMTP + 5 providers: SES, Mailgun, Postmark, SendGrid, Resend), web push
- Broadcasting over a pluggable hub (sea-streamer by default)
- Validation, CSRF, CORS, rate limiting, idempotency, request timeouts, structured errors

And one statically linked binary at the end of `cargo build --release`.

## What's underneath

| Concern | Crate |
|---|---|
| HTTP server | `hyper` + tower-ish middleware (own implementation) |
| Async runtime | `tokio` |
| Router | `matchit` |
| ORM | `sea-orm` (re-exported as `suprnova::sea_orm`) |
| Migrations | `sea-orm-migration` |
| Database drivers | `sqlx` (postgres / mysql / mariadb / sqlite) |
| Serialization | `serde` / `serde_json` |
| Validation | `validator` |
| Sessions | own (driver-based) |
| Templating | `tera` (for mail bodies; frontend is Inertia) |
| Crypto | `aes-gcm`, `argon2`, `bcrypt` |
| WebSockets | `hyper-tungstenite` |
| Streaming | `sea-streamer` (broadcasting fanout backend) |
| OAuth | `torii` (vendored fork) |
| Tracing | `tracing` + `tracing-subscriber` |

You won't typically reach for any of these directly — Suprnova
re-exports what you need. SeaORM is the deepest passthrough: `Entity`,
`Column`, `ActiveModel`, `ConnectionTrait`, the query builder, the
migration prelude. The escape hatch is `use suprnova::sea_orm;` if you
need something the curated surface doesn't cover.

## What Suprnova adds over raw Axum

Axum is excellent. So is Actix. So is Rocket. The reason Suprnova exists
isn't that those frameworks are bad — it's that every team building a
real product on them ends up re-implementing the same productivity
layer. Suprnova ships that layer:

| Capability | Hand-roll on Axum | In Suprnova |
|---|---|---|
| Routing macros that scale to hundreds of routes | Builder API, can get noisy | `routes!` macro with grouping, prefixes, middleware, naming |
| Route model binding (path id → loaded model) | Custom extractor per type | `#[handler]` resolves `post::Model` from `{id}` automatically |
| Eloquent-style chainable query builder | Use SeaORM directly | `Post::query().db_where(...).order_by(...).get().await?` |
| Soft deletes, observers, lifecycle events | Build per-model | `#[model(soft_deletes)] + impl Observer<Post>` |
| Migrations + entity generation | Wire sea-orm-cli + scripts | `suprnova db:sync` runs migrations and regenerates entities |
| Auth (sessions, providers, guards) | Stitch tower-sessions + own logic | `Auth::attempt`, `Auth::user`, `.middleware(AuthMiddleware)` per route |
| Email verification, password reset, 2FA, brute-force | Hand-build all four | All built in, configurable, idempotent |
| Background queue | Pick a driver, write workers | `Queue::push` + `cargo run --bin console queue:work` |
| Cron scheduling | Write a tokio task with `tokio_cron_scheduler` | `impl Task` + `Schedule::task(...).daily().at("03:00")` |
| Inertia bridge | Build extractors + a JS adapter | `inertia_response!(&req, "Page", props)` |
| Typed frontend props (Rust → TS) | Write a generator | `#[derive(InertiaProps)]` + `suprnova generate-types` |
| Broadcasting (public / private / presence channels) | Wire a streaming backend + auth | `BroadcastHub` + `Channel`/`PrivateChannel`/`PresenceChannel` traits |
| Mail with multiple providers | Pick one, write your own abstraction | `Mail::driver("ses")` etc., uniform `Mailable` API |
| WebPush | Read the spec, build a notifier | `WebPushChannel` ships, VAPID baked in |
| Validation + form requests | Use `validator` + custom extractor | `#[derive(Data, Validate)]` form requests, async validation |
| JSON:API resources | Hand-format responses | `#[derive(Resource)]` |
| Rate limiting with fail-open/closed policy | Build it | `RateLimiter` + `BackendErrorPolicy` |
| Idempotency keys | Build it | `Idempotency::remember(key, ttl, body)` with Stripe-style replay |
| CSRF (with Laravel-style glob exclusions) | Build it | `CsrfMiddleware` with `except` + `except_method` |
| Structured errors with sanitised 5xx | Build it | `FrameworkError` / `HttpError` trait, panic recovery |
| Container with task-local → thread-local → global scopes | Write your own | `App::bind` / `singleton` / `factory` with proper isolation |
| Health endpoint, request id, structured logging | Glue together | All on by default |

The trade-off is opinions: Suprnova picks a layout, picks a default
driver, picks a naming convention. You can deviate (drivers are pluggable,
config is overridable, the container lets you swap services), but the
defaults are designed to be the right choice for "build a product
quickly".

## Familiar Rust patterns

You'll recognise the shapes:

```rust
// A handler returns `Result<HttpResponse, HttpResponse>` (aliased Response).
pub async fn show(req: Request) -> Response {
    let id: i64 = req.param("id").unwrap_or("0").parse().unwrap_or(0);
    let post = Post::find_or_fail(id).await?;
    Ok(HttpResponse::json(serde_json::json!({ "post": post })))
}

// Middleware is a trait, not a closure:
#[async_trait]
impl Middleware for RequireAdmin {
    async fn handle(&self, req: Request, next: Next) -> Response {
        let user = Auth::user_as::<User>().await?
            .ok_or_else(|| HttpResponse::text("Unauthorized").status(401))?;
        if !user.is_admin {
            return Err(HttpResponse::text("Forbidden").status(403));
        }
        next(req).await
    }
}

// Background work is the `Job` trait — `handle(self)` runs the job:
#[async_trait]
impl Job for SendWelcomeEmail {
    fn job_name() -> &'static str { "SendWelcomeEmail" }

    async fn handle(self) -> Result<(), FrameworkError> {
        let user = User::find_or_fail(self.user_id).await?;
        Mail::to(&user.email).send(WelcomeMail { user }).await?;
        Ok(())
    }
}
```

If you're used to Tower middleware: Suprnova middleware is conceptually
the same (a wrapper around `next`), but uses an own trait (not Tower's
`Service`) because tower's combinator types get nasty when you start
nesting application-specific extractors. The shape is simpler; the
mental model is the same.

If you've used Axum's extractor pattern: Suprnova's `#[handler]` macro
plays the same role, but resolves through the service container rather
than via traits, which lets it inject app services as well as request
data. Route model binding (`Post` from `{id}`) is built in.

If you've used `sqlx` directly: Suprnova's ORM sits over SeaORM, which
sits over sqlx. You can drop to raw SQL via `DB::select(...)` /
`DB::select_one(...)` or use `DB::table("name")` for chainable dynamic
queries; you can drop straight to SeaORM for things the Eloquent
surface doesn't cover (e.g. raw `Statement` queries with custom result
mapping). The [Eloquent chapter](eloquent.md) covers the escape hatches.

## What's the productivity delta?

Pick a feature you've built before in raw Axum. Suprnova ships it as a
chapter:

- **"I built an auth system once and it took two weeks."** →
  [Authentication](authentication.md) + [Auth Flows](auth-flows.md). Set
  the migration, configure the guard, you're done.
- **"I wrote my own queue worker with retry/backoff."** →
  [Queues](queues.md). `Queue::push` + `cargo run --bin console queue:work`.
- **"I wired WebSockets with hyper-tungstenite once."** →
  [WebSockets](websockets.md). The `ws!()` macro types the handler;
  the upgrade, ping/pong heartbeat, close-frame handshake, and
  back-pressure are taken care of.
- **"I built an Inertia adapter from scratch."** →
  [Inertia](frontend.md). `inertia_response!(&req, "Page", props)`, with
  `InertiaProps` generating the TS types.
- **"I built a per-tenant rate limiter."** →
  [Rate Limiting](rate-limiting.md). Configurable key, configurable
  fail-open vs fail-closed policy, fail-closed returns 503.
- **"I implemented Stripe webhook signature verification + replay protection."** →
  [Payments: Stripe](payments-stripe.md). Built into the adapter,
  webhooks go into a mirror table with UNIQUE idempotency.

What you'd build by hand in two weeks, you import in one line.

## What you'll still recognise as "yours"

A few things stay close to raw Rust because the language gives you
something better than a framework abstraction:

- **Concurrency primitives.** `tokio::spawn`, `Arc`, `Mutex`, channels —
  use them. The framework doesn't wrap them.
- **Error types.** You define your domain errors. Implement the
  `HttpError` trait on them to get a proper status code + message in
  the wire response. The framework's `FrameworkError` and `AppError`
  are escape hatches for cross-cutting + ad-hoc errors respectively.
- **Custom drivers.** Cache, queue, mail, broadcasting, vector, payments
  — every "driver registry" subsystem accepts custom drivers. Implement
  the trait, register it in `bootstrap.rs`, done.
- **Raw SQL when you want it.** `DB::select(...)`, `DB::table(...).get()`
  for dynamic rows, or drop fully to SeaORM. The ORM gets out of the way.
- **Your own tower middleware?** Suprnova doesn't ship a Tower
  adapter — middleware here is `impl Middleware`, not `tower::Service`.
  If you need to bring a Tower-only crate, you'd adapt it by hand.
  In practice, the built-in middleware system covers almost everything
  you'd reach for. See [Middleware](middleware.md).

## What you give up

Honesty matters more than marketing:

- **Conventions.** Models live here, controllers there, migrations
  there, observers there. The scaffolder picks. You can fight it; you
  probably shouldn't. The conventions are Laravel's, audited and
  battle-tested.
- **Some flexibility in how the request flows.** The middleware chain
  has a fixed outermost order (request-id → globals → route middleware
  → handler). You can insert middleware anywhere in that, but you
  can't move the request-id or panic-recovery layers — they're
  invariants.
- **The PHP-shaped corners.** Where Laravel does something because PHP,
  Suprnova does the Rust-shaped thing instead — but we tell you when.
  Look for **"Why Suprnova diverges"** callouts in chapters.

## Why "Laravel-inspired" should matter to you even if you've never written PHP

The Rust web ecosystem is roughly where the PHP one was around 2009. The
crates exist; the patterns don't. Suprnova ports an extremely refined
set of patterns from a framework that has had 10+ years of production
pressure shaping it. You get patterns that already survived contact with
reality.

The cost is that Suprnova *is opinionated*. If you want a minimal
"pick-your-own-everything" framework, Axum is right there and it's
excellent. If you want a "framework that decides things so you can
focus on the product", that's Suprnova.

## Next steps

- [Installation](installation.md) — `suprnova new`, what gets scaffolded
- [Quickstart](quickstart.md) — build a tiny app in 5 minutes
- [Request Lifecycle](lifecycle.md) — how a request flows, what runs where
- [Service Container](container.md) — how services are bound and resolved
- [Eloquent](eloquent.md) — the longest chapter; the surface is wide

Or jump anywhere via [`documentation.md`](documentation.md).
