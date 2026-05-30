# Middleware

Middleware wraps a request handler. It runs before the handler sees the
request and again after the handler returns a response, so it's the
place to put cross-cutting work — auth, logging, CORS, throttling,
timing, transforming the request or response. Suprnova's surface is the
same one Laravel users already know: a `handle(request, next)` method
that decides whether to forward the request, short-circuit it, or
mutate the response on the way back out.

## The trait

A middleware is a struct that implements `Middleware`:

```rust
use suprnova::{async_trait, HttpResponse, Middleware, Next, Request, Response};

pub struct LoggingMiddleware;

#[async_trait]
impl Middleware for LoggingMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        // Pre-processing: runs before the handler.
        println!("--> {} {}", request.method(), request.path());

        // Forward to the next middleware (or the handler if this is
        // the last layer).
        let response = next(request).await;

        // Post-processing: runs after the handler returns.
        println!("<-- complete");

        response
    }
}
```

`handle` has three things to do, and you only have to do one of them
on any given request:

- **Forward.** Call `next(request).await` to pass control to the next
  layer. The returned `Response` is what every layer above will see.
- **Short-circuit.** Return `Err(HttpResponse::...)` without calling
  `next`. The framework collapses both arms of `Response`
  (`Result<HttpResponse, HttpResponse>`) into a single response — an
  `Err` is a response, not a crash. See [Error Model](error-model.md).
- **Mutate.** Modify the request before forwarding, or modify the
  response after.

`Next` is `Arc<dyn Fn(Request) -> MiddlewareFuture + Send + Sync>` —
treat it like an async function from `Request` to `Response`.

## Generating a stub

The CLI scaffolds a working middleware file:

```bash
suprnova make:middleware Auth         # → src/middleware/auth.rs (AuthMiddleware)
suprnova make:middleware RateLimit    # → src/middleware/rate_limit.rs
suprnova make:middleware CorsMiddleware  # "Middleware" suffix is fine, same result
```

The generated file isn't a TODO stub — it's a real middleware that
times the wrapped request and logs the inbound/outbound events with
the per-request id installed by `RequestIdMiddleware`. Replace the
body with whatever you actually need.

## Registering middleware

Three places to install it, depending on scope:

### Global

Runs on every request, in registration order. Use the
`global_middleware!` macro inside `bootstrap()`:

```rust
// src/bootstrap.rs
use suprnova::{global_middleware, FrameworkError};
use crate::middleware;

pub async fn bootstrap() -> Result<(), FrameworkError> {
    global_middleware!(middleware::LoggingMiddleware);
    global_middleware!(middleware::CorsMiddleware);
    Ok(())
}
```

`global_middleware!(M)` expands to `register_global_middleware(M)`.
Registration is **idempotent per concrete type** — registering the same
struct twice keeps the first registration and emits a debug log. That
makes re-running boot (tests, hot-reload, multiple `Server` instances
in a process) safe. To install several copies of the same behaviour
with different config, wrap each in a distinct newtype.

### Per route

Chain `.middleware(M)` on a route definition from the `routes!` macro:

```rust
// src/routes.rs
use suprnova::{routes, get};
use crate::{controllers, middleware::AuthMiddleware};

routes! {
    get!("/", controllers::home::index).name("home"),
    get!("/public", controllers::home::public),

    get!("/protected", controllers::dashboard::index)
        .middleware(AuthMiddleware),
    get!("/admin", controllers::admin::index)
        .middleware(AuthMiddleware),
}
```

### Per group

Apply middleware to every route in a `group(...)` block:

```rust
use suprnova::Router;
use crate::middleware::{ApiMiddleware, AuthMiddleware};
use crate::controllers::{user, admin};

Router::new()
    // Public routes — no middleware.
    .get("/", home_handler)
    .get("/login", login_handler)

    // Every route under /api carries ApiMiddleware.
    .group("/api", |r| {
        r.get("/users", user::index)
         .post("/users", user::store)
         .get("/users/{id}", user::show)
    })
    .middleware(ApiMiddleware)

    // Admin routes share auth.
    .group("/admin", |r| {
        r.get("/dashboard", admin::dashboard)
         .get("/settings", admin::settings)
    })
    .middleware(AuthMiddleware);
```

## Execution order

At runtime the chain runs outside-in:

```
Request  →  RequestId  →  globals  →  group MW  →  route MW  →  handler
                                                                  │
Response ←  RequestId  ←  globals  ←  group MW  ←  route MW  ←  handler
```

The first middleware added runs first. On the way back out, the order
reverses — `MiddlewareChain::execute` nests each layer's post-processing
inside the previous one.

If a middleware short-circuits with `Err(response)`, the chain unwinds
immediately: every layer ABOVE the short-circuit still sees the response
on the way back out, but layers BELOW (closer to the handler) do not run.

### Group middleware is flattened, not stacked

This one matters and is worth calling out. **Route-group middleware is
not a separate runtime layer.** When `GroupBuilder::try_finalize` runs,
it copies the group's middleware into each grouped route's
`(method, pattern)` middleware list. By execution time, group middleware
is indistinguishable from middleware attached directly to the route.

Two consequences:

- Runtime ordering is still correct (group middleware runs before route
  middleware because it's registered first), but **introspection cannot
  tell group from route middleware apart**.
- Middleware keyed by the matched pattern (`"/posts/{id}"`), not the
  raw path (`/posts/42`), so group middleware on parameterised routes
  fires reliably.

See `framework/src/routing/group.rs` for the flattening pass and
`framework/src/middleware/chain.rs` for the execution loop.

## Short-circuiting

Return early to block a request before it reaches the handler:

```rust
use suprnova::{async_trait, HttpResponse, Middleware, Next, Request, Response};

pub struct RequireApiKey;

#[async_trait]
impl Middleware for RequireApiKey {
    async fn handle(&self, request: Request, next: Next) -> Response {
        if request.header("X-Api-Key").is_none() {
            return Err(HttpResponse::text("Unauthorized").status(401));
        }
        next(request).await
    }
}
```

The chain collapses `Result<HttpResponse, HttpResponse>` to a single
response, so `Err(...)` is just a response with a different role. The
layers above this middleware still observe it on the way out and can
post-process it.

## Panic safety

`MiddlewareChain::execute` does NOT catch panics — a panic in any
middleware or in the handler unwinds straight out, like any other async
function. The request-path safety net lives one level up at the server
boundary in `execute_chain_safely`, which wraps the chain in
`catch_unwind` and converts a panic into a sanitised 500 with the
request id, dispatching `ErrorOccurred` for any observability listener.
See [Request Lifecycle](lifecycle.md) for the full panic-recovery flow.

This split is deliberate: standardised panic handling happens exactly
once, where the request lifecycle owns it, rather than being duplicated
inside the layer-agnostic primitive. A consumer driving a chain outside
that boundary is responsible for its own `catch_unwind`.

## Built-in middleware

A non-exhaustive map. Each ships ready to install — most need a config
struct, none need scaffolding.

| Middleware | Purpose |
|---|---|
| `RequestIdMiddleware` | Always-outermost layer; assigns a UUID per request and tags it through logs + `X-Request-Id` |
| `TimeoutMiddleware` | Bounds time-to-response; returns 503 when exceeded (see below) |
| `CorsMiddleware` | Handles CORS preflight + decorates cross-origin responses (see below) |
| `CsrfMiddleware` | Cookie-double-submit CSRF protection with configurable `OriginPolicy` |
| `RateLimitMiddleware` / `ThrottleRequestsMiddleware` | Token-bucket and sliding-window throttling; see [Rate Limiting](rate-limiting.md) |
| `SessionMiddleware` | Loads/persists the session over cookies; powers `req.session()` |
| `AuthMiddleware` / `GuestMiddleware` / `BearerTokenMiddleware` | Guard membership checks; see [Authentication](authentication.md) |
| `LoginThrottleMiddleware` / `EnsureEmailVerifiedMiddleware` / `TwoFactorChallengeMiddleware` | Auth-flow gates; see [Auth Flows](auth-flows.md) |
| `MaintenanceMiddleware` | Returns 503 when the cache or filesystem maintenance flag is set |
| `InertiaVersionMiddleware` / `EncryptHistoryMiddleware` | Inertia asset-version negotiation + history encryption |
| `IncludeMiddleware` | Per-field include sets for `#[derive(Data)]` partial reloads |

### Request timeouts

`TimeoutMiddleware` bounds how long a handler may take to *produce* a
response. A slow handler or a hung database query can otherwise hold
a connection open indefinitely; the timeout returns
`503 Service Unavailable` once the deadline is exceeded.

```rust
// src/bootstrap.rs — 30-second ceiling on every HTTP route.
use suprnova::{global_middleware, TimeoutMiddleware};

global_middleware!(TimeoutMiddleware::default()); // DEFAULT_TIMEOUT = 30s
```

```rust
// Tighten a single endpoint to 5 seconds.
use suprnova::{Router, TimeoutMiddleware};

Router::new()
    .get("/report", heavy_report_handler)
    .middleware(TimeoutMiddleware::seconds(5));
```

`TimeoutMiddleware::new(Duration)` accepts any duration;
`TimeoutMiddleware::seconds(n)` is shorthand for whole seconds.

Global middleware runs **outside** route middleware, so a global timeout
is an outer ceiling and a per-route timeout can only make a specific
route *stricter* — the shorter deadline fires first. To let one route
run longer than the global default, raise the global value or scope the
global middleware to a route group that excludes that endpoint.

Streaming responses (`HttpResponse::sse(...)`,
`HttpResponse::stream_bytes(...)`) are naturally exempt: the handler
returns immediately with a lazy body that hyper drains after the
middleware chain completes. WebSocket upgrades are also skipped
explicitly. See [Timeouts](timeout.md) for cancel-safety semantics.

### CORS

`CorsMiddleware` adds the `Access-Control-*` headers a browser needs to
let a cross-origin page read your responses, and answers the preflight
`OPTIONS` request browsers send before non-simple cross-origin calls.
Same-origin apps (the default Inertia setup) don't need it — it matters
only when a browser on a *different* origin calls your API.

CORS must be installed **globally** so preflights reach it (a preflight
never matches a route, so a per-route CORS middleware would never see
one). There is intentionally no permissive default — pick an origin
policy explicitly:

```rust
// src/bootstrap.rs
use suprnova::{global_middleware, CorsConfig, CorsMiddleware};

global_middleware!(CorsMiddleware::new(
    CorsConfig::allow_origins(["https://app.example", "https://admin.example"])
        .allow_credentials(true)
        .max_age(std::time::Duration::from_secs(600)),
));
```

`CorsConfig::any_origin()` opts into `Access-Control-Allow-Origin: *`
explicitly. Builder methods: `.methods([...])`, `.allow_headers([...])`
/ `.allow_any_headers()`, `.expose_headers([...])`, `.paths([...])`
(scope CORS to URL patterns), `.allow_origin_patterns([regex...])`,
`.skip_when(|req| bool)`, `.allow_credentials(bool)`,
`.max_age(Duration)`. Laravel-named aliases ship alongside (e.g.
`.supports_credentials`, `.allowed_methods`) so a Laravel config maps
directly.

`Access-Control-Allow-Origin: *` is invalid together with credentials —
the browser rejects it. When `.allow_credentials(true)` is set, the
middleware always echoes the specific request `Origin` instead of `*`,
so the invalid combination can never be emitted. Non-wildcard responses
also get `Vary: Origin` so shared caches stay correct. See
[CORS](cors.md).

## Pipeline — Laravel's `Illuminate\Pipeline\Pipeline`

`Pipeline` is the Suprnova analogue of Laravel's pipeline class — a
fluent builder over `MiddlewareChain` that mirrors the `send / through /
pipe / then / then_return / finally_with` shape Laravel users already
know. Useful when you want to assemble a middleware chain outside the
request lifecycle (a job, a CLI command, a one-off integration test):

```rust
use suprnova::{Pipeline, Request};

let response = Pipeline::new()
    .send(request)
    .through([AuthMiddleware, LoggingMiddleware])
    .pipe(CorsMiddleware::new(cors_config))
    .finally_with(|| tracing::info!("pipeline complete"))
    .then(|req| async move { handler(req).await })
    .await;
```

Rust-side aliases ship alongside the Laravel names: `with_request` for
`send`, `with_middleware` for `through`, `push` for `pipe`, `on_finally`
for `finally_with`, `execute` for `then`. Use whichever reads better in
your codebase.

| Pipeline method | Laravel | Rust alias | Purpose |
|---|---|---|---|
| `send(request)` | `send($passable)` | `with_request(request)` | Set the request being threaded through |
| `through(iter)` | `through($pipes)` | `with_middleware(iter)` | Replace the pipe list |
| `through_boxed(iter)` | — | — | Replace the pipe list with pre-boxed middleware |
| `pipe(M)` | `pipe($pipes)` | `push(M)` | Append a single middleware |
| `pipe_boxed(M)` | — | — | Append a pre-boxed middleware |
| `then(destination)` | `then($destination)` | `execute(destination)` | Run the chain with the destination handler |
| `then_with(req, dst)` | — | — | Override the passable inline |
| `then_return()` | `thenReturn()` | — | Run the chain, return a 204 No Content |
| `finally_with(F)` | `finally($callback)` | `on_finally(F)` | Run after the destination resolves |

## Terminable middleware — post-response hooks

Terminable middleware runs *after* the response has been sent to the
client. Use it for slow IO that doesn't need to block the response:
session persistence, audit logging, metrics flushes.

Suprnova ships this as a dedicated `Terminable` trait separate from
`Middleware`, so the request-path and termination-path stay clearly
typed. A type can implement one, the other, or both:

```rust
use suprnova::{Terminable, TerminationSnapshot, register_terminable, async_trait};

pub struct AuditLogTerminator;

#[async_trait]
impl Terminable for AuditLogTerminator {
    async fn terminate(&self, snapshot: &TerminationSnapshot) {
        tracing::info!(
            method = %snapshot.method,
            path = %snapshot.path,
            status = snapshot.status,
            "request handled",
        );
    }
}

// In bootstrap.rs
register_terminable(AuditLogTerminator);
```

The server iterates registered terminables in registration order after
every response (4xx and 5xx included) and awaits each one. Errors are
logged via `tracing::error!` and swallowed — the response has already
left the building, so there's nobody left to surface them to.

Registration is idempotent per concrete type. `registered_terminables()`,
`terminable_count()`, and `has_terminable::<T>()` provide introspection
for tests and boot-time diagnostics.

## Named aliases and groups

For consumers that prefer string-keyed middleware (Laravel's
`middlewareAliases` / `middlewareGroups`), Suprnova ships a
process-global alias + group registry:

```rust
use suprnova::middleware::{
    register_middleware_alias, register_middleware_group,
    resolve_middleware_group,
};

// Aliases are factory closures — invoked fresh per resolution, so each
// route registration produces an independent middleware instance.
register_middleware_alias("auth", || AuthMiddleware::new());
register_middleware_alias("throttle", || ThrottleRequestsMiddleware::default());

// Groups bundle aliases. Nested groups are supported.
register_middleware_group("api", ["auth".into(), "throttle".into()]);
register_middleware_group("web", ["session".into(), "auth".into()]);

// Resolve into a Vec<BoxedMiddleware> at boot or per-route.
let api_mws = resolve_middleware_group("api")?;
```

`resolve_middleware_group` returns `Err(MiddlewareResolveError)` on:

- `UnknownGroup(name)` — the named group was never registered;
- `UnknownAlias { group, missing }` — a group entry isn't a known alias;
- `UnknownNestedGroup { group, missing }` — a nested group reference
  fails to resolve;
- `CycleDetected { group }` — the group definition is recursive.

Registration of an alias or group is **last-wins** for the same name,
mirroring Laravel's reassignable kernel array.

## Middleware priority

`prepend_middleware_priority::<M>()` / `append_middleware_priority::<M>()`
register a `TypeId` in the process-global priority list — the Suprnova
analogue of Laravel's `Kernel::$middlewarePriority`. Middleware whose
type appears earlier in the list sorts to the front of the chain
regardless of registration order:

```rust
use suprnova::{append_middleware_priority};

// SessionMiddleware always runs before AuthMiddleware regardless of
// the order they were registered.
append_middleware_priority::<SessionMiddleware>();
append_middleware_priority::<AuthMiddleware>();
```

`middleware_priority()` returns a snapshot of the current `Vec<TypeId>`
for diagnostics or for an embedder that wants to drive its own sorter.

## Registry introspection

Beyond `register_global_middleware`, the registry exposes:

| Surface | Laravel | Purpose |
|---|---|---|
| `prepend_global_middleware(M)` | `prependMiddleware` | Insert at the front of the chain |
| `has_global_middleware::<M>()` | `hasMiddleware` | Whether type `M` is registered |
| `global_middleware_count()` | — | Number of globals currently registered |
| `MiddlewareRegistry::from_global()` | — | Snapshot the global registry into a per-server registry |
| `MiddlewareRegistry::prepend(M)` | — | Builder-style prepend on a registry instance |
| `MiddlewareRegistry::append_boxed(M)` | — | Append a pre-boxed middleware |
| `MiddlewareRegistry::prepend_boxed(M)` | — | Prepend a pre-boxed middleware |
| `MiddlewareRegistry::len()` / `is_empty()` | — | Builder introspection |

`MiddlewareRegistry::from_global()` snapshots the global registry at
call time. Register every global middleware BEFORE constructing the
server — a `global_middleware!` call made AFTER the server is built
does not retroactively apply, so a running server's middleware stack
cannot shift underneath it.

## File layout

A typical layout once you have a few middlewares:

```
src/
├── middleware/
│   ├── mod.rs          # mod + pub use
│   ├── auth.rs         # AuthMiddleware
│   ├── logging.rs      # LoggingMiddleware
│   └── audit.rs        # AuditLogTerminator
├── bootstrap.rs        # global_middleware! + register_terminable
├── routes.rs           # .middleware(M) per-route
└── main.rs
```

`make:middleware` keeps `src/middleware/mod.rs` in sync — it appends
the new `mod foo;` declaration and the matching `pub use foo::FooMiddleware;`
re-export when the file is generated.

## Why Suprnova diverges

Laravel registers middleware classes in `app/Http/Kernel.php` and
resolves them through the container, which performs reflection on
constructor type-hints to inject dependencies. PHP's request-per-process
model means the kernel is rebuilt every request, so the cost of
reflective resolution is paid once per request and disappears between
requests.

Suprnova's process model is one binary serving many concurrent requests
across many threads. Building a fresh chain per request would force a
synchronisation point on the global middleware list and re-allocate
`Arc<dyn Middleware>` for every layer on every request. Instead:

- Global middleware is registered into a `OnceLock<RwLock<Vec<...>>>`
  at boot, keyed by `TypeId` for idempotent registration.
- `MiddlewareRegistry::from_global()` snapshots the global list once at
  server construction; the per-request chain reuses that snapshot.
- The chain itself is composed by nesting `Arc<dyn Fn>` closures, so
  per-request work is one `Arc::clone` per layer rather than a fresh
  allocation.

The user-facing surface — `handle(request, next)`, the `global_middleware!`
macro, named aliases, priority lists, terminable hooks — is the same
one a Laravel developer reaches for. The machinery underneath swaps
PHP's per-request rebuild for a Rust-shaped snapshot-at-boot model so
the framework can serve concurrent requests without contending on the
registry.

## Next

- [Request Lifecycle](lifecycle.md) — where the chain runs and how
  panics are caught at the server boundary
- [Error Model](error-model.md) — what `Result<HttpResponse, HttpResponse>`
  actually means and how short-circuits collapse
- [Timeouts](timeout.md) — `TimeoutMiddleware` cancel-safety in detail
- [CORS](cors.md) — preflight handling, origin patterns, path scoping
- [Rate Limiting](rate-limiting.md) — `RateLimitMiddleware` /
  `ThrottleRequestsMiddleware` and `BackendErrorPolicy`
- [Routing](routing.md) — what `routes!`, `Router`, and `group(...)`
  expand into
