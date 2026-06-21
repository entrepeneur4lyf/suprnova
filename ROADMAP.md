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

| Laravel pattern | Suprnova approach |
|-----------------|-------------------|
| String-based service container (`app(MyService::class)`) | `App::resolve::<MyService>()` - compiler-verified |
| Eloquent reflection-driven attribute access | SeaORM derives - typed columns, IDE autocomplete works |
| Request-per-process model | Long-lived state, in-process workers, real async |
| Reflection-driven DI | Macros + traits - compile-time wiring |
| Single-backend per domain | Trait + driver |
| Magic config caching that breaks in dev | Env + typed config struct |

### 4. Testing ships with the feature

A track is not "done" until its testing surface is shipped alongside it.
When Queue ships, `Queue::fake()` + `Queue::assert_pushed(...)` ship in
the same commit; when Mail ships, `Mail::fake()` + `Mail::assert_sent(...)`
ship in the same commit. A track consumed by a controller must be
testable without spinning up real infrastructure.

## Shipped

Everything below is production-ready, tested, and live in the framework
today. Detail per feature lives in the [CHANGELOG](CHANGELOG.md) and the
[manual](manual/).

### HTTP & core

- [x] Routing — `routes!` DSL, groups, named routes, compile-time path validation
- [x] Middleware — typed pipeline, route groups, global middleware
- [x] Container — `#[service]` / `#[injectable]`, `App::resolve::<T>()` / `App::make::<T>()`
- [x] Requests & responses — extractors, `Response` result type, streaming bodies, SSE
- [x] Error pipeline — `FrameworkError` + layered context, 5xx/4xx tracing, `ErrorOccurred`
- [x] Sessions — async-safe task-local, DB-backed store, flash, AES-encrypted cookies
- [x] CSRF — middleware, helpers, meta-tag emit
- [x] Encryption — AES-256-GCM `Crypt`, `APP_KEY` (+ rotation ring)
- [x] Hashing — argon2, verify / needs_rehash
- [x] Context facade — per-request key/value bag
- [x] Logging — `tracing` structured logs, request-id propagation
- [x] HTTP client — `Http::*` over reqwest, fakeable
- [x] Graceful shutdown — signal-aware, flushes telemetry
- [x] Static files — `StaticFiles::public()` fallback handler

### Frontend (Inertia)

- [x] Inertia v3 protocol — full protocol, SSR client + CLI worker, Precognition, infinite scroll, history encryption, partial reloads
- [x] Frontend starters — React 19 / Vue 3.5 / Svelte 5 on Vite + Tailwind v4 + Inertia 3.4
- [x] Data objects — `#[derive(Data)]` DTOs + TS export, tri-state `Field<T>`
- [x] Pagination — length-aware + cursor paginators, encrypted cursors, Inertia scroll bridge
- [x] JSON:API resources — `#[json_resource]`, compound docs, sparse fieldsets, includes
- [x] API scaffolder — `suprnova new --api` JSON-only starter

### Auth & access

- [x] Auth — session-based, named guards (web + API token), `Authenticatable` + `EloquentUserProvider`
- [x] Auth methods — torii integration (password / oauth / passkey / magic-link)
- [x] Bearer-token API auth
- [x] Authorization — `Gate` / `#[policy]`, default-deny
- [x] RBAC — `HasRoles`, roles/permissions tables, role + permission middleware, `authorize_resource`
- [x] Auth flows — email verification, password reset, 2FA TOTP + recovery codes, brute-force / login throttling, remember-me

### Database & Eloquent

- [x] Database — SeaORM, SQLite / Postgres / MySQL / MariaDB, migrations, model derives
- [x] Eloquent foundation — `#[model]`, CRUD, dual-API builder, fillable/guarded, 22 casts, accessors/mutators, timestamps, soft deletes, prunable
- [x] Eloquent relationships — all relation kinds incl. morphs / through / pivots, eager loading (nested, counts, aggregates)
- [x] Eloquent lifecycle, collections & querying — 16 model events + observers, scopes, `Collection<M>`, 3 paginators, chunking / lazy / cursor, row locking, `DB::table`, transactions, multi-connection, replication
- [x] Query log / observability — `DB::listen` / `QueryExecuted` / query log covering reads and writes

### No-gatekeeping backends

- [x] Cache — Redis / memory, `remember` / `forever` / `tags` / `lock`
- [x] Queue — Redis / database / in-process, typed `Job`, supervised `queue:work`
- [x] Rate limiter — fluent attempts/decay/lockout + `ThrottleRequests`
- [x] Filesystem — `Storage::disk` over opendal (FS / memory / S3 / Azure / GCS)
- [x] Mail + Notifications — 6 transports, `Mailable`, mail/db/slack channels, fakeable
- [x] Vector — `VectorDriver` + Memory / Qdrant / Pinecone / MariaDB native

### Realtime, jobs & scheduling

- [x] WebSockets — `ws!()` macro, heartbeat, per-route middleware
- [x] Broadcasting — public / private / presence channels, sea-streamer fanout, `Broadcastable`
- [x] Supervised workers — `Supervisor` trait, panic-catch auto-restart
- [x] Events — typed dispatch / listen / fake, sync + queued delivery
- [x] Workflow — durable steps, deterministic ordering, claim protection
- [x] Schedule — cron expressions, task registry, daemon

### Tooling & extras

- [x] CLI — `new` / `serve` / `migrate*` / `make:*` / `schedule:*` / `workflow:*` / `ssr:*` / `db:sync` / `generate-types`
- [x] Console — `#[command]` + typed args, app-binary runner, `make:command`
- [x] Factories, seeders & typed config — `Factory` / `Persistable` / `Seeder` / `db:seed` / `Config::resolve`
- [x] Validation — `Rule` / `ContextualRule` / `AsyncRule`, `FormRequest` hooks
- [x] File uploads — `#[derive(MultipartRequest)]`, streaming, composable validators
- [x] Content rendering — Markdown pipeline (comrak → syntect → ammonia), `build_docs` catalog
- [x] Observability export — OpenTelemetry (opt-in `otel`), metrics facade
- [x] Payments — provider-neutral traits + Stripe + Paddle adapters, mirror tables, webhook idempotency
- [x] Feature flags — `Features::active`, DB + cached evaluators, middleware, admin CRUD

### Starter kits

Each ships in its own repo, pinned to Suprnova and dogfooding it end-to-end.

- [x] [Nebula](https://github.com/entrepeneur4lyf/Nebula) — Breeze-tier auth kit (Inertia 3 + Svelte 5)
- [x] [Pulsar](https://github.com/entrepeneur4lyf/Pulsar) — product-site + community kit (Vue 3.5 + Vuetify)

## Planned

New layers on the same trait + driver pattern. Shipped when
production-ready, not on a calendar.

### No-gatekeeping backends

- [ ] Graph DBs — Neo4j / ArangoDB / SurrealDB / MemGraph
- [ ] Search — Meilisearch / Typesense / Elasticsearch / Algolia
- [ ] Time-series — InfluxDB / TimescaleDB / QuestDB / ClickHouse

### Facades & helpers

- [ ] Schema & Query Builder facades over `sea-query` (`Schema::create`, `Query::table`)
- [ ] Translation (i18n) — file-based locales, `__("users.welcome", ...)`
- [ ] Support helpers — `Str::*` / `Arr::*` / `Stringable`
- [ ] `Process` facade over `tokio::process`
- [ ] Container scoped bindings — per-request + transient
- [ ] Natural-language cron — `.at("every day at 8am")`
- [ ] `suprnova doctor` — env / config / DB / migration / SSR diagnostics

### More starter kits

- [ ] Teams + 2FA kit
- [ ] Billing kit on the Payments surface

### Exploratory

- [ ] Hot module reload for dev handlers — dylib hot-swap so a `cargo build` of one module goes live without dropping WebSocket / queue / runtime state. Dev-only, research-grade, not on the critical path.

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
- Broadcasting via WebSocket scales horizontally with the shipped
  Redis-backed pub/sub bridge — multiple binaries each hold some
  connections; a publish on one fanout-emits via Redis to subscribers
  on the others.

The `suprnova docker:init` and `docker:compose` CLI commands already
ship the "one binary, multiple roles" pattern. Documentation for
Fly.io, Render, Kubernetes, and bare metal lives at
[suprnova.app](https://suprnova.app/).

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
