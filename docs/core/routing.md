---
title: 'Routing'
description: 'Define and organize your application routes with suprnova'
icon: 'route'
---

suprnova provides a clean, Laravel-inspired routing system that lets you define routes declaratively using the `routes!` macro. Routes map URLs to handler functions (controllers), support dynamic parameters, named routes for URL generation, and per-route middleware.

## Defining Routes

Routes are defined in `src/routes.rs` using the `routes!` macro. Each route specifies an HTTP method, a path, and a handler function:

```rust
// src/routes.rs
use suprnova::{get, post, put, delete, routes};
use crate::controllers;

routes! {
    get!("/", controllers::home::index).name("home"),
    get!("/users", controllers::user::index).name("users.index"),
    get!("/users/{id}", controllers::user::show).name("users.show"),
    post!("/users", controllers::user::store).name("users.store"),
    put!("/users/{id}", controllers::user::update).name("users.update"),
    delete!("/users/{id}", controllers::user::destroy).name("users.destroy"),
}
```

The `routes!` macro automatically generates a `register()` function that returns a configured `Router`.

## HTTP Methods

suprnova provides macros for all standard HTTP methods:

| Method | Macro | Usage |
|--------|-------|-------|
| GET | `get!(path, handler)` | Retrieve resources |
| POST | `post!(path, handler)` | Create resources |
| PUT | `put!(path, handler)` | Update resources |
| DELETE | `delete!(path, handler)` | Delete resources |

```rust
routes! {
    get!("/articles", controllers::article::index),
    post!("/articles", controllers::article::store),
    put!("/articles/{id}", controllers::article::update),
    delete!("/articles/{id}", controllers::article::destroy),
}
```

## Route Parameters

Dynamic segments in your URLs are defined using curly braces `{param}`. suprnova also supports Express/Rails-style colon syntax `:param` which is automatically converted. Both syntaxes are fully supported:

```rust
// Both of these are equivalent:
get!("/users/{id}", controllers::user::show),  // matchit-native syntax
get!("/users/:id", controllers::user::show),   // Express/Rails-style syntax

// Multiple parameters work with either syntax
get!("/posts/{post_id}/comments/{comment_id}", controllers::comment::show),
get!("/posts/:post_id/comments/:comment_id", controllers::comment::show),
```

> **Note:**
>
> Choose whichever syntax you prefer - suprnova automatically converts `:param` to `{param}` internally for compatibility with the underlying router.


Access parameters in your controller using `request.param()`:

```rust
// src/controllers/user.rs
use suprnova::{Request, Response, HttpResponse};

pub async fn show(request: Request) -> Response {
    // Extract the 'id' parameter from the URL
    let user_id = request.param("id").unwrap_or("0");

    Ok(HttpResponse::text(format!("User ID: {}", user_id)))
}
```

For nested parameters:

```rust
pub async fn show(request: Request) -> Response {
    let post_id = request.param("post_id").unwrap_or("0");
    let comment_id = request.param("comment_id").unwrap_or("0");

    Ok(HttpResponse::text(format!("Post: {}, Comment: {}", post_id, comment_id)))
}
```

## Route Model Binding

Route model binding automatically resolves database models from route parameters. When you use a Model type as a handler parameter, suprnova automatically fetches the model from the database using the route parameter value.

### Basic Usage

Simply use the Model type as a handler parameter with the `#[handler]` attribute:

```rust
// src/controllers/user.rs
use suprnova::{handler, json_response, Response};
use crate::models::user;

// Route: GET /users/{user}
#[handler]
pub async fn show(user: user::Model) -> Response {
    json_response!({ "name": user.name, "email": user.email })
}
```

The parameter name (`user`) matches the route parameter placeholder (`{user}`). suprnova will:
1. Extract the value from the `{user}` route parameter
2. Parse it as the primary key type (e.g., `i32`, `String`, `UUID`)
3. Fetch the model from the database
4. Return 404 Not Found if the model doesn't exist
5. Return 400 Bad Request if the parameter can't be parsed

### Route Definition

Define your route with a matching parameter name:

```rust
// src/routes.rs
use suprnova::{get, put, delete, routes};
use crate::controllers;

routes! {
    get!("/users/{user}", controllers::user::show).name("users.show"),
    put!("/users/{user}", controllers::user::update).name("users.update"),
    delete!("/users/{user}", controllers::user::destroy).name("users.destroy"),
}
```

### Multiple Models

You can bind multiple models in a single handler:

```rust
// Route: GET /posts/{post}/comments/{comment}
#[handler]
pub async fn show(post: post::Model, comment: comment::Model) -> Response {
    json_response!({
        "post_title": post.title,
        "comment_body": comment.body
    })
}
```

### Mixed Parameters

Combine model binding with primitive parameters and form requests:

```rust
// Route: PUT /users/{user}
#[handler]
pub async fn update(user: user::Model, form: UpdateUserRequest) -> Response {
    // user is automatically fetched from the database
    // form contains validated request data
    json_response!({ "updated": user.id })
}
```

### Requirements

Route model binding works automatically for any model whose Entity implements `suprnova::database::Model`:

```rust
// src/models/user.rs
pub use super::entities::user::*;
use sea_orm::entity::prelude::*;

impl ActiveModelBehavior for ActiveModel {}

// These trait implementations enable route model binding
impl suprnova::database::Model for Entity {}
impl suprnova::database::ModelMut for Entity {}
```

> **Note:**
>
> Route model binding supports any primary key type that implements `FromStr`, including `i32`, `i64`, `String`, and `uuid::Uuid`.


### Opting Out

If you don't want automatic model binding for a particular handler, simply don't use the Model type as a parameter. Instead, extract the ID and query manually:

```rust
#[handler]
pub async fn show(id: i32) -> Response {
    // Manual lookup with custom logic
    let user = user::Entity::find_by_id(id)
        .one(DB::connection()?.inner())
        .await?;

    match user {
        Some(u) => json_response!({ "user": u }),
        None => json_response!({ "error": "User not found" }),
    }
}
```

## Named Routes

Named routes allow you to generate URLs without hardcoding paths. Use `.name()` to assign a name to a route:

```rust
routes! {
    get!("/", controllers::home::index).name("home"),
    get!("/users", controllers::user::index).name("users.index"),
    get!("/users/{id}", controllers::user::show).name("users.show"),
    post!("/users", controllers::user::store).name("users.store"),
}
```

### Naming Conventions

Follow Laravel-style naming conventions for consistency:

| Route | Name |
|-------|------|
| `GET /users` | `users.index` |
| `GET /users/{id}` | `users.show` |
| `POST /users` | `users.store` |
| `PUT /users/{id}` | `users.update` |
| `DELETE /users/{id}` | `users.destroy` |

### URL Generation

Generate URLs from named routes using the `route()` function:

```rust
use suprnova::route;

// Simple route without parameters
let home_url = route("home", &[]);
// Returns: Some("/")

// Route with parameters
let user_url = route("users.show", &[("id", "123")]);
// Returns: Some("/users/123")

// Multiple parameters
let comment_url = route("comments.show", &[("post_id", "5"), ("comment_id", "42")]);
// Returns: Some("/posts/5/comments/42")
```

This is especially useful for redirects:

```rust
use suprnova::{route, HttpResponse, Response};

pub async fn store(request: Request) -> Response {
    // Create user...

    // Redirect to the user's profile
    let url = route("users.show", &[("id", "123")]).unwrap();
    HttpResponse::redirect(&url)
}
```

## Route Middleware

Apply middleware to specific routes using `.middleware()`:

```rust
use suprnova::{get, post, routes};
use crate::controllers;
use crate::middleware::AuthMiddleware;

routes! {
    // Public routes
    get!("/", controllers::home::index).name("home"),
    get!("/login", controllers::auth::login_form).name("login"),
    post!("/login", controllers::auth::login).name("login.submit"),

    // Protected routes
    get!("/dashboard", controllers::dashboard::index)
        .name("dashboard")
        .middleware(AuthMiddleware),
    get!("/profile", controllers::user::profile)
        .name("profile")
        .middleware(AuthMiddleware),
}
```

You can chain multiple middleware on a single route:

```rust
get!("/admin", controllers::admin::index)
    .middleware(AuthMiddleware)
    .middleware(AdminMiddleware),
```

> **Note:**
>
> For more details on creating middleware, see the [Middleware documentation](/core/middleware).


## Route Groups

Group related routes that share a common prefix and/or middleware using the `group!` macro inside `routes!`:

```rust
use suprnova::{get, post, group, routes};
use crate::controllers;
use crate::middleware::{AuthMiddleware, ApiMiddleware};

routes! {
    // Public routes
    get!("/", controllers::home::index).name("home"),
    get!("/login", controllers::auth::login_form).name("login"),

    // API routes with shared prefix
    group!("/api", {
        get!("/users", controllers::api::user::index).name("api.users.index"),   // GET /api/users
        post!("/users", controllers::api::user::store).name("api.users.store"),  // POST /api/users
        get!("/users/{id}", controllers::api::user::show).name("api.users.show"), // GET /api/users/{id}
    }).middleware(ApiMiddleware),

    // Admin routes with auth middleware
    group!("/admin", {
        get!("/dashboard", controllers::admin::dashboard).name("admin.dashboard"), // GET /admin/dashboard
        get!("/settings", controllers::admin::settings).name("admin.settings"),   // GET /admin/settings
    }).middleware(AuthMiddleware),
}
```

### Group Syntax

The `group!` macro takes a prefix and a block of routes:

```rust
group!("/prefix", {
    get!("/path", handler).name("route.name"),
    post!("/path", handler),
    // ... more routes
})
```

### Group with Middleware

Apply middleware to all routes in a group using `.middleware()`:

```rust
group!("/protected", {
    get!("/", controllers::dashboard::index).name("dashboard"),
    get!("/settings", controllers::settings::index).name("settings"),
}).middleware(AuthMiddleware)
```

### Multiple Middleware

Chain multiple middleware on a group:

```rust
group!("/api/v2", {
    get!("/users", controllers::api::user::index),
    post!("/users", controllers::api::user::store),
}).middleware(AuthMiddleware).middleware(RateLimitMiddleware)
```

### Groups without Middleware

Groups can be used purely for URL prefixing without any middleware:

```rust
group!("/users", {
    get!("/", controllers::user::index).name("users.index"),       // GET /users
    get!("/{id}", controllers::user::show).name("users.show"),    // GET /users/{id}
    post!("/", controllers::user::store).name("users.store"),      // POST /users
}),
```

### Nested Groups

Groups can be nested arbitrarily deep. Nested groups inherit middleware from their parent groups, and prefixes are concatenated:

```rust
use suprnova::{get, post, group, routes};
use crate::controllers;
use crate::middleware::{AuthMiddleware, AdminMiddleware};

routes! {
    group!("/api", {
        get!("/health", controllers::api::health),          // GET /api/health

        group!("/v1", {
            get!("/users", controllers::api::v1::users),    // GET /api/v1/users

            group!("/admin", {
                get!("/stats", controllers::admin::stats),  // GET /api/v1/admin/stats
            }).middleware(AdminMiddleware),
        }),
    }).middleware(AuthMiddleware),  // Applies to ALL routes including nested groups
}
```

In this example:
- `/api/health` has `AuthMiddleware`
- `/api/v1/users` has `AuthMiddleware`
- `/api/v1/admin/stats` has both `AuthMiddleware` AND `AdminMiddleware`

### Middleware Inheritance

When groups are nested, middleware is inherited from parent to child. The execution order is:
1. Parent group middleware (outermost)
2. Child group middleware
3. Route-specific middleware (innermost)

```rust
group!("/outer", {
    group!("/inner", {
        get!("/route", handler).middleware(RouteMiddleware),
    }).middleware(InnerMiddleware),
}).middleware(OuterMiddleware)
```

For the route `/outer/inner/route`, middleware executes in order: `OuterMiddleware` → `InnerMiddleware` → `RouteMiddleware`.

### Group Features

- **Prefix**: All routes in the group have the prefix prepended to their paths
- **Named Routes**: Routes inside groups can have names for URL generation
- **Middleware**: Apply middleware to all routes in the group at once
- **Chaining**: Multiple middleware can be chained on a group
- **Nesting**: Groups can be nested to any depth with inherited middleware

## Fallible Registration

Route registration runs once at boot, so a duplicate or malformed route is
treated as a programmer error: the plain helpers (`get`, `post`, `put`,
`delete`, `ws`, `.name(...)`, and the `From<GroupBuilder>` / `.into()`
conversion) **panic** to fail loudly at startup. That is the right default
for routes declared in source.

When route patterns or names come from a source you don't control at compile
time — dynamic configuration, a plugin system, a test that deliberately
registers conflicting routes — use the `try_*` siblings instead. They return
`Result<_, FrameworkError>` (the error names the offending method and path,
or the conflicting name) rather than panicking:

| Panicking | Fallible sibling | Returns |
|-----------|------------------|---------|
| `Router::get` / `post` / `put` / `delete` | `try_get` / `try_post` / `try_put` / `try_delete` | `Result<RouteBuilder, FrameworkError>` |
| `RouteBuilder::get` / `post` / `put` / `delete` | same `try_*` names | `Result<RouteBuilder, FrameworkError>` |
| `Router::ws` (and every `ws_*` variant) | `try_ws` (and every `try_ws_*`) | `Result<Router, FrameworkError>` |
| `RouteBuilder::name` | `try_name` | `Result<Router, FrameworkError>` |
| `GroupBuilder` → `Router` via `.into()` | `GroupBuilder::try_finalize` | `Result<Router, FrameworkError>` |

```rust
use suprnova::{FrameworkError, Router};

// `path` comes from dynamic config, so a malformed or duplicate pattern is
// a recoverable error rather than a startup panic. `try_get` yields a
// RouteBuilder on success, which `.into()` turns back into a Router.
fn register_dynamic(router: Router, path: &str) -> Result<Router, FrameworkError> {
    Ok(router.try_get(path, health)?.into())
}
```

A duplicate group route is recoverable the same way — because `From` cannot
be fallible, the fallible counterpart of `.into()` is the inherent
`try_finalize` method:

```rust
let router: Router = Router::new()
    .group("/api", |r| r.get("/users", list).post("/users", create))
    .try_finalize()?; // Err(FrameworkError) instead of a panic on a conflict
```

The panicking helpers remain as ergonomic escape hatches — the `try_*`
siblings are purely additive.

## Fallback Route

The `fallback!` macro allows you to define a custom handler that is called when no other routes match the request. This is useful for implementing custom 404 pages or catch-all handlers.

### Basic Usage

```rust
use suprnova::{get, fallback, routes};
use crate::controllers;

routes! {
    get!("/", controllers::home::index).name("home"),
    get!("/users", controllers::user::index).name("users.index"),

    // Custom 404 handler - called when no routes match
    fallback!(controllers::fallback::not_found),
}
```

### Fallback Controller Example

Create a controller to handle unmatched routes:

```rust
// src/controllers/fallback.rs
use suprnova::{Request, Response, HttpResponse};

pub async fn not_found(request: Request) -> Response {
    // You can access the original request path
    let path = request.path();

    Ok(HttpResponse::text(format!("Page not found: {}", path)).status(404))
}
```

### Fallback with Middleware

The fallback route supports middleware chaining, just like regular routes:

```rust
use suprnova::{get, fallback, routes};
use crate::controllers;
use crate::middleware::LoggingMiddleware;

routes! {
    get!("/", controllers::home::index),

    // Log all 404 requests
    fallback!(controllers::fallback::not_found).middleware(LoggingMiddleware),
}
```

### Fallback with Inertia

You can also return Inertia responses for SPA-style 404 pages:

```rust
// src/controllers/fallback.rs
use suprnova::{Request, Response, inertia_response, InertiaProps};
use serde::Serialize;

#[derive(InertiaProps, Serialize)]
pub struct NotFoundProps {
    pub requested_path: String,
}

pub async fn not_found(request: Request) -> Response {
    let path = request.path().to_string();

    inertia_response!("Error/NotFound", NotFoundProps {
        requested_path: path,
    })
}
```

> **Note:**
>
> If no fallback route is defined, suprnova returns a default plain-text "404 Not Found" response.


## File Organization

The standard file structure for routing in a suprnova application:

```
src/
├── routes.rs           # Route definitions
├── controllers/
│   ├── mod.rs         # Re-export all controllers
│   ├── home.rs        # Home controller
│   ├── user.rs        # User controller
│   └── api/
│       ├── mod.rs     # API controllers module
│       └── user.rs    # API user controller
├── middleware/
│   ├── mod.rs         # Re-export all middleware
│   └── auth.rs        # Auth middleware
└── main.rs
```

**src/routes.rs:**
```rust
use suprnova::{get, post, put, delete, group, routes};
use crate::controllers;
use crate::middleware::AuthMiddleware;

routes! {
    get!("/", controllers::home::index).name("home"),

    // User routes with shared prefix
    group!("/users", {
        get!("/", controllers::user::index).name("users.index"),
        get!("/{id}", controllers::user::show).name("users.show"),
        post!("/", controllers::user::store).name("users.store"),
    }),

    // Protected routes with middleware
    group!("/dashboard", {
        get!("/", controllers::home::dashboard).name("dashboard"),
        get!("/settings", controllers::settings::index).name("dashboard.settings"),
    }).middleware(AuthMiddleware),
}
```

## Summary

| Feature | Syntax | Description |
|---------|--------|-------------|
| Define routes | `routes! { ... }` | Macro for clean route definitions |
| GET route | `get!(path, handler)` | Handle GET requests |
| POST route | `post!(path, handler)` | Handle POST requests |
| PUT route | `put!(path, handler)` | Handle PUT requests |
| DELETE route | `delete!(path, handler)` | Handle DELETE requests |
| Route parameter | `/users/{id}` or `/users/:id` | Dynamic URL segment (both syntaxes supported) |
| Access parameter | `request.param("id")` | Get parameter value |
| Model binding | `user: user::Model` | Auto-fetch model from DB |
| Named route | `.name("users.show")` | Name for URL generation |
| Generate URL | `route("name", &[...])` | Generate URL from name |
| Route middleware | `.middleware(M)` | Apply middleware to route |
| Route group | `group!("/prefix", { ... })` | Group routes with shared prefix |
| Nested groups | `group!(..., { group!(...) })` | Nest groups with inherited middleware |
| Group middleware | `.middleware(M)` | Apply middleware to all group routes |
| Fallback route | `fallback!(handler)` | Custom handler when no routes match |
