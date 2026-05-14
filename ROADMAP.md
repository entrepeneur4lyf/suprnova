# Suprnova Roadmap

> **The pitch:** A Laravel developer should be able to install Suprnova,
> scaffold a new project, write `Auth::login(user)` / `Mail::to(...)` /
> `Cache::remember(...)` / `Event::dispatch(...)`, and feel at home —
> while every one of those calls runs inside a real concurrent Tokio
> runtime, every backend is the right tool for the job (not just the
> framework author's favorite), and the type system carries the load
> that PHP's reflection used to.

This document tracks what's done, what's left, and the design principles
guiding both. It's not a release schedule - Suprnova ships when a track
is genuinely production-ready, not on a calendar.

## Philosophy

Four rules, repeated everywhere we make a decision.

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

- Real long-lived connections (WebSockets, SSE, gRPC streams) - no
  request-per-process model.
- In-process background workers, supervised by a Tokio task tree.
- Type-checked routing, view contracts, and DI - no string-based magic.
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
| Vectors | Postgres `pgvector` only | Qdrant, Weaviate, Milvus, LanceDB, pgvector, MariaDB, LibSQL (SQLite) |
| Graph | - | Neo4j, ArangoDB, SurrealDB, MemGraph |
| Search | - | Meilisearch, Typesense, Elasticsearch, Algolia |
| Time-series | - | InfluxDB, TimescaleDB, QuestDB, ClickHouse |

Each domain is a trait. Each backend is a driver. The consumer picks
via env or programmatic config. The trait surface stays Laravel-shaped
even when the driver is something Laravel never supported.

### 3. We diverge intentionally where Rust makes things better

Things Laravel does that we deliberately don't replicate:

| Laravel pattern | Why we diverge | Suprnova approach |
|-----------------|----------------|-------------------|
| String-based service container (`app(MyService::class)`) | Run-time lookup that fails late | `App::resolve::<MyService>()` - compiler-verified |
| Eloquent reflection-driven attribute access | No reflection in Rust; would require codegen lies | SeaORM derives - typed columns, IDE autocomplete works |
| Request-per-process model | PHP's only option, our biggest gain | Long-lived state, in-process workers, real async |
| Reflection-driven DI | Same | Macros + traits - compile-time wiring |
| Single-backend per domain | Vendor lock-in | Trait + driver, see above |
| Magic config caching that breaks in dev | Confusing source of truth | Env + typed config struct |

The goal is for the LOC count of a Suprnova app to look like a Laravel
app to a Laravel dev, while the underlying mechanics are something they
couldn't have built in PHP.

### 4. Testing ships with the feature

A track is not "done" until its testing surface is shipped alongside it.
This is not polish — it's the definition of done.

When Queue ships, `Queue::fake()` + `Queue::assert_pushed(...)` ship in
the same commit. When Mail ships, `Mail::fake()` + `Mail::assert_sent(...)`
ship in the same commit. When Broadcasting ships, channels can be faked
and asserted against in tests without standing up a real WebSocket
server. When a track is consumed by a controller, the controller must
be testable without spinning up real infrastructure.

The alternative is what Laravel had to retrofit later, and the alternative
is what makes Rust frameworks reach 80% production-ready and stall.

## Where we are

**Production-ready and complete:**

- **Inertia v3 protocol** - every protocol field, every header, SSR client
  + CLI worker (`ssr:start` / `ssr:check`), Precognition (Validate-Only
  filter, 204/422 envelope, Vary header), infinite scroll
  (`Inertia::scroll` + `scrollProps`), preserveFragment via session-flash,
  history encryption, shared data, lazy/optional/defer/merge/once props,
  flash, 302→303 conversion. 229 framework tests.
- **Auth** (basic) - session-based, regenerate-on-login, UserProvider
  trait, login/logout/check/user facade.
- **Session** - `tokio::task_local!` (async-safe), DB-backed store with
  flash, CSRF token, regenerate-id, AES-keyed cookies.
- **CSRF** - middleware, helpers, meta-tag emit.
- **Middleware** - typed pipeline, route groups, global middleware.
- **Hashing** - argon2-style; verify/needs_rehash.
- **Workflow** - durable steps with deterministic ordering, claim
  protection, step cache by name.
- **Schedule** - cron expressions, task registry, daemon worker.
- **Routing** - `routes! { ... }` DSL, route groups, named routes,
  middleware-per-group, compile-time path validation via macros.
- **Container** - type-safe `#[injectable]` / `#[service]` macros,
  `App::make::<T>()` and `App::resolve::<T>()`.
- **Database** - SeaORM, three drivers (MySQL, Postgres, SQLite),
  migrations, model derives.
- **Frontend starters** - React 19 / Vue 3 / Svelte 5, all on Vite 8 +
  Tailwind v4 + Inertia 3.1, SSR-aware (`data-server-rendered` honored).
- **CLI** - `suprnova new` / `serve` / `migrate*` / `make:*` /
  `schedule:*` / `workflow:*` / `ssr:*` / `db:sync` / `generate-types`.

**Partial - needs filling in:**

- Validation (basic; FormRequest + Precognition work but no rule
  objects, no after-hooks, no error-bag-as-first-class)
- Cache (Redis + in-memory drivers; needs tags, locks, rate-limiter)
- Cookie (via session; standalone API unclear)
- HTTP request/response (inbound great; outbound HTTP client missing)
- Container scoped bindings (singletons work; per-request scoping
  underspecified)

**Missing - the rest of this document.**

## The remaining tracks

Grouped by "what does a real production app actually need" rather than
by Illuminate-module name. Each track lists what Laravel does, where
Suprnova should diverge, what backends to support, and a rough scope.

### Track 1 - Observability foundation + Error handling + minimal SSE

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

**Error handling pipeline.** Errors in Rust are values, not exceptions,
but the framework still needs an opinion on how they flow through. Two
behaviors live on every error: how it logs (reportable) and how it
becomes an HTTP response (renderable).

```rust
#[derive(Debug, thiserror::Error, Renderable, Reportable)]
pub enum CheckoutError {
    #[error("Payment declined")]
    #[render(status = 402, message = "Your card was declined")]
    #[report(level = "warn")]
    PaymentDeclined,

    #[error("Inventory unavailable for SKU {sku}")]
    #[render(status = 409)]
    OutOfStock { sku: String },
}
```

The `Renderable` derive controls per-variant HTTP response shape (status,
JSON body, Inertia error page, redirect — context-aware on Accept and
X-Inertia headers). The `Reportable` derive hooks into Track-1 Logging
automatically — no `Log::error(...)` peppered through controllers. Custom
error pages for 404/500/etc. via `ErrorPages::register("404", controller)`.

**Server-Sent Events (minimal).** Events deserve a delivery primitive
that ships in the same phase. SSE is the one-way push that pairs
naturally with `Event::dispatch`:

```rust
pub async fn live_feed(_req: Request) -> Response {
    sse::stream(|tx| async move {
        let mut events = Event::subscribe::<OrderPlaced>();
        while let Ok(event) = events.recv().await {
            tx.send(sse::Event::json("order", &event)).await?;
        }
        Ok(())
    })
}
```

This is the "real-time without infrastructure" demo at MVP scale — works
for live feeds, notifications, progress bars, log tailing. Full
WebSocket + presence + channel auth lives in Track 8; SSE here so
real-time is on the table from week one. Consumers like
`nation-x.com` (a social network) need feed updates well before they
need typed presence channels.

### Track 2 - Encryption + Outbound HTTP

**Encryption.** `Crypt::encryptString` / `decryptString`. Symmetric
AES-GCM via `aes-gcm` crate. Cookie encryption replaces the
sign-only path we have today. ~200 LOC.

**HTTP client.** `Http::get(url)` / `Http::post(url).json(body)` /
`Http::pool(...)`. Built on `reqwest`. Retries, fakes for tests,
async by default. **Rust gun:** real per-process connection pooling,
typed retry strategies with exponential backoff,
`Http::concurrent(vec![...])` fan-out as a one-liner.

### Track 3 - Filesystem + File uploads

`Storage::disk("s3").put(path, contents).await?` on the outbound side;
multipart parsing + validation + temp staging on the inbound side.
Both halves matter: storage drivers handle where a file goes, upload
handling determines how it arrives. Controllers touch both.

**Storage drivers (outbound):**
- Local (with subdir scoping)
- S3 and S3-compatible (R2, MinIO, Backblaze B2, DigitalOcean Spaces,
  Wasabi)
- Google Cloud Storage
- Azure Blob
- In-memory (for tests)

All first-class. **Rust gun:** streaming `AsyncRead` / `AsyncWrite`
everywhere - no `file_get_contents()` patterns that materialize 4GB
files in memory. Cross-disk operations (`local.copy_to(s3, ...)`) run
as concurrent streams over a Tokio bridge, not a buffered round-trip.

**File upload handling (inbound).** Multipart parsing baked into the
HTTP layer with size limits enforced at parse time (not after the
whole body lands in memory). Typed `UploadedFile` extracts:

```rust
pub async fn upload_avatar(
    user: AuthUser,
    file: UploadedFile<"avatar", Image, MaxSize::<5_MB>>,
) -> Response {
    let path = file.store_as(Storage::disk("s3"), &format!("avatars/{}.jpg", user.id)).await?;
    user.update_avatar(path).await?;
    redirect!("profile.show").into()
}
```

The `Image` validator checks magic bytes (not just Content-Type — that
lies); `MaxSize` is enforced as bytes-streamed-so-far during multipart
parse, so a 5GB POST against a 5MB limit gets rejected at byte 5_242_881
without buffering the rest. Image manipulation (resize, format-convert,
strip EXIF) via `image` crate on a tokio blocking pool. **Rust gun:**
streaming from the network straight to the storage driver without ever
hitting disk, when both sides support it (S3 multipart upload + chunked
request body).

### Track 4 - Mail

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

### Track 5 - Authorization + API mode

Two separate but related stories that together cover the gap between
"the user is logged in" (today's Auth) and "the user is allowed to do
this thing to this resource." Every real app needs this on day one.
Laravel ships Gates, Policies, and Sanctum for it; Suprnova ships the
typed equivalents.

**Authorization (Gates + Policies).**

```rust
#[policy]
impl PostPolicy {
    pub fn view(&self, user: &User, post: &Post) -> bool {
        post.is_public || user.id == post.author_id
    }

    pub fn update(&self, user: &User, post: &Post) -> Result<bool, AuthError> {
        if user.is_banned() {
            return Err(AuthError::Banned);
        }
        Ok(user.id == post.author_id || user.is_admin())
    }
}

// In a controller:
pub async fn update(post: Post, user: AuthUser, req: UpdatePostRequest) -> Response {
    Gate::authorize_on::<PostPolicy>("update", &user, &post)?;
    // ...
}
```

Gates work for non-resource checks (`Gate::define("admin-area", ...)`).
Policies attach to model types. The `#[policy]` macro registers the
type with the container and validates that policy method names match
controller action verbs. **Rust gun:** policies are trait impls — the
compiler refuses to let you call a policy method that doesn't exist,
and `Gate::authorize_on` requires the policy type as a generic, so
typos become compile errors instead of silent allow-alls.

**API mode + token auth (Sanctum equivalent).** Suprnova should be
just as good at building a JSON API as it is at building an Inertia
SPA. `suprnova new --api` scaffolds a project without the frontend
starter; `suprnova new --api+spa` ships both.

API surface:
- `#[derive(Resource)]` on a struct emits a JSON-API-style resource
  with `from(model)` + filter/include/sparse-field support.
- Personal access tokens via `User::create_token("api", &abilities)`
  → returns the plaintext once + hashed in DB. `TokenAuthMiddleware`
  reads `Authorization: Bearer …`, looks up by SHA-256, scopes the
  request to the token's abilities.
- Stateless route group (`api.middleware(TokenAuth)`) — no sessions,
  no CSRF, no cookies. Separate routing concern from the session-based
  web routes.
- Built-in OpenAPI emit via `suprnova openapi:emit` — reads the
  `#[derive(Resource)]` types + the route table + the `FormRequest`
  derives, emits an OpenAPI 3.1 spec. Free documentation, free SDK
  generation downstream.

**Rust gun:** token abilities are checked at compile time when the
controller declares what it needs (`AuthUser<Ability::WriteOrders>`)
— a route that requires `write:orders` won't accept a token without
that scope, and the type signature makes the requirement explicit.

### Track 6 - Validation, Pagination, Factories, Console, Configuration

The boring-but-essential leg-day reps. Each is small alone; together
they cover the day-one expectations a Laravel dev has.

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

**Factories + Seeders.** `User::factory().count(50).create().await?`.
The single feature Laravel devs reach for hardest in development and
testing. Suprnova ships:

```rust
#[factory(User)]
pub fn user_factory() -> UserFactory {
    UserFactory::default()
        .name(faker::name())
        .email(faker::email())
        .password(Hash::make("password"))
}

// In tests / seeders:
let users = User::factory().count(50).create().await?;
let admin = User::factory().state(|u| u.is_admin = true).create_one().await?;
let post = Post::factory().for_user(&admin).create_one().await?;
```

Seeders are typed structs with a `run()` method, registered via
`#[seeder]` macro. `suprnova db:seed` runs them in registered order;
`suprnova db:seed --class WelcomePostsSeeder` runs one. **Rust gun:**
factory states are typed methods, not stringly-typed
`->state('admin')` lookups. Faker integration via the `fake` crate.

**Configuration management.** Typed config struct with a `Config`
derive macro, loaded from env + optional TOML overlay:

```rust
#[derive(Config)]
#[config(prefix = "MAIL")]
pub struct MailConfig {
    pub driver: MailDriver,           // env: MAIL_DRIVER
    pub host: String,                 // env: MAIL_HOST
    #[config(default = 587)]
    pub port: u16,
    pub from_address: String,
    pub from_name: String,
}

// Access anywhere:
let mail_cfg = Config::resolve::<MailConfig>();
```

No `config('mail.driver')` string lookups — typos are compile errors.
Hot-reload in dev via file watcher; production loads at boot and stays
fixed. **Rust gun:** config validation happens at boot — a missing
required env var fails the app start, not a request three hours later.

### Track 7 - Queue + Cache + Notifications + Rate Limiting

**Queue.** Plain job dispatch - separate from the durable workflow
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
same process - for monolith apps, zero infrastructure required. Real
backends for when you need scale-out. Job retries, backoff, fail-handler
hooks. Built-in dead-letter-queue.

**Cache extensions.** Atomic locks, tags.

```rust
Cache::lock("import-{user_id}").get(|| import_user_data()).await?;
Cache::tags(&["users"]).flush().await?;
```

**Drivers we still need:** Memcached, DragonflyDB (Redis-compat works
today).

**Rate limiting** — promoted to its own first-class concern, not
buried inside cache extensions. Every production API needs it from
day one and it should not depend on the rest of cache shipping.

```rust
// Programmatic:
RateLimiter::for_("login").limit(5).per_minute().attempt(&ip)?;

// As middleware on a route group:
group!("/api", { ... }).middleware(ThrottleMiddleware::per_minute(60))
```

Built on the same Redis / DragonflyDB / in-process backends as cache,
but with the API exposed at the middleware layer where it's actually
applied. Per-IP, per-user, per-token-ability all supported.
**Rust gun:** the sliding-window algorithm runs on a Redis Lua script
or an in-process `DashMap`, both atomic — no race conditions under
load that PHP-style "increment + check" implementations leak.

**Notifications.** Channel-based delivery.

```rust
Notify::send(&user, OrderShipped { order_id })
    .via(&["mail", "slack", "database"]).await?;
```

Channels: mail, Slack, Discord, SMS via Twilio, database, webhook,
broadcast (Track 8). Depends on Mail (Track 4) and Broadcasting
(Track 8).

### Track 8 - Real-time at full strength (where Rust eats Laravel's lunch)

The "rust guns" track. Track 1 ships minimal SSE so events have a
delivery primitive from day one; this track ships the full
two-way WebSocket story plus presence and supervised workers.
Laravel uses Pusher or Reverb bolted on as a separate service.
Suprnova runs it in-process by default.

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

**Presence channels** — `PresenceChannel<RoomId>` knows who's
connected, server-side, in real time. Joining/leaving fires `Event`s
that other listeners (or other connected clients) react to.

**Supervised background workers** in-process.

```rust
Worker::supervise("payments.poll", Duration::from_secs(30), || async {
    poll_pending_payments().await
}).await?;
```

Crashes restart with exponential backoff. Scoped to app lifetime.
For monolith apps, this replaces the entire "deploy Horizon + Redis
+ a separate worker container" stack.

### Track 9 - No-gatekeeping differentiation

The Suprnova-specific value-add. Each is a trait + driver(s).

**Vector DBs.**

```rust
Vector::store("documents")
    .upsert(&[("doc-1", embedding, metadata)])
    .await?;
Vector::store("documents").similar(query_embedding, k: 10).await?;
```

Drivers: Qdrant, Weaviate, Milvus, LanceDB, pgvector, MariaDB
(VECTOR data type), LibSQL/SQLite (`vector` extension). Type-safe
embeddings (`Vec<f32>` of compile-checked dimension). The MariaDB
and LibSQL drivers are the concrete proof of "no gatekeeping" —
neither is supported by Laravel's vector story today.

**Graph DBs.** `Graph::node(...).related_to(...).match(...)`.
Drivers: Neo4j (Bolt protocol), ArangoDB, SurrealDB, MemGraph.

**Time-series.** `Timeseries::write(measurement, tags, fields, ts)`,
batched writes. Drivers: InfluxDB, TimescaleDB, QuestDB, ClickHouse.

**Search.** `Search::index("users").add(doc).query("alice").await?`.
Drivers: Meilisearch, Typesense, Elasticsearch, Algolia.

The pattern that matters: trait surface stays the same, drivers swap
behind it. A consumer migrating from Meilisearch to Typesense changes
one config value.

### Track 10 - Polish

Small individually, big collectively.

- **Translation (i18n)** - file-based locales, `__("users.welcome", name: "Sue")`.
- **Support helpers** - `Str::camel`, `Arr::pluck`, `Stringable` chain.
  Most exist in std/itertools; we ship the Laravel-named wrappers.
- **Routing extras** - resource routing (`Route::resource("users", UsersController)`
  → 7 RESTful routes), signed routes, named-route reverse,
  sub-domain routing. (Route throttling lives in Track 7 alongside
  rate limiting.)
- **Container scoped bindings** - singleton / per-request / transient.
  We have some of this; needs filling in.
- **Process** - `Process::run("git status").output().await`. Wraps
  `tokio::process`. Small.

> **Note on testing helpers.** `Mail::fake()` / `Queue::fake()` /
> `Event::fake()` / `Http::fake()` are *not* polish items. They ship
> with their respective tracks per Philosophy rule 4. They appear here
> only as a cross-reference, not as deferred work.

## Recommended sequencing

Each phase unblocks the next. Approximate effort in italics; not
commitments. Every phase ships its fakes/assertions in the same
commit (Philosophy rule 4).

**Phase 1: Logging + Events + Error handling + minimal SSE** *(5 weeks)*
Foundation observability. Everything else uses them. The longer we
wait, the more retrofitting we owe. Minimal SSE rides along so
events have a delivery primitive from day one.

**Phase 2: HTTP client + Pagination + Encryption** *(3 weeks)*
Small, high-leverage, often-used. Encryption replaces the sign-only
cookie path; HTTP client unblocks third-party API integrations every
real app needs.

**Phase 3: Authorization + API mode** *(4–5 weeks)*
Gates + Policies + token auth + JSON Resources + `--api` scaffolding.
Day-one expectation for any Laravel dev, separate from the Auth track
that already shipped. The bigger your app gets, the more this matters.

**Phase 4: Filesystem + File uploads + Validation parity** *(5–7 weeks)*
Storage drivers and upload handling together because controllers
touch both. Validation gets finished here because we already exercised
the gaps in Precognition.

**Phase 5: Queue + Mail + Notifications + Rate Limiting** *(5–6 weeks)*
Mail-via-queue is the canonical pattern; ship them together.
Rate limiting middleware in the same wave because cache + redis are
already set up. Notifications layer on top.

**Phase 6: Factories + Seeders + Configuration + Console** *(2–3 weeks)*
The Laravel-dev day-one expectations not covered earlier. Small but
high-impact for DX.

**Phase 7: Full Broadcasting + supervised background workers** *(5–6 weeks)*
WebSocket + presence + channel auth. The "Rust eats Laravel's lunch"
moment at full strength — Phase 1 already shipped SSE for the simpler
cases. This is the demo that gets a Laravel dev to say "wait, you can
do that in one process?"

**Phase 8: Differentiation** *(ongoing)*
Vectors, graphs, search, time-series. Driven by real consumer needs
(`nation-x.com` will exercise some). Ship one when the demand exists;
the others queue up behind.

**Phase 9: Polish** *(parallel with phases above)*
Translation, Support helpers, Process, scoped bindings, routing extras.
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

## Deployment

Suprnova's in-process design (workers, broadcasting, schedule, all
running in the same Tokio runtime as HTTP handlers) has deployment
implications that differ from PHP-FPM behind nginx. A Laravel app
typically ships as stateless PHP processes plus a queue worker plus
a broadcast server plus a scheduler — four moving parts to deploy
independently. A Suprnova app is one binary.

**The default deploy target: single long-lived process.** A VPS, a
container running on Fly.io / Render / Railway / a bare EC2, a
systemd unit on metal. The same binary that serves HTTP also runs
background workers (`Worker::supervise(...)`), the schedule daemon,
the WebSocket connections, the SSR worker spawn. State that needs to
persist across restarts (queue jobs, scheduled-task last-run-at)
lives in the configured backends.

**Scale-out story.** When one process isn't enough:
- HTTP scales horizontally — `LoadBalancer → N binaries`. Sessions
  live in the DB/Redis driver; nothing in process memory that can't
  be reconstituted.
- Background workers can be split off — `suprnova queue:work` runs
  jobs from the configured queue backend without serving HTTP.
- Broadcasting via WebSocket scales horizontally with a Redis-backed
  pub/sub bridge that ships in Track 8 — multiple binaries each hold
  some connections; a publish on one fanout-emits via Redis to
  subscribers on the others.

The `suprnova docker:init` and `docker:compose` CLI commands already
ship the "one binary, multiple roles" pattern. Documentation for
Fly.io, Render, Kubernetes, and bare metal lives at
[suprnova.app](https://suprnova.app/).

**What you don't need:** a separate queue-worker container, a separate
scheduler, a separate broadcast server, a separate SSR runtime
co-tenant — all of these run in the main binary by default. Pull them
out when scale demands it; until then, one box.

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

## Moonshot — hot module reload for Rust handlers

This is the kind of hard that earns the name. The biggest DX gap
between Suprnova and Laravel today is that `cargo build` takes time.
Laravel devs save a file and see the change instantly; Rust devs
wait. `cargo-watch` answers part of this but it's a full process
restart — WebSocket connections drop, session state in memory clears,
the Tokio runtime spins down and up.

**What if it didn't?** In dev mode, compile each controller / route
module / Inertia handler as a dylib. On file change: `cargo build`
just that crate, swap the dylib symbol via `libloading`, keep the
HTTP server, keep the WebSocket connections, keep the in-process
queue, keep everything. The new handler is live without a restart.

The hard parts:
- Dylib hot-swap is sound in C-style ABI but Rust ABI is unstable.
  Either we lock to the C ABI for hot-reloadable modules or we accept
  the constraints of `abi_stable` / similar.
- State that lives across the swap (session task-locals, container
  bindings, registered routes) needs to survive the symbol replacement
  without dangling.
- Type-system changes (you added a field to a `Props` struct) probably
  require a full rebuild anyway. The win is for changes that don't
  alter the public ABI of the swapped unit.
- Dev-mode-only — production runs the normal compiled binary with
  hot-reload disabled.

Why we'd want it anyway:
- The "wait, you can do that in Rust?" demo to a Laravel dev who's
  been told Rust is "too slow to iterate on for web work."
- WebSocket-heavy apps lose context every rebuild today (you have to
  reconnect the test page). With hot-swap, the connection stays,
  the page state stays, and the new handler is just live.
- Compounds with Phase 7's broadcasting/presence — iterate on a live
  feature without losing the multiplayer state you're testing.

This sits in research / not-on-the-critical-path. But if a contributor
shows up with experience in dylib hot-swap, supervised state
preservation, or `bevy_reflect`-style runtime type info, this is the
project to point them at. The reward isn't a feature — it's a
positioning moment that reframes what Rust web development feels
like.
