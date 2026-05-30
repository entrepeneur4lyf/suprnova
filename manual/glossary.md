# Glossary

Suprnova-specific terms, defined once. If a chapter uses a word without
explaining it, the definition lives here. Entries are alphabetical;
follow the cross-link for the chapter that uses the term in context.

A handful of conventions to keep in mind while reading the rest of this
list:

- **Trait** means a Rust trait — a behaviour contract you implement on a
  type. **Facade** means a zero-sized struct whose static methods are the
  entry point to a subsystem (`Cache`, `Mail`, `Auth`, `Storage`,
  `Bus`, `Notify`, `Vector`, `DB`, `Schedule`, `App`).
- **Driver** means a swappable backend behind a facade or registry —
  `CacheStore`, `QueueDriver`, `VectorDriver`, `RateLimiterDriver`,
  `MailDriver`. Drivers are picked at boot via environment variables
  and bound through the container.
- **Registry** means a process-global lookup populated at compile time
  via `inventory` or at boot via explicit registration —
  `ConnectionRegistry`, `MiddlewareRegistry`, `InertiaRegistry`,
  `ChannelRegistry`, `VectorRegistry`, `SupervisorRegistry`,
  `PaymentProviderRegistry`, `ScopeRegistry`.

## A

### Accessor

A read-side transformation declared on an Eloquent model with the
`#[accessor]` macro. Runs every time the property is read, returning a
computed value derived from one or more underlying columns
(`full_name` from `first_name + last_name`, for example). The dual of a
[Mutator](#mutator). See
[Eloquent — Accessors and mutators](eloquent.md#accessors-and-mutators).

### Action

An injectable service class that encapsulates one piece of business
logic — a single public method, dependencies injected via the
`#[injectable]` macro. The Suprnova analogue of Laravel's
single-action invokables. Actions are bound as singletons in the
container automatically and resolved by handlers, jobs, and other
actions. See [Actions](actions.md).

### Application

The fluent builder in `Application::new()` that registers your
config, bootstrap, routes, and migrations functions, then calls
`.run()` to dispatch the binary's CLI subcommand (`serve`, `migrate`,
`queue:work`, etc.). One per binary, lives in `src/app.rs`. See
[Request Lifecycle](lifecycle.md).

### Atomic counter

A cache operation (`Cache::increment`, `Cache::decrement`) that
mutates a numeric value as a single round-trip without read-modify-write
races. Backed by Redis `INCR`/`DECR` on the Redis store, by a held
guard on the in-memory store. See [Cache — Atomic Counters](cache.md#atomic-counters).

### Authenticatable

The trait an authenticated user type implements
(`get_auth_identifier() -> String`, `get_auth_password()`, etc.) so
guards and middleware can talk to it without knowing the concrete user
struct. See [Authentication](authentication.md).

### Authorizable

The trait that gives a user type the policy entry points (`can`,
`can_any`, `cannot`) used by the [Gate](#gate). See
[Authorization](authorization.md).

## B

### Backoff schedule

The sequence of delays a queue worker waits between retries of a
failing job. `BackoffSchedule::linear`, `BackoffSchedule::exponential`,
or a custom `Vec<Duration>`. See [Queues — Backoff schedules](queues.md#backoff-schedules).

### Batch (queue)

A group of jobs dispatched together and tracked as a unit —
`PendingBatch::new().add(job).add(other).dispatch()` returns the
persisted batch id. Useful when you want to fan out work and run a
callback when the whole batch completes. See [Queues — Queued batches](queues.md#queued-batches).

### `BelongsTo`

The inverse-of-`HasOne`/`HasMany` relation kind — child holds the
foreign key, parent is on the other side. One of the eleven Eloquent
relation kinds. See
[Eloquent — Relationships](eloquent.md#relationships).

### `BelongsToMany`

A many-to-many relation kind that goes through a third, first-class
[Pivot](#pivot) model. `BelongsToMany<Local, Related, Pivot>` — the
pivot is named in the type, not synthesised by string convention. See
[Eloquent — Relationships](eloquent.md#relationships).

### Bootstrap

The `bootstrap_fn` you register on the `Application` builder and that
runs once at boot (after config, before serving). Where you bind
services into the [Container](#container), register observers and
event listeners, configure default headers, and so on. The Suprnova
analogue of Laravel's service providers, collapsed into one function.
See [Application Bootstrap](bootstrap.md).

### Broadcastable

The trait an [Event](#event) implements when it should be pushed to
WebSocket subscribers instead of (or in addition to) local in-process
listeners. The bridge between the event dispatcher and the
[Broadcast Hub](#broadcasthub). See [Broadcasting](broadcasting.md).

### `BroadcastHub`

The trait that names "the thing that fans out a message to all
WebSocket subscribers of a channel" — the in-memory implementation
(`InMemoryBroadcastHub`) is the default; the sea-streamer
implementation (`SeaStreamerBroadcastHub`) is the multi-process
production deployment. See [Broadcasting — Multi-Process Fanout](broadcasting.md#multi-process-fanout).

### Builder (Eloquent)

The fluent query object returned by `Model::query()` — the chainable
surface where you build `where`, `order_by`, `with`, `limit`, etc.
before `.get()`, `.first()`, or `.paginate(...)`. Dual-named: every
filter method exists under both its Laravel name (`db_where`,
`db_or_where`) and its Rust-native synonym (`filter`, `or_filter`).
See [Eloquent — Query builder](eloquent.md#query-builder--dual-api).

### Bus command

A serialisable struct dispatched through `Bus::dispatch(cmd)` that
routes to a single registered `Handler<C>`. Bus commands are for
in-process work that should bubble its result back to the caller —
queue [Job](#job)s are for work that should be persisted and retried
in the background. See [Command Bus](bus.md).

## C

### Cache driver

The selected backend (`memory` or `redis`) behind the `Cache` facade.
Picked at boot via `CACHE_DRIVER` and surfaced through the
[CacheStore](#cachestore) trait. See [Cache](cache.md).

### `CacheStore`

The trait that defines the cache driver SPI — `get`, `put`, `forget`,
`increment`, etc. `InMemoryCache` and `RedisCache` are the shipped
implementations. See [Cache — Configuration](cache.md#configuration).

### Cast (Eloquent)

A bidirectional transformation declared with `casts!` on an Eloquent
model — DB column type ↔ Rust type. 22 built-ins ship
(`AsBool`, `AsDateTime`, `AsJson`, `AsEncrypted`, `AsArray`, etc.); a
user-implemented `Cast` trait covers anything else. See
[Eloquent — Casts](eloquent.md#casts).

### Chain (queue)

A sequence of [Job](#job)s linked so each one runs only if the
previous one succeeds. Built with `PendingChain::dispatch` /
`Queue::chain`. See [Queues — Queued chains](queues.md#queued-chains).

### Channel (broadcasting)

The trait an event broadcasts to — `PublicChannel`, `PrivateChannel`,
or `PresenceChannel`. The channel struct names itself
(`fn name() -> String`) and authorises connection
(`fn authorize(...)`); private and presence channels add stronger
trait bounds. See [Broadcasting — Channels](broadcasting.md#channels).

### Channel (notification)

The trait that routes a [Notification](#notification) to a delivery
mechanism — mail, database, broadcast, web push. A notification names
its channels in `fn via(...)`; each channel resolves the destination
and sends. Distinct from the broadcasting trait of the same name. See
[Notifications — Channels](notifications.md#channels).

### Container

The three-layer (task-local → thread-local → global) registry where
services are bound and resolved through the `App` facade. The
Suprnova analogue of Laravel's service container, with extra layers
for per-request and per-test isolation. See
[Service Container](container.md).

### Context (per-request)

The per-request bag of typed values reachable from any code in the
same async task — `Context::set::<T>(value)`, `Context::get::<T>()`.
Survives task spawns when you propagate it explicitly. Distinct from
the feature-flag context that shares the name. See
[Context](context.md).

### CORS

Cross-Origin Resource Sharing. The browser security rule that gates a
JavaScript fetch from origin A to origin B; Suprnova ships
`CorsMiddleware` to emit the response headers that signal which
cross-origin requests are allowed. See [CORS](cors.md).

### CSRF

Cross-Site Request Forgery. The attack a stateful session has to
defend against; Suprnova ships `CsrfMiddleware` to require a matching
token on every state-changing request. See [CSRF Protection](csrf.md).

## D

### `DB` facade

The model-less entry point to the database — `DB::table(...)`,
`DB::transaction(...)`, `DB::raw(...)`. For queries that don't fit
the Eloquent shape (dynamic columns, joined aggregates, raw SQL). See
[Eloquent — DB facade](eloquent.md#db-facade--model-less-queries).

### Disk

A named storage backend registered through the `Storage` facade —
`Storage::disk("s3")`, `Storage::disk("local")`. Each disk implements
[DiskExt](#diskext) and is keyed by its registration name. See
[File Storage](filesystem.md).

### `DiskExt`

The trait every storage backend implements — `put`, `get`, `delete`,
`list`, `signed_url`, etc. Backed by `opendal` under the hood; ships
adapters for local fs, in-memory, S3, Azure Blob, and GCS. See
[File Storage](filesystem.md).

## E

### Eloquent

The whole ORM layer — `Model` trait, `Builder<M>`, relations,
casts, scopes, observers, events, soft deletes, prunable, factories.
The Laravel name for what other ecosystems call an ORM; in Suprnova
it sits on top of SeaORM (which the user shouldn't see). See
[Eloquent](eloquent.md).

### Envelope (queue)

The wrapper struct (`Envelope { payload, attempts, max_attempts,
delay, ... }`) that a queue driver actually serialises and stores.
Insulates the [Job](#job) payload from queue plumbing. See
[Queues](queues.md).

### Event

A clonable struct dispatched through `EventDispatcher::dispatch(evt)`
and delivered to every registered `Listener<E>`. Suprnova ships the
trait, the facade (`EventFacade`), the `Subscriber` aggregator, and
hooks for [Queued Listener](#queued-listener)s. See [Events](events.md).

### Event listener

See [Listener](#listener).

## F

### Facade

The naming convention for a zero-sized struct whose `impl` block
holds the public API of a subsystem — `Cache`, `Mail`, `Auth`,
`Storage`, `Bus`, `Notify`, `Vector`, `DB`, `Schedule`, `App`.
Inherited from Laravel; in Suprnova the underlying implementation is
resolved through the [Container](#container) rather than via PHP's
magic-call. See [Service Container](container.md).

### Factory (Eloquent)

The `#[derive(Factory)]` macro and `Factory` trait that produce
realistic test rows with `fake`-driven defaults — `UserFactory::times(5)
.create_many().await?`. The Rust counterpart of Laravel's model factories.
See [Macros — Factories](macros.md#factories).

### Fail-closed

A driver-failure policy where a backend outage causes the request to
reject with a 5xx — used by rate limit, session, and idempotency when
"better to refuse than to leak". The opposite of [Fail-open](#fail-open).
Configured via `BackendErrorPolicy::FailClosed`. See [Rate Limiting](rate-limiting.md).

### Fail-open

A driver-failure policy where a backend outage lets the request
through (with a logged warning) rather than rejecting it — used when
availability outranks the limit. Configured via
`BackendErrorPolicy::FailOpen`. See [Rate Limiting](rate-limiting.md).

### Feature flag

A boolean (or typed value) keyed by name and evaluated against the
current user/context — `feature!(MyFeature)`. Backed by the
`Evaluator` trait; ships a database evaluator and a TTL-cached
evaluator on top. See [Feature Flags](feature-flags.md).

### Fillable

The compile-time allowlist that says which model columns can be
mass-assigned from a hash of untrusted attributes — declared on the
model struct via the `#[fillable]` attribute or the `Fillable` trait.
The dual of `#[guarded]`. See [Eloquent — Mass assignment](eloquent.md#mass-assignment).

### Filesystem

The whole storage subsystem — the `Storage` facade, registered
[Disk](#disk)s, [DiskExt](#diskext) trait, cross-disk streaming
copy. See [File Storage](filesystem.md).

### Form request

A struct implementing `FormRequest` (or derived via `#[request]`)
that extracts and validates a request body before the handler runs.
The composable, type-safe analogue of Laravel's form-request classes.
See [Validation](validation.md).

### `FrameworkError`

The single enum every framework-internal failure converts into. Carries
its own `HttpResponse` projection (`From<FrameworkError> for
HttpResponse`) that sanitises 5xx bodies and stamps a request id.
See [Error Model](error-model.md).

## G

### Gate

The authorization entry point — `Gate::allows("update-post", user,
post)`. Resolves against registered policies (declared via the
`#[policy]` macro) and short-circuits on allow/deny. Returns a
`GateResponse` (re-exported as the authorization `Response`). See
[Authorization](authorization.md).

### Global scope

A query constraint applied to every `Model::query()` call until
explicitly removed (`Builder::without_global_scope`). Implemented via
the `GlobalScope` trait and registered in bootstrap. See
[Eloquent — Scopes](eloquent.md#scopes).

### Guard (auth)

The named authentication strategy attached to a request —
`session` (stateful, cookie-backed), `token` (stateless,
bearer-token). Multiple guards coexist; `Auth::guard("api")` picks
one. See [Authentication](authentication.md).

### Guarded

The compile-time blocklist that says which model columns *cannot* be
mass-assigned. The dual of [Fillable](#fillable). See
[Eloquent — Mass assignment](eloquent.md#mass-assignment).

## H

### `HasMany`

A one-to-many relation kind — parent holds the local key, children
hold the foreign key. One of the eleven Eloquent relation kinds. See
[Eloquent — Relationships](eloquent.md#relationships).

### `HasManyThrough`

A relation that reaches the related model by hopping through a third
intermediate model — `Country -> User -> Post`. See
[Eloquent — Relationships](eloquent.md#relationships).

### `HasOne`

The single-row sibling of [HasMany](#hasmany) — parent holds the
local key, child has the foreign key, returns at most one row. See
[Eloquent — Relationships](eloquent.md#relationships).

### Hash facade

The password-hashing entry point — `hash(password)`,
`verify(password, hash)`. Picks bcrypt or argon2 via `HASH_DRIVER`;
`needs_rehash` lets you migrate users between algorithms on login.
See [Hashing](hashing.md).

### Handler

The async function that returns a `Response` for a matched route —
turned into the framework's typed handler shape by the `#[handler]`
macro. Composed at the inner edge of the middleware chain. See
[Routing](routing.md), [Controllers](controllers.md).

### `HttpError`

The trait a user-defined error type implements to specify how it
should render as an HTTP response — status, body, headers. Mirrors
Laravel's `Renderable` exceptions. See [Error Handling](errors.md).

### `HttpResponse`

The concrete HTTP response type produced by handlers and middleware.
Wraps a status code, headers, and a body — the thing actually written
to the wire. See [Responses](responses.md).

## I

### Idempotency key

A client-supplied header (`Idempotency-Key`) that says "if you've
already processed a request with this key, replay the same response
instead of running the handler again". Required for retry-safe
POST/PUT/PATCH/DELETE; Suprnova ships `Idempotency`, `Idempotent`, and
`Replay` to wrap handlers. See [Idempotency](idempotency.md).

### Inertia response

A response that returns a typed component name plus serialised props
instead of HTML — the bridge between a Rust handler and a Svelte /
React / Vue page. Built with `Inertia::render(...)` or the
`#[derive(InertiaProps)]` macro plus `inertia_response!`. See
[Frontend](frontend.md), [Inertia Responses](frontend-inertia-responses.md).

### `InertiaProps`

The derive macro that generates the `Serialize` impl plus
TypeScript-type metadata for a struct used as an Inertia page's
props. Drives the `suprnova generate-types` command. See
[TypeScript Types](frontend-typescript-types.md).

## J

### Job

A serialisable struct implementing the `Job` trait — has a
`handle(self)` method, enqueued through `Queue::push(job)` (or
`Queue::push_later(job, when)` for a delayed dispatch). Persisted into
the queue driver's storage and run by a worker. See [Queues](queues.md).

### Job middleware

The composable wrappers (`WithoutOverlapping`, `RateLimited`,
`ThrottlesExceptions`, `Skip`, `FailOnException`,
`SkipIfBatchCancelled`) that run around a job's `handle` call. The
queue equivalent of HTTP middleware. See
[Queues — Job middleware](queues.md#job-middleware).

### `JobOutcome`

The discriminated enum a job's settlement produces —
`Completed`, `Failed`, `Released`, `Deleted`, `Skipped` — reported
through job lifecycle events and the queue metrics counter. See
[Queues](queues.md).

## L

### Lazy collection

The streaming counterpart to [Collection](#collection-eloquent) —
`Model::query().lazy().await` returns a `LazyCollection<M>` that pulls
rows from the database in chunks rather than loading every row into
memory. See
[Eloquent — Chunking and lazy iteration](eloquent.md#chunking-and-lazy-iteration).

### Length-aware paginator

The classic numbered-page paginator (`Builder::paginate(per_page)`)
that runs the query plus a `COUNT(*)` — knows the total row count.
See [Eloquent — Pagination](eloquent.md#pagination).

### Listener

The trait an event handler implements — `Listener<E>::handle(evt)`.
Registered with `EventDispatcher::listen::<E, _>(arc_listener)` or
via the `Subscriber` aggregator. See [Events](events.md).

### Lock guard (cache)

The handle returned by `Cache::lock(key, ttl).acquire()` representing
mutual exclusion across processes — `LockGuard`. Releasing the guard
releases the lock; dropping it on the floor relies on the TTL. See
[Cache](cache.md).

### Lock policy

The project-wide policy for handling `std::sync::Mutex` /
`std::sync::RwLock` poisoning in a long-lived process — two sanctioned
patterns (map-to-error or recover-in-place); never bare
`.lock().unwrap()`. See [Lock Policy](lock-policy.md).

## M

### `Mailable`

The trait a mail message implements — `subject`, `to`, `cc`, `bcc`,
`view`, attachments. Either hand-written or derived via the
`#[derive(NotificationMailable)]` macro; sent through
`Mail::to(...).send(MyMail).await`. See [Mail](mail.md).

### Maintenance mode

A request-time flip that takes the application offline for everyone
except an allowlist — `maintenance_mode().set(payload)`. Backed by
`FileMaintenanceMode` (default, a sentinel file) or
`CacheMaintenanceMode` (cache-backed for multi-instance deployments);
served by `MaintenanceMiddleware`. Re-exported at the crate root.

### Middleware

A composable wrapper around a handler — sees the request before, the
response after, and can short-circuit by returning `Err(resp)`.
Registered globally, by route, or by group; runs in a fixed
outside-in order. See [Middleware](middleware.md).

### Model

A struct annotated with `#[suprnova::model]` that names a database
table. The struct *is* the SeaORM `Model` after the macro expands —
Suprnova doesn't wrap it. Carries CRUD via the `Model` trait, query
construction via `Model::query()`, factories, casts, scopes,
relations, observers. See [Eloquent](eloquent.md).

### Morph

Short for "polymorphic". A morph relation lets a single relation
point at one of several model types — `MorphTo` (single owner of
several possible types), `MorphMany`/`MorphOne` (the inverse,
collecting morphed children), `MorphToMany`/`MorphedByMany`
(many-to-many across morphed types). The framework keeps a runtime
[Registry](#registry) of `MorphTypeEntry` mappings between
discriminator strings and Rust types. See
[Eloquent — Relationships](eloquent.md#relationships).

### Mutator

A write-side transformation declared with the `#[mutator]` macro —
runs every time the property is set, before the value is stored on
the model. The dual of an [Accessor](#accessor). See
[Eloquent — Accessors and mutators](eloquent.md#accessors-and-mutators).

## N

### Notifiable

The trait a user (or any object that can receive notifications)
implements — `routes` returns the address per channel (mail address,
push subscription, broadcast user id, etc.). See
[Notifications — The Notifiable Trait](notifications.md#the-notifiable-trait).

### Notification

The trait a notification message implements — `via` returns the list
of channels it should fan out to; each channel calls back into the
notification with `to_<channel>(notifiable)` for the channel-specific
payload. Dispatched through `Notify::send(user, notif).await`. See
[Notifications](notifications.md).

## O

### Observer

A struct implementing `Observer<M>` that listens for an Eloquent
model's lifecycle events — `creating`, `created`, `updating`,
`updated`, `deleting`, `deleted`, `saving`, `saved`, `retrieved`,
`replicating`, etc. Registered via the `#[suprnova::observer(M)]`
macro; drained from the inventory at boot. See
[Eloquent — Observers and lifecycle events](eloquent.md#observers-and-lifecycle-events).

### `OriginPolicy`

The CSRF middleware's enforcement choice for the `Origin` header on
state-changing requests — `Strict` (must match host),
`AllowList`, or `None`. See [CSRF Protection](csrf.md).

## P

### Paginator

The result of a `.paginate(...)` call — one of three flavours.
`LengthAwarePaginator` (numbered pages with a `COUNT(*)`),
`Paginator` (next/prev, no total), `CursorPaginator` (opaque cursor
for stable iteration over a moving result set). All three serialise to
a Laravel-shaped JSON payload. See
[Eloquent — Pagination](eloquent.md#pagination).

### Panic boundary

The `AssertUnwindSafe(...).catch_unwind()` wrapper around the
middleware chain (and around each background-worker handler) that
converts an unhandled panic into a sanitised 500 plus a logged
`ErrorOccurred` event. A safety net, not a contract — public APIs
should still return `Result`. See [Request Lifecycle — Panic boundary](lifecycle.md#5-panic-boundary--execute_chain_safely).

### Payment provider

A type implementing the `PaymentProvider` super-trait (= `Checkout`
+ `Subscription` + `CustomerStore` + `WebhookHandler`). Reference
adapters: `suprnova-payments-stripe` (gateway, full `Payment` impl)
and `suprnova-payments-paddle` (merchant-of-record, no `Payment`).
See [Payments](payments.md), [Provider Guide](payments-provider-guide.md).

### Pivot

The intermediate model in a [BelongsToMany](#belongstomany)
relation — a first-class `#[suprnova::model]` with its own struct,
casts, and timestamps, named explicitly as the third type parameter
(`BelongsToMany<L, R, P>`). Suprnova does not synthesise an implicit
pivot from a table name. See
[Eloquent — Relationships](eloquent.md#relationships).

### Presence channel

A [Channel](#channel-broadcasting) variant where the server tracks
who is currently subscribed and emits join/leave events with each
member's metadata. Useful for "who's online" indicators. See
[Broadcasting — Presence Channels](broadcasting.md#presence-channels).

### Private channel

A [Channel](#channel-broadcasting) variant that requires
authorisation on subscribe — `authorize(...)` must return true for
the subscribing user. Useful for per-user notification streams. See
[Broadcasting — Channels](broadcasting.md#channels).

### Prunable

The trait that marks a soft-deleted (or queryable) model as eligible
for cleanup by `model:prune` — `Prunable::prunable_query()` returns
the builder for rows that should go. `MassPrunable` deletes in a
single `DELETE WHERE`; the default issues per-row deletes so observers
fire. Tagged for the registry via the `#[prunable]` macro. See
[Eloquent — Prunable](eloquent.md#prunable).

## Q

### Queue

The whole background-work subsystem — `Queue` facade, [Job](#job)
trait, [Envelope](#envelope-queue), drivers (memory, sync, redis,
database, null), worker, batches, chains. See [Queues](queues.md).

### Queue driver

A type implementing `QueueDriver` (push, pop, release, etc.) —
ships `MemoryQueueDriver`, `SyncQueueDriver` (run inline),
`RedisQueueDriver`, `DatabaseQueueDriver`, `NullQueueDriver`. Picked
at boot via `QUEUE_DRIVER`. See
[Queues — Drivers](queues.md#drivers).

### Queue worker

The long-lived loop that pulls envelopes off the queue driver, runs
job middleware around the handler, and reports the outcome. Boots
through the same lifecycle as the HTTP server so observers and
listeners fire identically. Started by `suprnova queue:work`. See
[Queues](queues.md).

### Queued listener

A `Listener<E>` that, when invoked, persists the event payload to
the queue and runs `handle` in a background worker rather than
in-process. Useful when an event listener does I/O that shouldn't
block the dispatch path. Wrapped via the `QueuedListener` adapter.
See [Events](events.md).

## R

### Rate limiter

The whole rate-limiting subsystem — `RateLimiter` (the cache-backed
facade), `Limit` builder, `SlidingWindowConfig` (sliding-window
driver), `RateLimitMiddleware` (route-mounted),
`ThrottleRequestsMiddleware` (Laravel-named alias),
`BackendErrorPolicy` (fail-open vs fail-closed). See
[Rate Limiting](rate-limiting.md).

### Redirect

A specialised [HttpResponse](#httpresponse) wrapping a `Location`
header — built via `Redirect::to(...)`, `Redirect::route(...)`,
`Redirect::back()`, with `.with(...)`/`.with_input(...)` chains for
flash data. See [URL Generation](urls.md), [Responses](responses.md).

### Registry

A process-global lookup populated either at compile time by
`inventory` (`ModelEntry`, `RelationEntry`, `MorphTypeEntry`,
`ObserverEntry`, `PrunerEntry`, `TaskEntry`, `PaymentProviderEntry`,
`CommandEntry`) or at boot by explicit registration
(`ConnectionRegistry`, `MiddlewareRegistry`, `InertiaRegistry`,
`ChannelRegistry`, `VectorRegistry`, `SupervisorRegistry`). All
are drained or queried during the boot sequence.

### Relation

The trait every relation kind implements — `BelongsTo`, `HasOne`,
`HasMany`, `BelongsToMany`, `HasOneThrough`, `HasManyThrough`,
`MorphTo`, `MorphOne`, `MorphMany`, `MorphToMany`, `MorphedByMany`.
A model declares its relations as methods returning a relation
struct; the framework drives eager loading, `with(...)`,
relation-existence queries, and cascading touches from the trait. See
[Eloquent — Relationships](eloquent.md#relationships).

### Request

The framework's typed request struct — wraps the underlying hyper
request and exposes `req.param("id")`, `req.json::<T>()`,
`req.form_data()`, `req.flash()`, etc. Re-exported as
`suprnova::Request`. See [Requests](requests.md).

### `Response`

Suprnova binds `http::Response` to `Result<HttpResponse,
HttpResponse>` — both arms carry an `HttpResponse`. Handler bodies
return `Response`, propagate fallible work with `?`, and the runtime
collapses both arms with `result.unwrap_or_else(|e| e)`. The
authorization decision type is re-exported as `GateResponse` to avoid
the collision. See [Responses](responses.md),
[Request Lifecycle](lifecycle.md#the-response-contract).

### Resource

Two unrelated things share the name; both ship.

1. **JSON:API resource** — a `#[derive(Resource)]` struct that
   serialises a model into the JSON:API shape with sparse fieldsets
   and includes. See [API Resources](eloquent-resources.md).
2. **Resource routing** — a route helper that mounts a CRUD
   `index`/`show`/`store`/`update`/`destroy` set against a
   `ResourceController` impl. See [Routing](routing.md).

### `routes!` macro

The compile-time macro that expands a routing DSL
(`get!("/users", users::index)`, `group!`, `middleware!(Auth)`) into
a `Router` factory function. The single source of route truth for an
application. See [Routing](routing.md), [Macros](macros.md).

## S

### Scope (local)

A reusable query fragment declared on an Eloquent model with the
`#[scopes(Model)]` macro — `Post::query().published().recent().get()`.
Local scopes are off by default; they only run when invoked. The
counterpart of [Global scope](#global-scope). See
[Eloquent — Scopes](eloquent.md#scopes).

### Seeder

A type implementing the `Seeder` trait that populates the database
with starting data — registered through `suprnova db:seed`. Often
backed by a [Factory](#factory-eloquent). See [Eloquent](eloquent.md).

### Signed URL

A URL whose query string carries an HMAC signature
(`?signature=...&expires=...`) proving it was produced by the
application and hasn't been tampered with. Built via
`sign_url(...)` / `sign_route(...)`; verified by middleware or via
`verify_signature(...)`. See [URL Generation — Signed URLs](urls.md#signed-urls).

### Soft deletes

The pattern where deleting a model row sets a `deleted_at` timestamp
instead of issuing `DELETE`. Opt-in per model via
`soft_deletes = true` on the `#[suprnova::model]` attribute;
`Model::query()` auto-filters out trashed rows; `with_trashed()` and
`only_trashed()` opt back in. See
[Eloquent — Deleting and soft deletes](eloquent.md#deleting-and-soft-deletes).

### `Storage` facade

The entry point to the filesystem subsystem —
`Storage::disk("s3")`, `Storage::disk("local")` — returning a
[DiskExt](#diskext) implementation. See [File Storage](filesystem.md).

### Subscriber

An aggregator that registers many listeners in one call — implements
`Subscriber::subscribe(dispatcher)` and is registered via
`EventDispatcher::subscribe(subscriber)`. See [Events](events.md).

### Supervisor

The trait a long-lived background actor implements (`Supervisor::run`)
to live under the `SupervisorRegistry`. The registry catches panics in
the run loop, applies a `RestartPolicy`, and re-spawns. The Rust
equivalent of Erlang's `gen_server` supervisor pattern. See
[Supervisors](supervisors.md).

## T

### Task

A struct implementing the `Task` trait — declares a cron expression
or a higher-level frequency (`daily()`, `every_minute()`) and runs on
the scheduler. Discovered at compile time via the `TaskEntry`
inventory. See [Task Scheduling](scheduling.md).

### Terminable middleware

Middleware that registers a hook to run *after* the response has been
written to the client — implemented via the `Terminable` trait,
captured into a `TerminationSnapshot`, and dispatched by
`dispatch_termination`. Useful for logging, metric flushes, post-flight
auditing. See [Middleware — Terminable middleware](middleware.md#terminable-middleware-post-response-hooks).

### Through (relation)

A relation that hops through a third intermediate model —
[HasManyThrough](#hasmanythrough) and `HasOneThrough`. See
[Eloquent — Relationships](eloquent.md#relationships).

### Timeout

The middleware that bounds a single request's wall-clock time and
returns 504 when the bound is exceeded — `TimeoutMiddleware`. Distinct
from queue worker timeouts (`TimeoutExceeded` on the queue side) and
from HTTP-client timeouts. See [Timeout](timeout.md).

### `TypedCommand`

The console-side trait — implemented by `#[derive(Command)]` structs —
that gives a console command typed arguments (via `clap`) and an
async `handle(self)` method. Registered into the `CommandEntry`
inventory at compile time. See [Console](console.md).

## U

### `UserId`

The opaque string identifier returned by `Auth::id()` — a
torii-issued user id, not the database primary key. Sessions store
the `UserId`; user-provider lookups translate it to the concrete user
struct. The intentional indirection lets you swap user backends
without rewriting handler code. See [Authentication](authentication.md).

## V

### VAPID

Voluntary Application Server Identification — the IETF spec for
identifying a web-push sender. Suprnova ships `VapidKey`,
`VapidSigner`, `VapidClaims`, and the `WebPushClient` that signs each
push request. See [Web Push](web-push.md).

### `Vector` facade

The entry point to the vector-search subsystem —
`Vector::driver("qdrant").await?.upsert(...)`. Backed by
`VectorDriver` implementations: in-memory, Qdrant, Pinecone (feature
gated), MariaDB native. See [Vector Search](vector.md).

### `VectorDriver`

The trait every vector backend implements — `upsert`,
`search`, `delete`, `count`. Allows the framework to support multiple
vector DBs without forcing one. See [Vector Search](vector.md).

## W

### Web push

The web-platform push-notification protocol — encrypted payloads
delivered through the user agent's push service. Suprnova ships
`WebPushClient` (VAPID signer, retry-after parsing, 8 KiB rejection
cap) and `WebPushChannel` for [Notification](#notification) delivery.
See [Web Push](web-push.md).

### Webhook

An HTTP request sent by a third-party (payment provider, identity
provider, …) into your application to report an event. Suprnova
treats every webhook as idempotent by default — provider adapters
implement `WebhookHandler::verify(...)` and store the provider's
event id in a `UNIQUE` constraint that rejects replays. See
[Payments — Webhook Handling](payments.md#webhook-handling),
[Idempotency](idempotency.md).

### Workflow

A long-running, stateful piece of background work composed of typed
steps — `#[workflow]` and `#[workflow_step]` macros. Each step's
return value is persisted, so a worker restart mid-workflow resumes
from the last completed step. The Suprnova answer to multi-step
background processes that don't fit a single [Job](#job). See
[Workflows](workflows.md).

### `WsConfig`

The per-route WebSocket configuration — payload size caps (default
1 MiB text / 64 KiB binary), max frame size, ping interval, idle
timeout, origin policy. Used by `ws!()` routes. See [WebSockets](websockets.md).

### `WsSocket`

The framework's typed WebSocket handle handed to a `ws!()` handler.
Split into a `Sink` (send) and a `Stream` (receive) half via
`WsSocket::split()`; pings/pongs are managed by a heartbeat task with
an `AbortHandle` so a dropped handler always tears down cleanly. See
[WebSockets](websockets.md).

## Next

- [Laravel Parity Map](parity.md) — feature-by-feature comparison
  against Laravel 13
- [Environment Variables](env-vars.md) — every `env!` the framework reads
- [Documentation index](documentation.md) — the chapter map
