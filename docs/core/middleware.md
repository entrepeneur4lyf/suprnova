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

## Practical Examples

### CORS Middleware

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

## Summary

| Feature | Usage |
|---------|-------|
| Create middleware | Implement `Middleware` trait |
| Global middleware | `global_middleware!(MyMiddleware)` in `bootstrap.rs` |
| Route middleware | `.middleware(MyMiddleware)` on route definition |
| Group middleware | `.middleware(MyMiddleware)` on route group |
| Short-circuit | Return `Err(HttpResponse::...)` without calling `next()` |
| Continue chain | Call `next(request).await` |
| Request timeout | `global_middleware!(TimeoutMiddleware::default())` (global ceiling) or `.middleware(TimeoutMiddleware::seconds(n))` (per route) |
