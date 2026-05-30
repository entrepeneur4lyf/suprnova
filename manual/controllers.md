# Controllers

A Suprnova controller is just an async function. It takes whatever it
needs from the request — typed path parameters, a loaded model, a
validated form — and returns a `Response`. There is no controller base
class. There is no service-locator wiring file. The function is the
unit, and the `#[handler]` attribute glues it to the routing macros.

```rust
use suprnova::{handler, json_response, Response};
use crate::models::user;

// GET /users/{user}
#[handler]
pub async fn show(user: user::Model) -> Response {
    json_response!({
        "id": user.id,
        "name": user.name,
        "email": user.email,
    })
}
```

That handler's signature does three things at once: declares the route
parameter (`user`), pulls the row out of the database, and 404s if the
row isn't there. None of it is written by hand. `#[handler]` reads the
argument types and generates the extraction.

## Generating a controller

```bash
suprnova make:controller User
```

This writes `src/controllers/user.rs` with a single `invoke` stub and
adds `pub mod user;` to `src/controllers/mod.rs`. The stub is the
minimum-viable handler:

```rust
//! User controller

use suprnova::{handler, json_response, Request, Response};

#[handler]
pub async fn invoke(_req: Request) -> Response {
    json_response!({
        "controller": "User"
    })
}
```

Add as many functions to the file as you want — Suprnova doesn't track
controller "classes", just functions. Many apps split by resource
(`controllers::user::{index, show, store, update, destroy}`), but
nothing in the framework enforces it.

The name is converted to `snake_case` for the filename: `OrderItem`
becomes `order_item.rs`.

## The `#[handler]` attribute

The macro classifies each parameter's type and generates the matching
extractor. Four categories:

| Parameter type | Extracted via | Failure mode |
|---|---|---|
| `Request` | passes the request through unchanged | — |
| `i32`, `i64`, `u32`, `u64`, `usize`, `String` | `FromParam` — parses the route param of the same name | 400 on parse failure, 400 on missing |
| `T: AutoRouteBinding` (any Eloquent `Model`) | parses the param as the model's primary key, loads the row | 400 on parse failure, 404 if not found |
| Anything else (`T: FromRequest`) | calls `T::from_request(req)` — typically a `#[derive(FormRequest)]` validator | whatever `from_request` returns; 422 for validation errors |

The macro runs the extractions in declaration order, so the body of
your function sees fully-typed values. If any extraction fails, the
error short-circuits via `?` and the handler body never runs.

### Path parameters

```rust
// Route: get!("/users/{id}", controllers::user::show)
#[handler]
pub async fn show(id: i64) -> Response {
    json_response!({ "user_id": id })
}

// Route: get!("/posts/{post_id}/comments/{comment_id}", show_comment)
#[handler]
pub async fn show_comment(post_id: i64, comment_id: i64) -> Response {
    json_response!({
        "post_id": post_id,
        "comment_id": comment_id,
    })
}
```

The argument name must match the route placeholder: `{id}` requires
`id: …`. The argument type is parsed via `FromParam`. Bad input
(`/users/abc` against `id: i64`) returns 400 with a message naming
the parameter and target type.

### Route model binding

`Eloquent` models implement `AutoRouteBinding` automatically. Declare
the model as an argument and the framework loads it:

```rust
use suprnova::{handler, json_response, Response};
use crate::models::user;

// Route: get!("/users/{user}", controllers::user::show)
#[handler]
pub async fn show(user: user::Model) -> Response {
    json_response!({
        "id": user.id,
        "name": user.name,
        "email": user.email,
    })
}
```

The route placeholder name (`{user}`) and the argument name (`user`)
must match. The framework parses the param string as the model's
primary-key type, calls `Entity::find_by_pk`, and returns 404 if the
row is missing. No need for `route_binding!` — that legacy macro is
deprecated; any `#[suprnova::model]` struct binds automatically.

### Form requests

Anything that implements `FromRequest` plugs in the same way. The
common case is a `#[derive(FormRequest)]` struct that validates the
request body and surfaces a 422 with field-keyed errors on failure:

```rust
use suprnova::{attrs, handler, json_response, Response};
use crate::models::user;
use crate::requests::UpdateUserRequest;

// Route: put!("/users/{user}", controllers::user::update)
#[handler]
pub async fn update(user: user::Model, form: UpdateUserRequest) -> Response {
    let id = user.id;
    user.update(attrs! { name: form.name, email: form.email }).await?;
    json_response!({ "updated": id })
}
```

See [Form Requests](requests.md) for the validator derive and the
full validation pipeline.

### When you want the raw `Request`

If you'd rather extract things by hand — or you need a header, a
cookie, a query string — take `Request` directly:

```rust
use suprnova::{handler, json_response, Request, Response};

#[handler]
pub async fn show(req: Request) -> Response {
    let id = req.param("id")?;             // route param, 400 on miss
    let ua = req.header("User-Agent");      // Option<&str>
    let page: u32 = req.query_param("page") // Option<String>
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    json_response!({ "id": id, "ua": ua, "page": page })
}
```

You can mix and match: `pub async fn nested(category_id: i64, product: product::Model, req: Request)` is a valid signature. The macro extracts each argument by its own rule.

## The `Response` contract

`Response` is an alias for `Result<HttpResponse, HttpResponse>`. Both
arms carry the same payload type, which is why `?` works everywhere.
The middleware chain collapses the result with one line at the
boundary:

```rust
result.unwrap_or_else(|e| e)
```

This is the same contract every `?` propagation point relies on.
Errors get converted via `From<FrameworkError> for HttpResponse`
before they reach the chain — see [Error Model](error-model.md) for
the full picture.

The body of a handler reads top-to-bottom and uses `?` to bail:

```rust
use suprnova::{handler, json_response, Response};
use crate::models::user;

#[handler]
pub async fn show(id: i64) -> Response {
    let user = user::Model::find_or_fail(id).await?;
    let invoices = user.invoices().get().await?;
    json_response!({ "user": user, "invoices": invoices })
}
```

If `find_or_fail` returns `Err`, the function exits with a 404. If
`invoices().get()` errors, you get a 500. No `match` statements, no
exception handlers.

## Creating responses

Three macros and a builder cover the common cases:

```rust
use suprnova::{handler, json_response, text_response, HttpResponse, Response, ResponseExt};

#[handler]
pub async fn json_handler() -> Response {
    json_response!({
        "users": [
            {"id": 1, "name": "John"},
            {"id": 2, "name": "Jane"},
        ]
    })
}

#[handler]
pub async fn health() -> Response {
    text_response!("OK")
}

#[handler]
pub async fn store() -> Response {
    // Built-in chainable status / headers via ResponseExt.
    json_response!({ "id": 1, "created": true }).status(201)
}

#[handler]
pub async fn page() -> Response {
    Ok(HttpResponse::html("<h1>Hello</h1>"))
}
```

`json_response!`, `text_response!`, and `HttpResponse::*` all produce
the same `Response` type. The `ResponseExt` trait adds `.status(...)`,
`.header(...)`, `.cookie(...)`, and `.with_headers(...)` so you can
chain configuration onto a macro result.

For everything else — file downloads, streaming bodies, Inertia
responses, redirects — see [Responses](responses.md).

## Redirects

`redirect!("route.name")` validates the route exists at compile time
and returns a builder you can chain configuration onto:

```rust
use suprnova::{handler, redirect, Response};

#[handler]
pub async fn store() -> Response {
    // Create the user…
    redirect!("users.index").into()
}

#[handler]
pub async fn update(id: i64) -> Response {
    redirect!("users.show")
        .with("id", id.to_string())
        .into()
}

#[handler]
pub async fn search() -> Response {
    redirect!("users.index")
        .query("page", "1")
        .query("sort", "name")
        .into()
}
```

`.with(key, value)` fills a route placeholder; `.query(key, value)`
appends a query string parameter; `.flash(key, value)` writes to the
session flash bag for the next request. `.into()` converts the
builder to a `Response`.

If the named route doesn't exist, the macro fails the compile with
a list of available route names — typos surface before staging.

## Container-injected services

Resolve services from the container with `App::resolve` (concrete
types) or `App::resolve_make` (trait objects). Both return
`Result<_, FrameworkError>` so they compose with `?`:

```rust
use suprnova::{handler, json_response, App, Response};
use crate::services::UserService;

#[handler]
pub async fn index() -> Response {
    let user_service = App::resolve::<UserService>()?;
    let users = user_service.list_all().await?;
    json_response!({ "users": users })
}
```

If you're binding actions with `#[injectable]`, this is how a
controller calls them. See [Actions](actions.md) for the action
shape, and [Service Container](container.md) for the full container
surface — binding, factories, the task-local / thread-local /
global lookup cascade.

## A worked RESTful controller

```rust
// src/controllers/user.rs
use suprnova::{attrs, handler, json_response, redirect, Response, ResponseExt};
use crate::models::user;
use crate::requests::{StoreUserRequest, UpdateUserRequest};

// GET /users
#[handler]
pub async fn index() -> Response {
    let users = user::Model::all().await?;
    json_response!({ "users": users })
}

// GET /users/{user}
#[handler]
pub async fn show(user: user::Model) -> Response {
    json_response!({ "user": user })
}

// POST /users
#[handler]
pub async fn store(form: StoreUserRequest) -> Response {
    let user = user::Model::create(attrs! {
        name: form.name,
        email: form.email,
    }).await?;
    json_response!({ "user": user }).status(201)
}

// PUT /users/{user}
#[handler]
pub async fn update(user: user::Model, form: UpdateUserRequest) -> Response {
    let id = user.id;
    user.update(attrs! {
        name: form.name,
        email: form.email,
    }).await?;
    json_response!({ "updated": id })
}

// DELETE /users/{user}
#[handler]
pub async fn destroy(user: user::Model) -> Response {
    user.delete().await?;
    redirect!("users.index").into()
}
```

Register them with the `routes!` macro:

```rust
// src/routes.rs
use suprnova::{delete, get, post, put, routes};
use crate::controllers;

routes! {
    get!("/users",           controllers::user::index   ).name("users.index"),
    get!("/users/{user}",    controllers::user::show    ).name("users.show"),
    post!("/users",          controllers::user::store   ).name("users.store"),
    put!("/users/{user}",    controllers::user::update  ).name("users.update"),
    delete!("/users/{user}", controllers::user::destroy ).name("users.destroy"),
}
```

The route placeholder `{user}` matches the argument name `user: user::Model`, which is how the framework knows which path segment loads the model.

## The `Request` API

The methods you'll reach for most often when taking `Request` directly:

| Method | Returns | Notes |
|---|---|---|
| `method()` | `&hyper::Method` | HTTP method |
| `path()` | `&str` | URL path |
| `param(name)` | `Result<&str, ParamError>` | route param; `?` to bail |
| `params()` | `&HashMap<String, String>` | all route params |
| `query()` | `Option<&str>` | raw query string |
| `query_param(key)` | `Option<String>` | single query string value |
| `query_params()` | `HashMap<String, String>` | all query params |
| `query_into::<T>()` | `Result<T, FrameworkError>` | typed deserialize |
| `header(name)` | `Option<&str>` | single header |
| `headers()` | `&hyper::HeaderMap` | full header map |
| `has_header(name)` | `bool` | presence check |
| `bearer_token()` | `Option<String>` | parsed `Authorization: Bearer …` |
| `cookie(name)` | `Option<String>` | single cookie value |
| `cookies()` | `HashMap<String, String>` | all cookies |
| `ip()` | `Option<String>` | peer IP, X-Forwarded-For-aware |
| `secure()` | `bool` | HTTPS detection (incl. proxies) |
| `is_method(m)` | `bool` | case-insensitive |
| `is_inertia()` | `bool` | Inertia XHR header |
| `ajax()` | `bool` | `X-Requested-With: XMLHttpRequest` |
| `expects_json()` / `wants_json()` | `bool` | Accept-header inspection |
| `route_name()` | `Option<String>` | matched route's `.name(...)` |
| `json::<T>()` | `Result<T, FrameworkError>` | parse body as JSON (consumes) |
| `form::<T>()` | `Result<T, FrameworkError>` | parse as form-urlencoded |
| `input::<T>()` | `Result<T, FrameworkError>` | content-type-dispatched parse |

This is a Laravel-shaped surface — every method here mirrors a method
on Laravel's `Request` class.

## File layout

Convention:

```
src/
├── controllers/
│   ├── mod.rs          # pub mod home; pub mod user; ...
│   ├── home.rs
│   ├── user.rs
│   └── api/
│       ├── mod.rs
│       └── user.rs
├── routes.rs           # routes! { ... }
└── main.rs
```

Nothing in the framework enforces this layout — controllers can live
anywhere reachable from `routes.rs`. The convention exists because
it's what scaffolding emits and because routes/controllers are the
natural pair.

## Why Suprnova diverges

Laravel controllers are classes that extend `Illuminate\Routing\Controller`.
Methods are called on instances the container resolves per-request,
which is where constructor-injection happens. The pattern is fine on
PHP — `new`-on-every-request is cheap when the entire process tears
down after the response.

In Rust, that pattern would mean either (a) allocating a controller
struct per request, which costs an `Arc` clone you don't need, or (b)
re-implementing dependency injection through a base class hierarchy
that doesn't pay for itself.

Suprnova picks the simpler model: a controller is a free async
function, and "dependencies" are either container resolutions
(`App::resolve::<Service>()?`) or extraction-typed arguments
(`form: UpdateUserRequest`). Constructor injection happens at the
`#[injectable]` boundary in [Actions](actions.md), where it belongs.
The handler stays a pure function from request to response, which
makes it trivial to test in isolation: build a `Request`, call the
function, assert on the result.

## Next

- [Routing](routing.md) — what `routes!`, `get!`, `post!`, and `.name()` expand into
- [Form Requests](requests.md) — typed validation via `#[derive(FormRequest)]`
- [Responses](responses.md) — JSON, HTML, files, streams, Inertia pages, redirects
- [Service Container](container.md) — what `App::resolve` actually does
- [Actions](actions.md) — where business logic lives outside the controller
- [Error Model](error-model.md) — how `?` turns `FrameworkError` into a response
