# Error Handling

This is the day-to-day patterns guide for writing fallible code in
Suprnova handlers, services, and middleware. For the underlying model
— the conversion contract, the panic boundary, the 5xx sanitisation
rule, observability hooks — read [Error Model](error-model.md). This
chapter shows what to actually type.

The shape to remember:

- Handlers return `Response = Result<HttpResponse, HttpResponse>`.
- The `?` operator collapses `FrameworkError`, `AppError`, `DbErr`,
  `ParamError`, `ValidationErrors`, and any typed `HttpError` into an
  `HttpResponse` automatically.
- Three free helpers (`abort_with`, `abort_if`, `abort_unless`) let
  you short-circuit at a status code without naming an error type.

```rust
use suprnova::{Request, Response, json_response};

pub async fn show(req: Request) -> Response {
    let id = req.param("id")?;          // 400 if missing
    let user = find_user(id).await?;    // 500 on DbErr, 404 on Option::None
    json_response!({ "user": user })
}
```

The rest of the chapter is the catalogue of error producers — what
to construct, what status it returns, what shape the client sees.

## `?` is the conversion

Every `?` in a handler body runs `From<E> for HttpResponse`. The
framework wires those impls so the things you actually call return
errors that already know how to render. You don't write the
conversion; you write the failure.

```rust
use suprnova::{DB, FrameworkError, Request, Response, json_response};
use sea_orm::EntityTrait;

pub async fn show(req: Request) -> Response {
    let id: i64 = req.param("id")?.parse()
        .map_err(|_| FrameworkError::param_parse("id", "i64"))?;

    let user = users::Entity::find_by_id(id)
        .one(&*DB::get()?)
        .await?
        .ok_or_else(|| FrameworkError::not_found("User"))?;

    json_response!({ "user": user })
}
```

Three things happen in that snippet — none of them are visible:

1. `req.param("id")?` → `ParamError` → `FrameworkError::ParamError` (400).
2. `.await?` on a SeaORM call → `DbErr` → `FrameworkError::Database` (500,
   sanitised on the wire).
3. `.ok_or_else(...)?` constructs a `FrameworkError::ModelNotFound`
   directly (404).

All three pass through the same `From<FrameworkError> for HttpResponse`
impl described in [Error Model](error-model.md).

## `AppError` — inline domain errors

Use `AppError` for one-off errors that don't deserve a dedicated type.
The constructors map onto Laravel's `abort($status, $msg)` shape:

| Constructor | Status |
|---|---|
| `AppError::new(msg)` | 500 |
| `AppError::bad_request(msg)` | 400 |
| `AppError::unauthorized(msg)` | 401 |
| `AppError::forbidden(msg)` | 403 |
| `AppError::not_found(msg)` | 404 |
| `AppError::conflict(msg)` | 409 |
| `AppError::unprocessable(msg)` | 422 |
| `AppError::new(msg).status(code)` | any |

`AppError` has a `From` into `FrameworkError`, so `?` works with no
ceremony:

```rust
use suprnova::{AppError, Request, Response, json_response};

pub async fn transfer(req: Request) -> Response {
    let amount: i64 = req.param("amount")?.parse()
        .map_err(|_| AppError::bad_request("amount must be a number"))?;

    if amount <= 0 {
        return Err(AppError::unprocessable("amount must be positive").into());
    }

    if amount > balance() {
        return Err(AppError::forbidden("amount exceeds daily limit").into());
    }

    json_response!({ "transferred": amount })
}
```

Note the asymmetry: `AppError::unauthorized` is **401** (missing
authentication credentials), while `FrameworkError::Unauthorized` is
**403** (policy denied an authenticated user). They mean different
things; pick the one that matches the failure.

## `FrameworkError` — the canonical enum

Internal extractors, the container, route binding, validation, the
database layer, and storage all produce `FrameworkError`. You usually
construct one through a convenience constructor and let `?` route it.

```rust
use suprnova::FrameworkError;

FrameworkError::not_found("User");                    // 404
FrameworkError::bad_request("Bad input");             // 400
FrameworkError::param("user_id");                     // 400
FrameworkError::param_parse("user_id", "i64");        // 400
FrameworkError::validation("email", "required");      // 422
FrameworkError::domain("Conflict", 409);              // 409 (any code)
FrameworkError::internal("disk full");                // 500
FrameworkError::database("timeout");                  // 500
FrameworkError::service_not_found::<MyService>();     // 500
FrameworkError::model_not_found("Post");              // 404
```

The full variant set, with implications for the response shape, is in
[Error Model](error-model.md). The constructors above cover every
common case; you reach for the variants directly only when matching on
an error you received.

### Automatic conversions

`FrameworkError` already speaks the dialects your dependencies emit.
Both of these `?`s convert automatically:

```rust
use suprnova::{DB, FrameworkError};
use sea_orm::ActiveModelTrait;

pub async fn create_user(new_user: users::ActiveModel)
    -> Result<users::Model, FrameworkError>
{
    // DB::get returns Result<_, FrameworkError>.
    // .insert returns Result<_, DbErr>, with From<DbErr> for FrameworkError.
    let user = new_user.insert(&*DB::get()?).await?;
    Ok(user)
}
```

The framework also implements `From<opendal::Error>` for storage
operations and `From<ParamError>` for path-parameter extraction.

### Re-raising with context

When you want to annotate where an error came from without losing the
status code, use `.context()`:

```rust
db.insert(user).await
    .map_err(FrameworkError::from)
    .map_err(|e| e.context("creating new user"))?;
```

The message becomes `"creating new user: <original>"`. Structured
variants (`Validation`, `ValidationError`, `ModelNotFound`,
`ParamParse`, `PrecognitionFailure`, `Unauthorized`) keep their
variant so the response renderer still emits the right shape; flat
message-carrying variants (`Internal`, `Database`, `Domain`) flatten
into a `Domain` with the prefixed message and the original status
preserved.

### Turning duplicate-key errors into 422

The `Unique` validation rule runs a `SELECT COUNT(*)` before the
write, so it's advisory — two concurrent requests can both pass and
then both attempt the insert. The losing request gets a database
unique-constraint violation, which would otherwise leak as a 500.
`from_unique_violation` translates it into the same 422 the advisory
rule would have produced:

```rust
use suprnova::FrameworkError;

let user = new_user.insert(db).await.map_err(|e| {
    FrameworkError::from_unique_violation(
        "email",
        "That email address is already registered.",
        e,
    )
})?;
```

If the underlying `DbErr` isn't a unique-constraint violation it
passes through unchanged as a 500-class `Database` error. Backend
coverage is whatever SeaORM's `DbErr::sql_err` recognises — Postgres,
MySQL/MariaDB, and SQLite all map their duplicate-key errors through.

## Custom domain errors

Three tiers, depending on how reusable the error needs to be.

### `#[domain_error]` for the typed case

Most reusable errors want a name, a fixed status, and a fixed message
template — no per-call message. The `#[domain_error]` attribute macro
generates `Display`, `std::error::Error`, `HttpError`, and `From` for
`FrameworkError` in one shot:

```rust
use suprnova::domain_error;

#[domain_error(status = 404, message = "User not found")]
pub struct UserNotFound;

#[domain_error(status = 402, message = "Insufficient funds")]
pub struct InsufficientFunds {
    pub available: i64,
    pub requested: i64,
}
```

Use them at the call site with `?`:

```rust
use crate::errors::user_not_found::UserNotFound;

pub async fn show(req: Request) -> Response {
    let id: i64 = req.param("id")?.parse()
        .map_err(|_| FrameworkError::param_parse("id", "i64"))?;

    let user = find_user(id).await
        .ok_or_else(|| FrameworkError::from(UserNotFound))?;

    json_response!({ "user": user })
}
```

The macro rejects malformed attributes loudly at compile time —
overflowed status codes (`status = 70_000`), wrong literal types
(`message = 42`), unknown keys — so you can't silently get the wrong
status because of a typo.

#### Scaffold one with the CLI

```bash
suprnova make:error UserNotFound
```

Writes `src/errors/user_not_found.rs` with a default `status = 500`
and an inferred sentence-cased message, and updates `src/errors/mod.rs`
to re-export it. Edit the `status` and `message` to taste.

### `HttpError` for the hand-rolled case

When a domain error needs runtime state in the message (e.g. the IDs
involved in the failure), implement `HttpError` directly. The trait
has two methods with sensible defaults:

```rust
use suprnova::HttpError;

#[derive(Debug)]
pub struct InsufficientFunds {
    pub available: i64,
    pub requested: i64,
}

impl std::fmt::Display for InsufficientFunds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Insufficient funds: have {}, need {}",
            self.available, self.requested)
    }
}

impl std::error::Error for InsufficientFunds {}

impl HttpError for InsufficientFunds {
    fn status_code(&self) -> u16 { 402 }
    fn error_message(&self) -> String {
        format!("Need {} units, only {} available.",
            self.requested, self.available)
    }
}
```

To bridge a hand-rolled `HttpError` into `?`, call
`FrameworkError::from_http_error`. A blanket `From<T: HttpError> for
FrameworkError` would conflict with the existing `From<AppError>`
impl, so the bridge is an explicit constructor:

```rust
account.withdraw(amount)
    .map_err(FrameworkError::from_http_error)?;
```

### Error enums for one module's failures

When a service has several related failures, group them in an enum
and write one `From` for the whole enum:

```rust
use suprnova::FrameworkError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OrderError {
    #[error("Order {0} not found")]
    NotFound(i64),

    #[error("Insufficient stock for product {product_id}")]
    InsufficientStock { product_id: i64 },

    #[error("Payment failed: {0}")]
    PaymentFailed(String),

    #[error("Order already shipped")]
    AlreadyShipped,
}

impl From<OrderError> for FrameworkError {
    fn from(err: OrderError) -> Self {
        let status = match &err {
            OrderError::NotFound(_) => 404,
            OrderError::InsufficientStock { .. } => 422,
            OrderError::PaymentFailed(_) => 402,
            OrderError::AlreadyShipped => 409,
        };
        FrameworkError::Domain {
            message: err.to_string(),
            status_code: status,
        }
    }
}
```

Once the `From` exists, the enum threads through `?` the same as any
other error type.

## `abort_with` / `abort_if` / `abort_unless`

Three helpers short-circuit a handler at a status. They mirror
Laravel's `abort` / `abort_if` / `abort_unless`. (The free function is
exported as `abort_with` rather than `abort` to keep the latter
available as a method name on user types.)

```rust
use suprnova::{abort_if, abort_unless, abort_with, Request, Response, json_response};

pub async fn show(req: Request) -> Response {
    abort_unless(req.user().is_some(), 401, "must be logged in")?;
    abort_if(req.param("id")? == "0", 404, "User not found")?;
    abort_with(503, "scheduled maintenance")?;

    json_response!({ "ok": true })
}
```

Each returns `Result<(), FrameworkError>`, so `?` does the work. The
underlying error is `FrameworkError::Domain { message, status_code }`,
which renders through the same body shape as every other error. Out-of-range
status codes are coerced to 500 by the response renderer; you don't need to
defend against bad input at the call site.

## `ValidationErrors` — the Laravel-shaped error bag

When validation fails — at `#[derive(Validate)]` time or in an
`after_validation` body — the framework emits the JSON shape Laravel
and Inertia front-ends expect:

```json
{
    "message": "The given data was invalid.",
    "errors": {
        "email": ["The email field must be a valid email address."],
        "password": ["The password field must be at least 8 characters."]
    },
    "request_id": "8f9e1a2b-c3d4-..."
}
```

Most of the time you don't construct this directly — `#[derive(Validate)]`
runs and the framework converts `validator::ValidationErrors` for
you. When you need to add errors imperatively (cross-field rules, async
uniqueness checks that complement `Unique`), build a `ValidationErrors`
and return it:

```rust
use suprnova::{FrameworkError, ValidationErrors};

pub async fn after_validation(payload: &Signup) -> Result<(), FrameworkError> {
    let mut errs = ValidationErrors::new();

    if payload.email.ends_with("@example.com") {
        errs.add("email", "example.com addresses are not allowed");
    }
    if payload.password == payload.email {
        errs.add("password", "password must not match email");
    }

    errs.into_result().map_err(FrameworkError::Validation)
}
```

`add_to_bag` scopes a field under a named bag (Laravel's
`withErrors($errors, 'profile')` shape) by prepending the bag with a
`.` separator. Useful when one response carries errors from multiple
sub-forms that can't share a flat namespace:

```rust
let mut errs = ValidationErrors::new();
errs.add_to_bag("profile", "bio", "must be under 280 characters");
errs.add_to_bag("billing", "card", "expired");
// errors map: { "profile.bio": [...], "billing.card": [...] }
```

`from_validator(ve)` converts a `validator::ValidationErrors`;
`retain_fields(&keep)` returns a copy containing only the listed
entries (used by Precognition's `Precognition-Validate-Only` header
internally).

## Hooking observability with `ErrorOccurred`

Every 5xx response fires an `ErrorOccurred` event — including the
ones synthesised from panics. Listen the same way you listen for any
event:

```rust
use std::sync::Arc;
use suprnova::{ErrorOccurred, EventFacade, FrameworkError, Listener};

pub struct SentryReporter;

#[suprnova::async_trait]
impl Listener<ErrorOccurred> for SentryReporter {
    async fn handle(&self, evt: &ErrorOccurred) -> Result<(), FrameworkError> {
        sentry::capture_message(&evt.error_message, sentry::Level::Error);
        Ok(())
    }
}

// In bootstrap.rs:
EventFacade::listen::<ErrorOccurred>(Arc::new(SentryReporter)).await?;
```

The event carries the raw error message (the wire body is still
sanitised — see [Error Model](error-model.md)), the status, and the
correlatable request id. This is Suprnova's equivalent of Laravel's
`report()` callback on the exception handler.

## Patterns you'll write a lot

### Parse a path parameter as a typed value

```rust
let id: i64 = req.param("id")?.parse()
    .map_err(|_| FrameworkError::param_parse("id", "i64"))?;
```

`ParamError` already converts to 400; `param_parse` is the parse-failure
equivalent and renders the same shape.

### Look up by ID, 404 on absent

```rust
let user = users::Entity::find_by_id(id)
    .one(&*DB::get()?)
    .await?
    .ok_or_else(|| FrameworkError::not_found("User"))?;
```

Or, with the Eloquent layer:

```rust
let user = User::find_or_fail(id).await?;
```

`find_or_fail` is `find(id).ok_or(ModelNotFound)` packaged up.

### Authorize an action

```rust
let user = req.user().ok_or(AppError::unauthorized("login required"))?;
abort_unless(post.owner_id == user.id || user.is_admin, 403,
    "you don't own this post")?;
```

`abort_unless` returns `Result<(), FrameworkError>`; the `?` collapses
it back into your handler's error arm.

### Service returning typed errors

```rust
use suprnova::{App, FrameworkError, injectable};

#[injectable]
pub struct UserService;

impl UserService {
    pub async fn find_by_email(&self, email: &str)
        -> Result<users::Model, FrameworkError>
    {
        users::Entity::find()
            .filter(users::Column::Email.eq(email))
            .one(&*DB::get()?)
            .await?
            .ok_or_else(|| FrameworkError::not_found("User"))
    }
}

// Call site:
pub async fn show(req: Request) -> Response {
    let email = req.param("email")?;
    let user = App::resolve::<UserService>()?
        .find_by_email(email)
        .await?;
    json_response!({ "user": user })
}
```

`App::resolve::<UserService>()?` returns `Result<Arc<UserService>,
FrameworkError>`. The chained `?` collapses both the resolve failure
and the lookup failure to a response.

## Cheat sheet

| You want… | Reach for |
|---|---|
| Inline error with a status | `AppError::bad_request("…")` and friends |
| Typed reusable error | `#[domain_error(status = …, message = "…")]` |
| Generated scaffold | `suprnova make:error UserNotFound` |
| Hand-rolled with runtime state | `impl HttpError for MyError` |
| Bridge hand-rolled into `?` | `FrameworkError::from_http_error(e)` |
| Short-circuit at a status | `abort_with` / `abort_if` / `abort_unless` |
| 404 on missing model | `FrameworkError::not_found("User")` / `Model::find_or_fail` |
| Parse-failure on path param | `FrameworkError::param_parse("id", "i64")` |
| Field-level validation error | `FrameworkError::validation("email", "…")` |
| Multi-field error bag | `ValidationErrors::new().add(…)` + `Validation(errs)` |
| Duplicate-key violation → 422 | `FrameworkError::from_unique_violation(field, msg, e)` |
| Annotate an existing error | `err.context("creating user")` |
| Observe every 5xx | Listen for `ErrorOccurred` |

## Next

- [Error Model](error-model.md) — variants, conversion contract,
  5xx sanitisation, panic boundary
- [Validation](validation.md) — `#[derive(Validate)]`, form requests,
  and `after_validation`
- [Responses](responses.md) — `HttpResponse` builders, status, headers
- [Events](events.md) — listening to `ErrorOccurred` and other
  built-in events
- [Request Lifecycle](lifecycle.md) — where in the request flow the
  error conversion runs
