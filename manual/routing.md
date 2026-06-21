# Routing

Routing is how Suprnova turns an inbound HTTP request into a handler call.
You declare your routes in `src/routes.rs` using the `routes!` macro (or
build a `Router` by hand), then `Server::from_config` takes that router
and runs it for the life of the process. Same shape as Laravel's
`routes/web.php`, with Rust types instead of facades.

```rust
// src/routes.rs
use suprnova::{routes, get, post, put, delete};
use crate::controllers;

routes! {
    get!("/", controllers::home::index).name("home"),
    get!("/users", controllers::users::index).name("users.index"),
    get!("/users/{id}", controllers::users::show).name("users.show"),
    post!("/users", controllers::users::store).name("users.store"),
    put!("/users/{id}", controllers::users::update).name("users.update"),
    delete!("/users/{id}", controllers::users::destroy).name("users.destroy"),
}
```

The macro expands to `pub fn register() -> Router { ... }`. Call it from
your bootstrap and hand the result to the server.

## HTTP verbs

One macro per verb. All seven take a path-then-handler pair and return a
builder you can chain `.name(...)` and `.middleware(...)` onto.

| Macro | Method | Use for |
|---|---|---|
| `get!`     | GET     | Read endpoints, static pages |
| `post!`    | POST    | Create resources |
| `put!`     | PUT     | Full replacement updates |
| `patch!`   | PATCH   | Partial updates (RFC 5789) |
| `delete!`  | DELETE  | Destroy |
| `head!`    | HEAD    | Headers-only probes (HEAD falls back to the GET registry per RFC 9110 § 9.3.2 when not explicitly registered) |
| `options!` | OPTIONS | Capability discovery, `Accept-Patch`. CORS preflight is answered by `CorsMiddleware` before the router, so you usually don't need this one |

```rust
use suprnova::{routes, get, post, patch, delete};

routes! {
    get!("/articles", controllers::articles::index),
    post!("/articles", controllers::articles::store),
    patch!("/articles/{id}", controllers::articles::update),
    delete!("/articles/{id}", controllers::articles::destroy),
}
```

Every verb macro checks at compile time that the path starts with `/` —
a missing leading slash fails the build, not a request.

### Multi-method and `any!`

`any!` registers one handler against all seven common verbs. Use it for
webhook receivers and other endpoints that need to accept whatever HTTP
sends.

```rust
use suprnova::{routes, any};

routes! {
    any!("/webhooks/inbound", controllers::webhooks::inbound)
        .name("webhooks.inbound")
        .middleware(SignatureCheck),
}
```

When you only want a subset of verbs sharing one handler, reach for the
builder API and `Router::methods`:

```rust
use suprnova::Router;
use hyper::Method;

let router = Router::new()
    .methods(&[Method::PUT, Method::PATCH], "/posts/{id}", update_post)
    .name("posts.update")
    .middleware(AuthMiddleware);
```

`.name(...)` and `.middleware(...)` fan across every verb the route was
registered against, so reverse-lookup yields the same URL whichever
method the caller looks up.

### WebSocket routes

`ws!` registers a long-lived upgrade handler. The macro is part of the
same `routes!` body — covered in detail by [WebSockets](websockets.md).

## Route parameters

Dynamic segments use curly braces (`{id}`). For familiarity Suprnova also
accepts Express/Rails-style colons (`:id`) and normalises them to braces
before handing the pattern to `matchit`.

```rust
routes! {
    get!("/users/{id}", controllers::users::show),       // matchit-native
    get!("/users/:id", controllers::users::show),        // Express/Rails — same thing
    get!("/posts/{post_id}/comments/{comment_id}", controllers::comments::show),
}
```

The colon is only treated as a parameter opener at the start of a path
segment, so literal colons mid-segment survive untouched
(`/files/note:draft` stays a literal route, not `/files/{draft}`).

Read parameters off the request inside a handler:

```rust
use suprnova::{Request, Response, HttpResponse};

pub async fn show(req: Request) -> Response {
    let user_id = req.param("id").unwrap_or("0");
    Ok(HttpResponse::text(format!("User ID: {}", user_id)))
}
```

For typed extraction without the `unwrap_or` dance, see route model
binding below or `#[handler]` in [Controllers](controllers.md).

## Route model binding

When a handler parameter is a SeaORM `*::Model` type, `#[handler]`
extracts the matching path parameter, parses it as the primary-key type,
and fetches the row from the database. A missing row yields 404; a
parameter the PK type can't parse yields 400.

```rust
use suprnova::{handler, json_response, Response};
use crate::models::users;

// Route: GET /users/{user}
#[handler]
pub async fn show(user: users::Model) -> Response {
    json_response!({ "name": user.name, "email": user.email })
}
```

The parameter name (`user`) is what `#[handler]` looks up in the matched
route's params — so the placeholder must match (`/users/{user}`, not
`/users/{id}`).

Multiple models in one signature work the same way; mix them with form
requests, primitives, or `Request`:

```rust
// Route: PUT /posts/{post}/comments/{comment}
#[handler]
pub async fn update(
    post: posts::Model,
    comment: comments::Model,
    form: UpdateCommentRequest,
) -> Response {
    // post and comment are already fetched; form is validated.
    json_response!({ "post_id": post.id, "comment_id": comment.id })
}
```

### Requirements

Binding is automatic for any SeaORM model whose `Entity` implements
`suprnova::database::EntityExt` and whose primary-key type implements
`FromStr`. `EntityExt`'s blanket-friendly add-on traits give you
`Entity::find_by_pk(id)`, `::all()`, `::first()`, and friends; route
model binding is just `find_by_pk` driven by the path parameter.

```rust
// src/models/users.rs (the legacy SeaORM-style layout)
pub use super::entities::users::*;
use sea_orm::entity::prelude::*;

impl ActiveModelBehavior for ActiveModel {}

// Enables route model binding (and the Laravel-shaped reader surface).
impl suprnova::database::EntityExt for Entity {}
impl suprnova::database::EntityExtMut for Entity {}
```

If your model is declared with the `#[suprnova::model]` macro (the
Eloquent surface in [Eloquent](eloquent.md)), you reach for it directly:
`User::find_by_pk(id).await?`. Route model binding via `#[handler]` still
expects the `*::Model` shape — pass the SeaORM model type, not the
wrapper struct.

### Binding is identity, not authorization

Route model binding answers "does this row exist?" — it does **not**
answer "is the current user allowed to see this row?". A bare bound
handler lets any authenticated user view any post by guessing
`/posts/N`. Authorize against the bound model using `Gate::authorize` or
the `#[policy]` macro — see [Authorization](authorization.md).

### Opting out

Don't use the `*::Model` parameter type. Extract the ID and query
manually:

```rust
use suprnova::{handler, json_response, Response, FrameworkError};
use crate::models::users;
use suprnova::database::EntityExt;

#[handler]
pub async fn show(id: i32) -> Response {
    let user = users::Entity::find_by_pk(id)
        .await?
        .ok_or(FrameworkError::not_found("User"))?;
    json_response!({ "id": user.id, "name": user.name })
}
```

## Named routes

Names give you stable identifiers for URL generation. Attach one with
`.name(...)`:

```rust
routes! {
    get!("/", controllers::home::index).name("home"),
    get!("/users", controllers::users::index).name("users.index"),
    get!("/users/{id}", controllers::users::show).name("users.show"),
    post!("/users", controllers::users::store).name("users.store"),
}
```

Names follow the Laravel convention `<resource>.<action>` —
`users.show`, `posts.destroy`, `admin.dashboard`. Look them up with the
top-level `route(name, &[...])` helper:

```rust
use suprnova::route;

let home = route("home", &[]);
//   Some("/")

let profile = route("users.show", &[("id", "123")]);
//   Some("/users/123")
```

`route` returns `Option<String>` and percent-encodes parameter values
into path-safe form (so `("slug", "a/b")` becomes `/posts/a%2Fb` —
matchit-safe and round-trips through `req.param("slug")`). For redirect
targets and email links use the strict sibling `suprnova::routing::try_route`,
which returns `Result<String, RouteUrlError>` and refuses to emit a URL
containing an unfilled `{placeholder}` segment. See
[URL Generation](urls.md) for the full URL surface (signed URLs,
absolute URLs, `Redirect::route`).

Route names are globally unique and process-global. Registering the same
name to two different paths panics at boot — silent shadowing was a
security-shaped bug because redirects would route to whichever
registration happened to win. Use `RouteBuilder::try_name` (or
`suprnova::routing::try_register_route_name`) for the fallible variant.

## Per-route middleware

Chain `.middleware(M)` on any route builder:

```rust
use suprnova::{routes, get, post};
use crate::middleware::{AuthMiddleware, AdminMiddleware};

routes! {
    // Public
    get!("/", controllers::home::index).name("home"),

    // Protected
    get!("/dashboard", controllers::dashboard::index)
        .name("dashboard")
        .middleware(AuthMiddleware),

    // Multiple middleware compose left-to-right (outermost first)
    get!("/admin", controllers::admin::index)
        .middleware(AuthMiddleware)
        .middleware(AdminMiddleware),
}
```

Route-local middleware runs after any global middleware
(`Server::with_middleware`) and any group middleware that wraps the
route. The middleware map is keyed by `(method, path)`, so attaching
auth to `POST /api/posts` never bleeds onto a public `GET /api/posts`
on the same path. For the middleware contract and writing your own, see
[Middleware](middleware.md).

## Route groups

`group!` factors out a shared path prefix and/or shared middleware:

```rust
use suprnova::{routes, get, post, group};
use crate::middleware::{AuthMiddleware, ApiMiddleware};

routes! {
    get!("/", controllers::home::index).name("home"),

    // Shared /api prefix + middleware
    group!("/api", {
        get!("/users", controllers::api::users::index).name("api.users.index"),
        post!("/users", controllers::api::users::store).name("api.users.store"),
        get!("/users/{id}", controllers::api::users::show).name("api.users.show"),
    }).middleware(ApiMiddleware),

    // Admin area
    group!("/admin", {
        get!("/dashboard", controllers::admin::dashboard).name("admin.dashboard"),
        get!("/settings", controllers::admin::settings).name("admin.settings"),
    }).middleware(AuthMiddleware),
}
```

A group prefix is concatenated with each route path. A route at `/`
inside a group resolves to the group prefix exactly
(`group!("/users", { get!("/", index) })` → `GET /users`).

### Nested groups

Groups nest to any depth. Prefixes concatenate; middleware inherits from
parent to child:

```rust
routes! {
    group!("/api", {
        get!("/health", controllers::api::health),

        group!("/v1", {
            get!("/users", controllers::api::v1::users),

            group!("/admin", {
                get!("/stats", controllers::admin::stats),
            }).middleware(AdminMiddleware),
        }),
    }).middleware(AuthMiddleware),
}
```

| Route | Effective path | Middleware chain |
|---|---|---|
| `/api/health` | `/api/health` | `AuthMiddleware` |
| `/api/v1/users` | `/api/v1/users` | `AuthMiddleware` |
| `/api/v1/admin/stats` | `/api/v1/admin/stats` | `AuthMiddleware` → `AdminMiddleware` |

For a single route inside a nested group, the execution order is
**outermost middleware first**: parent group → child group → route-local.
Per-route `.middleware(...)` runs innermost.

## Fallback route

`fallback!` registers a handler that runs when no other route matches.
Use it for custom 404 pages.

```rust
use suprnova::{routes, get, fallback};

routes! {
    get!("/", controllers::home::index),

    fallback!(controllers::errors::not_found),
}
```

```rust
// src/controllers/errors.rs
use suprnova::{Request, Response, HttpResponse};

pub async fn not_found(req: Request) -> Response {
    Ok(HttpResponse::text(format!("Page not found: {}", req.path()))
        .status(404))
}
```

Fallback supports its own middleware chain (`fallback!(handler).middleware(M)`).
If no fallback is registered, the framework returns a plain-text
`404 Not Found`.

## Resource routing

For a standard 7-action REST surface, implement `ResourceController` and
register the resource through the `Router` builder. Laravel parity for
`Route::resource()` and `Route::apiResource()`.

```rust
use suprnova::{Router, ResourceController, ResourceAction, Request, Response, HttpResponse};
use std::pin::Pin;
use std::future::Future;

struct PostsCtl;

impl ResourceController for PostsCtl {
    fn index(&self, _req: Request) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        Box::pin(async { Ok(HttpResponse::text("list")) })
    }
    fn show(&self, _req: Request) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        Box::pin(async { Ok(HttpResponse::text("one")) })
    }
    // store / update / destroy / create / edit default to 404.
}

let router: Router = Router::new()
    .resource("posts", PostsCtl)
    .into();
```

Methods you don't override return 404. Use `api_resource` to drop
`create` and `edit` — the two routes that exist only to render forms.

### Default routes and names

| Verb | Path | Trait method | Name |
|---|---|---|---|
| GET    | `/posts`             | `index`   | `posts.index`   |
| GET    | `/posts/create`      | `create`  | `posts.create`  |
| POST   | `/posts`             | `store`   | `posts.store`   |
| GET    | `/posts/{post}`      | `show`    | `posts.show`    |
| GET    | `/posts/{post}/edit` | `edit`    | `posts.edit`    |
| PUT    | `/posts/{post}`      | `update`  | `posts.update`  |
| DELETE | `/posts/{post}`      | `destroy` | `posts.destroy` |

The path parameter defaults to the singular of the resource name —
`posts` → `{post}`, `categories` → `{category}`. Irregular plurals get
the literal last segment; override with `.parameter(...)`.

### Restricting and renaming

```rust
use suprnova::{Router, ResourceAction};

Router::new()
    .resource("posts", PostsCtl)
    .only(&[ResourceAction::Index, ResourceAction::Show])      // pin to two verbs
    .names([("index", "posts.list")])                          // rename a default
    .parameter("post_id")                                      // {post} → {post_id}
    .into();
```

Rust-side aliases that read better in some call sites: `.keep(...)` for
`.only(...)`, `.drop(...)` for `.except(...)`, `.rename(...)` for
`.names(...)`.

### Bulk registration

```rust
Router::new()
    .resources([
        ("posts",    Box::new(PostsCtl)    as Box<dyn ResourceController>),
        ("comments", Box::new(CommentsCtl) as Box<dyn ResourceController>),
    ])
    .api_resources([("authors", Box::new(AuthorsCtl) as Box<dyn ResourceController>)]);
```

### Authorizing the whole resource

`authorize_resource::<U, R>()` attaches the conventional ability check to
every generated route as per-route middleware — Laravel's
`authorizeResource` parity. Without it, a resource surface is ungated
unless every controller body remembers to call `Gate::authorize`; a single
forgotten `destroy` ships an ungated delete.

```rust
use suprnova::{Router, Gate};

// Abilities are keyed on (ability, user type, resource marker type).
Gate::define::<User, Post>("view",   |u, _p| u.is_member);
Gate::define::<User, Post>("create", |u, _p| u.is_author);
Gate::define::<User, Post>("update", |u, _p| u.is_author);
Gate::define::<User, Post>("delete", |u, _p| u.is_admin);

let router: Router = Router::new()
    .resource("posts", PostsCtl)
    .authorize_resource::<User, Post>()
    .into();
```

The action → ability mapping mirrors Laravel:

| Action(s) | Ability |
|---|---|
| `index`, `show`     | `view`   |
| `create`, `store`   | `create` |
| `edit`, `update`    | `update` |
| `destroy`           | `delete` |

`PATCH` shares the `update` action, so it is gated identically to `PUT`. A
denied ability short-circuits with `403` before the handler runs, and an
unauthenticated request fails closed. The resource marker `R` only needs
`Default` — the gate discriminates on its *type*, the way Laravel
discriminates on the model class. See the [authorization chapter](authorization.md)
for defining the abilities themselves.

## Router-level redirects and views

Three sugar methods on `Router` cover route declarations that don't need
a handler function:

```rust
use suprnova::Router;
use serde_json::json;

let router = Router::new()
    // Static redirect: GET /old-pricing → 302 /pricing
    .redirect("/old-pricing", "/pricing", 302)
    // 301 sibling
    .permanent_redirect("/legacy", "/new")
    // Inertia static page: GET /about renders the About component with constant props
    .view("/about", "About", json!({ "team_size": 4 }));
```

`Router::view` is Suprnova's analogue of Laravel's `Route::view($uri,
$view, $data)`. Laravel renders a Blade template; Suprnova renders an
Inertia component, because the framework's templating system is Inertia,
not Blade.

For redirect *responses* (not route declarations) — `Redirect::route`,
`Redirect::back`, `Redirect::intended`, signed redirects — see
[URL Generation](urls.md) and [Responses](responses.md).

## Signed URLs

HMAC-signed routes are routing-adjacent (you mint a URL against a named
route, then verify the signature on the inbound request). They're
covered in full by [URL Generation](urls.md); the short version:

```rust
use suprnova::url;

let reset = url::signed_route("password.reset", &[("user", "42")])?;
// /password/reset/42?signature=...

let expires_at = chrono::Utc::now().timestamp() + 3600;
let verify = url::temporary_signed_route("verify.email", &[("user", "42")], expires_at)?;
// /verify/email/42?expires=1748803600&signature=...
```

Verify inside a handler with `url::has_valid_signature(&request)` (boolean)
or `url::signature_verdict(&request)` (the three-way
`Valid`/`Expired`/`Invalid` split, so you can render a "request a fresh
link" page instead of a generic 403).

## Fallible registration

Route registration runs once at boot, so a duplicate or malformed route
is treated as a programmer error: the plain helpers (`Router::get`,
`post`, `put`, `delete`, `ws`, `RouteBuilder::name`, the
`GroupBuilder` → `Router` `From` conversion) **panic** to fail loudly at
startup. That's the right default for routes declared in source.

When patterns or names come from a fallible source — dynamic config, a
plugin system, a test that deliberately registers conflicting routes —
use the `try_*` siblings. They return `Result<_, FrameworkError>`
(naming the offending method, path, or conflicting name) instead of
panicking:

| Panicking | Fallible sibling | Returns |
|---|---|---|
| `Router::get` / `post` / `put` / `patch` / `delete` / `head` / `options` | `try_get` / `try_post` / `try_put` / `try_patch` / `try_delete` / `try_head` / `try_options` | `Result<RouteBuilder, FrameworkError>` |
| `Router::ws` (and every `ws_*` variant) | `try_ws` (and every `try_ws_*`) | `Result<Router, FrameworkError>` |
| `RouteBuilder::name` | `try_name` | `Result<Router, FrameworkError>` |
| `GroupBuilder` → `Router` via `.into()` | `GroupBuilder::try_finalize` | `Result<Router, FrameworkError>` |
| `ResourceRoutes::register` | `try_register` | `Result<Router, FrameworkError>` |

```rust
use suprnova::{FrameworkError, Router};

// `path` comes from dynamic config; a malformed or duplicate pattern
// is recoverable, not a startup panic.
fn register_dynamic(router: Router, path: &str) -> Result<Router, FrameworkError> {
    Ok(router.try_get(path, health)?.into())
}
```

A duplicate group route is recoverable the same way — because `From`
cannot be fallible, the fallible counterpart of `.into()` is the
inherent `try_finalize` method:

```rust
let router: Router = Router::new()
    .group("/api", |r| r.get("/users", list).post("/users", create))
    .try_finalize()?;
```

The panicking helpers stay as ergonomic escape hatches; the `try_*`
siblings are purely additive.

## Why Suprnova diverges

**Dual path-parameter syntax.** Laravel uses `{param}`; Express uses
`:param`. Suprnova accepts both and normalises `:param` to `{param}`
before the path reaches `matchit`. Both styles compose with everything
else — groups, model binding, signed URLs. The reason isn't
indecisiveness; it's that we can't predict which background you bring,
and routing syntax is too high-frequency a friction point to make people
relearn.

**Two co-equal APIs: macro and builder.** Laravel ships one DSL
(`Route::get(...)`). Suprnova ships the declarative `routes! { ... }`
macro AND the chainable `Router::new().get(...).name(...)` builder.
They produce identical registrations. The macro reads better for
top-level route tables; the builder reads better when you're composing
routers dynamically (plugins, generated routes, tests). Pick whichever
fits the call site — there's no canonical answer because both shapes
are first-class.

**Boot-time panics, not silent shadowing.** A duplicate route name or
pattern collision panics at startup. Laravel's array-keyed registries
silently let the later registration win, which is fine when your routes
file is the only registrar but unsafe once plugins or generated routes
enter the picture. `try_*` siblings are the escape hatch when fallibility
is what you actually want.

## Next

- [Controllers](controllers.md) — `#[handler]`, form requests, returning JSON/Inertia
- [Middleware](middleware.md) — the `Middleware` trait, ordering, building your own
- [URL Generation](urls.md) — named-route URLs, signed URLs, redirects, `RouteUrlError`
- [Authorization](authorization.md) — gates and policies for bound models
- [WebSockets](websockets.md) — `ws!`, the `WebSocketHandler` trait, per-route config
