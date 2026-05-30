# Laravel Parity Map

The honest, feature-by-feature mapping between Laravel 13.x and Suprnova.
Use this when you're asking "does Suprnova have X?" and want a yes/no/where
answer in one row.

Sections mirror the Laravel docs index so a Laravel developer can scan
top-to-bottom. Within each section the columns are always the same:

| Laravel | Suprnova | Status | Notes / link |
|---|---|---|---|

The **Status** column uses four values:

| Symbol | Meaning |
|---|---|
| **shipped** | Same surface, same behaviour (often same method names) |
| **diverged** | Same job, different shape because Rust makes a better choice possible |
| **not yet** | Genuinely planned, not yet on disk |
| **by design no** | Won't ship — explanation in the Notes column |

The relevant chapter (where one exists) is linked from the **Notes** column.

This is a live map. Per the [Introduction](introduction.md), the Laravel
13.x parity sweep ran across 30 module groups and the test suite is gate
clean; the gaps listed below are the real, current gaps as of the
shipped framework.

## Architecture concepts

| Laravel | Suprnova | Status | Notes / link |
|---|---|---|---|
| Request Lifecycle | `Application` → `Server` → `handle_request` chain | shipped | [Lifecycle](lifecycle.md) |
| Service Container | `Container` + `App` facade, three-layer (task / thread / global) | diverged | Task-local for per-request, thread-local for tests — [Container](container.md) |
| Service Providers | `bootstrap()` function + `#[service]`, `#[policy]`, `#[command]`, observer macros | diverged | No registration class — bootstrap is one function; macros use `inventory` for compile-time registration. [Bootstrap](bootstrap.md) |
| Facades | Static `App::get`, `Cache::*`, `Mail::*`, `Auth::*`, `Storage::*`, `Queue::*`, `Bus::*`, `Event::*`, `Notification::*`, `Gate::*`, `Schedule::*`, `DB::*`, `Vector::*` | shipped | Same call shape; the facades are real types, not aliases |
| Contracts | Traits — `Mailer`, `KeyValueStore`, `Hasher`, `Channel`, `VectorDriver`, `Evaluator`, `PaymentProvider`, etc. | shipped | All public seams live on traits; bind by trait, swap implementations freely |

## Getting started

| Laravel | Suprnova | Status | Notes / link |
|---|---|---|---|
| Installation | `cargo install --git …suprnova-cli` then `suprnova new <name>` | shipped | [Installation](installation.md) |
| Configuration | Typed config via `#[derive(Config)]` + `Config::register` | diverged | Compile-time typed instead of array bags. [Configuration](configuration.md) |
| Agentic Development (AI) | No first-class AI SDK in framework | by design no | Use the crates you'd use anyway (`async-openai`, `anthropic-rs`, `tokenizers`, etc.) under `App::bind(Arc<dyn YourLlm>)` |
| Directory Structure | `src/{actions,bootstrap,controllers,middleware,models,routes}` | shipped | Same intent, Rust-idiomatic layout. [Structure](structure.md) |
| Frontend | Inertia v3 over Svelte 5 / React 19 / Vue 3.5 | shipped | [Frontend](frontend.md), [Pages](frontend-pages.md), [TS Types](frontend-typescript-types.md) |
| Starter Kits | Plain `suprnova new --frontend <…>` ships auth, dashboard, payment-ready layout | not yet | Breeze/Jetstream/Spark-tier kits planned; today the default scaffold is closest to Breeze |
| Deployment | Single binary; Docker / Railway / DO / Hetzner recipes | diverged | One artifact, not a PHP runtime + opcache + FPM. [Deployment](deployment.md) |

## The basics

| Laravel | Suprnova | Status | Notes / link |
|---|---|---|---|
| Route definitions | `routes!` macro + `get!` / `post!` / `put!` / `patch!` / `delete!` / `any!` / `head!` / `options!` / `fallback!` / `ws!` | shipped | [Routing](routing.md) |
| Route parameters | `{id}` path params + `req.param("id")` | shipped | Optional params via `{id?}`; constraints via `where!()` |
| Route names | `.name("posts.show")` on the route + `url("posts.show", &[("id", "42")])` | shipped | [URL Generation](urls.md) |
| Route groups | `group!` macro with `.prefix()` / `.middleware()` / `.name()` / `.controller()` | shipped | Group middleware is flattened onto each route at register time |
| Resource routes | `resource!("posts", PostController)` registers the 7 standard routes | shipped | `apiResource!`, `only(...)`, `except(...)` all supported |
| Signed URLs | `sign_url(...)`, `sign_route(...)`, `verify_signature(...)` | shipped | HMAC-SHA256 with `APP_KEY` |
| Route model binding | `#[handler]` extracts `Post` from `{post}` via `RouteBinding` impl | shipped | `AutoRouteBinding` derive auto-implements for `#[suprnova::model]` types |
| Rate limiting | `throttle:60,1` middleware + `RateLimiter::for_signature` | shipped | [Rate Limiting](rate-limiting.md) |
| Middleware | `impl Middleware` trait; register globally or per-route | shipped | [Middleware](middleware.md) |
| Middleware groups + aliases | `register_middleware_group`, `register_middleware_alias` | shipped | Look up by string name in routes |
| CSRF Protection | `CsrfMiddleware` + `csrf_token()` / `csrf_field()` / `csrf_meta_tag()` | shipped | Origin policy enforces same-origin POST. [CSRF](csrf.md) |
| Controllers | `#[handler] pub async fn show(req: Request) -> Response` | shipped | Controllers are modules of free functions, not classes. [Controllers](controllers.md) |
| Single-action controllers | A handler is already a single function; group into modules | shipped | The Rust convention — no `__invoke` ceremony |
| Requests | `Request` struct with `.input()`, `.param()`, `.query()`, `.header()`, `.cookie()`, `.json()`, `.file()`, etc. | shipped | [Requests](requests.md) |
| Form Requests | `#[derive(Data, Validate, FormRequest)]` | shipped | Validation runs as you extract |
| File uploads | `req.file("avatar")?` returns `UploadedFile`; streaming multipart with size + part caps | shipped | Auto-spill to tempfile above threshold |
| Responses | `HttpResponse` builders + `json!()` / `text!()` / `Redirect::to` / `view` | shipped | [Responses](responses.md) |
| Views (Blade) | Server-rendered Inertia pages (Svelte/React/Vue) — no Blade equivalent | diverged | Inertia is the view layer. Use [Pages](frontend-pages.md) instead of Blade |
| Asset Bundling (Vite) | Vite 6 ships in every scaffold; `suprnova serve` runs Vite + backend together | shipped | Manifest reading + HMR auto-wired |
| URL Generation | `url("posts.show", &[…])`, `route("posts.show", …)`, `redirect(...)`, `redirect_to(...)` | shipped | [URL Generation](urls.md) |
| Session | `session()`, `session_mut()`, flash bag via `req.flash()` | shipped | DB-backed via `DatabaseSessionDriver`; cookie-backed by default. [Session](session.md) |
| Validation | `#[derive(Validate)]` + 17 built-in rules + `Rule`/`AsyncRule` traits | shipped | Async rules (e.g. `Unique`) hit the DB. [Validation](validation.md) |
| Error Handling | `FrameworkError`, `AppError`, `HttpError` trait, panic boundary in `execute_chain_safely` | shipped | [Error Handling](errors.md), [Error Model](error-model.md) |
| Logging | `tracing` subscriber with structured fields, `LogFormat` (json / pretty / compact) | diverged | One log line is a JSON document; `request_id` always present. Listed but the dedicated chapter is **not yet** written |
| Abort helpers | `abort_if(cond, status, msg)`, `abort_unless(...)`, `abort_with(status, msg)` | shipped | Same shape as Laravel's `abort_if` family |

## Digging deeper

| Laravel | Suprnova | Status | Notes / link |
|---|---|---|---|
| Artisan Console | Per-app `console` binary built from `#[command]` + `#[derive(Command)]` | shipped | [Console](console.md). `cargo run --bin console <subcommand>` |
| Tinker (REPL) | No REPL | by design no | Write a one-off `cargo run --bin xxx` script or a `#[suprnova_test]` |
| Broadcasting | `BroadcastHub` + `Channel` / `PrivateChannel` / `PresenceChannel` + `Broadcastable` | shipped | sea-streamer fanout for multi-node. [Broadcasting](broadcasting.md) |
| Cache | `Cache::get/put/forget/remember/rememberForever/increment/...` + `InMemoryCache`, `RedisCache`, `DatabaseCacheStore` | shipped | Atomic ops + tagged cache + cache locks (`LockGuard`). [Cache](cache.md) |
| Collections | `eloquent::Collection<M>` with Laravel-shape methods | shipped | `Deref<Target = Vec<M>>` so existing Vec idioms still work. Listed but the dedicated chapter is **not yet** written |
| Concurrency | Tokio everywhere — `tokio::spawn`, `tokio::join!`, `tokio::select!` | shipped | The whole framework is async. The Laravel `Concurrency::run([...])` facade doesn't ship; Tokio is the answer |
| Context | `Context::put` / `Context::get` / `ContextStore` + auto-injection into queue / mail / events | shipped | [Context](context.md) |
| Contracts | All public seams are traits | shipped | See the "Architecture / Contracts" row above |
| Events | `Event::dispatch(e).await?`, `#[derive(Event)]`, `EventDispatcher`, queued listeners, subscribers | shipped | [Events](events.md) |
| File Storage | `Storage::disk("local"\|"s3"\|"azblob"\|"gcs"\|"memory")` over OpenDAL | shipped | Same `put/get/delete/copy/move/exists/url` surface. Path-traversal protection built in. [Filesystem](filesystem.md) |
| Helpers | Equivalents are in their home modules (no kitchen-sink `helpers.md`) | diverged | E.g. URL helpers live in [urls.md](urls.md), string helpers in `std`/`heck`, array helpers in `std::collections` — Rust does this with crates, not a global namespace |
| HTTP Client | `Http::get/post/...` builder + `Http::fake(...)` for tests | shipped | Auto-records requests; `assert_sent` / `assert_not_sent`. [HTTP Client](http-client.md) |
| Localization | No first-class i18n module yet | not yet | Today: use a crate like `fluent` or `rust-i18n` directly. Translation file loading + `__("key", params)` style helper planned |
| Mail | `Mail::to(...).send(MyMail { ... }).await?` + drivers `smtp/ses/mailgun/postmark/sendgrid/resend/log/memory` | shipped | `Mailable` trait + Tera-rendered HTML/text bodies. [Mail](mail.md) |
| Notifications | `Notify::send(&user, notif).await?` + channels `mail/database/broadcast/webpush` | shipped | `Notifiable` trait + `Notification` per channel. [Notifications](notifications.md), [Web Push](web-push.md) |
| Package Development | Workspace adapter crates (e.g. `suprnova-payments-stripe`) | shipped | Same shape as Laravel packages: depend on the framework, bind into the container, expose macros if needed |
| Processes (running shell commands) | `tokio::process::Command` from the stdlib | by design no | No facade — Tokio's API is already the right shape |
| Queues | `Queue::dispatch(job).await?` + drivers `sync/memory/database/redis/null`, batches, chains, `JobMiddleware`, `FailedJobStore` | shipped | [Queues](queues.md) |
| Rate Limiting | `RateLimiter::for_signature(...)`, `ThrottleRequestsMiddleware`, `RateLimitMiddleware` | shipped | Sliding window via `SlidingWindowConfig`. [Rate Limiting](rate-limiting.md) |
| Search (Scout) | No first-party full-text search adapter | not yet | Vector search ships today via [Vector](vector.md); keyword-search Scout-equivalent is planned |
| Strings (helpers) | `heck` crate (case conversions), `std::str`, `regex` | diverged | Same crates the rest of the Rust ecosystem uses; no `Str::camel($x)` global |
| Task Scheduling | `Schedule::call/command/task` + `#[derive(Task)]` + cron syntax + `schedule:run` worker | shipped | [Scheduling](scheduling.md) |
| Idempotency keys | `IdempotencyMiddleware` + `#[derive(Idempotent)]` on the request type | shipped | Stripe-style replay protection on POST/PUT. [Idempotency](idempotency.md) |
| Request timeout | `TimeoutMiddleware` configurable per route | shipped | Rust-native — abort the in-flight future, free the worker. [Timeout](timeout.md) |
| Feature Flags (Pennant) | `Feature` + `Evaluator` + `FeatureMiddleware` + admin CRUD | shipped | Sub-second propagation via `FeatureSync` trait. [Feature Flags](feature-flags.md) |
| Observability (Pulse) | OpenTelemetry via `init_telemetry`, `Metrics`, `tracing` everywhere | diverged | OTel is the lingua franca for Rust observability — point your collector at the binary. [Observability](observability.md) |
| Telescope (debug dashboard) | No equivalent yet | not yet | Deferred to v2+; the framework's tracing + OTel output covers most diagnostic needs |
| Pulse (perf dashboard) | No equivalent yet | not yet | Same as Telescope — surface metrics with your existing observability stack until a dashboard ships |
| Vector search | `Vector::driver("memory"\|"qdrant"\|"pinecone"\|"mariadb")` | shipped | No "Postgres pgvector only" gatekeeping. [Vector Search](vector.md) |

### Suprnova-exclusive (no Laravel equivalent)

| Suprnova | What it is | Notes / link |
|---|---|---|
| `ws!()` macro + WebSocket handlers | Typed WS routes that share the router + middleware stack | [WebSockets](websockets.md) |
| Server-Sent Events | `SseEvent` + `HttpResponse::sse(...)` | [SSE](sse.md) |
| Workflows | Long-running stateful work with retries, sleep, step boundaries | [Workflows](workflows.md) |
| Supervisors | `Supervisor` trait with panic-catch auto-restart for long-lived tokio tasks | [Supervisors](supervisors.md) |
| Web Push (VAPID) | Browser push notifications as a first-class channel | [Web Push](web-push.md) |
| Multi-connection read/write split | `READ_REPLICA_CONNECTION_NAME` + `DB::on("read").select(...)` | [Database](database.md) |
| HTTP/2 + WebSocket on the same socket | `hyper.with_upgrades()` in `Server::run` | [Lifecycle](lifecycle.md) |

## Security

| Laravel | Suprnova | Status | Notes / link |
|---|---|---|---|
| Authentication | `Auth::user/check/login/logout/attempt`, `Authenticatable` trait, `Guard` per name | shipped | [Authentication](authentication.md) |
| Multiple guards | `Guard` registered by name (`web`, `api`, …) via `AuthManager` | shipped | `SessionGuard`, `TokenGuard`, custom impls |
| User providers | `EloquentUserProvider<U>`, `DatabaseUserProvider`, custom via `UserProvider` trait | shipped | [Auth Flows](auth-flows.md) |
| Email Verification | `EmailVerification` + `EnsureEmailVerifiedMiddleware` + `EmailVerificationMail` | shipped | [Auth Flows](auth-flows.md) |
| Password Reset | `PasswordReset` + `PasswordResetMail` + `PasswordChangedMail` | shipped | [Auth Flows](auth-flows.md) |
| Brute-force throttling | `BruteForce` + `LoginThrottleMiddleware` | shipped | Per-IP + per-user accounting |
| Two-Factor (TOTP) | `TwoFactor` + `TwoFactorChallengeMiddleware` + `TwoFactorUser` trait | shipped | Recovery codes + replay protection |
| Remember-me | Long-lived signed cookie via `SessionGuard` | shipped | Re-exported from torii |
| OAuth (Socialite) | Via the vendored `torii_integration` fork (Google / GitHub / Apple etc.) | shipped | [Authentication](authentication.md) |
| Sanctum (API tokens) | `TokenGuard` + DB-backed tokens via torii | diverged | Token model + bearer middleware ship; no separate Sanctum API surface |
| Passport (OAuth server) | Not yet | not yet | If you need an OAuth provider, run a dedicated identity service (Keycloak, Hydra) behind Suprnova |
| Fortify (auth backend) | Replaced by `auth_flows` module + `auth_flows::*` types | shipped | Same job; no headless-vs-headed split needed because the frontend is Inertia |
| Authorization (Policies / Gates) | `Gate::allows/denies` + `#[policy] impl PostPolicy` + `Authorizable` trait + macro registration | shipped | [Authorization](authorization.md) |
| Encryption | `Crypt::encrypt/decrypt` + `CryptPurpose` AAD binding | shipped | AES-256-GCM, key rotation via `APP_KEY_PREVIOUS`. [Encryption](encryption.md) |
| Hashing | `hash::*` + `BcryptHasher`, `Argon2idHasher`, `Argon2iHasher`, `needs_rehash`, `is_hashed`, `verify` | shipped | Bcrypt default; argon2id available. [Hashing](hashing.md) |

## Database

| Laravel | Suprnova | Status | Notes / link |
|---|---|---|---|
| DB::table('users')->where(...)->get() | `DB::table("users").db_where("id", "=", 1).get().await?` | shipped | [Database](database.md), [Queries](queries.md) |
| Multiple connections | `DB::on("read")` + `ConnectionRegistry` | shipped | Read/write split first-class |
| Transactions | `DB::transaction(\|tx\| async move { ... }).await?` | shipped | Savepoints + retry-on-deadlock |
| Query events | `QueryListener` + `QueryExecuted` event | shipped | `DB::listen(\|q\| { ... })` |
| Raw expressions | `DB::raw("...")`, `DB::select("...", &[...])` | shipped | Parameter binding required (no string interpolation) |
| Postgres / MySQL / SQLite | All three first-class via SeaORM | shipped | URL detection in `database::config::database_type()` |
| MariaDB | First-class as its own option (vector + JSON + temporal) | diverged | Treated separately because of multi-paradigm features Laravel ships as Postgres-only |
| Redis | Used by drivers (cache/queue/rate-limit) — no separate `Redis::*` facade | diverged | Reach for `redis` crate directly when you need ad-hoc commands; cache/queue/rate-limit cover 95% of typical use |
| MongoDB | No first-party adapter yet | not yet | Use `mongodb` crate directly via `App::bind` |
| Query Builder | `Builder<M>` with `db_where` / `or_where` / `where_in` / `where_between` / `where_null` / `where_has` / `with` / `with_count` / `order_by` / `group_by` / `having` / `paginate` / etc. | shipped | [Queries](queries.md) |
| Pagination | `LengthAwarePaginator`, `Paginator` (simple), `CursorPaginator` | shipped | All three serialise to Laravel-shape JSON. Dedicated chapter is **not yet** written |
| Migrations | `#[derive(DeriveMigrationName)] struct M;` + `up`/`down` + `Migrator` | shipped | Run via `suprnova migrate`/`migrate:rollback`/`migrate:status`/`migrate:fresh`. [Migrations](migrations.md), [CLI Migrations](cli-migrations.md) |
| Seeders | `Seeder` trait + `db:seed` subcommand | shipped | Per-model factories. Dedicated chapter is **not yet** written |

## Eloquent ORM

| Laravel | Suprnova | Status | Notes / link |
|---|---|---|---|
| `class User extends Model` | `#[suprnova::model(table = "users")] struct User { ... }` | shipped | The struct IS the SeaORM `Model`. [Eloquent](eloquent.md) |
| Find / first / get | `User::find(id)`, `User::query().first()`, `User::all()`, `Builder::get` | shipped | All async |
| Create / update / delete | `User::create(attrs)`, `user.update(attrs)`, `user.delete()` | shipped | `attrs! { name: "...", email: "..." }` macro for partial attrs |
| Mass assignment guards | `#[model(fillable = [...])]` / `#[model(guarded = [...])]` + `unguarded \|\| { ... }` scope | shipped | `prevent_silently_discarding_attributes()` for strict mode |
| Soft deletes | `#[model(soft_deletes)]` auto-injects `deleted_at` + `SoftDeletes` trait | shipped | `with_trashed()`, `only_trashed()`, `restore()`, `force_delete()` |
| Prunable / MassPrunable | `#[prunable] impl Prunable for User { ... }` + `model:prune` worker | shipped | Cascade-pinned to relations |
| Timestamps | Auto `created_at`/`updated_at` if columns are present | shipped | Disable via `#[model(timestamps = false)]` |
| Primary key types | i64 default; UUID / ULID via `#[model(unique_id = "uuid")]` or `unique_id = "ulid"` | shipped | Auto-generates id on insert |
| Local scopes | `#[scopes(User)] impl User { fn active(b: &mut Builder<User>) { ... } }` | shipped | Method dispatch on `Builder<M>` |
| Global scopes | `impl GlobalScope for ActiveOnly { ... }` + register | shipped | Stripped via `Builder::without_global_scope` |
| Relationships (11 kinds) | `HasOne`, `HasMany`, `BelongsTo`, `BelongsToMany`, `HasOneThrough`, `HasManyThrough`, `MorphOne`, `MorphMany`, `MorphTo`, `MorphToMany`, `MorphedByMany` | shipped | Per-family morph enum. Dedicated chapter is **not yet** written |
| Eager loading | `User::query().with(&["posts", "posts.comments"]).get()` | shipped | `EagerLoadDispatch` is sealed; only macro-generated relations can implement it |
| Lazy loading prevention | `prevent_silently_discarding_attributes(true)` | shipped | Same shape as Laravel's `preventLazyLoading` |
| Aggregates on relations | `with_count("posts")`, `with_sum("orders", "total")`, `with_avg`, `with_min`, `with_max` | shipped | Single subquery per aggregate |
| `whereHas` / `whereDoesntHave` | `where_has("posts", \|q\| q.db_where("published", "=", true))` | shipped | Correlated EXISTS engine |
| `loadMissing` | `user.load_missing(&["posts"]).await?` | shipped | Operates collection-wide |
| Cloning a record | `user.replicate()` / `user.replicate_into::<OtherType>()` | shipped | Dispatches `Replicating` event |
| Touching parent timestamps | `#[model(touches = ["post"])]` | shipped | `without_touching \|\| { ... }` to skip |
| Observers | `impl Observer<User>` + `#[suprnova::observer(User)]` | shipped | 16 lifecycle events |
| 16 lifecycle events | `Created`, `Creating`, `Saving`, `Saved`, `Updating`, `Updated`, `Deleting`, `Deleted`, `Trashed`, `Restoring`, `Restored`, `Retrieved`, `Replicating`, `ForceDeleting`, `ForceDeleted`, `Pruning` | shipped | Per-model `events::*` submodule. `EventResult::cancel(_)` short-circuits with a 400 |
| Mutators / Accessors | `#[accessor] fn full_name(&self) -> String { ... }` + `#[mutator] fn set_password(&mut self, v: String)` | shipped | [Eloquent](eloquent.md). Dedicated mutators chapter is **not yet** written |
| Casts (21 built-in) | `casts! { AsString, AsInt, AsFloat, AsBool, AsJson, AsArray, AsArrayObject, AsObject, AsCollection, AsDate, AsDateTime, AsImmutableDate, AsImmutableDateTime, AsOptionalDateTime, AsTimestamp, AsDecimal, AsEnum<E>, AsEncrypted, AsEncryptedObject, AsEncryptedArray, AsEncryptedCollection, AsHashed }` | shipped | Implement `Cast` for custom |
| Collections | `Collection<M>` with `pluck`, `filter`, `map`, `each`, `chunk`, `groupBy`, `keyBy`, `sort_by`, `where_`, `first`, `last`, `count`, `is_empty`, `to_array` and Laravel friends; `Deref<Target = Vec<M>>` so all `Vec` idioms keep working | shipped | Dedicated chapter is **not yet** written |
| API Resources | `#[derive(Resource)]` + `IntoJsonResource` + `JsonApiResponse` + fieldsets + includes | shipped | JSON:API shape + Laravel-style resource shape both available. [API Resources](eloquent-resources.md) |
| Serialization | `#[model(hidden = [...], visible = [...], appends = [...])]` | shipped | Same control over which attributes serialise. Dedicated chapter is **not yet** written |
| Factories | `#[derive(Factory)] struct UserFactory` + `User::factory().count(5).create().await?` | shipped | `Sequence` for cycling values. Dedicated chapter is **not yet** written |
| Lifecycle: chunking / lazy / cursor | `Builder::chunk(n, \|page\| async { ... })`, `lazy()`, `cursor()` | shipped | Memory-bounded iteration over large tables |
| Pessimistic locking | `Builder::lock_for_update()`, `shared_lock()` | shipped | Inside a transaction |
| `whereJsonContains` family | Available via SeaORM's column expressions (driver-aware) | shipped | The exact spelling differs per backend; helpers ship for the common cases |

## Pagination

| Laravel | Suprnova | Status | Notes / link |
|---|---|---|---|
| `LengthAwarePaginator` | `LengthAwarePaginator` (page + total + per_page + last_page) | shipped | `Builder::paginate(n).await?` |
| `Paginator` (simple) | `Paginator` (page + per_page + has_more, no count) | shipped | `Builder::simple_paginate(n).await?` |
| `CursorPaginator` | `CursorPaginator` (opaque cursor token + direction) | shipped | `Builder::cursor_paginate(n).await?`; deterministic for infinite scroll |
| Inertia integration | `IntoInertiaScroll` trait + `ScrollMetadata` | shipped | Wires straight into Inertia's `WhenVisible` / `merge` |

## AI (Laravel ships native today; we don't gatekeep)

| Laravel | Suprnova | Status | Notes / link |
|---|---|---|---|
| AI SDK | No first-party AI SDK | by design no | Bring the crate you already use (`async-openai`, `anthropic-sdk`, `ollama-rs`, `tokenizers`, etc.) and bind under `App` |
| MCP (Model Context Protocol) | No first-party MCP server adapter | by design no | The Rust MCP crates (`mcp-rs`, `mcp-sdk-rust`) sit cleanly under the existing routing / supervisor surface |
| Boost (Laravel coding agent) | n/a | by design no | Out of framework scope |

## Testing

| Laravel | Suprnova | Status | Notes / link |
|---|---|---|---|
| `php artisan test` | `cargo test` | shipped | [Testing](testing.md) |
| Pest / PHPUnit style | `#[suprnova_test]` (async-aware) + `expect!()` Jest-like assertions + `describe!()` / `test!()` BDD macros | shipped | All three work interchangeably |
| Feature tests (HTTP) | Drive `handle_request(router, registry, req)` in-process — no socket open | shipped | Dedicated chapter is **not yet** written |
| Console tests | Run `dispatch_argv(["console", "..."])` and assert | shipped | Same shape as HTTP tests for the console binary |
| Browser tests (Dusk) | n/a in framework — use Playwright / WebdriverIO / `gstack` agent browser | by design no | Cross-language tooling already exists; we don't reinvent it |
| Database tests | `TestDatabase::fresh::<Migrator>()` + per-test rollback | shipped | Dedicated chapter is **not yet** written |
| Mocking & fakes | Per-facade fakes: `MailFake`, `NotifyFakeGuard`, `EventFakeGuard`, `Queue::fake`, `Bus::fake`, `Http::fake`, `Storage::fake` | shipped | Recorded calls + assertion helpers. Dedicated chapter is **not yet** written |
| Time travel | `tokio::time::{pause, advance, resume}` from the stdlib runtime | shipped | Don't ship our own — Tokio's API already does it |
| Container isolation | `TestContainer::fake(\|tc\| tc.bind(...))` — thread-local | diverged | Parallel-safe by construction. [Container](container.md) |

## Payments (Laravel's Cashier; ours is provider-generic)

| Laravel | Suprnova | Status | Notes / link |
|---|---|---|---|
| Cashier (Stripe) | `suprnova-payments-stripe` adapter crate behind generic `Payment` / `Subscription` / `CustomerStore` / `WebhookHandler` traits | diverged | Generic surface, concrete adapter. [Payments](payments.md), Stripe-specific chapter **not yet** |
| Cashier (Paddle) | `suprnova-payments-paddle` adapter | diverged | Merchant-of-Record flow + no direct `Payment` impl (Paddle owns the gateway). Paddle-specific chapter **not yet** |
| Custom provider | Implement `PaymentProvider` + `SessionPayload` + `WebhookHandler` | shipped | [Provider Guide](payments-provider-guide.md) |
| Inertia checkout components | Ship in the scaffold | shipped | [Payments Frontend](payments-frontend.md) |
| Subscription lifecycles | `Subscription::create / update / cancel / resume / swap` (where the provider supports them) | shipped | `NotSupported` returned where the provider doesn't (e.g. Paddle subscription updates) |
| Webhook idempotency | `webhook_events` mirror table with `UNIQUE(provider, external_id)` | shipped | Stripe-style replay protection |
| Mirror tables | `customers`, `payment_methods`, `payments`, `subscriptions`, `invoices`, `webhook_events` | shipped | `provider_metadata` JSONB column on each for adapter-specific fields |

## Frontend (Laravel has Blade + starter kits; we have Inertia)

| Laravel | Suprnova | Status | Notes / link |
|---|---|---|---|
| Blade | n/a — Inertia is the view layer | diverged | [Frontend](frontend.md) |
| Inertia.js | First-class: v3 over Svelte 5 / React 19 / Vue 3.5 | shipped | [Inertia Responses](frontend-inertia-responses.md), [Pages](frontend-pages.md) |
| Partial reloads | `#[derive(Data)]` + `req.includes("subset")` + Inertia's partial-reload protocol | shipped | Type-safe include sets |
| Deferred props | `Prop::deferred(...)` + `DeferConfig` | shipped | Inertia v3 deferred-props protocol |
| Merge props | `MergeConfig` + `MergeStrategy::{Append, Prepend, Replace}` | shipped | Inertia v3 merge protocol |
| Encrypt history | `EncryptHistoryMiddleware` | shipped | History encrypted at rest in the client |
| Scroll position | `ScrollConfig` + `ScrollMetadata` | shipped | Auto-restore on navigation |
| TypeScript types | `suprnova generate-types` reads `#[derive(InertiaProps)]` and emits `.d.ts` | shipped | [TypeScript Types](frontend-typescript-types.md) |
| Vite manifest reading | Auto-wired via `Inertia::root_view` | shipped | HMR in dev, hashed assets in prod |

## CLI

| Laravel | Suprnova | Status | Notes / link |
|---|---|---|---|
| `php artisan` | Per-app `console` binary built from `#[command]` macros | shipped | [Console](console.md), [CLI overview](cli.md) |
| `make:controller` / `make:model` / etc. | `suprnova make:controller / make:middleware / make:action / make:error / make:inertia / make:migration / make:task` | shipped | [Generators](cli-generators.md) |
| `serve` | `suprnova serve` (backend + Vite dev server together) | shipped | [Serve](cli-serve.md) |
| `migrate` family | `suprnova migrate / migrate:rollback / migrate:status / migrate:fresh` | shipped | [Migrations CLI](cli-migrations.md) |
| `db:seed` | `cargo run --bin console db:seed` (via per-app console) | shipped | Seeders registered via `Seeder` trait |
| `schedule:run` / `schedule:work` / `schedule:list` | Same names via per-app console binary | shipped | [Scheduling CLI](cli-scheduling.md) |
| `queue:work` | Same name via per-app console binary | shipped | Graceful shutdown on SIGTERM/SIGINT |
| `tinker` | No REPL | by design no | See the row in "Digging deeper" |

## Deployment

| Laravel | Suprnova | Status | Notes / link |
|---|---|---|---|
| `php artisan optimize` | `cargo build --release` | diverged | One binary, no opcache step |
| `php artisan config:cache` | Typed config is compile-time-checked already | diverged | No runtime cache to invalidate |
| `php artisan route:cache` | Routes are macro-expanded at compile time | diverged | The router is built at boot from already-typed routes |
| Envoy (SSH deploys) | Use any orchestrator — Docker, systemd, Kubernetes, fly.io, Railway | by design no | The binary is the deploy artifact |
| Forge / Vapor | Not ours to ship — but the recipes for Railway, DO, and Hetzner cover the same job | diverged | [Deployment](deployment.md), [Railway](deployment-railway.md), [Digital Ocean](deployment-digital-ocean.md), [Hetzner](deployment-hetzner.md) |
| Horizon (queue dashboard) | No dashboard yet | not yet | Failed-job inspection via `cargo run --bin console queue:failed` until then |

## Packages (Laravel's official packages — ours either ship in core, ship as adapters, or are deliberate gaps)

| Laravel package | Suprnova | Status | Notes / link |
|---|---|---|---|
| Cashier (Stripe) | `suprnova-payments-stripe` | shipped | Generic + adapter. [Payments](payments.md) |
| Cashier (Paddle) | `suprnova-payments-paddle` | shipped | MoR flow. [Payments](payments.md) |
| Dusk | n/a | by design no | Cross-language browser tooling already exists (Playwright, etc.) |
| Envoy | n/a | by design no | Containers / systemd / orchestrators do the job |
| Fortify | Replaced by `auth_flows` | shipped | Same job, integrated. [Auth Flows](auth-flows.md) |
| Folio | n/a — page-based routing isn't idiomatic Rust | by design no | Use `routes!` for explicit routing |
| Homestead | n/a — use Docker / DevContainers | by design no | [Docker recipe](cli-docker.md) |
| Horizon | n/a yet | not yet | Failed jobs surface via the per-app console |
| Mix | Replaced by Vite | diverged | Vite ships in every scaffold |
| Octane | n/a — we are already long-lived Tokio | by design no | Single binary, always warm, no FPM to swap out |
| Passport | n/a yet | not yet | Run a dedicated IdP behind Suprnova until shipped |
| Pennant (feature flags) | Re-implemented as `features::*` | shipped | [Feature Flags](feature-flags.md) |
| Pint (PHP code style) | `cargo fmt` + `cargo clippy` | diverged | Standard Rust toolchain |
| Precognition | Inertia precognitive requests via partial reloads + the same `#[derive(Data, Validate, FormRequest)]` types | shipped | The two halves of Precog (early validation + lightweight reload) both fall out of Inertia v3 + form requests |
| Prompts (CLI UI) | Use the `dialoguer` / `inquire` crate when needed | by design no | Rust ecosystem already covers this |
| Pulse | n/a yet | not yet | OTel today, dashboard later |
| Reverb (WebSocket server) | Built into Suprnova (`ws!()` + `BroadcastHub`) | diverged | No separate server needed — it's the same process |
| Sail (Docker dev) | `suprnova-cli` ships Docker recipes inline | shipped | [CLI Docker](cli-docker.md) |
| Sanctum | `TokenGuard` + bearer middleware | diverged | Token model ships; no separate package surface |
| Scout (full-text search) | n/a yet | not yet | Vector search ships ([Vector](vector.md)); keyword Scout-equivalent later |
| Socialite | Via the vendored torii fork | shipped | [Authentication](authentication.md) |
| Telescope | n/a yet | not yet | Tracing + OTel cover the diagnostic gap until a dashboard ships |
| Valet | n/a — Rust apps run directly | by design no | `suprnova serve` is the dev runner |

## Macros (Rust-specific surface; closest Laravel analogues for context)

Suprnova ships a wide set of proc-macros that don't have a Laravel analogue
because Laravel doesn't have macros — it has runtime reflection. Including
them here so you don't miss them.

| Macro | Closest Laravel idea | What it does |
|---|---|---|
| `#[suprnova::model]` | `extends Model` | Generates SeaORM entity + impls `Model` trait |
| `#[suprnova::observer(M)]` | `User::observe(UserObserver::class)` | Registers an `Observer<M>` impl via `inventory` |
| `#[scopes(M)]` | Local scopes on a model | Adds methods to `Builder<M>` |
| `#[accessor]` / `#[mutator]` | Eloquent accessors / mutators | Field-level get/set hooks |
| `#[handler]` | Controller `__invoke` | Auto-extracts typed params from `Request` |
| `#[command]` / `#[derive(Command)]` | Artisan command class | Registers a console subcommand |
| `#[policy]` | Policy class | Registers a `Policy` impl via `inventory` |
| `#[service(T)]` | Service provider `register` | Binds `T` into the container |
| `#[injectable]` | Constructor injection | Generates an `App::make`-backed constructor |
| `#[derive(InertiaProps)]` | Inertia props | TypeScript codegen + Inertia serialization |
| `#[derive(Data)]` | Request DTO | Extractable from `Request` with include-set support |
| `#[derive(FormRequest)]` | `FormRequest` class | Validation + auth gate + transformation |
| `#[derive(Factory)]` | Model factory | Faker-backed test data generation |
| `#[derive(Resource)]` | API Resource | JSON:API + Laravel-shape serialization |
| `#[workflow]` / `#[workflow_step]` | n/a in Laravel | Long-running stateful work |
| `routes!` + `get!` / `post!` / `ws!` etc. | `Route::get` / `Route::post` | Compile-time route registration |
| `casts!` | `protected $casts = [...]` | Per-model cast declaration |
| `attrs!` | Mass-assignment array | Partial-attribute builder |
| `json_response!` / `text_response!` | `response()->json(...)` | Quick `Ok(HttpResponse::...)` |

See [Macros](macros.md) for the full reference.

## Helper functions (Laravel's global helpers; ours are typed)

Laravel ships hundreds of small globals (`str_replace_first`, `array_flatten`,
`now()`, `tap()`, `optional()` …). Most of them have a direct Rust equivalent
in `std` or a small standard crate, so Suprnova doesn't reintroduce them as a
single namespace. The ones that *are* useful to have aliased ship under their
home module.

| Laravel helper | Suprnova / Rust equivalent | Where |
|---|---|---|
| `auth()` | `Auth::user(&req).await?` | [Authentication](authentication.md) |
| `cache()` | `Cache::get/put/...` | [Cache](cache.md) |
| `config('app.name')` | `Config::get::<AppConfig>()?.name` | [Configuration](configuration.md) |
| `csrf_token()` | `csrf_token()` (same name) | [CSRF](csrf.md) |
| `dd()` | `Builder::dd()` (Eloquent query dump-and-die) / `dbg!()` from the stdlib | `Builder::dump()` / `Builder::dd()` exist for query inspection; use `dbg!()` for general values |
| `env('APP_KEY')` | `env("APP_KEY")` / `env_required("APP_KEY")` / `env_optional("APP_KEY")` | [Configuration](configuration.md), [Env Vars](env-vars.md) |
| `now()` | `chrono::Utc::now()` (re-exported as `suprnova::chrono`) | — |
| `optional($x)->y` | `x.as_ref().map(\|x\| x.y)` | Rust handles this with `Option<T>` directly |
| `redirect('/')` | `redirect("/")` (same name) | [Routing](routing.md) |
| `request()` | `Request` is passed into your handler | [Requests](requests.md) |
| `response()` | `HttpResponse::json/text/redirect/...` | [Responses](responses.md) |
| `route('posts.show', ['post' => 1])` | `url("posts.show", &[("post", "1")])` | [URL Generation](urls.md) |
| `session('key')` | `session().get("key")` | [Session](session.md) |
| `str()` / `Str::camel($x)` | `heck` crate methods (`ToUpperCamelCase`, etc.) | — |
| `tap($x, fn) → $x` | `tap` from `tap` crate, or `dbg!` for quick inspection | Use the `tap` crate idiomatically |
| `today()` | `chrono::Utc::now().date_naive()` | — |
| `value($x)` | Just call the closure: `x()` | n/a — Rust closures need no helper |
| `view('home', $data)` | Inertia response: `Inertia::render("Home", data)` | [Inertia Responses](frontend-inertia-responses.md) |

## What we genuinely don't have yet

A consolidated list of every **not yet** above, so you can see the
shape of the gap in one place:

| Area | What's missing | Workaround until shipped |
|---|---|---|
| Starter kits (Breeze/Jetstream/Spark tier) | Themed auth + dashboard + billing scaffolds | The default `suprnova new` scaffold is closest to Breeze |
| Localization (i18n) | Translation file loader + `__("key", params)` helper | Use `fluent`, `rust-i18n`, or `gettext` crates directly |
| Search (Scout — keyword) | Algolia / Meilisearch / Elastic adapter | Roll your own with `meilisearch-sdk` / `elasticsearch` until shipped; [Vector](vector.md) handles semantic search today |
| Passport (OAuth server) | First-party OAuth identity provider | Run Hydra / Keycloak behind Suprnova |
| Telescope (debug dashboard) | Web UI for requests / queries / events / cache hits | Use OTel + tracing output ([Observability](observability.md)) |
| Pulse (perf dashboard) | Web UI for slow queries / errors / hot routes | Same: OTel surface today, dashboard later |
| Horizon (queue dashboard) | Web UI for queue depth / failed jobs / throughput | `cargo run --bin console queue:failed` and OTel metrics |
| Logging (dedicated chapter) | Walks through tracing subscriber config, log levels, sinks | Listed in [`documentation.md`](documentation.md); the surface is shipped — the chapter isn't written yet |
| Pagination (dedicated chapter) | Walkthrough of the three paginators + Inertia integration | The three paginators ship today; chapter is **not yet** |
| Seeding (dedicated chapter) | Per-model factories + `Seeder` trait walkthrough | Surface ships; chapter is **not yet** |
| Eloquent — Relationships / Collections / Mutators / Serialization / Factories | Dedicated chapters | All five surfaces ship today and are documented in [Eloquent](eloquent.md); standalone chapters are **not yet** |
| HTTP Tests / Database Tests / Mocking | Dedicated chapters | Surfaces ship; chapters are **not yet** |
| Payments — Stripe / Paddle (dedicated chapters) | Adapter-specific walkthroughs | Adapter crates ship today; chapters are **not yet**. [Provider Guide](payments-provider-guide.md) covers building one |
| Release Notes / Contribution Guide | Listed but not yet written | v0.1.0 is gated on these landing |
| Environment Variables (dedicated chapter) | The full table of `APP_KEY` / `DATABASE_URL` / `MAIL_DRIVER` / etc. | Listed in [`documentation.md`](documentation.md); chapter is **not yet** |
| Glossary (dedicated chapter) | Alphabetical concept list | Listed; chapter is **not yet** |

## What we won't ship (and why)

| Laravel feature | Why Suprnova doesn't have it |
|---|---|
| Tinker (REPL) | Rust doesn't have a productive REPL story for compiled binaries. A short `#[suprnova_test]` or a one-off `cargo run --bin <thing>` script does the job |
| Blade templates | Inertia is the view layer; we don't ship a parallel server-rendered template engine |
| `helpers.md` kitchen-sink | Rust ships `std` + small focused crates (`heck`, `chrono`, `regex`); we don't reintroduce a single global namespace |
| Mix | Vite covers it and ships in every scaffold |
| Octane | Suprnova is already long-lived Tokio; there's no FPM mode to optimise out of |
| Dusk (browser tests) | Cross-language tooling (Playwright, WebdriverIO, `gstack` agent browser) already solves this |
| Sail (Docker dev) | Docker recipes ship inline ([CLI Docker](cli-docker.md)); no separate package needed |
| Valet | `suprnova serve` is the dev server |
| Envoy (SSH deploys) | Containers / systemd / orchestrators do the job; we don't need a bespoke SSH DSL |
| Concurrency facade (`Concurrency::run`) | Tokio (`tokio::join!` / `tokio::spawn` / `tokio::select!`) is the answer; no facade needed |
| Processes facade | `tokio::process::Command` is already the right shape |
| First-party AI SDK / MCP / Boost | Pick the Rust crates you already use; we don't gatekeep |
| Dedicated Redis facade | Cache/queue/rate-limit cover 95% of typical use; reach for the `redis` crate when you need ad-hoc commands |
| Strings facade | `heck`, `regex`, `std::str` cover it; no `Str::camel($x)` global |
| Prompts (CLI UI library) | `dialoguer` / `inquire` already exist; we don't reinvent |
| First-party Localization facade *(planned)* | Will ship as a translator trait + driver pattern when the dedicated module lands — listed under "not yet" above, not "won't ship" |

## How this list stays honest

Every row in the **shipped** column is verifiable by:

1. Grepping `framework/src/lib.rs` for the named export
2. Running the framework test suite (`cargo test --workspace`)
3. Reading the linked chapter

Every row in the **not yet** column is on the roadmap. Every row in the
**by design no** column has a one-sentence reason in the Notes column;
those reasons are the design principles in [Introduction](introduction.md)
applied to a specific feature.

If you find a Laravel feature you reach for that isn't on this map, open
an issue — it either has a Suprnova answer that's missing a row, or it's
a real gap and we want to know.

## Next

- [From Laravel](from-laravel.md) — the same map, narrated as a side-by-side
- [Introduction](introduction.md) — the design principles this parity work follows
- [`documentation.md`](documentation.md) — the master TOC across every chapter
