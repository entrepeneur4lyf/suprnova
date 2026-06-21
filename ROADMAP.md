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

The list below is the shipped surface as of the **v0.1.0** release
(2026-06-10). Every item here is production-ready, tested, and live in
the framework today.

- **Inertia v3 protocol** - every protocol field, every header, SSR client
  + CLI worker (`ssr:start` / `ssr:check`), Precognition (Validate-Only
  filter, 204/422 envelope, Vary header), infinite scroll
  (`Inertia::scroll` + `scrollProps`), preserveFragment via session-flash,
  history encryption, shared data, lazy/optional/defer/merge/once props,
  flash, 302→303 conversion. 229 framework tests.
- **Auth** - session-based with regenerate-on-login, `Authenticatable`
  trait + `EloquentUserProvider<M>`, multiple named guards (web session
  + API token), `Auth::attempt` / `login` / `user` / `user_as<T>` /
  `logout` / `check` facade. Email verification, password reset,
  two-factor TOTP (with recovery codes + replay protection),
  brute-force / login throttling, and remember-me cookies all ship.
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
- **Database** - SeaORM, four databases (SQLite, Postgres, MySQL, and MariaDB via the MySQL driver),
  migrations, model derives.
- **Frontend starters** - React 19 / Vue 3 / Svelte 5, all on Vite 8 +
  Tailwind v4 + Inertia 3.1, SSR-aware (`data-server-rendered` honored).
- **CLI** - `suprnova new` / `serve` / `migrate*` / `make:*` /
  `schedule:*` / `workflow:*` / `ssr:*` / `db:sync` / `generate-types`.
- **Logging** - `tracing`-based structured logs with env-driven config
  (`LOG_LEVEL` / `LOG_FORMAT=pretty|json`). `RequestId` UUID-v4 task_local
  installed by `RequestIdMiddleware` outermost; `X-Request-Id` echoed
  on every response (success or error).
- **Events** - `EventFacade::dispatch` / `EventFacade::listen` /
  `EventFacade::fake` + typed `Event` / `Listener<E>` traits.
  Sync + queued (per-listener `tokio::spawn`) delivery. Built-in
  `ErrorOccurred` event dispatched on every 5xx with
  `error_message` / `status_code` / `request_id`. Fake guard records
  events for `assert_dispatched` / `assert_not_dispatched` /
  `dispatched_count` assertions.
- **Error pipeline** - `FrameworkError::context(msg)` for layered
  error context (preserves status code, prepends message).
  `From<FrameworkError> for HttpResponse` emits `tracing::error!`
  for 5xx + `tracing::warn!` for 4xx, and spawns `ErrorOccurred`
  for 5xx.
- **SSE delivery primitive** - `HttpResponse::sse(stream)` over the
  new `Body::Stream(BoxBody)` variant. `SseEvent::data`/`with_event`/
  `with_id`/`json` builders + spec-compliant wire framing. Works
  end-to-end against a real socket.
- **Streaming response body** - `Body::Static(Bytes) | Body::Stream(BoxBody)`
  enum replaces the old `body: String`. `HttpResponse::stream_bytes`
  builds arbitrary `Stream`-backed responses.
- **OpenTelemetry export** (feature `otel`, opt-in) - OTLP HTTP-proto
  exporter for traces + metrics + logs. `init_telemetry(LogConfig,
  OtelConfig)` returns a `TelemetryGuard` that flushes providers on
  shutdown. Enabled when `OTEL_EXPORTER_OTLP_ENDPOINT` is set and
  `OTEL_SDK_DISABLED != "true"`. Default OFF — pulls reqwest only
  when the feature is enabled.
- **Metrics facade** - `Metrics::counter("x").inc()` /
  `inc_by(n)` / `inc_with(attrs)`, `Metrics::histogram("x").record(v)`,
  `Metrics::gauge("x").set(v)`. Instrument handles cached in
  `OnceLock<RwLock<HashMap>>` so the inc/record/set path is an
  `Arc<Counter>.add(...)` call. No-ops cleanly when the `otel`
  feature is off (zero-cost stubs preserve the same API).
- **Graceful shutdown** - `Server::run` waits on `tokio::select!`
  between `listener.accept()`, `ctrl_c()`, and unix `SIGTERM`. On
  signal, calls `TelemetryGuard::shutdown().await` to flush OTel
  batch processors before exit.
- **Context facade** - Laravel-shaped per-request key/value bag.
  `Context::add` / `get` / `push` (stack) / `has` / `forget` / `all`
  + `hidden_add` / `hidden_get` (separate bag, excluded from
  `all()`). Scoped via `tokio::task_local!`; `RequestIdMiddleware`
  seeds `_request_id` automatically.
- **Encryption** - AES-256-GCM via the `aes-gcm` crate. `EncryptionKey`
  loads a 32-byte key from `APP_KEY` (base64-url-no-pad) or generates
  one. `Crypt::encrypt_string` / `decrypt_string` / `encrypt<T>` /
  `decrypt<T>` use a 12-byte random nonce per call, empty AAD; wire
  format is base64-url-no-pad over `nonce || ciphertext || tag`.
  `Crypt::init` runs at `Server::from_config` boot.
- **HTTP client** - `Http::get` / `post` / `put` / `patch` / `delete`
  return a `RequestBuilder` with `json` / `form` / `body` / `header` /
  `bearer_token` / `basic_auth` / `timeout` and a single-shot `send`.
  Backed by reqwest 0.12 with rustls (single TLS backend shared with
  the OTel exporter), 30s default timeout, `suprnova/<version>`
  user-agent. `ClientResponse` exposes `status` / `header` / `json` /
  `text` / `bytes` / `into_inner`. `Http::fake()` returns an
  `HttpFakeGuard` that intercepts every outbound request — queue
  canned responses with `fake_response(method, url_substring, status,
  body)`; assert via `assert_sent` / `assert_not_sent`.
- **Pagination** - `LengthAwarePaginator` (`data`/`total`/`per_page`/
  `current_page`/`last_page`) and `CursorPaginator`
  (`data`/`next_cursor`/`prev_cursor`). `Pagination::length_aware` runs
  COUNT(*) + OFFSET/LIMIT against a SeaORM `Select<E>`;
  `Pagination::cursor` walks an `order_col` ASC with keyset filters.
  Cursors are AES-256-GCM-encrypted via `Crypt` (plain-base64 fallback
  with `tracing::warn!` when `Crypt` is uninitialized). `IntoInertiaScroll`
  trait maps either paginator into `(ScrollMetadata, Vec<T>)`.
  `Inertia::paginate(key, paginator)` and
  `InertiaResponse::paginate(key, paginator)` wire a paginator into a
  scroll prop in one line.
- **Encrypted session cookies** - session cookies are now AES-256-GCM
  encrypted using the `APP_KEY` installed at server boot. Pre-1.0 hard
  cut: existing plaintext sessions silently become unreadable after
  deploy and clients get a fresh session ID.
- **Data Objects** - `#[derive(Data)]` unifies request DTOs, response
  DTOs, and TypeScript exports in one struct. `Field<T> = Absent | Null
  | Value(T)` tri-state for PATCH endpoints. `?include=`/`?fields[type]=`
  task-locals scoped by `IncludeMiddleware` with default-deny 400 on
  unknown keys. Lazy flavors (`plain`/`inertia`/`deferred`/`closure`/
  `when_loaded`) + auto-registration of `allow_include` allowlist via
  `inventory`. Type-aware route-param coercion at macro time. Generic
  structs (TS interface generics + suppressed FormRequest where bounds
  can't verify). Paired `<Name>` / `<Name>Input` TS interfaces.
- **Authorization** - `Gate::define::<U, R>(action, |u, r| ...)`,
  `allows` / `denies` / `authorize` (`FrameworkError::Unauthorized` on
  deny). Default-deny on missing gates. `#[policy(User, Resource)]`
  proc macro emits Gate registrations per impl method via `inventory`
  free-function shims (avoids the `'static` constraint on
  `inventory::submit!`). `init_policies()` (guarded by `Once`) called
  from `Server::serve`.
- **Auth methods (torii integration)** - `Auth::password()` /
  `Auth::oauth(provider)` / `Auth::passkey()` / `Auth::magic_link()`
  facades over `torii` 0.5.x + `torii-storage-seaorm`. Process-global
  `OnceLock<Torii<SeaORMRepositoryProvider>>` initialised by
  `init_torii(ToriiConfig)`. Provider configs registered via
  `Auth::oauth("github").configure(OAuthProviderConfig { ... })`.
  Passkey WebAuthn challenge/verify with sensible defaults.
- **Bearer-token API auth** - `BearerTokenMiddleware` resolves
  `Authorization: Bearer <token>` to a torii session, binds
  `user_id` to the request session scope without short-circuiting on
  missing/invalid tokens (route-level `AuthMiddleware` produces the
  canonical 401).
- **JSON:API resources** - `#[derive(Data)] #[json_resource("<type>")]`
  emits an `IntoJsonResource` impl alongside the existing
  `IntoInertiaData`. `Resource::single` / `Resource::collection` /
  `Resource::paginated` produce spec-compliant envelopes:
  `data.type`/`id`/`attributes`/`relationships`, top-level
  `included` compound documents (deduplicated by `(type, id)`),
  `links`/`meta`. Sparse fieldsets via `?fields[users]=name,email`
  + `RequestFieldsetSet` task-local. Multi-level dot-notation
  `?include=author.posts.tags` parsed into `IncludeTree` and walked
  recursively; unknown keys produce a JSON:API 400 errors envelope
  (`FrameworkError::into_json_api_response`). Relationships type
  `Option<T>` / `Vec<T>` / `T` where `T: IntoJsonResource`;
  `Prop` remains Inertia-only.
- **API mode scaffolder** - `suprnova new <name> --api` emits a pure
  JSON-API starter: `Cargo.toml`, `main.rs`, `lib.rs`, `bootstrap.rs`
  (registers `BearerTokenMiddleware` + `IncludeMiddleware` globally,
  no Inertia), `routes.rs` (JSON-only routes including register/login
  and an example `Resource::collection`), example `UserResource`
  (`#[derive(Data)] #[json_resource("users")]`), Post-style models,
  migrations, `.env` placeholders.
- **Filesystem** — `Storage::disk("name")` facade backed by `opendal`
  0.56, with FS / memory / S3 / Azure Blob / GCS drivers all
  first-class. `copy_between_disks` streams 64 KiB chunks across any
  backend pair. `Storage::fake()` provides registry isolation for tests.
- **File uploads** — `#[derive(MultipartRequest)]` strongly-typed
  extractor with streaming `multer` parser. `UploadedFile<V>` supports
  composable validators (`Image` magic-byte detection, `MaxSize<N>`
  byte-boundary rejection, `MimeType<L>` allowlists, plus tuple
  composition). Array uploads (`Vec<UploadedFile<V>>`) preserve
  duplicate-name parts for `photos[]`-style forms.
  `MultipartRequestHooks` mirrors `FormRequest`'s `authorize` /
  `after_validation` lifecycle.
- **Validation parity** — `Rule` trait (`Required`, `Email` via
  `validator::ValidateEmail`, `Min`, `Max`), `ContextualRule` for
  conditional rules (`RequiredIf`, `RequiredWith`, `RequiredUnless`),
  `AsyncRule` with `Unique` issuing real parameterized DB queries
  (no in-memory fakes). `FormRequest::after_validation` cross-field
  hook. `ValidationErrors::add_to_bag` for named-scope errors.
- **Queue + Rate Limiter + Cache** — `Queue::dispatch` /
  `Queue::push` over Redis / Database / in-process drivers; typed
  `Job` trait; supervised workers via `queue:work`. `RateLimiter::for`
  fluent attempts/decay/lockout API + `ThrottleRequests` middleware.
  Multi-store `Cache::store("redis")` / `Cache::store("memory")` with
  `remember` / `forever` / `tags` / `lock` (atomic per-key lock).
- **Mail + Notifications** — `Mail::to(&user).send(WelcomeEmail)`
  facade over SMTP / SES / Postmark / Mailgun / Resend / SendGrid /
  log drivers (six first-class transports). `Mailable` trait +
  Markdown + plain-text + HTML rendering via `tera`. `Notifications`
  facade with mail + database + slack channels. `#[derive(NotificationMailable)]`
  for compile-time notification wiring. Test fakes (`Mail::fake()` /
  `Notification::fake()`) record sends for `assert_sent` / `assert_to`.
- **Factories + Seeders + Typed Config + Console** —
  `Factory` trait + `FactoryBuilder` with `Sequence`, `count(n)`,
  `state(closure)`. `Persistable` bridges factory output to the DB.
  `Seeder` trait with `db:seed` console command. `Config::resolve::<T>()`
  uses `envy` for compile-time-typed env-driven config. `#[command]`
  attribute + `#[derive(Command)]` typed args + `inventory`-registered
  CLI surface. App-binary console runner (`cargo run --bin console`) —
  Rust analogue of `php artisan`. `make:command` generator. `silent`
  error variant for graceful CLI exit codes.
- **WebSockets + Broadcasting + Supervised Workers** —
  `ws!()` macro + `Router::ws` + `hyper-tungstenite` upgrade
  integration. `WsSocket` Sink/Stream split. Per-connection heartbeat
  with close-on-no-pong. `BroadcastHub` trait + in-memory + sea-streamer
  fanout (feature-gated). `Channel` / `PrivateChannel` / `PresenceChannel`
  with join/leave/here events. JSON envelope protocol (`ClientFrame` /
  `ServerFrame`). `Broadcastable` trait bridges typed events to
  channels via `EventFacade`. Per-route `ws!(...).middleware(...)`.
  `Supervisor` trait + registry with panic-catch auto-restart and
  graceful shutdown. Cross-process presence via sea-streamer
  meta-channel.
- **Eloquent Foundation** — `#[suprnova::model]` attribute
  macro emits SeaORM-bridged Entity + Column + ActiveModel + inner
  Model. `Model` trait CRUD (`find` / `save` / `update` / `delete` /
  `refresh` / `replicate` / `first_or_create` / `update_or_create` /
  `increment` / `decrement`). `Builder<M>` with dual-API where surface
  (`filter*` Rust-shape + `db_where`/`where_*` Laravel-shape, both
  first-class aliases). Inventory-backed `ModelEntry` registry for
  admin enumeration. `Fillable` / `Guarded` + `attrs!` macro +
  `unguarded(closure)` task-local bypass. **22 built-in casts**:
  primitive (`AsBool` / `AsInt` / `AsFloat` / `AsString` / `AsDecimal`),
  temporal (`AsDate` / `AsDateTime` / `AsTimestamp` / `AsImmutableDate*`),
  structured (`AsJson` / `AsObject` / `AsArray` / `AsArrayObject` /
  `AsCollection`), enum (`AsEnum`), encrypted (`AsEncrypted` /
  `AsEncryptedObject` / `AsEncryptedArray` / `AsEncryptedCollection`
  with `APP_KEY_PREVIOUS` rotation ring), hashed (`AsHashed`). Custom
  cast trait + `with_casts` runtime override. `#[accessor]` /
  `#[mutator]` function-level macros with `appends = [...]`. Auto-managed
  timestamps + `touch()` + parent touching via `#[model(touches=...)]`.
  Soft deletes (`delete` / `restore` / `force_delete` / `with_trashed`
  / `only_trashed`). `Prunable` (per-row with `pruning()` hook) +
  `MassPrunable` (set-based DELETE) + inventory-registered `model:prune`
  command.
- **Eloquent Relationships** — every Eloquent relation
  kind: `HasOne` / `BelongsTo` (with `with_default` closure) / `HasMany`
  / `BelongsToMany` (first-class `Pivot` models as their own
  `#[suprnova::model]`, `attach` / `detach` / `sync` mutators, transactional
  sync, pivot context surfaced via `row.pivot::<P>()` accessor) /
  `HasOneThrough` + `HasManyThrough` (with key inference) /
  `MorphTo` (with per-family generated enum `<Name>Morph { Variant(T),
  Unknown(String, i64) }`) / `MorphOne` / `MorphMany` / `MorphToMany`
  / `MorphedByMany`. Relations declared in `#[model(relations = { ... })]`
  with options (`fk`, `lk`, `with_pivot`, `with_timestamps`,
  `with_default`, `related_key`, `pivot_*`, `first_key`, `second_key`,
  `target_morph_type`, `name`, `targets`). **Eager loading** — `with(["posts"])`,
  nested paths (`"posts.comments.author"`), `with_count` + `with_sum`
  / `with_avg` / `with_min` / `with_max` (per-relation `<rel>_sum_of(col)`
  accessors that compose), `with_where_<rel>(closure)` (typed
  per-relation methods, no `Builder<R>` annotation needed),
  `Collection::load` / `load_missing` (per-row partition + nested-tail
  recursion). All count + aggregate dispatchers issue **server-side
  GROUP BY** queries (zero client-side reduce). `EagerLoadCache` +
  `__pivot` fields auto-injected per model via macro field rewriting.
  `EagerLoadDispatch` sealed trait + four per-model dispatchers
  (`__eager_load` / `__recurse_eager_load` / `__count_relation` /
  `__aggregate_relation`) with backend-aware CAST + parameter
  placeholders. `MorphTypeEntry` inventory provides runtime
  `morph_type -> TypeId` lookup; MorphTo dispatch consults the
  registry (not heuristic keys) so custom `morph_type = "blog_post"`
  declarations route correctly. `with_trashed` / `only_trashed`
  forwarding on all 10 relation types. Prunable does NOT cascade
  (correct Laravel default); cascade via DB FK or `pruning()` hook.
- **Vector** — `VectorDriver` trait + `Vector::register` /
  `Vector::store` facade + `VectorItem` / `VectorMatch` contract.
  Four production drivers ship in v1: `MemoryVectorDriver` (in-process,
  cosine similarity), `QdrantVectorDriver` (gRPC via `qdrant-client`,
  auto-create collections, parse-then-hash-to-UUID-5 id mapping with
  `__suprnova_id` payload key), `PineconeVectorDriver` (gRPC via
  `pinecone-sdk`, namespace-scoped, native string ids, JSON ↔
  `prost_types::Struct` metadata bridge), and `MariaDbVectorDriver`
  (native `VECTOR(N)` + HNSW via direct `sqlx::MySqlPool`, requires
  MariaDB 11.7+, `tokio::sync::OnceCell`-cached version check, native
  `VARCHAR(255)` ids, JSON metadata column, identifier-validated +
  backtick-quoted store names defending against SQL injection,
  source-verified score normalization per metric). Each driver exposes
  a `client()` / `pool()` trapdoor for filter expressions, scroll,
  quantization, or raw SQL not surfaced via the trait. Laravel ships
  pgvector-only; we don't gatekeep. Weaviate + Milvus + LanceDB +
  pgvector + LibSQL queue up behind real consumer demand. The MariaDB
  driver anchors the "one DB, four jobs" production positioning
  (relational + vector + JSON + temporal on one engine — see
  `docs/core/vector.md` for the SQLite-dev / MariaDB-prod story).
- **Eloquent Lifecycle + Collections + Querying Power**
  — Model events: 16 lifecycle structs in a per-model `events::*`
  submodule (`Retrieving` / `Retrieved` / `Saving` / `Creating` /
  `Updating` / `Deleting` / `Restoring` / `Created` / `Updated` /
  `Saved` / `Deleted` / `Trashed` / `Restored` / `Replicating` /
  `ForceDeleting` / `ForceDeleted`) + `EventResult::Ok | Cancel(String)`
  for the cancellable family + cancel-propagates-as-`bad_request`.
  `Observer<M>` trait with 16 default no-op methods +
  `#[suprnova::observer(M)]` attribute that walks the impl block
  and emits adapter listeners only for overridden methods +
  `#[model(observers = [...])]` declarative + `Model::observe()`
  manual + `bootstrap_observers()` inventory drain at boot.
  Local scopes via `#[suprnova::scopes(Model)]` impl-block
  attribute — emits both a static `Model::active()` helper and a
  `Builder<Model>::active()` chainable extension via a per-(scope,
  model) trait. Global scopes via `GlobalScope<M>` trait +
  `ScopeRegistry::register` + `without_global_scope::<T>()` /
  `without_global_scopes()` opt-out (per-type and all-at-once).
  `Collection<T>` Laravel surface (~25 generic methods) +
  `Collection<M>` model-aware methods (string-keyed pluck /
  group_by / sort_by / sum / avg / min / max routed through
  macro-emitted `field_value`) + `Builder::get → Collection<M>`
  return-type change (deref preserves Vec compat). Serialization:
  `Model::to_array() -> Value` + `Model::to_json() -> String`
  honouring `hidden = [...]` / `visible = [...]` / `appends = [...]`,
  with `__eager` / `__pivot` always stripped. Three paginators —
  `LengthAwarePaginator` (offset + COUNT(*)) + `Paginator` (simple,
  no COUNT) + `CursorPaginator` (keyset) — all Serialize, with
  `paginate_using("page_param", n)` query-param override.
  Chunking — `chunk(n)` (OFFSET) + `chunk_by_id(n)` (PK-cursor,
  concurrent-safe) + `chunk_map(n)` + `each` + `lazy()` /
  `lazy_by_id(n)` / `cursor()` returning a `LazyCollection<M>`
  stream wrapper. Row locking — `lock_for_update()` /
  `shared_lock()` with backend-aware SQL (`FOR UPDATE` / `LOCK IN
  SHARE MODE` / `FOR SHARE`); SQLite no-op with `warn!`-once.
  DB facade — `DB::table(name)` raw query builder returning
  `Collection<DynamicRow>` + `DB::select` / `update` / `delete` /
  `statement` / `affecting_statement` raw escapes + `DynamicRow`
  newtype with typed accessors. Transactions — `DB::transaction(closure)`
  HRTB closure form (auto-detects via `tokio::task_local` so
  Builder<M> / Model ops inside compose) + `DB::begin_transaction()`
  manual form + `tx.savepoint(name)` / `tx.rollback_to(name)` +
  `DB::transaction_with_attempts(n, closure)` retry-on-deadlock +
  `Builder::with_tx(&tx)` / `Model::save_with_tx(&tx)` explicit
  scope. Multi-connection — `DB::register_named(name, config)` at
  boot + `Model::on(name)` per-query + `#[model(connection = "...")]`
  per-model default + read-write split via reserved
  `__read_replica__` connection name + `Model::on_write_connection()`
  opt-out from replica. Active tx ignores `on(name)` — every op
  uses tx connection. Replication — `Model::replicate()` is async
  + fires `Replicating` event (listeners mutate via
  `Arc<Mutex<Self>>`) + `replicate_except([...])` drops named
  fields + `replicate_into::<T>()` cross-type bridge via
  `serde_json`. Debugging — `Builder::dump()` (chainable, logs at
  `tracing::info!`) + `Builder::dd()` (returns `!`, logs at
  `tracing::error!` then panics with `eloquent dd: <sql>`).
- **Auth Flows** — `EmailVerification::send` / `verify` /
  `resend` over signed URLs. `PasswordReset::start` / `verify` /
  `complete` with token expiry + session-revocation on completion +
  remember-me cookie invalidation. `BruteForce` facade + `LoginThrottleMiddleware`
  (configurable attempts/decay/lockout). **2FA** TOTP enrollment +
  verify + recovery codes (one-time-use); time-step replay prevention;
  rate-limited verify path. Six events (`Registered` / `Verified` /
  `PasswordReset` / `LockoutTriggered` / `TwoFactorEnabled` / etc.).
- **Feature Flags** — `Features::active("flag_name")` /
  `for_user` / `for_team` evaluation. `DatabaseEvaluator` (SeaORM-backed
  snapshot) + `CachedEvaluator` (TTL over `Cache`) composition.
  `FeatureMiddleware` with extractor builder for per-request
  `Context`. Admin CRUD endpoints. `FeatureSync` trait for sub-second
  propagation (live evaluator refresh on admin mutations). String-typed
  user IDs end-to-end. `FeatureRetrieved` event for analytics.
- **Payments** — generic provider-neutral trait surface (`Checkout` +
  `Payment`-optional + `Subscription` + `CustomerStore` +
  `WebhookHandler`). Two reference adapters as workspace member crates:
  `suprnova-payments-stripe` (Stripe gateway, full `Payment` impl) and
  `suprnova-payments-paddle` (Merchant-of-Record, no `Payment` impl by
  design). Six mirror tables with a `provider_metadata` jsonb escape
  hatch + free idempotency via `UNIQUE(provider, provider_event_id)` on
  webhook events. Flow-tagged `SessionPayload` enum drives Inertia
  frontend dispatch. No gatekeeping — third parties can publish their
  own adapter crates without coordinating with the framework.

**Starter kits:**

Two production starter kits ship in their own repos, each pinned to a
released Suprnova and dogfooding the framework end-to-end:

- **[Nebula](https://github.com/entrepeneur4lyf/Nebula)** — a
  Breeze-tier authentication kit on Inertia 3 + Svelte 5: register,
  email verification, login with remember-me, password reset, and
  profile management.
- **[Pulsar](https://github.com/entrepeneur4lyf/Pulsar)** — a full
  product-site and community kit on Vue 3.5 + Vuetify: auth, a
  marketing landing page, a dashboard, a Markdown docs pipeline, a
  blog with RSS, member profiles, taxonomy, role-based access control,
  and an admin / moderation surface.

## What's next

The framework's core surface is shipped. What follows is the forward
roadmap — new capability layers built on the same trait + driver
pattern the shipped framework already uses. We ship a piece when it's
production-ready, not on a calendar.

### More no-gatekeeping backends

The shipped Vector layer (`VectorDriver` trait + Memory / Qdrant /
Pinecone / MariaDB drivers) is the template. The next domains follow the
same trait + driver shape, with the consumer picking a backend via env
or programmatic config:

- **Graph DBs** — `Graph::node(...).related_to(...).match(...)` across
  Neo4j (Bolt), ArangoDB, SurrealDB, and MemGraph.
- **Search** — `Search::index("users").add(doc).query("alice")` across
  Meilisearch, Typesense, Elasticsearch, and Algolia.
- **Time-series** — `Timeseries::write(measurement, tags, fields, ts)`
  with batched writes across InfluxDB, TimescaleDB, QuestDB, and
  ClickHouse.

The trait surface stays the same; swapping a backend is a one-line
config change. None of these are gatekept behind a single engine.

### Admin Panel

A TOML-driven CRUD / RBAC / audit surface over every entity, served as a
separate Inertia app at `/admin` that reuses Suprnova's auth, routing,
and policies — no parallel framework underneath. A file like
`admin/tables/users.toml` declares which columns show, which relations
expand, which actions appear, and which `#[policy]` impls gate them, so
the common case needs no UI code; drop to a custom Inertia page for
bespoke views. Opt-in audit trails write a diff row per
create/update/delete. Every admin read goes through the same SeaORM
entity + Policy gate as the application — there is no admin-bypass path.

### Schema & Query Builder facades

Laravel-shape `Schema::create("users", |t| { ... })` for migrations and
ad-hoc `Query::table("users").where(...).get()` for entity-less queries,
exposed as thin `suprnova::schema::*` / `suprnova::query::*` re-export
facades over [`sea-query`](https://github.com/SeaQL/sea-query) — already
a transitive dependency via SeaORM, so it covers MySQL / Postgres /
SQLite with one DSL.

### More starter kits

Beyond Nebula and Pulsar, additional kits layer on the shipped framework
— for example a teams-and-2FA kit and a billing kit on top of the
Payments surface.

### Polish

Small individually, big collectively:

- **Translation (i18n)** — file-based locales,
  `__("users.welcome", name: "Sue")`.
- **Support helpers** — `Str::camel`, `Arr::pluck`, `Stringable`
  chains; the Laravel-named wrappers over std / itertools.
- **`Process` facade** — `Process::run("git status").output().await`
  over `tokio::process`.
- **Container scoped bindings** — per-request and transient scoping
  alongside today's singletons.
- **Natural-language cron** — `Schedule::call(...).at("every day at
  8am")` via [`english-to-cron`](https://crates.io/crates/english-to-cron).
- **`suprnova doctor`** — a diagnostic command that validates env vars,
  config files, DB connectivity, migration state, and SSR-worker
  reachability.

Beyond these, the hot module reload moonshot has its own section below.

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
- Broadcasting via WebSocket scales horizontally with the shipped
  Redis-backed pub/sub bridge — multiple binaries each hold some
  connections; a publish on one fanout-emits via Redis to subscribers
  on the others.

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
- Compounds with broadcasting and presence — iterate on a live
  feature without losing the multiplayer state you're testing.

This sits in research / not-on-the-critical-path. But if a contributor
shows up with experience in dylib hot-swap, supervised state
preservation, or `bevy_reflect`-style runtime type info, this is the
project to point them at. The reward isn't a feature — it's a
positioning moment that reframes what Rust web development feels
like.
