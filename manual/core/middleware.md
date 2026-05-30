---
title: 'Middleware'
description: 'Intercept and process HTTP requests with suprnova middleware'
icon: 'filter'
---

suprnova provides a powerful middleware system for intercepting and processing HTTP requests before they reach your route handlers. Middleware can inspect, modify, or short-circuit requests, and also post-process responses.

## Generating Middleware

The fastest way to create a new middleware is using the suprnova CLI:

```bash
suprnova make:middleware Auth
```

This command will:
1. Create `src/middleware/auth.rs` with a middleware stub
2. Update `src/middleware/mod.rs` to export the new middleware

```bash Examples
# Creates AuthMiddleware in src/middleware/auth.rs
suprnova make:middleware Auth

# Creates RateLimitMiddleware in src/middleware/rate_limit.rs
suprnova make:middleware RateLimit

# You can also include "Middleware" suffix (same result)
suprnova make:middleware CorsMiddleware
```

```rust Generated File
//! Auth middleware

use suprnova::{async_trait, Middleware, Next, Request, Response};

/// Auth middleware
pub struct AuthMiddleware;

#[async_trait]
impl Middleware for AuthMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        // TODO: Implement middleware logic
        next(request).await
    }
}
```

## Overview

Middleware sits between the incoming request and your route handlers, allowing you to:

- Authenticate and authorize requests
- Log requests and responses
- Add CORS headers
- Rate limit requests
- Transform request/response data
- And much more

## Creating Middleware

To create middleware, define a struct and implement the `Middleware` trait:

```rust
use suprnova::{async_trait, HttpResponse, Middleware, Next, Request, Response};

pub struct LoggingMiddleware;

#[async_trait]
impl Middleware for LoggingMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        // Pre-processing: runs before the route handler
        println!("--> {} {}", request.method(), request.path());

        // Call the next middleware or route handler
        let response = next(request).await;

        // Post-processing: runs after the route handler
        println!("<-- Request complete");

        response
    }
}
```

### The `handle` Method

The `handle` method receives:
- `request`: The incoming HTTP request
- `next`: A function to call the next middleware in the chain (or the route handler)

You can:
- **Continue the chain**: Call `next(request).await` to pass control to the next middleware
- **Short-circuit**: Return a response early without calling `next()`
- **Modify the request**: Transform the request before calling `next()`
- **Modify the response**: Transform the response after calling `next()`

### Short-Circuiting Requests

Return early to block a request from reaching the route handler:

```rust
use suprnova::{async_trait, HttpResponse, Middleware, Next, Request, Response};

pub struct AuthMiddleware;

#[async_trait]
impl Middleware for AuthMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        // Check for Authorization header
        if request.header("Authorization").is_none() {
            // Short-circuit: return 401 without calling the route handler
            return Err(HttpResponse::text("Unauthorized").status(401));
        }

        // Continue to the route handler
        next(request).await
    }
}
```

## Registering Middleware

suprnova supports three levels of middleware:

### 1. Global Middleware

Global middleware runs on **every request**. Register it in `bootstrap.rs` using the `global_middleware!` macro:

```rust
// src/bootstrap.rs
use suprnova::{global_middleware, DB};
use crate::middleware;

pub async fn register() {
    // Initialize database
    DB::init().await.expect("Failed to connect to database");

    // Global middleware runs on every request (in registration order)
    global_middleware!(middleware::LoggingMiddleware);
    global_middleware!(middleware::CorsMiddleware);
}
```

### 2. Route Middleware

Apply middleware to individual routes using the `.middleware()` method:

```rust
// src/routes.rs
use suprnova::{routes, get, post};
use crate::controllers;
use crate::middleware::AuthMiddleware;

routes! {
    get!("/", controllers::home::index).name("home"),
    get!("/public", controllers::home::public),

    // Protected route - requires AuthMiddleware
    get!("/protected", controllers::dashboard::index).middleware(AuthMiddleware),
    get!("/admin", controllers::admin::index).middleware(AuthMiddleware),
}
```

### 3. Route Group Middleware

Apply middleware to a group of routes that share a common prefix:

```rust
use suprnova::Router;
use crate::middleware::{AuthMiddleware, ApiMiddleware};

Router::new()
    // Public routes (no middleware)
    .get("/", home_handler)
    .get("/login", login_handler)

    // API routes with shared middleware
    .group("/api", |r| {
        r.get("/users", list_users)
         .post("/users", create_user)
         .get("/users/{id}", show_user)
    })
    .middleware(ApiMiddleware)

    // Admin routes with auth middleware
    .group("/admin", |r| {
        r.get("/dashboard", admin_dashboard)
         .get("/settings", admin_settings)
    })
    .middleware(AuthMiddleware)
```

## Middleware Execution Order

Middleware executes in the following order:

1. **Global middleware** (in registration order)
2. **Route group middleware**
3. **Route-level middleware**
4. **Route handler**

For responses, the order is reversed (post-processing happens in reverse order).

```
Request -> Global MW -> Group MW -> Route MW -> Handler
                                                  |
Response <- Global MW <- Group MW <- Route MW <- Handler
```

## Request Timeouts

`TimeoutMiddleware` is a built-in middleware that bounds how long a handler may take to **produce a response**. A slow handler or a hung database query can otherwise hold a connection open indefinitely; the timeout returns `503 Service Unavailable` once the deadline is exceeded.

### Installing the timeout

Install it globally for a process-wide ceiling (the default is 30 seconds), or per-route to tighten a specific endpoint.

```rust
// src/bootstrap.rs — a 30s ceiling on every HTTP route
use suprnova::{global_middleware, TimeoutMiddleware};

pub async fn register() {
    global_middleware!(TimeoutMiddleware::default()); // 30 seconds
    // ... other global middleware
}
```

```rust
// Tighten a single endpoint to 5 seconds
use suprnova::{Router, TimeoutMiddleware};

Router::new()
    .get("/report", heavy_report_handler)
    .middleware(TimeoutMiddleware::seconds(5));
```

`TimeoutMiddleware::new(duration)` accepts any `Duration`; `TimeoutMiddleware::seconds(n)` is shorthand for whole seconds.

### Global is a ceiling; per-route tightens

Global middleware runs **outside** route middleware, so a global timeout is an outer ceiling and a per-route timeout can only make a route *stricter* — the shorter deadline fires first. To let one route run *longer* than the global default, either raise the global value or scope the global middleware to a route group that excludes that endpoint.

### Streaming responses and WebSockets are exempt

The deadline bounds *time-to-response* — the moment your handler returns its `HttpResponse` — not how long the body streams afterwards:

- **SSE and streaming responses** (`HttpResponse::sse(...)`, `HttpResponse::stream_bytes(...)`) are naturally exempt. The handler returns immediately with a lazy body that the server drains afterwards, so a long-lived event stream is never cut off by the timeout.
- **WebSocket upgrades** (requests carrying `Upgrade: websocket`) are skipped explicitly and never armed.

### Cancel safety

When the deadline elapses the in-flight handler is **cancelled** — its future is dropped at the current `.await` point. Anything held across that point is released by its `Drop` impl, so open transactions roll back and locks release. Work you moved off the request with `tokio::spawn` is detached and will **not** be cancelled, so keep handlers cancel-safe.

## Cross-Origin Resource Sharing (CORS)

`CorsMiddleware` is a built-in middleware that adds the `Access-Control-*` headers a browser needs to let a cross-origin page read your responses, and answers the preflight `OPTIONS` request browsers send before non-simple cross-origin calls. Same-origin apps (the default Inertia setup) don't need it — it matters once a browser on a *different* origin calls your API (public API, separate SPA host, mobile webview).

### Installing CORS

CORS must be installed **globally** so preflight requests reach it (see below). There is intentionally no permissive default — choose an origin policy explicitly:

```rust
// src/bootstrap.rs
use suprnova::{global_middleware, CorsConfig, CorsMiddleware};

pub async fn register() {
    global_middleware!(CorsMiddleware::new(
        CorsConfig::allow_origins(["https://app.example", "https://admin.example"])
            .allow_credentials(true)
            .max_age(std::time::Duration::from_secs(600)),
    ));
}
```

`CorsConfig::any_origin()` opts into `Access-Control-Allow-Origin: *` explicitly. Builder methods: `.methods([...])`, `.allow_headers([...])` / `.allow_any_headers()`, `.expose_headers([...])`, `.allow_credentials(bool)`, `.max_age(Duration)`.

### Why CORS must be global

A preflight is an `OPTIONS` request carrying `Access-Control-Request-Method`, and the router has no `OPTIONS` routes — so a preflight never matches a route. suprnova still runs the global middleware chain for unmatched requests (terminating in a 404), so a globally-installed `CorsMiddleware` intercepts the preflight and answers it with `204` before the 404 is produced. A *per-route* CORS middleware would never see preflights.

### Credentials and `*`

`Access-Control-Allow-Origin: *` is invalid together with credentials — the browser rejects it. When `.allow_credentials(true)` is set, the middleware always echoes the specific request `Origin` (and reflects requested headers) instead of `*`, so the invalid combination can't be emitted. Non-wildcard responses also get `Vary: Origin` so shared caches stay correct.

## Practical Examples

### CORS Middleware

> **Note:** This is a hand-rolled illustration. For production, prefer the built-in `suprnova::CorsMiddleware` (see *Cross-Origin Resource Sharing (CORS)* above) — it handles preflight `OPTIONS`, credentials, and `Vary` correctly.

```rust
use suprnova::{async_trait, Middleware, Next, Request, Response, HttpResponse};

pub struct CorsMiddleware;

#[async_trait]
impl Middleware for CorsMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let response = next(request).await;

        // Add CORS headers to the response
        match response {
            Ok(mut res) => {
                res = res
                    .header("Access-Control-Allow-Origin", "*")
                    .header("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE")
                    .header("Access-Control-Allow-Headers", "Content-Type, Authorization");
                Ok(res)
            }
            Err(mut res) => {
                res = res
                    .header("Access-Control-Allow-Origin", "*");
                Err(res)
            }
        }
    }
}
```

### Rate Limiting Middleware

```rust
use suprnova::{async_trait, Middleware, Next, Request, Response, HttpResponse};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub struct RateLimitMiddleware {
    requests: Arc<AtomicUsize>,
    max_requests: usize,
}

impl RateLimitMiddleware {
    pub fn new(max_requests: usize) -> Self {
        Self {
            requests: Arc::new(AtomicUsize::new(0)),
            max_requests,
        }
    }
}

#[async_trait]
impl Middleware for RateLimitMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let count = self.requests.fetch_add(1, Ordering::SeqCst);

        if count >= self.max_requests {
            return Err(HttpResponse::text("Too Many Requests").status(429));
        }

        next(request).await
    }
}
```

### Request Timing Middleware

```rust
use suprnova::{async_trait, Middleware, Next, Request, Response};
use std::time::Instant;

pub struct TimingMiddleware;

#[async_trait]
impl Middleware for TimingMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let start = Instant::now();
        let path = request.path().to_string();

        let response = next(request).await;

        let duration = start.elapsed();
        println!("{} completed in {:?}", path, duration);

        response
    }
}
```

## File Organization

The recommended file structure for middleware:

```
src/
├── middleware/
│   ├── mod.rs          # Re-export all middleware
│   ├── auth.rs         # Authentication middleware
│   ├── logging.rs      # Logging middleware
│   └── cors.rs         # CORS middleware
├── bootstrap.rs        # Register global middleware
├── routes.rs           # Apply route-level middleware
└── main.rs
```

**src/middleware/mod.rs:**
```rust
mod auth;
mod logging;
mod cors;

pub use auth::AuthMiddleware;
pub use logging::LoggingMiddleware;
pub use cors::CorsMiddleware;
```

## Pipeline (Laravel `Illuminate\Pipeline\Pipeline`)

`Pipeline` is the Suprnova analogue of Laravel's pipeline class. It's a fluent
builder that wraps `MiddlewareChain`, mirroring the `send / through / pipe /
then / then_return / finally_with` shape Laravel users already know.

```rust
use suprnova::{Pipeline, HttpResponse};

let response = Pipeline::new()
    .send(request)
    .through([AuthMiddleware, LoggingMiddleware])
    .pipe(CorsMiddleware)
    .finally_with(|| tracing::info!("pipeline complete"))
    .then(|req| async move { handler(req).await })
    .await;
```

Dual-API aliases live on the Rust side: `with_request` for `send`,
`with_middleware` for `through`, `push` for `pipe`, `on_finally` for
`finally_with`, and `execute` for `then`. Use whichever reads more naturally
in your codebase — the Laravel names ship as first-class so a Laravel
developer can write the same code they already know.

| Pipeline method | Laravel | Rust-side alias | Purpose |
|---|---|---|---|
| `send(request)` | `send($passable)` | `with_request(request)` | Set the request being threaded through |
| `through(iter)` | `through($pipes)` | `with_middleware(iter)` | Replace the pipe list |
| `through_boxed(iter)` | — | — | Replace the pipe list using pre-boxed middleware |
| `pipe(M)` | `pipe($pipes)` | `push(M)` | Append a single middleware |
| `pipe_boxed(M)` | — | — | Append a pre-boxed middleware |
| `then(destination)` | `then($destination)` | `execute(destination)` | Run the chain with the destination handler |
| `then_with(req, dst)` | — | — | Override the passable inline |
| `then_return()` | `thenReturn()` | — | Run the chain, return a 204 No Content |
| `finally_with(F)` | `finally($callback)` | `on_finally(F)` | Run after the destination resolves |

## Terminable middleware (post-response hooks)

Terminable middleware runs *after* the response has been sent to the client.
Use it for slow IO that doesn't need to block the response — session
persistence, audit logging, metrics flushes. Suprnova ships this as a
dedicated `Terminable` trait so the request-path and termination-path are
clearly typed and a middleware can opt into one, the other, or both.

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

Termination is dispatched on the background runtime by the server after every
response — including 4xx and 5xx — so hooks never block the wire. Registration
is idempotent per concrete type, matching the global middleware contract.
`registered_terminables()`, `terminable_count()`, and `has_terminable::<T>()`
provide introspection for tests and boot-time diagnostics.

## Named middleware aliases and groups

For consumers that prefer string-keyed middleware (Laravel's
`middlewareAliases` / `middlewareGroups`), Suprnova ships a process-global
alias and group registry:

```rust
use suprnova::middleware::{
    register_middleware_alias, register_middleware_group,
    resolve_middleware_group,
};

// Aliases are factory closures — invoked fresh per resolution so each
// route registration produces an independent middleware instance.
register_middleware_alias("auth", || AuthMiddleware);
register_middleware_alias("throttle", || ThrottleMiddleware::default());
register_middleware_alias("cors", || CorsMiddleware::default());

// Groups bundle aliases (and other groups — nesting is supported).
register_middleware_group("api", ["auth".into(), "throttle".into()]);
register_middleware_group("web", ["cors".into(), "auth".into()]);

// Resolve into a Vec<BoxedMiddleware> at boot or per-route.
let api_mws = resolve_middleware_group("api")?;
```

`resolve_middleware_group` returns `Err(MiddlewareResolveError)` on:

- `UnknownGroup(name)` — the named group was never registered;
- `UnknownAlias { group, missing }` — the group entry isn't a known alias;
- `UnknownNestedGroup { group, missing }` — a nested group reference fails to resolve;
- `CycleDetected { group }` — the group definition is recursive.

Registration of an alias or group is **last-wins** for the same name, mirroring
Laravel's reassignable kernel array.

## Middleware priority

`prepend_middleware_priority::<M>()` / `append_middleware_priority::<M>()`
register a `TypeId` in the process-global priority list. The list is the
Suprnova analogue of Laravel's `Kernel::$middlewarePriority`: middleware whose
type appears earlier in the list sorts to the front of the chain regardless
of registration order.

```rust
use suprnova::{append_middleware_priority, prepend_middleware_priority};

// SessionMiddleware always runs before AuthMiddleware.
append_middleware_priority::<SessionMiddleware>();
append_middleware_priority::<AuthMiddleware>();
```

`middleware_priority()` returns a snapshot of the current `Vec<TypeId>`
for diagnostics or for an embedder that wants to drive its own sorter.

## Global middleware introspection

Beyond `register_global_middleware`, the registry exposes:

| Surface | Laravel | Purpose |
|---|---|---|
| `prepend_global_middleware(M)` | `prependMiddleware` | Insert at the front of the chain |
| `has_global_middleware::<M>()` | `hasMiddleware` | Whether type `M` is registered |
| `global_middleware_count()` | — | Number of globals currently registered |
| `MiddlewareRegistry::prepend(M)` | — | Builder-style prepend on a registry instance |
| `MiddlewareRegistry::append_boxed(M)` | — | Append a pre-boxed middleware |
| `MiddlewareRegistry::prepend_boxed(M)` | — | Prepend a pre-boxed middleware |
| `MiddlewareRegistry::len()` / `is_empty()` | — | Builder introspection |

## Summary

| Feature | Usage |
|---------|-------|
| Create middleware | Implement `Middleware` trait |
| Global middleware | `global_middleware!(MyMiddleware)` in `bootstrap.rs` |
| Prepend global | `prepend_global_middleware(MyMiddleware)` |
| Route middleware | `.middleware(MyMiddleware)` on route definition |
| Group middleware | `.middleware(MyMiddleware)` on route group |
| Pipeline builder | `Pipeline::new().send(req).through([...]).then(dst).await` |
| Named aliases | `register_middleware_alias("auth", \|\| AuthMw)` |
| Named groups | `register_middleware_group("api", ["auth", "throttle"])` |
| Priority ordering | `append_middleware_priority::<MyMw>()` |
| Terminable hooks | `register_terminable(MyTerminator)` — runs post-response |
| Short-circuit | Return `Err(HttpResponse::...)` without calling `next()` |
| Continue chain | Call `next(request).await` |
| Request timeout | `global_middleware!(TimeoutMiddleware::default())` (global ceiling) or `.middleware(TimeoutMiddleware::seconds(n))` (per route) |
| CORS | `global_middleware!(CorsMiddleware::new(CorsConfig::allow_origins([...])))` — must be global so preflight is handled |
