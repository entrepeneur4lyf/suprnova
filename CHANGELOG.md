# Changelog

All notable changes to Suprnova are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [SemVer](https://semver.org/spec/v2.0.0.html).

Pre-1.0, internal API churn is expected. Semver guarantees begin at `1.0.0`.

## [Unreleased]

### Added

- **Resource-route authorization** — `ResourceRoutes::authorize_resource::<U, R>()`
  attaches the conventional ability check to every generated resource route as
  per-route middleware (Laravel `authorizeResource` parity). The action→ability
  map is `index`/`show` → `view`, `create`/`store` → `create`,
  `edit`/`update` → `update`, `destroy` → `delete`. One call gates the whole
  seven-action surface instead of relying on every controller body to remember
  a `Gate::authorize`.
- **Atomic rate-limit hit** — `RateLimiter::hit_and_check(key, max, decay)`
  increments a fixed window and tests it in a single round-trip, returning
  whether the bucket is now over its limit (`i64::MAX` means unlimited).
- **Constant-time comparison helper** — `constant_time_eq(a, b)` (subtle-backed)
  for webhook signature verification; `WebhookHandler::verify` docs now mandate
  constant-time digest comparison.

### Security

- **Resource routes** fail closed on the authorization registry's type-erased
  downcast instead of panicking, and `authorize_resource` denials /
  unauthenticated requests are refused before the handler runs.
- **Rate limiter** closes a fixed-window check-then-hit race by incrementing and
  comparing atomically (`hit_and_check`).
- **Queue `RateLimited` middleware** now admits jobs through that atomic
  `hit_and_check` instead of a separate `too_many_attempts` + `hit` pair, so
  concurrent workers can no longer all pass the budget check before any of them
  increments and over-admit past `max_attempts`.
- **Upload validators** (`mimetypes` / `mime`) content-sniff the uploaded bytes
  instead of trusting the client-supplied `Content-Type`.
- **Filesystem path guard** canonicalizes paths to catch symlink traversal out
  of the storage root, beyond the prior lexical `../` / absolute / UNC checks.
- **Auth** closes a passwordless-login timing oracle — a matched-but-passwordless
  account given a password now runs a fixed-cost verify — and `dummy_verify`
  drives the configured hasher so the unmatched-user path is constant-time.
- **Eloquent** validates column identifiers on the `pluck` / `value` /
  `pluck_keyed` / `sole_value` and `sum` / `avg` / `min` / `max` projection
  paths.
- **Payments** — the mock provider's verifier fails closed outside a development
  environment, and webhook source IPs resolve through `TrustedProxiesConfig`
  (`req.ip()`) rather than a raw `X-Forwarded-For` header.

### Documentation

- Documented resource-route authorization (`authorize_resource`) in the routing
  and authorization chapters, and the atomic `hit_and_check` counter in the
  rate-limiting chapter.

## [0.2.0] — 2026-06-21

Adds role-based access control, a Markdown content / docs-rendering pipeline, and
native static-file serving.

### Added

- **Tier-2 RBAC** — `HasRoles` trait; roles + permissions with a
  `role_has_permissions` join; `PermissionMiddleware` / `RoleMiddleware` (both
  fail-closed / default-deny); the `CreateRbacTables` migration; and
  `create_role` / `create_permission` / `give_permission_to_role` helpers.
- **Content rendering** — Markdown rendering and a docs-build pipeline:
  `MarkdownRenderer`, `build_docs`, `DocsCatalog` / `DocsChapter`, heading
  extraction and `slugify_heading`. Rendered HTML is sanitized
  (comrak + syntect + ammonia).
- **Native static-file serving** — `StaticFiles::public()` fallback handler for
  serving a `public/` directory at the web root, replacing hand-rolled per-asset
  whitelist controllers in apps.

### Fixed

- Freshly generated apps inherit a framework-level `time = 0.3.47` compatibility
  pin, avoiding Rust 1.96 coherence conflicts from `time 0.3.48` in fresh
  scaffold dependency resolutions.

### Documentation

- Documented the two shipped starter kits — **Nebula** (Breeze-tier auth) and
  **Pulsar** (product site + community) — across the manual, README, and roadmap;
  restructured the roadmap around the shipped surface; and reconciled version
  references throughout the docs.

## [0.1.0] — 2026-06-10

The initial Suprnova release. Suprnova is a Laravel-inspired web
framework for Rust, forked from Kit and taken in its own direction.
Today's parity target is Laravel 13.x.

This release uses the git distribution model: framework consumers depend
on `suprnova = { git = "https://github.com/entrepeneur4lyf/suprnova.git" }`,
and the CLI installs with `cargo install --git`.

### Added

#### HTTP, routing, and middleware

- `Router` with route groups, prefixes, parameter constraints, named routes
- Compile-time-validated route registration via the `routes!` macro
- Resource routing (`Router::resource`) producing the seven standard routes
- Signed URLs (`url::signed_route` / `url::temporary_signed_route` free
  functions, plus `Redirect::signed_route` / `Redirect::temporary_signed_route`)
- Redirect helpers — `Redirect::to`, `Redirect::back`, `Redirect::route`,
  `Redirect::with_input`, `Redirect::with_errors`, `with_flash`
- Middleware trait with global, group, and per-route layers
- Built-in middleware — CORS, CSRF, session, request timeout,
  request ID, throttle / login throttle, signed-URL verify,
  authenticated, email-verified, brute-force
- Abort helpers (`abort`, `abort_unless`, `abort_if`)
- `suprnova::handle_request(...)` — public adapter to serve a single
  hyper request against a router + middleware chain

#### Inertia.js frontend bridge

- `#[derive(InertiaProps)]` with TypeScript type emission
- `inertia_response!` macro with compile-time component validation
- Three first-class starter frontends — **Svelte 5** (runes-on),
  **React 19**, **Vue 3.5** — all on Inertia 3.1.1 + Vite 8 + Tailwind v4
- Partial reloads (`only` / `except`), deferred props, persistent
  layout, encrypted history, scroll preservation
- `Inertia::paginate(component, key, paginator)` for paginator → Inertia
  prop wiring

#### Eloquent-style ORM (over SeaORM)

- `#[suprnova::model]` attribute macro that emits a SeaORM entity and
  the user-facing Eloquent struct in one shot
- Full `Model` trait — `create`, `find`, `find_or_fail`, `find_many`,
  `all`, `query`, `save`, `update`, `delete`, `force_delete`, `refresh`,
  `fresh`, `replicate`, `replicate_into`, `increment`/`decrement`,
  `destroy`, `is`/`is_not`, `to_array`/`to_json`
- Fillable / guarded mass-assignment with `Attrs` envelope
- 22 attribute casts — booleans, integers, floats, dates, enums,
  hashed, encrypted, JSON, collections, money, datetime with timezone
- Accessors / mutators via `#[suprnova::model]`
- Auto-timestamps (`created_at`, `updated_at`)
- Soft deletes (`deleted_at`) with `force_delete`, `restore`, `trashed`,
  `only_trashed`, `with_trashed`
- Eleven relation kinds — `HasOne`, `HasMany`, `BelongsTo`,
  `BelongsToMany`, `HasOneThrough`, `HasManyThrough`, `MorphOne`,
  `MorphMany`, `MorphTo`, `MorphToMany`, `MorphedByMany`
- Per-family morph enums + morph registry with `APP_KEY_PREVIOUS` rotation
- Eager loading via `.with(...)`, `.with_count(...)`, `.load_missing(...)`
- Correlated EXISTS engine for `has` / `where_has`
- Sixteen lifecycle events (retrieving, retrieved, creating, created,
  updating, updated, saving, saved, deleting, deleted, restoring,
  restored, force-deleting, force-deleted, replicating, trashed)
- `Observer<M>` trait with per-method auto-registration via inventory
- Local scopes via `#[scopes(M)]`, global scopes via `GlobalScope`
- `Collection<M>` Laravel surface — `pluck`, `key_by`, `group_by`,
  `where_in`, `first_where`, `contains_where`, `partition`, etc.
- Three paginators — `paginate` (length-aware), `simple_paginate`,
  `cursor_paginate` — all serializing to Laravel-shape JSON
- `chunk` / `lazy` / `cursor` for bulk-row iteration without OOM
- `lock_for_update` / `shared_lock` row-level locking
- `DB::table(...)` query builder with `DynamicRow` for ad-hoc queries
- `DB::transaction(...)` with savepoints, retry-on-deadlock,
  multi-connection read/write split
- `DB::listen(...)` + `QueryExecuted` / `TransactionBegan` /
  `TransactionCommitted` / `TransactionRolledBack` events
- `Prunable` trait + `model:prune` console command
- `dump` / `dd` query-helper methods
- `#[model(unique_id="...")]` for UUID / ULID primary keys

#### Auth

- `Authenticatable` trait + `EloquentUserProvider<M>`
- `Auth::attempt`, `Auth::login`, `Auth::user`, `Auth::user_or_fail`,
  `Auth::user_as<T>`, `Auth::logout`, `Auth::check`
- Multiple named guards (web session, API token)
- Email verification flow — `EmailVerification`,
  `EnsureEmailVerifiedMiddleware`, signed verification URLs,
  `EmailVerificationMail`
- Password reset flow — `PasswordReset`, throttled tokens,
  `PasswordChangedMail`, `PasswordResetLinkSent` event
- Two-factor TOTP — enroll, verify, recovery codes, replay protection
- Brute-force / login throttle — IP + identifier keyed,
  `LoginThrottleMiddleware`
- Remember-me cookies with stable opaque tokens
- Six auth events — `LoginAttempted`, `LoggedIn`, `Authenticated`,
  `LoggedOut`, `PasswordResetLinkSent`, `EmailVerified`
- Browser sessions backed by the Torii fork at
  `github.com/entrepeneur4lyf/suprnova-torii-rs`

#### Authorization

- `Gate` facade — `define`, `allows`, `denies`, `authorize`, `any`,
  `none`, `check` (sync + async variants)
- `#[policy(Model)]` macro for policy registration
- Resource-route auto-authorization

#### Payments

- Provider-agnostic five-trait surface — `Checkout`, `Payment`,
  `Subscription`, `CustomerStore`, `WebhookHandler`
- `PaymentProvider` umbrella trait + capability-querying via `as_payment()`
- DB mirror — `customers`, `subscriptions`, `subscription_items`,
  `payments`, `refunds`, `payment_webhook_events` (UNIQUE for idempotency)
- Flow-tagged `SessionPayload` enum (one-shot vs subscription)
- Two reference adapters as workspace crates —
  `suprnova-payments-stripe` (gateway, full `Payment` impl),
  `suprnova-payments-paddle` (Merchant of Record, no `Payment` impl)
- Mock provider for tests

#### Queue, jobs, batches, chains

- `Job` trait — `handle`, `max_tries`, `backoff`, `timeout`,
  `fail_on_timeout`
- `Queue::push`, `Queue::push_later`, `Queue::push_unique`,
  `Queue::push_unique_later`
- Drivers — `sync`, `null`, `redis`, `database`
- `JobMiddleware` trait — six built-in middleware
- Batches and chains — `Queue::batch(jobs).dispatch()`, fluent chain
  builder, cancellation, progress tracking
- Failed-jobs store with replay
- Worker with graceful shutdown, configurable concurrency, panic
  recovery via `catch_unwind`, settlement metrics
- Twelve queue events covering queueing, processing, failure, release,
  worker lifecycle

#### Broadcasting and WebSockets

- `ws!()` macro + `Router::ws` for typed WebSocket endpoints
- `WsSocket` Sink/Stream split
- Auto-restart supervisors via `Supervisor` trait
- `BroadcastHub` with `Channel`, `Private`, `Presence` channels
- JSON-envelope protocol, presence join/leave/here, configurable
  presence TTL with crash recovery
- `Broadcastable` bridge to `EventDispatcher`
- Close-on-no-pong heartbeat with configurable WS_TASKS drain
- Per-route WebSocket middleware
- 1 MiB / 64 KiB safer defaults + `WsConfig::generous()` factory
- Origin policy + 1011 close-on-protocol-violation

#### Notifications and mail

- `Notification` trait + `Notify::send(recipient, notification).await`
- Mailable + Markdown template rendering
- Database / mail / broadcast / web-push channels
- VAPID signing + RFC 8291 ECE payload encryption (via
  `suprnova-web-push`)
- VAPID subject validation, retry-after parsing, 8 KiB rejection-body cap
- Notifiable trait for recipient typing

#### Events

- Typed event dispatcher — `EventFacade::dispatch`,
  `EventFacade::listen<E, L>`, `EventFacade::forget`
- Cancellable saving/updating events (return `EventResult::cancel`)
- Queueable listeners

#### Filesystem

- `Storage::disk("name")` with multi-driver support — local, S3,
  Azure, GCS via OpenDAL
- Move, copy, exists, size, mime, last-modified, prepend/append
- Streaming uploads and downloads

#### Cache

- `Cache::store("name")` + driver registration
- Drivers — memory, redis (with bounded connect-timeout), database, file
- `remember`, `forever`, `tags`, atomic increment/decrement, locks

#### Vector DB

- `VectorDriver` trait with four drivers — in-memory, Qdrant
  (UUID-5 ID mapping), Pinecone (native string IDs), MariaDB native
  `VECTOR(N)` + HNSW indexes (11.7+)
- Cosine / dot / euclidean distance

#### Console binary and CLI

- Per-project `console` binary — Rust analogue of `php artisan`,
  runs user-defined commands via `#[suprnova::console::command]`
- `#[derive(Command)]` for typed arguments
- `suprnova` CLI — `new`, `serve`, `migrate`, `db:sync`,
  `generate-types`, `key:generate`, `make:{controller,middleware,action,error,inertia,migration,task,command}`,
  `db:seed`, `model:prune`
- `--version` flag
- Scaffold templates for backend + API starters across three frontends

#### Feature flags

- `DatabaseEvaluator` with snapshot loading
- `CachedEvaluator` with TTL
- `FeatureMiddleware` extractor
- Admin CRUD surface
- `FeatureSync` trait for sub-second propagation across processes

#### Schedule

- Cron expression parser
- `Schedule::task(...)` with composable predicates
- Single-server locks, overlap prevention, dispatch tracking
- `schedule:run` console command

#### Validation

- `validator` 0.20 integration
- `#[request]` + `#[derive(FormRequest)]` macros
- `#[form_request(max_body_bytes = N)]` per-form size cap
- `#[form_request(custom_hooks)]` opt-out for user-written
  `impl FormRequest`
- Lifecycle hooks — `authorize`, `after_validation`,
  `after_validation_async`

#### Database drivers

- SeaORM-backed support for SQLite, Postgres, MySQL, MariaDB
- URL-based driver detection
- Migration system + `migrate`, `migrate:rollback`, `migrate:status`,
  `migrate:fresh`, `migrate:refresh`

#### HTTP client

- `Http` facade — `get` / `post` / `put` / `patch` / `delete`
  returning a `RequestBuilder`; `.send().await` produces a
  `ClientResponse`
- rustls TLS, 30s default timeout, `suprnova/<version>` user-agent
- `json` / `form` / `body` / `header` / `bearer_token` / `basic_auth`
  / `timeout` chainable methods
- `RequestBuilder::retry(max_attempts, base_backoff)` — exponential
  backoff for transient failures and 5xx; respects `Retry-After`
- `Http::fake(|| async { ... }).await` test guard with
  `fake_response(method, url_substring, status, body)` +
  `assert_sent` / `assert_not_sent`

#### Encryption

- `Crypt` static facade + `EncryptionKey` (`crypto::*`); AES-256-GCM
  with 12-byte random nonces
- `encrypt_string` / `decrypt_string` / `encrypt<T>` / `decrypt<T>`
- `CryptPurpose` AAD binding preventing cross-protocol replay
- `APP_KEY_PREVIOUS` rotation
- `suprnova key:generate` CLI command for minting fresh keys

#### Testing

- `#[suprnova_test]` async test macro
- `TestDatabase::fresh::<Migrator>()` with parallel-safe instances
- `TestContainer::bind` for per-test mocks
- HTTP test helpers — `Test::get`, `Test::post`, JSON / form / multipart
- Queue / Mail / Notification / Event fakes
- `assert_emitted`, `assert_dispatched`, `assert_dispatched_times`

### Changed

- Auth verification and password-reset flows now operate through the
  configured user provider instead of Torii internals.
- Generated apps must implement `get_auth_password`; scaffolded examples
  now fail loudly instead of allowing login to always fail silently.
- The local release gate is wired into `scripts/release.sh`, and the repo
  includes an enforced pre-push hook for fmt, clippy, tests, docs, and
  feature builds.
- Scaffolded dev-port documentation moved to the current backend/frontend
  defaults (`8765` / `5765`), with `dev:tls` and `--with-portless`
  documented.
- `MAIL_FROM` is validated before verification or reset tokens are issued,
  avoiding orphaned auth-flow rows when mail configuration is invalid.

### Fixed

- React scaffold template drift from the released starter.
- Root route groups no longer generate duplicate `//` paths.
- Literal-path redirects now dispatch through the intended routing path.
- Broadcasting fanout tests now handle `track` / `untrack` results.
- The mail log driver emits the rendered text body, so verification and
  password-reset links surface in local development logs.
- Password-reset coverage pins session and remember-me revocation behavior.

### Notes

- **Distribution model**: git-based end-to-end.
  `suprnova = { git = "https://github.com/entrepeneur4lyf/suprnova.git" }`;
  CLI via `cargo install --git`. Nothing is published to crates.io.
- **Permanent deferrals**: Phase 14 Telescope/Pulse (use `tracing` +
  OpenTelemetry instead); Phase 15 browser testing (use
  `chrome-devtools-mcp` instead).
- **Internal API churn through `0.1.x`** is expected and intentional;
  semver guarantees begin at `1.0.0`.

[Unreleased]: https://github.com/entrepeneur4lyf/suprnova/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/entrepeneur4lyf/suprnova/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/entrepeneur4lyf/suprnova/releases/tag/v0.1.0
