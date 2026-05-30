# Macros

Suprnova ships about three dozen macros, every one of them re-exported
from `suprnova::*`. They're the joints where the framework meets your
code — `routes!` builds the router, `#[handler]` adapts a function
into one, `#[suprnova::model]` turns a struct into an Eloquent model,
`#[derive(Data)]` produces a typed Inertia payload. This chapter is
the index. Each macro gets a one-paragraph description, a minimal
example, and a pointer to the chapter that uses it for real work.

A few principles that hold across the whole surface:

- **Macros emit fully-qualified paths.** Generated code writes
  `::suprnova::…` so the macros work whether or not you've imported
  the underlying types.
- **Heavy use of `inventory::submit!`.** Models, commands, policies,
  observers, payment providers, and more register themselves at
  compile time and the framework drains the registry at boot. You
  almost never wire registration by hand.
- **Compile-time validation where it pays.** `inertia_response!`
  checks that the named component file exists. `redirect!` checks
  that the named route exists. `routes!` rejects paths that don't
  start with `/`. Errors that can be caught at build time are.

## Routing

| Macro | Returns | What it does |
|---|---|---|
| `routes!` | `pub fn register() -> Router` | Top-level list of routes — exports a `register()` your `app.rs` calls |
| `get!` / `post!` / `put!` / `delete!` / `patch!` / `head!` / `options!` / `any!` | `RouteDefBuilder<H>` | One HTTP route — chainable `.name(...)` / `.middleware(...)` |
| `group!` | `GroupDef` | Prefix + middleware applied to a child list of routes |
| `fallback!` | `FallbackDefBuilder<H>` | Custom 404 handler when no route matches |
| `ws!` | `WsRouteDef` | One WebSocket route — chainable `.middleware(...)` / `.config(...)` |

```rust
use suprnova::{routes, get, post, ws, group};
use crate::{controllers, middleware::AuthMiddleware, ws::ChatHandler};

routes! {
    get!("/", controllers::home::index).name("home"),
    get!("/users/{id}", controllers::user::show).name("users.show"),
    post!("/users", controllers::user::store).name("users.store"),

    group!("/admin", {
        get!("/dashboard", controllers::admin::dashboard),
    }).middleware(AuthMiddleware),

    ws!("/ws/chat", ChatHandler),
}
```

The route-path string is checked at compile time — `validate_route_path`
rejects anything that doesn't start with `/`. Route names registered
via `.name("…")` are also checked for uniqueness at boot through
`register_route_name`. See [Routing](routing.md) for the full
expansion and [WebSockets](websockets.md) for `ws!`.

## Handlers and requests

### `#[handler]`

Rewrites a controller function so it can extract typed parameters
(via `FromRequest`) directly from the incoming request — instead of
manually pulling fields off `Request`, you declare what the handler
needs and the macro wires it up.

```rust
use suprnova::{handler, Response, json_response, request};

#[request]
pub struct CreateUserRequest {
    #[validate(email)]
    pub email: String,

    #[validate(length(min = 8))]
    pub password: String,
}

#[handler]
pub async fn store(form: CreateUserRequest) -> Response {
    // `form` is already validated — 422 returned automatically on failure
    json_response!({ "email": form.email })
}
```

A `Request`-shaped first parameter is still accepted as the
identity case. See [Controllers](controllers.md).

### `#[request]` and `#[derive(FormRequest)]`

`#[request]` is the recommended way to declare a validated request
type. It auto-derives `Deserialize`, `Validate`, and `FormRequest`,
so the struct works with both `application/json` and
`application/x-www-form-urlencoded` bodies.

`#[derive(FormRequestDerive)]` is the underlying derive if you want
to opt out of the attribute (you'll need to derive `Deserialize` and
`Validate` yourself). The attribute is what we recommend; the derive
exists for the edge case. See [Requests](requests.md) and
[Validation](validation.md).

### `#[derive(MultipartRequest)]`

Strongly-typed extractor for `multipart/form-data` — bind text fields
and uploaded files in one struct, with per-field type-level validators.

```rust
use suprnova::{MultipartRequest};
use suprnova::http::upload::{Image, MaxSize, UploadedFile};

#[derive(MultipartRequest)]
pub struct AvatarUpload {
    #[field("avatar")]
    pub avatar: UploadedFile<(Image, MaxSize<5_242_880>)>,

    #[field("caption")]
    pub caption: Option<String>,
}
```

Built-in validators (`Image`, `MimeAllowlist<…>`, `MaxSize<…>`,
`MimeType<…>`) compose via tuples. See [Requests](requests.md).

## Responses

### `json_response!` and `text_response!`

The two short-form response macros. Both wrap `HttpResponse::*` in
`Ok(...)` so they slot straight into a handler's return position:

```rust
use suprnova::{handler, json_response, text_response, Response};

#[handler]
pub async fn health() -> Response {
    json_response!({ "status": "ok" })
}

#[handler]
pub async fn robots() -> Response {
    text_response!("User-agent: *\nDisallow:")
}
```

See [Responses](responses.md).

### `inertia_response!`

Builds an Inertia page response, validating at compile time that the
named component file (`.svelte` / `.tsx` / `.jsx` / `.vue`) exists in
`frontend/src/pages/`. If you misspell the component name, the build
fails with suggestions:

```rust
use suprnova::{handler, inertia_response, InertiaProps, Request, Response};

#[derive(InertiaProps)]
struct HomeProps {
    title: String,
    user_count: i64,
}

#[handler]
pub async fn index(req: Request) -> Response {
    inertia_response!(&req, "Home", HomeProps {
        title: "Welcome".into(),
        user_count: 42,
    })
}
```

`#[derive(InertiaProps)]` generates the `Serialize` impl the response
shape needs. See [Inertia Responses](frontend-inertia-responses.md).

### `redirect!`

Type-safe redirect to a named route — the route name is verified at
compile time against the names registered through `routes!`:

```rust
use suprnova::redirect;

// Compiles only if "users.show" is a registered route name
let resp = redirect!("users.show").with("id", "42").into();
```

See [URL Generation](urls.md).

## Eloquent

### `#[suprnova::model]`

Turns a plain struct into a full Eloquent model: generates SeaORM
`Entity`, `Model`, `ActiveModel`, `Column`, `Relation` stubs, plus
all the trait impls Eloquent needs. Also `inventory::submit!`s a
`ModelEntry` so the framework can enumerate every model at boot.

```rust
use suprnova::model;

#[model(table = "users")]
pub struct User {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
```

Attribute keys include `table`, `primary_key`, `key_type`,
`auto_increment`, `connection`, `fillable`, `guarded`, `casts`,
`timestamps`, `soft_deletes`, `appends`, `hidden`, `visible`,
`mutators`, `touches`, and `unique_id` (for UUID/ULID PKs). See
[Eloquent](eloquent.md).

### `#[suprnova::scopes(Model)]`

Walks an `impl Model { … }` block and turns every method whose
signature matches `fn name(query: Builder<Self>[, args…]) -> Builder<Self>`
into a scope — generating both `Model::scope_name(args)` and a
chainable `.scope_name(args)` on `Builder<Model>`.

```rust
use suprnova::{scopes, Builder};

#[suprnova::scopes(User)]
impl User {
    pub fn active(query: Builder<Self>) -> Builder<Self> {
        query.filter("active", true)
    }

    pub fn popular(query: Builder<Self>, threshold: i64) -> Builder<Self> {
        query.filter_op("followers_count", ">", threshold)
    }

    // Not a scope — passes through unchanged
    pub fn display_name(&self) -> String { self.name.clone() }
}

// Both call sites compile:
// User::active().popular(500).get().await?;
// User::query().filter_op("id", ">", 0).active().get().await?;
```

The chainable form requires the generated trait
`HasScope_<scope>_<Model>` in scope when called from a different
module. See [Eloquent](eloquent.md).

### `#[suprnova::observer(Model)]`

Wires an `impl Observer<M>` block into the lifecycle-event system —
each of the 16 overridden methods becomes a registered listener,
submitted to inventory and drained at boot.

```rust
use async_trait::async_trait;
use suprnova::eloquent::observers::Observer;
use suprnova::eloquent::events::EventResult;
use suprnova::eloquent::attrs::Attrs;
use suprnova::FrameworkError;

pub struct AuditObserver;

#[suprnova::observer(User)]
#[async_trait]
impl Observer<User> for AuditObserver {
    async fn creating(&self, attrs: &mut Attrs) -> EventResult {
        if attrs.get("email").is_none() {
            return EventResult::cancel("email is required");
        }
        EventResult::ok()
    }

    async fn created(&self, user: &User) -> Result<(), FrameworkError> {
        tracing::info!(user.id = user.id, "user created");
        Ok(())
    }
}
```

**Required attribute ordering: `#[suprnova::observer(M)]` must come
before `#[async_trait]`.** Attribute macros expand outside-in — if
`async_trait` runs first, it rewrites every `async fn` into a
desugared shape and the observer macro's name-match against the 16
trait method names silently finds nothing. See [Events](events.md).

### `#[suprnova::accessor]` and `#[suprnova::mutator]`

Function-level markers on `impl Model { … }` methods that hook into
the model's `to_json()` / `fill()` paths. Reference the field name
in `#[model(appends = […])]` (accessor) or `#[model(mutators = […])]`
(mutator) for the macro to wire them up.

```rust
#[suprnova::model(appends = ["full_name"], mutators = ["password"])]
pub struct User {
    pub id: i64,
    pub first_name: String,
    pub last_name: String,
    pub password: String,
}

impl User {
    #[suprnova::accessor]
    pub fn full_name(&self) -> String {
        format!("{} {}", self.first_name, self.last_name)
    }

    #[suprnova::mutator]
    pub fn set_password(
        &mut self,
        value: serde_json::Value,
    ) -> Result<(), suprnova::FrameworkError> {
        let raw: String = serde_json::from_value(value)
            .map_err(|e| suprnova::FrameworkError::validation("password", format!("{e}")))?;
        self.password = bcrypt(raw);
        Ok(())
    }
}
```

See [Mutators & Casts](eloquent-mutators.md).

### `#[suprnova::prunable]`

Wraps a `Prunable` (or `MassPrunable`) impl and submits a `PrunerEntry`
into the registry that `model:prune` walks at runtime:

```rust
use async_trait::async_trait;
use chrono::{Duration, Utc};
use suprnova::eloquent::Prunable;

#[suprnova::prunable]
#[async_trait]
impl Prunable for Session {
    fn prunable() -> suprnova::Builder<Self> {
        Self::query().filter_op(
            "expires_at",
            "<",
            (Utc::now() - Duration::days(30)).to_rfc3339(),
        )
    }
}
```

See [Eloquent](eloquent.md).

### `attrs!`

Builds an ordered `Attrs` map (`IndexMap<&'static str, serde_json::Value>`)
for `Model::create` / `Model::update` / `Model::fill`:

```rust
use suprnova::attrs;

let user = User::create(attrs! {
    name: "Alice",
    email: "alice@example.com",
    age: 32,
}).await?;
```

See [Eloquent](eloquent.md).

### `casts!`

Builds a per-query cast map you can pass to `Builder::with_casts`:

```rust
use suprnova::{casts, AsDate, AsJson};

let map = casts! {
    birthday = AsDate,
    metadata = AsJson<serde_json::Value>,
};
let rows = User::query().with_casts(map).get().await?;
```

See [Mutators & Casts](eloquent-mutators.md).

### `route_binding!`

Implements `RouteBinding` for a hand-rolled SeaORM entity so it
resolves automatically from a route parameter. Models defined with
`#[suprnova::model]` register automatically and don't need this; reach
for `route_binding!` when you wrote the entity by hand:

```rust
use suprnova::route_binding;

route_binding!(crate::entities::user::Entity, User, "user");
```

After that, `get!("/users/{user}", controllers::user::show)` passes
a fully-loaded `User` to your handler. See [Routing](routing.md).

## Data and Inertia

### `#[derive(Data)]`

The composite derive for typed payloads. Produces a `Serialize` impl
that respects `#[data(input_only)]` fields, plus a `Deserialize` impl
that rejects payloads attempting to set `#[data(output_only)]` fields.
Pair with `#[json_resource("type")]` for JSON:API output via the
`Resource` chapter.

```rust
use suprnova::{Data, Validate};

#[derive(Data, Validate)]
struct UserDto {
    pub id: i64,
    pub name: String,

    #[data(input_only)]
    #[validate(length(min = 8))]
    pub password: String,

    #[data(output_only)]
    pub computed_handle: String,

    #[data(allow_include)]
    pub posts: Vec<PostDto>,
}
```

`#[data(allow_include)]` registers the field in the partial-reload
include allowlist via `inventory::submit!`. See
[Data Objects](data.md) and [API Resources](eloquent-resources.md).

### `#[derive(InertiaProps)]`

Generates the `Serialize` impl `inertia_response!` needs. Plain marker
derive — most apps reach for `#[derive(Data)]` instead because it gives
you partial-reload includes for free.

```rust
use suprnova::InertiaProps;

#[derive(InertiaProps)]
struct DashboardProps {
    title: String,
    user: User,
}
```

See [Inertia Responses](frontend-inertia-responses.md).

### `when_loaded!`

Emits a `Prop::lazy(…)` only when a named relation has been
eager-loaded on the entity; otherwise emits `Prop::EagerNone` so the
prop is skipped from the response entirely:

```rust
use suprnova::when_loaded;

let songs_prop = when_loaded!(&artist, "songs", || async {
    serde_json::to_value(&artist.songs).unwrap()
});
```

See [Data Objects](data.md).

## Dependency injection

### `#[service]`

Adds `Send + Sync + 'static` to a trait so it slots into the container:

```rust
use suprnova::service;

#[service]
pub trait HttpClient {
    async fn get(&self, url: &str) -> Result<String, FrameworkError>;
}

// App::bind::<dyn HttpClient>(Arc::new(RealHttpClient::new()));
// let client = App::make::<dyn HttpClient>()?;
```

See [Service Container](container.md).

### `#[injectable]`

Auto-registers a concrete type as a singleton. Derives `Default` +
`Clone` and submits a registration that runs at boot:

```rust
use suprnova::injectable;

#[injectable]
pub struct AppState {
    pub counter: u32,
}

// let state: AppState = App::get().unwrap();
```

See [Service Container](container.md).

## Errors

### `#[domain_error]`

Defines a domain error that implements `Display`, `Error`, `HttpError`,
and `From<T> for FrameworkError` — so it short-circuits a handler via
`?`:

```rust
use suprnova::domain_error;

#[domain_error(status = 404, message = "User not found")]
pub struct UserNotFoundError {
    pub user_id: i32,
}

pub async fn get_user(id: i32) -> Result<User, FrameworkError> {
    let user = User::find(id).await?
        .ok_or_else(|| UserNotFoundError { user_id: id })?;
    Ok(user)
}
```

See [Error Handling](errors.md).

## Console and background work

### `#[command]`

Marks an `async fn(Vec<String>) -> Result<(), FrameworkError>` as a
console command. Submits a `CommandEntry` so `dispatch_argv` finds it
when the per-project console binary runs:

```rust
use suprnova::{command, FrameworkError};

#[command(name = "db:seed", description = "Run all registered seeders")]
async fn db_seed(_args: Vec<String>) -> Result<(), FrameworkError> {
    suprnova::seed::run_all().await
}
```

See [Console](console.md).

### `#[derive(Command)]`

The typed-args alternative. Goes on top of `#[derive(clap::Parser)]`,
reads `#[console(...)]` for metadata, and emits the runner that calls
your `TypedCommand::run`:

```rust
use async_trait::async_trait;
use suprnova::{Command, FrameworkError, TypedCommand};

#[derive(clap::Parser, Command)]
#[console(name = "greet", description = "Greet someone")]
pub struct Greet {
    #[arg(short, long)]
    name: Option<String>,
    #[arg(long)]
    loud: bool,
}

#[async_trait]
impl TypedCommand for Greet {
    async fn run(self) -> Result<(), FrameworkError> {
        let target = self.name.unwrap_or_else(|| "world".into());
        println!("{}", if self.loud { format!("HELLO {target}!") } else { format!("Hello {target}") });
        Ok(())
    }
}
```

See [Console](console.md).

### `#[workflow]` and `#[workflow_step]`

`#[workflow]` registers an async fn as a durable workflow — runnable
state, retriable steps, persisted history. Each `#[workflow_step]`
inside the body is a checkpoint the runtime can resume from after a
crash or restart.

```rust
use suprnova::{workflow, workflow_step, FrameworkError};

#[workflow]
async fn onboard_user(user_id: i64) -> Result<(), FrameworkError> {
    send_welcome_email(user_id).await?;
    enable_default_features(user_id).await?;
    Ok(())
}

#[workflow_step]
async fn send_welcome_email(user_id: i64) -> Result<(), FrameworkError> {
    // …
    Ok(())
}
```

### `start_workflow!`

Kicks off a workflow by path, serialising the args into the workflow
runtime's envelope shape:

```rust
use suprnova::start_workflow;

let handle = start_workflow!(crate::workflows::onboard_user, 42).await?;
```

See [Workflows](workflows.md).

### `schedule_task!`

Sugar around `TaskBuilder::from_async` so a closure schedules cleanly
alongside trait-based `Task` impls:

```rust
use suprnova::{schedule_task, FrameworkError};

let task = schedule_task!(|| async {
    println!("ticking");
    Ok::<(), FrameworkError>(())
})
    .every_minute()
    .name("tick");
```

See [Task Scheduling](scheduling.md).

## Authorization

### `#[policy(UserType, ResourceType)]`

Wraps an `impl Policy` block and registers each method as a named
gate action. The gate name combines the method name with the
lowercased resource type — `fn view(...)` on `Comment` becomes
`"view-comment"`:

```rust
use suprnova::policy;

struct CommentPolicy;

#[policy(User, Comment)]
impl CommentPolicy {
    fn view(_user: &User, _comment: &Comment) -> bool { true }
    fn update(user: &User, comment: &Comment) -> bool {
        comment.author_id == user.id
    }
}
```

`Server::run` calls `authorization::init_policies()` automatically.
See [Authorization](authorization.md).

## Notifications and mail

### `#[derive(NotificationMailable)]`

Auto-generates `to_mail` from a `#[mail(...)]` attribute — inline or
file-backed Tera templates for subject, HTML body, and text body.
Compile-time checks: subject required, at least one body present,
exclusive html/html_template, `from_name` requires `from`:

```rust
use serde::{Serialize, Deserialize};
use suprnova::NotificationMailable;

#[derive(Serialize, Deserialize, NotificationMailable)]
#[mail(
    subject = "Your order shipped — tracking {{ tracking }}",
    html    = "<p>Tracking: <code>{{ tracking }}</code></p>",
    text    = "Tracking: {{ tracking }}",
    from    = "orders@suprnova.dev",
)]
pub struct OrderShipped { pub tracking: String }
```

The notification trait itself is hand-implemented — there is no
`#[derive(Notification)]`. See [Notifications](notifications.md) and
[Mail](mail.md).

## Validation

### `validate!`

Sync, declarative validation entry point. Each row pairs a field name
with one or more `Rule` (or `ContextualRule`) values, with `?:` for
"present-only validate" and `?=>` for conditionally-required optional
fields:

```rust
use suprnova::{validate, ValidationErrors};
use suprnova::validation::rules::*;

fn validate_form(self_ref: &SignupForm) -> Result<(), ValidationErrors> {
    validate! { self_ref =>
        email   => Required, Email;
        password => Required, Min(8);
        bio     ?: Max(500);
        card_number ?=> RequiredIf { other: "billing_type", value: "card" } => with ctx;
    }
}
```

`Validate` is re-exported from the `validator` crate — `#[validate(...)]`
attributes (e.g. `#[validate(email)]`) come from `validator` and run
through `FormRequest`'s sync path. Use `validate!` when you need
contextual / cross-field rules, async rules, or rules from the
`suprnova::validation::rules` palette. See [Validation](validation.md).

## Factories

### `#[derive(Factory)]`

Generates a sibling `<Model>Factory` marker and a `Factory` impl that
produces models via `fake::Faker`. The model must implement
`fake::Dummy<fake::Faker>` — typically via `#[derive(Dummy)]`:

```rust
use suprnova::{Dummy, Factory};

#[derive(Dummy, Factory)]
pub struct User {
    pub id: i32,
    pub name: String,
    pub email: String,
}

// UserFactory exists:
let users = UserFactory::new().count(10).make_many();
```

See [Factories](eloquent-factories.md).

## Testing

### `#[suprnova_test]`

Wraps an `async fn` test with an in-memory SQLite database (running
`crate::migrations::Migrator` by default), invokes `App::init()` and
`App::boot_services()`, and runs the body under `#[tokio::test]`.
Parallel tests stay hermetic through the container's per-thread
layer — bind test-specific services through `TestContainer::fake`
(not `App::bind`) so each thread sees its own fakes:

```rust
use suprnova::suprnova_test;
use suprnova::testing::TestDatabase;

#[suprnova_test]
async fn creates_a_user(db: TestDatabase) {
    let user = User::create(attrs! { name: "A", email: "a@x.com" }).await.unwrap();
    assert!(user.id > 0);
}
```

A custom migrator goes via `#[suprnova_test(migrator = MyMigrator)]`.
See [Testing](testing.md).

### `test_database!`

The one-line `TestDatabase` constructor for tests that don't take the
`db` parameter through `#[suprnova_test]`:

```rust
let db = test_database!();
let db = test_database!(my_crate::CustomMigrator);
```

### `describe!`, `test!`, `expect!`

Jest-style grouping + fluent assertions. `describe!` is a module,
`test!` produces a `#[test]` (sync or async, with or without a
`TestDatabase` parameter), and `expect!` wraps a value for chained
assertions with file/line context on failure:

```rust
use suprnova::{describe, test, expect};

describe!("CreateUserAction", {
    test!("creates a user", async fn(db: TestDatabase) {
        let user = CreateUserAction::new()
            .execute("test@example.com").await.unwrap();
        expect!(user.email).to_equal("test@example.com".to_string());
    });
});
```

See [Testing](testing.md).

## Middleware

### `global_middleware!`

Registers a middleware that runs on every request, in registration
order, before any route-specific middleware. Idempotent per type:

```rust
use suprnova::global_middleware;
use crate::middleware;

pub fn register() {
    global_middleware!(middleware::LoggingMiddleware);
    global_middleware!(middleware::CorsMiddleware);
}
```

Must run before `Server::from_config` / `Server::new` — the server
snapshots the global registry at build time. See
[Middleware](middleware.md).

## Pitfalls

A short list of failure modes that are easy to hit and easy to fix.

### Attribute ordering — `#[observer]` must come before `#[async_trait]`

```rust
// CORRECT
#[suprnova::observer(User)]
#[async_trait]
impl Observer<User> for AuditObserver { … }

// WRONG — silently emits zero listeners
#[async_trait]
#[suprnova::observer(User)]
impl Observer<User> for AuditObserver { … }
```

Attribute macros expand outside-in. `async_trait` rewrites every
`async fn` into a desugared `Pin<Box<dyn Future>>` shape. If it runs
first, the observer macro can no longer match by method name and
emits nothing. The same outside-in rule applies whenever you stack
multiple macros — put the Suprnova attribute outermost when in doubt.

### The inherent-impl trap

An inherent `impl` method **cannot** shadow a trait's default method
through trait dispatch. If you write a macro (or hand-write code)
that defines `fn save(&self)` on a model as an inherent method,
calls that go through the `Model` trait (`some_model.save()` where
the call site only knows it as `&dyn Model`) will pick the trait
default — not your inherent override.

Fix: emit a trait-method override, never an inherent method, when the
generated behaviour must participate in trait dispatch. This is why
the framework's macros (notably `#[suprnova::model]`) write to the
trait impl. If you're hand-rolling Eloquent extensions, do the same.

### `global_middleware!` only takes effect before `Server::from_config`

The server snapshots the global registry when it's built. Calling
`global_middleware!(M)` after `Server::from_config(...)` does not
retroactively apply to that server. Register every global middleware
in `bootstrap()`, before `Application::run()` reaches the serve step.

### `redirect!` and `inertia_response!` are build-time checks

Both macros refuse to compile if the named target doesn't exist —
that's the point. If a refactor removes a route or component name,
every call site that mentions it breaks the build, which is exactly
what you want. If the build error surprises you, search for the
string literal in your `routes!` block / pages directory before
"fixing" the macro call.

### `?:` skips on `None`; `?=>` runs even on `None`

In `validate!` rows, `?:` only runs rules when the field is `Some`.
A presence-conditional rule like `RequiredIf` on a `?:` row therefore
can never fail an absent field. Use `?=>` (which treats absence as
`""`) for the require-when-X case.

### `#[derive(Validate)]` is from the `validator` crate, not Suprnova

Suprnova re-exports `validator::Validate` so you don't take a direct
dep on `validator`. The `#[validate(...)]` attributes come from
`validator`. Suprnova's own `validate!` macro is the runtime
cross-field / contextual entry point; the two complement each other
but live in different namespaces.

## Why Suprnova diverges

Laravel discovers routes, commands, mail templates, model classes,
factories, observers, and policies at runtime — through reflection,
filesystem scanning, and string-based dispatch. PHP makes that cheap
(autoloading + opcache amortise the cost), and the developer
experience is excellent: drop a file in the right directory and it
shows up.

That model doesn't fit Rust. We don't have runtime reflection on
trait impls, runtime is a single statically-linked binary, and
filesystem scans at boot are a worse fit for a process model where
each binary serves millions of requests.

So Suprnova does the same job at compile time. Routes are validated,
component names are checked against the pages directory, mail
templates are embedded via `include_str!`, route names are checked
for uniqueness through inventory, models register themselves in an
inventory the framework drains at boot, commands the same. The
developer experience is similar — drop a file, add a `#[command]`
or `#[suprnova::model]`, run the binary — but the wiring happens
before `main` instead of at the first request.

The trade is that misspellings, missing components, and broken
references are build errors instead of runtime errors, and there's
zero per-request reflection cost.

## Next

- [Routing](routing.md) — full `routes!` expansion, naming, model binding
- [Controllers](controllers.md) — `#[handler]` and `#[request]` together
- [Eloquent](eloquent.md) — `#[suprnova::model]` and friends in context
- [Validation](validation.md) — `validate!`, contextual rules, async rules
- [Console](console.md) — `#[command]` and `#[derive(Command)]` end to end
- [Testing](testing.md) — `#[suprnova_test]`, `expect!`, fakes
