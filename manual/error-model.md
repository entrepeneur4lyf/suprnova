# Error Model

This chapter is the model underneath Suprnova's error handling — the
types, the conversion contract, and the safety guarantees the framework
gives you for free. For day-to-day handler patterns (`?`, returning
errors, building custom domain errors) see [Error Handling](errors.md);
this chapter explains *why* those patterns work the way they do.

If you remember one thing from this page: **errors in Suprnova are
values, not exceptions**. Every error eventually becomes an
`HttpResponse` via a single, total conversion. There is no global
exception handler because there is no global exception.

## The shape

Suprnova's error model has five moving parts:

| Type | Role |
|---|---|
| `Response = Result<HttpResponse, HttpResponse>` | The contract every handler satisfies — both arms are already responses |
| `FrameworkError` | The framework's canonical error enum; every internal error path produces one |
| `AppError` | Ad-hoc domain error for inline use without a dedicated type |
| `HttpError` (trait) | What your own typed domain errors implement to get a status + message |
| `ValidationErrors` | The Laravel/Inertia-shaped error bag for per-field failures |

All five collapse to a single `HttpResponse` through `From` impls. The
`?` operator does the conversion at the call site; the middleware
chain does it at the request boundary; the panic handler does it when
something unwound. There is one body shape for everything, and one
sanitisation rule for 5xx.

## `Response` is `Result<HttpResponse, HttpResponse>`

Every handler returns this:

```rust
pub type Response = Result<HttpResponse, HttpResponse>;
```

Both arms carry the same payload type, which is the whole point. When
the middleware chain finishes executing your handler it collapses the
result with one line:

```rust
result.unwrap_or_else(|e| e)
```

The framework does not need to know whether your handler "succeeded"
or "failed" — both arms are already rendered HTTP responses. The
distinction exists only so `?` can do its job:

```rust
use suprnova::{Request, Response, json_response};

pub async fn show(req: Request) -> Response {
    // `?` short-circuits on Err. Each conversion below produces an
    // HttpResponse via a From impl — the chain collapses both arms.
    let id: i64 = req.param("id")?.parse().map_err(|_| {
        suprnova::FrameworkError::param_parse("id", "i64")
    })?;
    let user = User::find_or_fail(id).await?;  // 404 if missing
    Ok(json_response!({ "user": user }))
}
```

That single contract — every error path produces an `HttpResponse`
through `From` — is the core of the model. Everything else in this
chapter is what the various `From` impls actually do.

### Why Suprnova diverges

Laravel throws exceptions and routes them through a global `Handler`
class registered in `app/Exceptions/Handler.php`. The framework
catches everything, asks the handler "what do I render?", and emits
the response. PHP's unwinding-exception model makes this natural.

Rust has no unwinding exceptions in user code. Suprnova's equivalent
is the `From<FrameworkError> for HttpResponse` impl plus the
`ErrorOccurred` event. The conversion is the renderer; the event is
where you hook observability (Sentry, PagerDuty, structured shippers).
You don't register a handler class — the conversion is a function and
listening for `ErrorOccurred` is the extension point. Same surface,
different machinery.

## `FrameworkError` — the canonical enum

Every error path inside the framework — extractors, route binding,
the container, validation, the database layer, storage — produces a
`FrameworkError`. It's an enum with twelve variants, each tagged with
its HTTP status:

```rust
pub enum FrameworkError {
    ServiceNotFound { type_name: &'static str },        // 500
    ParamError { param_name: String },                   // 400
    ValidationError { field: String, message: String },  // 422
    Database(String),                                    // 500
    Internal { message: String },                        // 500
    Domain { message: String, status_code: u16 },        // *
    Validation(ValidationErrors),                        // 422
    Unauthorized,                                        // 403
    ModelNotFound { model_name: String },                // 404
    ParamParse { param: String, expected_type: &'static str }, // 400
    UnsupportedMediaType,                                // 415
    PrecognitionSuccess,                                 // 204
    PrecognitionFailure(ValidationErrors),               // 422
    AlreadyReported,                                     // CLI-only
}
```

You rarely match on the variant. You construct one through a
convenience constructor and let `?` do the rest:

```rust
use suprnova::FrameworkError;

// All of these produce a FrameworkError with the right status:
FrameworkError::not_found("User");                    // → ModelNotFound, 404
FrameworkError::bad_request("Bad input");             // → Domain, 400
FrameworkError::param("user_id");                     // → ParamError, 400
FrameworkError::param_parse("user_id", "i64");        // → ParamParse, 400
FrameworkError::validation("email", "required");      // → ValidationError, 422
FrameworkError::domain("Conflict", 409);              // → Domain, 409
FrameworkError::internal("disk full");                // → Internal, 500
FrameworkError::database("timeout");                  // → Database, 500
```

There are no `unauthorized()` or `forbidden()` constructors on
`FrameworkError` — `Unauthorized` is a fixed variant carrying the
Laravel "This action is unauthorized." message at 403, and 401 cases
go through `AppError::unauthorized` (next section). Note: the variant
is named `Unauthorized` but the status is 403 because it models
Laravel's authorization rejection, not HTTP authentication.

### Automatic conversion

`FrameworkError` implements `From<sea_orm::DbErr>` and
`From<opendal::Error>` so database and storage errors flow through `?`
without a wrap:

```rust
use suprnova::{DB, FrameworkError};
use sea_orm::ActiveModelTrait;

pub async fn create_user(new_user: ActiveModel) -> Result<Model, FrameworkError> {
    // Both `?` calls here convert into FrameworkError automatically:
    // - DB::get returns Result<_, FrameworkError>
    // - insert returns Result<_, DbErr>, which has From<DbErr> for FrameworkError
    let user = new_user.insert(&*DB::get()?).await?;
    Ok(user)
}
```

If your code returns `Result<_, FrameworkError>`, every common error
your dependencies produce already speaks the right language. The
controller's `?` does no work beyond converting one error type into
another.

### Wrapping context

When you need to re-raise an error with operation context, use
`.context()`:

```rust
db.insert(user).await
    .map_err(FrameworkError::from)
    .map_err(|e| e.context("creating new user"))?;
```

The message becomes `"creating new user: <original>"`. The variant is
preserved where it matters — `Validation`, `ValidationError`,
`PrecognitionFailure`, `Unauthorized`, `ModelNotFound`, and
`ParamParse` keep their structure so the response renderer still emits
the correct shape. Plain message-carrying variants (`Internal`,
`Database`, `Domain`) flatten into a `Domain` with the prefixed
message.

## `AppError` — ad-hoc domain errors

For one-off errors where you don't want to define a dedicated type,
use `AppError`. It implements `HttpError` and has a `From` into
`FrameworkError`, so `?` works directly:

```rust
use suprnova::{AppError, Request, Response, json_response};

pub async fn transfer(req: Request) -> Response {
    let amount: i64 = req.param("amount")?.parse()
        .map_err(|_| AppError::bad_request("amount must be a number"))?;

    if amount <= 0 {
        return Err(AppError::unprocessable("amount must be positive").into());
    }

    if amount > 1_000_000 {
        return Err(AppError::forbidden("amount exceeds daily limit").into());
    }

    Ok(json_response!({ "transferred": amount }))
}
```

The constructors map cleanly onto Laravel's `abort($status, $msg)`
shape:

| `AppError::*` | Status |
|---|---|
| `bad_request(msg)` | 400 |
| `unauthorized(msg)` | 401 |
| `forbidden(msg)` | 403 |
| `not_found(msg)` | 404 |
| `conflict(msg)` | 409 |
| `unprocessable(msg)` | 422 |
| `new(msg)` | 500 |
| `.status(code)` | any |

Note `AppError::unauthorized` is **401** (HTTP authentication missing),
while `FrameworkError::Unauthorized` is **403** (authorization denied,
matching Laravel's policy rejection). They mean different things; pick
the one that matches the failure.

## `HttpError` — custom typed errors

When the same domain error appears in many places, model it as a
type. Implement `HttpError` and the conversion is yours:

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

`HttpError` has two methods, both with defaults:

```rust
pub trait HttpError: std::error::Error + Send + Sync + 'static {
    fn status_code(&self) -> u16 { 500 }
    fn error_message(&self) -> String { self.to_string() }
}
```

### Bridging to `?`

A naive `impl<T: HttpError> From<T> for FrameworkError` would conflict
with the existing `From<AppError>` impl (because `AppError` itself
implements `HttpError`). Suprnova resolves the orphan-rule problem
with an explicit bridge constructor instead:

```rust
use suprnova::{FrameworkError, HttpError};

pub async fn debit(account: &mut Account, amount: i64) -> Result<(), FrameworkError> {
    account.withdraw(amount)
        .map_err(FrameworkError::from_http_error)?;
    Ok(())
}
```

The status code and message are taken from `HttpError::status_code`
and `HttpError::error_message` and stored in a `FrameworkError::Domain`
variant. The response renderer then follows the normal `Domain` path.

### `#[domain_error]` for boilerplate-free types

If you want the typed-error pattern without writing the `Display`,
`Error`, and `HttpError` impls by hand, use the `#[domain_error]`
attribute macro:

```rust
use suprnova::domain_error;

#[domain_error(status = 404, message = "User not found")]
pub struct UserNotFoundError;

#[domain_error(status = 402, message = "Insufficient funds")]
pub struct InsufficientFundsError {
    pub available: i64,
    pub requested: i64,
}
```

`#[domain_error]` generates the full impl set *including*
`From<YourError> for FrameworkError`, so `?` works directly with no
bridge call:

```rust
pub async fn show(req: Request) -> Response {
    let id: i64 = req.param("id")?.parse()
        .map_err(|_| FrameworkError::param_parse("id", "i64"))?;
    let user = User::find(id).await?
        .ok_or_else(|| FrameworkError::from(UserNotFoundError))?;
    Ok(json_response!({ "user": user }))
}
```

The three tiers of custom error story — `AppError` for inline,
`#[domain_error]` for typed-with-macro, hand-rolled `HttpError` for
full control — give you the right tool at every level of formality.

## `ValidationErrors` — the Laravel-shaped error bag

When a request fails validation, Suprnova emits the same JSON shape
Laravel and Inertia front-ends expect:

```json
{
    "message": "The given data was invalid.",
    "errors": {
        "email": ["The email field must be a valid email address."],
        "password": ["The password must be at least 8 characters."]
    },
    "request_id": "8f9e1a2b-c3d4-..."
}
```

You usually don't build this by hand — `#[derive(Validate)]` on a
form request and the `validator` crate behind it produces a
`validator::ValidationErrors` which Suprnova converts via
`ValidationErrors::from_validator`. But the type is public when you
need it:

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

`add_to_bag` scopes errors under a named bag (Laravel's
`withErrors($errors, 'profile')` shape) by prepending the bag name
with a `.` separator:

```rust
let mut errs = ValidationErrors::new();
errs.add_to_bag("profile", "bio", "must be under 280 characters");
errs.add_to_bag("billing", "card", "expired");
// errors map: { "profile.bio": [...], "billing.card": [...] }
```

`retain_fields` keeps only the listed entries — used internally by
Precognition's `Precognition-Validate-Only` header so the server runs
full validation but reports errors only for the fields the client
asked about.

## The conversion contract

When a `FrameworkError` reaches an HTTP boundary it goes through
`From<FrameworkError> for HttpResponse`. Three things happen, in
order:

1. **Status routing**. The variant's `status_code()` is read once.
2. **Logging + observability**. 5xx fires `tracing::error!` and
   dispatches `ErrorOccurred`; 4xx fires `tracing::warn!`. Both carry
   the request id when one is in scope.
3. **Body rendering**. A JSON body in the Laravel shape, sanitised
   for 5xx.

### The body shape

All error bodies follow the same JSON skeleton:

```json
{
    "message": "<human readable>",
    "errors": { "field": ["msg", ...] },
    "request_id": "<uuid>" | null,
    "debug_message": "<dev only>"
}
```

- `message` is always present.
- `errors` only appears for validation-style errors
  (`Validation`, `ValidationError`) — both render the same shape so
  consumers parse one path.
- `request_id` always appears (`null` when outside a request scope —
  e.g. during early boot or in tests with no request context).
- `debug_message` only appears for 5xx when `APP_DEBUG=true`. It is
  strictly additive — production clients must not key on it.

### The 5xx sanitisation rule

This is the safety guarantee worth memorising. For any error with
status ≥ 500, the JSON body's `message` is replaced with the literal
string:

```json
{ "message": "Internal Server Error", "request_id": "..." }
```

The raw error detail does **not** leak to the response body. It goes
to:

- the `tracing::error!` log entry, with the request id and status
- the `ErrorOccurred` event, which any listener can pick up

When `APP_DEBUG=true` (false by default outside `local`/`dev`/`test`),
the response also carries a `debug_message` field with the raw detail
— but `message` stays generic in both modes, so frontends and clients
can't accidentally couple to dev-only data.

This is the contract that lets you call `FrameworkError::internal("db
connection refused: password mismatch on user 'app_rw'")` without
leaking the password to the wire. The `message` you pass is for
operators reading logs; the `message` the client sees is `"Internal
Server Error"`.

For 4xx errors, the caller-facing message is preserved — `404 User
not found`, `400 Missing required parameter: user_id`. These are
domain errors the client needs to act on, not internal failures.

### Where the contract lives

The whole conversion is one function — `impl
From<FrameworkError> for HttpResponse` in
`framework/src/http/response.rs`. Read it once and you've read the
entire error rendering surface of Suprnova. There is no other path.

## The panic boundary

A panic in a middleware or handler would otherwise propagate up the
per-connection task and tear down the hyper service mid-response,
leaving the client with a TCP reset and no HTTP response. Suprnova
catches it.

`execute_chain_safely` in `framework/src/server.rs` wraps the
middleware chain in `AssertUnwindSafe(...).catch_unwind().await`. On
a panic it:

1. Extracts the panic payload (handles `&'static str` and `String`
   payloads; anything else surfaces as `"panic with non-string
   payload"`).
2. Logs `tracing::error!` with the request method, path, and id.
3. Constructs `FrameworkError::internal(format!("request handler
   panicked: {msg}"))` and routes it through the *same*
   `From<FrameworkError> for HttpResponse` conversion every other 5xx
   uses.
4. Echoes the request id back as `X-Request-Id`.

The panic payload stays in the log entry; the client gets the
sanitised `{"message": "Internal Server Error"}` body. Observability
listeners that fire on `ErrorOccurred` for returned 5xx errors also
fire on panics — there is no separate panic-event surface to wire up.

The same panic-recovery pattern is used by:

- WebSocket handlers (`framework/src/server.rs`)
- Scheduled tasks (`framework/src/schedule/mod.rs`)
- Workflows (`framework/src/workflow/mod.rs`)
- The `Supervisor` trait (broadcasting)

A panic in one of these subsystems is logged and either translated to
an error state or auto-restarted; it does not bring down the worker
task.

## Hooking observability with `ErrorOccurred`

`ErrorOccurred` is a built-in event the framework dispatches on every
5xx response (including the ones synthesised from panics):

```rust
pub struct ErrorOccurred {
    pub error_message: String,
    pub status_code: u16,
    pub request_id: Option<String>,
}
```

Listen for it the same way you listen for any event:

```rust
use suprnova::{Event, EventFacade, ErrorOccurred, Listener, FrameworkError};
use std::sync::Arc;

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

This is the Suprnova equivalent of Laravel's `report()` callback on
the global exception handler. The event arrives with the original
unsanitised `error_message` (the body the client sees is still
sanitised), the status code, and the correlatable request id.

## Abort helpers

Three free functions short-circuit a handler at a given status. They
mirror Laravel's `abort` / `abort_if` / `abort_unless`:

```rust
use suprnova::{abort_with, abort_if, abort_unless, Request, Response, json_response};

pub async fn show(req: Request) -> Response {
    abort_unless(req.user().is_some(), 401, "must be logged in")?;
    abort_if(req.param("id")? == "0", 404, "User not found")?;
    abort_with(503, "scheduled maintenance")?;
    Ok(json_response!({ "ok": true }))
}
```

Each returns `Result<(), FrameworkError>`. Use them with `?`. The
underlying error is `FrameworkError::Domain { message, status_code }`,
so it renders through the same body shape and sanitisation rules as
every other error. Out-of-range status codes are coerced to 500 by
the response renderer's status validation; you don't need to defend
against bad input at the call site.

## The CLI sentinel: `AlreadyReported`

One variant of `FrameworkError` has no HTTP meaning. `AlreadyReported`
is constructed via `FrameworkError::silent()` and used by the console
dispatcher when clap has already formatted and printed its own
argument-parse error. The binary's `main` translates the sentinel to
a non-zero exit code without `eprintln`, so users never see two error
messages for the same failure.

If `AlreadyReported` ever reaches an HTTP response converter, it
indicates a request handler accidentally returned `silent()`. The
converter logs a loud `tracing::error!` identifying the leak and
returns a generic 500 — the variant has no business in the request
path, and the loud log makes the bug observable instead of silent.

You don't normally see this variant; it's documented here because the
enum is `HTTP-flavoured` and the otherwise-unexplained variant would
puzzle anyone reading the source.

## Safety guarantees, in summary

The contract Suprnova gives you:

- **Total conversion**. Every `FrameworkError` produces an
  `HttpResponse`. There is no error path that crashes the server or
  drops the connection silently.
- **Sanitised 5xx**. The wire body for any 5xx is the generic
  `{"message": "Internal Server Error", "request_id": "..."}`. Detail
  flows to logs + `ErrorOccurred`.
- **Optional debug visibility**. `APP_DEBUG=true` adds a
  `debug_message` field for 5xx, never `message`. Production clients
  cannot accidentally couple to dev-only data.
- **Correlatable request ids**. Every error body carries the request
  id (or `null` when no request scope exists); the same id appears in
  the log line and the `ErrorOccurred` event.
- **Panic recovery**. Panics in handlers and middleware are caught,
  logged, and routed through the same `From` impl as returned errors.
  No connection drop, no observability gap.
- **One shape for everything**. Validation errors, parameter errors,
  panics, custom domain errors, and storage failures all collapse to
  the same JSON skeleton. Frontend code parses one structure.

## Where each piece lives

| Piece | File |
|---|---|
| `FrameworkError`, `AppError`, `HttpError`, `ValidationErrors` | `framework/src/error.rs` |
| `From<FrameworkError> for HttpResponse` (conversion + sanitisation) | `framework/src/http/response.rs` |
| `abort`, `abort_if`, `abort_unless` | `framework/src/http/abort.rs` |
| `execute_chain_safely` (panic boundary) | `framework/src/server.rs` |
| `ErrorOccurred` event | `framework/src/events/builtins.rs` |
| `#[domain_error]` macro | `suprnova-macros/src/domain_error.rs` |

## Next

- [Error Handling](errors.md) — the practical handler patterns that
  use this model
- [Request Lifecycle](lifecycle.md) — where in the request flow the
  error conversion runs
- [Validation](validation.md) — `#[derive(Validate)]`, form requests,
  and how `ValidationErrors` gets populated
- [Responses](responses.md) — `HttpResponse` builders, headers,
  cookies, streaming
- [Events](events.md) — listening to `ErrorOccurred` and other
  built-in events
