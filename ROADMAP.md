# Suprnova Roadmap

> **The pitch:** A Laravel developer should be able to install Suprnova,
> scaffold a new project, write `Auth::login(user)` / `Mail::to(...)` /
> `Cache::remember(...)` / `Event::dispatch(...)`, and feel at home —
> while every one of those calls runs inside a real concurrent Tokio
> runtime, every backend is the right tool for the job (not just the
> framework author's favorite), and the type system carries the load
> that PHP's reflection used to.

This document tracks what's done, what's left, and the design principles
guiding both. It's not a release schedule — Suprnova ships when a track
is genuinely production-ready, not on a calendar.

## Philosophy

Three rules, repeated everywhere we make a decision.

### 1. Laravel-ish, not Laravel-clone

Familiar surface APIs:

```rust
Auth::login(user_id);
Cache::remember("users", Duration::from_secs(60), || fetch_users());
Mail::to(&user).send(WelcomeEmail::new(&user));
Event::dispatch(OrderPlaced { order_id });
Storage::disk("s3").put("invoices/1.pdf", &bytes).await?;
```

This is the surface a Laravel dev recognizes on sight. The engine
underneath is something they couldn't build in PHP:

- Real long-lived connections (WebSockets, SSE, gRPC streams) — no
  request-per-process model.
- In-process background workers, supervised by a Tokio task tree.
- Type-checked routing, view contracts, and DI — no string-based magic.
- Async everything by default; the consumer never opts in to
  concurrency.

### 2. No gatekeeping

Laravel ships first-class support for *one* backend per domain and
treats everything else as "use a community package." We don't.

| Domain | Laravel default | Suprnova first-class targets |
|--------|----------------|------------------------------|
| Cache | Redis (others 🟡) | Redis, Memcached, DragonflyDB, KeyDB |
| Queue | Redis/database | Redis, RabbitMQ, NATS, Kafka, SQS, in-process |
| Filesystem | S3 | S3, R2, MinIO, B2, GCS, Azure, local |
| Mail | SMTP | SMTP, SES, Postmark, SendGrid, Mailgun, Resend, log |
| Vectors | Postgres `pgvector` only | Qdrant, Weaviate, Milvus, LanceDB, pgvector |
| Graph | — | Neo4j, ArangoDB, SurrealDB, MemGraph |
| Search | — | Meilisearch, Typesense, Elasticsearch, Algolia |
| Time-series | — | InfluxDB, TimescaleDB, QuestDB, ClickHouse |

Each domain is a trait. Each backend is a driver. The consumer picks
via env or programmatic config. The trait surface stays Laravel-shaped
even when the driver is something Laravel never supported.

### 3. We diverge intentionally where Rust makes things better

Things Laravel does that we deliberately don't replicate:

| Laravel pattern | Why we diverge | Suprnova approach |
|-----------------|----------------|-------------------|
| String-based service container (`app(MyService::class)`) | Run-time lookup that fails late | `App::resolve::<MyService>()` — compiler-verified |
| Eloquent reflection-driven attribute access | No reflection in Rust; would require codegen lies | SeaORM derives — typed columns, IDE autocomplete works |
| Request-per-process model | PHP's only option, our biggest gain | Long-lived state, in-process workers, real async |
| Reflection-driven DI | Same | Macros + traits — compile-time wiring |
| Single-backend per domain | Vendor lock-in | Trait + driver, see above |
| Magic config caching that breaks in dev | Confusing source of truth | Env + typed config struct |

The goal is for the LOC count of a Suprnova app to look like a Laravel
app to a Laravel dev, while the underlying mechanics are something they
couldn't have built in PHP.

## Where we are

**Production-ready and complete:**

- **Inertia v3 protocol** — every protocol field, every header, SSR client
  + CLI worker (`ssr:start` / `ssr:check`), Precognition (Validate-Only
  filter, 204/422 envelope, Vary header), infinite scroll
  (`Inertia::scroll` + `scrollProps`), preserveFragment via session-flash,
  history encryption, shared data, lazy/optional/defer/merge/once props,
  flash, 302→303 conversion. 229 framework tests.
- **Auth** (basic) — session-based, regenerate-on-login, UserProvider
  trait, login/logout/check/user facade.
- **Session** — `tokio::task_local!` (async-safe), DB-backed store with
  flash, CSRF token, regenerate-id, AES-keyed cookies.
- **CSRF** — middleware, helpers, meta-tag emit.
- **Middleware** — typed pipeline, route groups, global middleware.
- **Hashing** — argon2-style; verify/needs_rehash.
- **Workflow** — durable steps with deterministic ordering, claim
  protection, step cache by name.
- **Schedule** — cron expressions, task registry, daemon worker.
- **Routing** — `routes! { ... }` DSL, route groups, named routes,
  middleware-per-group, compile-time path validation via macros.
- **Container** — type-safe `#[injectable]` / `#[service]` macros,
  `App::make::<T>()` and `App::resolve::<T>()`.
- **Database** — SeaORM, three drivers (MySQL, Postgres, SQLite),
  migrations, model derives.
- **Frontend starters** — React 19 / Vue 3 / Svelte 5, all on Vite 6 +
  Tailwind v4 + Inertia 3.1, SSR-aware (`data-server-rendered` honored).
- **CLI** — `suprnova new` / `serve` / `migrate*` / `make:*` /
  `schedule:*` / `workflow:*` / `ssr:*` / `db:sync` / `generate-types`.

**Partial — needs filling in:**

- Validation (basic; FormRequest + Precognition work but no rule
  objects, no after-hooks, no error-bag-as-first-class)
- Cache (Redis + in-memory drivers; needs tags, locks, rate-limiter)
- Cookie (via session; standalone API unclear)
- HTTP request/response (inbound great; outbound HTTP client missing)
- Container scoped bindings (singletons work; per-request scoping
  underspecified)

**Missing — the rest of this document.**

## The remaining tracks

Grouped by "what does a real production app actually need" rather than
by Illuminate-module name. Each track lists what Laravel does, where
Suprnova should diverge, what backends to support, and a rough scope.

### Track 1 — Observability foundation

Every other track wants these. The longer we wait, the more retrofitting
we owe. The current `eprintln!` calls and the `on_ssr_error` callback
hook are technical debt that get paid back here.

**Logging.** `Log::info(...)` / `Log::error(...)` /
`Log::with_context(...)` facade. Drivers: stderr-pretty (dev), JSON
(prod), file rotation, syslog. Built on the `tracing` ecosystem with
the Laravel-shaped facade on top. **Rust gun:** structured spans that
survive `.await` resumes, automatic request-id propagation via
`task_local`, span context attached to errors.

**Events.** `Event::dispatch(event)` / `Event::listen<E>(handler)`.
Sync + async + queued listeners. Typed events (no string event names),
compile-time listener registration via `#[listener]` macro. **Rust gun:**
Tokio broadcast channels for fan-out, listener trait objects the IDE
understands. The reflection-driven `EventServiceProvider` becomes a
macro-generated `register_events!()` call.

### Track 2 — Encryption + Outbound HTTP

**Encryption.** `Crypt::encryptString` / `decryptString`. Symmetric
AES-GCM via `aes-gcm` crate. Cookie encryption replaces the
sign-only path we have today. ~200 LOC.

**HTTP client.** `Http::get(url)` / `Http::post(url).json(body)` /
`Http::pool(...)`. Built on `reqwest`. Retries, fakes for tests,
async by default. **Rust gun:** real per-process connection pooling,
typed retry strategies with exponential backoff,
`Http::concurrent(vec![...])` fan-out as a one-liner.

### Track 3 — Filesystem

`Storage::disk("s3").put(path, contents).await?`. The no-gatekeeping
rule starts mattering.

**Drivers:**
- Local (with subdir scoping)
- S3 and S3-compatible (R2, MinIO, Backblaze B2, DigitalOcean Spaces,
  Wasabi)
- Google Cloud Storage
- Azure Blob
- In-memory (for tests)

All first-class. **Rust gun:** streaming `AsyncRead` / `AsyncWrite`
everywhere — no `file_get_contents()` patterns that materialize 4GB
files in memory. Cross-disk operations (`local.copy_to(s3, ...)`) run
as concurrent streams over a Tokio bridge, not a buffered round-trip.

### Track 4 — Mail

`Mail::to(user).send(WelcomeEmail::new(user))`. Same drivers-everywhere
story.

**Drivers:** SMTP, AWS SES, Postmark, SendGrid, Mailgun, Resend,
log (dev/test mode that records sends for assertions).

**Mailables.** Typed templates with `#[derive(Mailable)]` macro that
provides `to()`, `subject()`, `view()`. Template engine: pick one and
ship it (likely `askama` for compile-time-checked templates that match
the framework's type-safety stance, with `minijinja` as a runtime
alternative for hot-reload).

**Rust gun:** mail building is synchronous and fast; sending is
async on the in-process queue (Track 6) so a controller can
`Mail::queue(welcome_email)` and return without blocking. No separate
queue worker needed for transactional mail.

### Track 5 — Validation, Pagination, Console

The boring-but-essential leg-day reps.

**Validation parity.** Richer `FormRequest`:
- `prepareForValidation` hook
- `withValidator` for chained custom rules
- `after` hooks
- Rule objects: `Rule::unique("users", "email").ignoring(self.id)`,
  `Rule::exists("posts", "id")`, custom rule via `Rule` trait
- Error bags as first-class (we wire the bag name; the rules need to
  know about it)
- Conditional rules: `required_if`, `required_with`, `required_unless`
- Cross-field validation

**Pagination.** SeaORM-aware paginator with offset, cursor, and simple
modes. `Inertia::paginate(query, per_page)` shorthand that wires
through to our existing `scrollProps`. ~400 LOC.

**Console.** User-registrable Artisan-style commands.

```rust
#[command(name = "user:create", description = "Create a new user")]
pub struct CreateUserCommand {
    #[arg(long)]
    email: String,
    #[arg(long, default_value = "user")]
    role: String,
}

impl CreateUserCommand {
    pub async fn handle(self) -> Result<(), FrameworkError> {
        // ...
    }
}
```

Lives alongside `suprnova-cli`'s built-in commands. **Rust gun:**
typed args via `clap` underneath, zero string-arg parsing in the
handler body.

### Track 6 — Queue + Cache extensions + Notifications

**Queue.** Plain job dispatch — separate from the durable workflow
runtime we already have.

```rust
#[job]
pub struct SendInvoice { order_id: i64 }

impl SendInvoice {
    pub async fn handle(self) -> Result<(), FrameworkError> { ... }
}

// Dispatch from a controller:
Queue::push(SendInvoice { order_id: 42 }).await?;
```

**Drivers:** Redis, database (default for new apps), RabbitMQ, NATS,
Amazon SQS, in-process.

**Rust gun:** the in-process driver runs jobs on Tokio tasks in the
same process — for monolith apps, zero infrastructure required. Real
backends for when you need scale-out. Job retries, backoff, fail-handler
hooks. Built-in dead-letter-queue.

**Cache extensions.** Atomic locks, tags, rate limiters.

```rust
Cache::lock("import-{user_id}").get(|| import_user_data()).await?;
Cache::tags(&["users"]).flush().await?;
RateLimiter::for_("login").limit(5).per_minute().attempt(&ip)?;
```

**Drivers we still need:** Memcached, DragonflyDB (Redis-compat works
today).

**Notifications.** Channel-based delivery.

```rust
Notify::send(&user, OrderShipped { order_id })
    .via(&["mail", "slack", "database"]).await?;
```

Channels: mail, Slack, Discord, SMS via Twilio, database, webhook,
broadcast (Track 7). Depends on Mail (Track 4) and Broadcasting
(Track 7).

### Track 7 — Real-time (where Rust eats Laravel's lunch)

The "rust guns" track. Laravel uses Pusher or Reverb bolted on as a
separate service. Suprnova runs it in-process by default.

**Broadcasting via WebSocket.**

```rust
// Define a typed channel:
#[channel("orders.{id}")]
pub struct OrderChannel { pub id: i64 }

impl OrderChannel {
    pub async fn authorize(self, user: &User) -> bool {
        user.can_view_order(self.id).await
    }
}

// Broadcast:
Broadcast::channel(OrderChannel { id: 42 }).send(OrderUpdated { ... }).await?;
```

**Rust gun:** WebSocket connections held by the same process running
your HTTP handlers. No separate broadcast server. Channels are typed;
presence and private auth are compile-time-checked. Built on
`tokio-tungstenite`.

**SSE (Server-Sent Events)** as a first-class one-way alternative.
Simpler than WebSocket; great for live dashboards, log streaming,
progress indicators. `sse::stream(|tx| async move { tx.send(...).await; })`.

**Supervised background workers** in-process.

```rust
Worker::supervise("payments.poll", Duration::from_secs(30), || async {
    poll_pending_payments().await
}).await?;
```

Crashes restart with exponential backoff. Scoped to app lifetime.
For monolith apps, this replaces the entire "deploy Horizon + Redis
+ a separate worker container" stack.

### Track 8 — No-gatekeeping differentiation

The Suprnova-specific value-add. Each is a trait + driver(s).

**Vector DBs.**

```rust
Vector::store("documents")
    .upsert(&[("doc-1", embedding, metadata)])
    .await?;
Vector::store("documents").similar(query_embedding, k: 10).await?;
```

Drivers: Qdrant, Weaviate, Milvus, LanceDB, pgvector. Type-safe
embeddings (`Vec<f32>` of compile-checked dimension).

**Graph DBs.** `Graph::node(...).related_to(...).match(...)`.
Drivers: Neo4j (Bolt protocol), ArangoDB, SurrealDB, MemGraph.

**Time-series.** `Timeseries::write(measurement, tags, fields, ts)`,
batched writes. Drivers: InfluxDB, TimescaleDB, QuestDB, ClickHouse.

**Search.** `Search::index("users").add(doc).query("alice").await?`.
Drivers: Meilisearch, Typesense, Elasticsearch, Algolia.

The pattern that matters: trait surface stays the same, drivers swap
behind it. A consumer migrating from Meilisearch to Typesense changes
one config value.

### Track 9 — Polish

Small individually, big collectively.

- **Translation (i18n)** — file-based locales, `__("users.welcome", name: "Sue")`.
- **Support helpers** — `Str::camel`, `Arr::pluck`, `Stringable` chain.
  Most exist in std/itertools; we ship the Laravel-named wrappers.
- **Routing extras** — resource routing (`Route::resource("users", UsersController)`
  → 7 RESTful routes), signed routes, route throttling, named-route
  reverse, sub-domain routing.
- **Container scoped bindings** — singleton / per-request / transient.
  We have some of this; needs filling in.
- **Process** — `Process::run("git status").output().await`. Wraps
  `tokio::process`. Small.
- **Testing helpers** — `Mail::fake()` / `Mail::assertSent(...)`,
  `Queue::fake()` / `Queue::assertPushed(...)`, `Event::fake()`,
  `Http::fake()`. Trickles in as each domain ships.

## Recommended sequencing

Each phase unblocks the next. Approximate effort in italics; not
commitments.

**Phase 1: Logging + Events** *(4 weeks)*
Foundation observability. Everything else uses them. The longer we
wait, the more retrofitting we owe.

**Phase 2: HTTP client + Pagination + Encryption** *(3 weeks)*
Small, high-leverage, often-used.

**Phase 3: Filesystem + Validation parity** *(4–6 weeks)*
Validation gets finished here because we already exercised the gaps in
Precognition.

**Phase 4: Queue + Mail + Notifications** *(5–6 weeks)*
Mail-via-queue is the canonical pattern; ship them together.
Notifications layer on top.

**Phase 5: Broadcasting + supervised background workers** *(5–6 weeks)*
The "Rust eats Laravel's lunch" moment. Real-time everything; no
separate infrastructure. This is the demo that gets a Laravel dev to
say "wait, you can do that in one process?"

**Phase 6: Differentiation** *(ongoing)*
Vectors, graphs, search, time-series. Driven by real consumer needs
(`nation-x.com` will exercise some). Ship one when the demand exists;
the others queue up behind.

**Phase 7: Polish** *(parallel with phases above)*
Translation, Support helpers, Process, scoped bindings, Console.
These fit between bigger pieces.

## How a Laravel dev experiences this

The end-state, written from a Laravel developer's perspective the first
time they sit down with Suprnova:

```rust
// app/src/controllers/users.rs
use suprnova::{Auth, Cache, Mail, Inertia, Event};

pub async fn store(req: CreateUserRequest) -> Response {
    let user = User::create(req.into()).await?;

    Cache::tags(&["users"]).flush().await?;
    Mail::to(&user).send(WelcomeEmail::new(&user)).await?;
    Event::dispatch(UserRegistered { user_id: user.id });

    Auth::login(user.id);

    redirect!("users.index").into()
}
```

That code looks like Laravel. It compiles with type checks every
Laravel dev wishes they had. The mailer runs on a Tokio queue without
a separate Horizon process. The event fires to in-process listeners
*and* a Redis-backed listener pool that scales independently. The
cache flush is atomic across all replicas because Suprnova's default
Redis driver supports tag-set CAS the way Laravel's doesn't.

A Laravel dev gets here in an afternoon. Their app runs at scales PHP
couldn't reach without rearchitecture.

## What we will not do

- **Compile-to-PHP cross-targets.** No. Suprnova is Rust.
- **Reflection emulation.** Macros are the load-bearing layer.
- **Replicating Eloquent's magic accessors.** SeaORM derives, typed
  columns. Familiar shape, no string lookups.
- **Single-backend gatekeeping.** Every domain ships multi-driver.
- **Caching config at build time in a way that breaks dev.** Env
  loads at boot; if you want a typed config struct, `cargo build`.
- **A separate "queue worker" container as the default deploy story.**
  In-process is the default; scale-out is the upgrade.

## Contributing

This doc lives in the repo at the root because anyone evaluating
Suprnova should be able to see where it's going before they commit to
building on it. Track-level proposals (especially around backend
support inside a track) are welcome via GitHub issues.

The working agreement (from `CLAUDE.md`):

> **We only do full implementations, well tested and production ready.**
> No deferring. No "we can do that later." No partial scaffolds with
> TODOs sprinkled in. If a feature, test, edge case, or polish item is
> needed for the work to be production-ready, it gets done now as part
> of the same change.

That applies to roadmap tracks too. A track ships when it's complete,
not when it has a viable prototype.
