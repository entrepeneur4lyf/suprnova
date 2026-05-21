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
- **Queue + Rate Limiter + Cache** (Phase 5A) — `Queue::dispatch` /
  `Queue::push` over Redis / Database / in-process drivers; typed
  `Job` trait; supervised workers via `queue:work`. `RateLimiter::for`
  fluent attempts/decay/lockout API + `ThrottleRequests` middleware.
  Multi-store `Cache::store("redis")` / `Cache::store("memory")` with
  `remember` / `forever` / `tags` / `lock` (atomic per-key lock).
- **Mail + Notifications** (Phase 5B) — `Mail::to(&user).send(WelcomeEmail)`
  facade over SMTP / SES / Postmark / Mailgun / Resend / SendGrid /
  log drivers (six first-class transports). `Mailable` trait +
  Markdown + plain-text + HTML rendering via `tera`. `Notifications`
  facade with mail + database + slack channels. `#[derive(NotificationMailable)]`
  for compile-time notification wiring. Test fakes (`Mail::fake()` /
  `Notification::fake()`) record sends for `assert_sent` / `assert_to`.
- **Factories + Seeders + Typed Config + Console** (Phases 6A + 6B) —
  `Factory` trait + `FactoryBuilder` with `Sequence`, `count(n)`,
  `state(closure)`. `Persistable` bridges factory output to the DB.
  `Seeder` trait with `db:seed` console command. `Config::resolve::<T>()`
  uses `envy` for compile-time-typed env-driven config. `#[command]`
  attribute + `#[derive(Command)]` typed args + `inventory`-registered
  CLI surface. App-binary console runner (`cargo run --bin console`) —
  Rust analogue of `php artisan`. `make:command` generator. `silent`
  error variant for graceful CLI exit codes.
- **WebSockets + Broadcasting + Supervised Workers** (Phases 7A + 7B) —
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
- **Eloquent Foundation** (Phase 10A) — `#[suprnova::model]` attribute
  macro emits SeaORM-bridged Entity + Column + ActiveModel + inner
  Model. `Model` trait CRUD (`find` / `save` / `update` / `delete` /
  `refresh` / `replicate` / `first_or_create` / `update_or_create` /
  `increment` / `decrement`). `Builder<M>` with dual-API where surface
  (`filter*` Rust-shape + `db_where`/`where_*` Laravel-shape, both
  first-class aliases). Inventory-backed `ModelEntry` registry for
  Phase 8 admin enumeration. `Fillable` / `Guarded` + `attrs!` macro +
  `unguarded(closure)` task-local bypass. **21 built-in casts**:
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
- **Eloquent Relationships** (Phase 10B) — every Eloquent relation
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
- **Eloquent Lifecycle + Collections + Querying Power** (Phase 10C)
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
- **Auth Flows** (Phase 11) — `EmailVerification::send` / `verify` /
  `resend` over signed URLs. `PasswordReset::start` / `verify` /
  `complete` with token expiry + session-revocation on completion +
  remember-me cookie invalidation. `BruteForce` facade + `LoginThrottleMiddleware`
  (configurable attempts/decay/lockout). **2FA** TOTP enrollment +
  verify + recovery codes (one-time-use); time-step replay prevention;
  rate-limited verify path. Six events (`Registered` / `Verified` /
  `PasswordReset` / `LockoutTriggered` / `TwoFactorEnabled` / etc.).
- **Feature Flags** (Phase 13) — `Features::active("flag_name")` /
  `for_user` / `for_team` evaluation. `DatabaseEvaluator` (SeaORM-backed
  snapshot) + `CachedEvaluator` (TTL over `Cache`) composition.
  `FeatureMiddleware` with extractor builder for per-request
  `Context`. Admin CRUD endpoints. `FeatureSync` trait for sub-second
  propagation (live evaluator refresh on admin mutations). String-typed
  user IDs end-to-end. `FeatureRetrieved` event for analytics.

**Partial - needs filling in:**

- Cookie (via session; standalone API unclear)
- Container scoped bindings (singletons work; per-request scoping
  underspecified)
- Schema DSL + Query Builder facades (migrations work via SeaORM's
  migration types, but no Laravel-shape `Schema::create("users", |t| { ... })`
  builder; no entity-less `Query::table("users").where(...).get()`
  for ad-hoc queries). **Foundation: re-export
  [`sea-query`](https://github.com/SeaQL/sea-query) 1.0
  (MIT/Apache-2.0)** — it's already a transitive dep via SeaORM, so
  exposing `suprnova::schema::*` and `suprnova::query::*` as thin
  re-export facades is free leverage. Covers MySQL/Postgres/SQLite
  with the same DSL.
**Missing - the rest of this document.**

Major missing tracks beyond Phase 10C:

- **Phase 8 — Admin Panel** (8A backend contract + 8B Scheduler/Queue
  inspectors + 8C UI shell on Inertia). Phase 8A walks the
  `ModelEntry` + `RelationEntry` + `MorphTypeEntry` + `CommandEntry` +
  `SupervisorEntry` inventories to enumerate every administerable
  surface in the binary.
- **Phase 9A — Vector** (Qdrant + Pinecone + Memory drivers verified;
  Weaviate + Milvus + LanceDB + pgvector + MariaDB + LibSQL planned).
  Laravel ships pgvector-only; we don't gatekeep.
- **Phase 12 — Billing v1** (Stripe + PayPal + Paddle). Subscription
  + one-off + webhook handling.

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

**Foundation: [`opendal`](https://opendal.apache.org/) (Apache-2.0).**
A unified data-access trait over 40+ storage backends from the Apache
foundation. We adopt it as the storage abstraction layer; every
`Storage::disk(...)` driver is an opendal `Operator` under a
Suprnova-named alias. Switching backends is one config-value change.
Matches our "no gatekeeping" philosophy: S3, Azure Blob, GCS, local
FS, in-memory, HTTP, WebDAV, FTP/SFTP, IPFS, and many more all ship
through the same facade — Laravel's Flysystem is the closest analog
but has fewer adapters and no streaming-first design. (Validated by
loco's choice of the same library.)

**Storage drivers (outbound):**
- Local (`services-fs`) — with subdir scoping
- S3 and S3-compatible (R2, MinIO, Backblaze B2, DigitalOcean Spaces,
  Wasabi) via `services-s3`
- Azure Blob via `services-azblob`
- Google Cloud Storage via `services-gcs`
- In-memory (`services-memory`) — for tests
- All other opendal-supported backends (Aliyun OSS, Tencent COS, IPFS,
  WebDAV, etc.) are wired through the same `Storage::disk(...)` facade
  with a single feature-flag flip.

For users requiring STS / AssumeRole / IMDS / cross-account IAM
patterns beyond what opendal's S3 service handles, an optional
`s3-aws` feature flag pulls in `aws-sdk-s3` as an alternate driver.
Default install stays slim.

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

**Foundation: [`lettre`](https://github.com/lettre/lettre) 0.11.x
(MIT).** lettre gives us the `Message` builder (MIME, multipart,
attachments, headers, DKIM), the SMTP transport with built-in
connection pooling, and tokio-native async via the `tokio1-rustls`
feature. We pin it as the email primitives layer; every Suprnova
mail driver ships a `lettre::Message` over its preferred wire.
`#[derive(Mailable)]` is a thin proc macro that compiles to a
`lettre::Message` builder. The vendored reference lives at
`reference/lettre-0.11.22/` for cross-checking while we wrap it.

**Drivers:** SMTP and sendmail (lettre native transports), AWS SES,
Postmark, SendGrid, Mailgun, Resend (our own HTTP `Transport` impls
that accept `lettre::Message` and POST via the provider API), log
and file (dev/test modes that record sends for assertions —
`Mail::fake()` + `Mail::assert_sent(...)`).

**Mailables.** Typed templates with `#[derive(Mailable)]` macro that
provides `to()`, `subject()`, `view()`. Template engine: pick one and
ship it (likely `askama` for compile-time-checked templates that match
the framework's type-safety stance, with `minijinja` as a runtime
alternative for hot-reload).

**Rust gun:** mail building is synchronous and fast; sending is
async on the in-process queue (Track 7) so a controller can
`Mail::queue(welcome_email)` and return without blocking. No separate
queue worker needed for transactional mail. lettre's connection pool
means a burst of `Mail::send` calls reuses an open SMTP/TLS session
instead of negotiating a new handshake per message.

### Track 5 - Authorization + API mode

Two separate but related stories that together cover the gap between
"the user is logged in" (today's Auth) and "the user is allowed to do
this thing to this resource." Every real app needs this on day one.
Laravel ships Gates, Policies, and Sanctum for it; Suprnova ships the
typed equivalents.

**Foundation: [`torii-rs`](https://github.com/cmackenzie1/torii-rs).**
We adopt `torii-core` + `torii-storage-seaorm` as the auth-method
foundation (skipping `torii-axum` since we have our own HTTP layer).
This gives us password + **OAuth/OIDC** + **Passkeys/WebAuthn** +
**Magic Links** + session management without reinventing months of
careful crypto work. MIT-licensed. The `suprnova::auth::{passkey,
oauth, magic_link, password}` facades are thin adapters over torii;
the existing session-based auth we ship today becomes one option
among several. If upstream churn becomes painful, we fork into the
workspace as `suprnova-torii` at a pinned version.

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

**Foundation: [`sea-streamer`](https://github.com/SeaQL/sea-streamer)
0.5 (MIT/Apache-2.0).** A backend-agnostic stream processing toolkit
with first-class **Redis Streams** and **Kafka** backends behind a
common trait, plus file + stdio backends for testing and dev. We
adopt it as the underlying transport for the Queue (and reuse it for
fanout in Broadcasting, Track 8). Redis Streams as a queue backend
gives us consumer groups, per-message acknowledgment, and replay —
strictly better than the Redis-list pattern Laravel uses. Kafka comes
free in the same package; the same `Queue::push` call targets either
by changing one URL. The vendored reference lives at
`reference/sea-streamer-0.5.2/` for cross-checking.

**Drivers:** Redis Streams (sea-streamer), Kafka (sea-streamer),
database (default for new apps — Postgres / MySQL / SQLite), NATS,
Amazon SQS, in-process. File-based queue (also via sea-streamer)
ships as a dev/test backend with replayable history — a regression
captured as a `.ss` file can be replayed deterministically.

**Rust gun:** the in-process driver runs jobs on Tokio tasks in the
same process — for monolith apps, zero infrastructure required. Real
backends for when you need scale-out. Job retries, backoff,
fail-handler hooks. Built-in dead-letter-queue. With sea-streamer
under the hood, the read/process/ack loops are decoupled so a
single worker can saturate throughput on the I/O-bound legs while
processing in parallel — Laravel Horizon's whole reason to exist
is matching this for Redis-list queues; we get it natively because
Redis Streams + decoupled loops are built in.

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
    .via(&["mail", "slack", "database", "web-push"]).await?;
```

Channels: mail, Slack, Discord, SMS via Twilio, database, webhook,
**browser push (Web Push API)** via
[`web-push`](https://crates.io/crates/web-push) (Apache-2.0,
HTTP-ECE + VAPID), broadcast (Track 8). Depends on Mail (Track 4)
and Broadcasting (Track 8).

The `web-push` channel is worth calling out: Laravel needs the
community `laravel-notification-channels/webpush` package for this;
Suprnova ships it as a first-class channel. A controller that
dispatches an `OrderShipped` notification reaches all four channels
(email + DB record + Slack + browser push) in one call.

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

**Multi-process fanout.** When you scale beyond one server,
`sea-streamer` (foundation library from Track 7) handles
inter-process pub/sub via Redis Streams or Kafka. Each
WebSocket-handling process subscribes to a stream key
(`channel:orders.42`) and re-broadcasts received events to its
locally-connected clients. Same library, same code path, different
URL — no separate fanout service to deploy.

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
- **Schedule polish** — natural-language cron strings via
  [`english-to-cron`](https://crates.io/crates/english-to-cron):
  `Schedule::call(send_digests).at("every day at 8am")` compiles to
  the right cron expression at runtime. Cribbed from loco's DX win.
- **`suprnova doctor`** — diagnostic command that validates env vars,
  config files, DB connectivity, migration state, and Inertia/SSR
  worker reachability. First port-of-call when a new dev clones the
  repo and `serve` fails. Modeled on `cargo loco doctor`.

> **Note on testing helpers.** `Mail::fake()` / `Queue::fake()` /
> `Event::fake()` / `Http::fake()` are *not* polish items. They ship
> with their respective tracks per Philosophy rule 4. They appear here
> only as a cross-reference, not as deferred work.

### Track 11 - Admin Panel

CRUD on every entity, search, RBAC views, audit trails. Production
apps need this by month one — Laravel ships Nova / Filament; Rails
ships ActiveAdmin / Avo / RailsAdmin. Real Rust apps deserve the
same productivity boost. **Inspired by [SeaORM Pro](https://www.sea-ql.org/sea-orm-pro/)'s
TOML-driven config pattern** (which itself is loco-bound today, so
we crib the design and build our own implementation against the
Suprnova HTTP layer + auth + policies).

**TOML-config approach.** A file at `admin/tables/users.toml`
declares which columns show, which relations expand, which actions
appear, and which policies gate them. No UI code required for the
common case. Override with a custom Inertia page when you need
bespoke UX.

```toml
[table]
entity = "users"
title = "Users"
icon = "user"

[[columns]]
field = "email"
sortable = true
searchable = true

[[columns]]
field = "created_at"
format = "datetime"

[policies]
view = "UserPolicy::view"
edit = "UserPolicy::edit"
delete = "UserPolicy::delete"

[audit]
enabled = true
```

**Architecture.** The admin panel is a separate Inertia app served
at `/admin` (configurable). It reuses Suprnova's auth, routing,
policies (Track 5), and migrations — no separate framework
underneath. Built on our React / Vue / Svelte starter (user picks
at scaffold time; default React 19). Gated with an `[admin]`
middleware that requires an `is_admin` claim or a `super_admin`
role from Track 5's Authorization layer.

**Composite views.** SeaORM Pro's `composite_tables` pattern
translated to TOML: declare a "Sales Order" view that joins
`sales_order_header` with `sales_order_detail` and `customer`, all
rendered in one page with related-record navigation.

**RBAC.** Built on top of Track 5's Authorization. Policies declared
in TOML reference our `#[policy]` impls — the admin panel does not
invent a parallel auth system. Same Gate trips for both end users
and admin staff.

**Audit trail.** Opt-in per table via `[audit] enabled = true`. The
framework writes a row to `audits` on every create/update/delete
with the acting user id, table, row id, action, and a JSON diff of
the changed columns. Powers "who edited this record" queries
without instrumenting each controller.

**Rust gun:** every admin read goes through the same SeaORM entity
+ Policy gate as the application — no "admin bypass" path that
silently skips authorization (a recurring source of pwned Laravel
apps). Streaming pagination over millions of rows because we use
async cursors, not `LIMIT N OFFSET M` page joins.

## Recommended sequencing

Each phase unblocks the next. Every phase ships its fakes /
assertions in the same commit (Philosophy rule 4). Order is set
by dependency; we ship a phase when it's done, not on a calendar.

**Phase 1: Logging + Events + Error handling + minimal SSE.**
Foundation observability. Everything else uses them. The longer we
wait, the more retrofitting we owe. Minimal SSE rides along so
events have a delivery primitive from day one.

**Phase 2: HTTP client + Pagination + Encryption.**
Small, high-leverage, often-used. Encryption replaces the sign-only
cookie path; HTTP client unblocks third-party API integrations every
real app needs.

**Phase 3: Authorization + API mode.**
Gates + Policies + token auth + JSON Resources + `--api` scaffolding.
Day-one expectation for any Laravel dev, separate from the Auth track
that already shipped. The bigger your app gets, the more this matters.

**Phase 4: Filesystem + File uploads + Validation parity.**
Storage drivers and upload handling together because controllers
touch both. Validation gets finished here because we already exercised
the gaps in Precognition.

**Phase 5: Queue + Mail + Notifications + Rate Limiting.**
Mail-via-queue is the canonical pattern; ship them together.
Rate limiting middleware in the same wave because cache + redis are
already set up. Notifications layer on top.

**Phase 6: Factories + Seeders + Configuration + Console.**
The Laravel-dev day-one expectations not covered earlier. Small but
high-impact for DX.

**Phase 7: Full Broadcasting + supervised background workers.**
WebSocket + presence + channel auth. The "Rust eats Laravel's lunch"
moment at full strength — Phase 1 already shipped SSE for the simpler
cases. This is the demo that gets a Laravel dev to say "wait, you can
do that in one process?"

**Phase 8: Admin Panel.**
TOML-driven CRUD + RBAC + audit trails over every entity. Depends
on Authorization (Phase 3) and Filesystem (Phase 4) shipping first.

**Phase 9: Differentiation** *(ongoing as consumers demand it).*
Vectors, graphs, search, time-series. Driven by real consumer needs
(`nation-x.com` will exercise some). Ship one when the demand exists;
the others queue up behind.

**Phase 10: Polish** *(parallel with the phases above).*
Translation, Support helpers, Process, scoped bindings, routing extras,
`english-to-cron`, `suprnova doctor`. These fit between bigger pieces.

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
