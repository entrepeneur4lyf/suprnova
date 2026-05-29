---
title: 'Responses'
description: 'Create HTTP responses, handle errors, and perform redirects in suprnova'
icon: 'reply'
---

suprnova provides a flexible response system inspired by Laravel, allowing you to create JSON responses, text responses, redirects, and handle errors elegantly. The `Response` type enables using Rust's `?` operator for clean error propagation.

## The Response Type

In suprnova, controller handlers return a `Response` type, which is an alias for `Result<HttpResponse, HttpResponse>`:

```rust
pub type Response = Result<HttpResponse, HttpResponse>;
```

This design allows you to:
- Return successful responses with `Ok(HttpResponse::...)`
- Return error responses with `Err(HttpResponse::...)`
- Use the `?` operator for automatic error conversion

## Creating Responses

### JSON Responses

The most common response type. Use `HttpResponse::json()` or the `json_response!` macro:

```rust
use suprnova::{HttpResponse, Response, Request};
use serde_json::json;

pub async fn index(_req: Request) -> Response {
    // Using HttpResponse directly
    Ok(HttpResponse::json(json!({
        "users": [
            {"id": 1, "name": "John"},
            {"id": 2, "name": "Jane"}
        ]
    })))
}
```

Or with the convenient `json_response!` macro:

```rust
use suprnova::{json_response, Request, Response};

pub async fn index(_req: Request) -> Response {
    json_response!({
        "users": [
            {"id": 1, "name": "John"},
            {"id": 2, "name": "Jane"}
        ]
    })
}
```

### Text Responses

For plain text responses:

```rust
use suprnova::{HttpResponse, Response, Request};

pub async fn health(_req: Request) -> Response {
    Ok(HttpResponse::text("OK"))
}
```

Or with the `text_response!` macro:

```rust
use suprnova::{text_response, Request, Response};

pub async fn health(_req: Request) -> Response {
    text_response!("OK")
}
```

### Setting Status Codes

Chain `.status()` to set the HTTP status code:

```rust
use suprnova::{json_response, HttpResponse, Response, ResponseExt};

// On HttpResponse directly
pub async fn created(_req: Request) -> Response {
    Ok(HttpResponse::json(json!({"id": 1, "created": true}))
        .status(201))
}

// With macros using ResponseExt trait
pub async fn created_macro(_req: Request) -> Response {
    json_response!({"id": 1, "created": true})
        .status(201)
}
```

### Adding Headers

Add custom headers with `.header()`:

```rust
use suprnova::{HttpResponse, Response};

pub async fn download(_req: Request) -> Response {
    Ok(HttpResponse::text("file content")
        .header("Content-Disposition", "attachment; filename=\"data.txt\"")
        .header("X-Custom-Header", "value"))
}
```

## Redirects

suprnova provides two ways to create redirects:

### Simple Redirects

Redirect to a specific URL or path:

```rust
use suprnova::{Redirect, Response};

pub async fn legacy(_req: Request) -> Response {
    Redirect::to("/new-path").into()
}
```

### Named Route Redirects

Redirect to a named route using the `redirect!` macro (with compile-time route validation):

```rust
use suprnova::{redirect, Response};

pub async fn store(_req: Request) -> Response {
    // Create user...

    // Redirect to users.index route
    redirect!("users.index").into()
}
```

### Redirects with Parameters

Add route parameters and query strings:

```rust
use suprnova::{redirect, Response};

pub async fn update(_req: Request) -> Response {
    // Update user...

    // Redirect to users.show with route parameter
    redirect!("users.show")
        .with("id", "123")
        .into()
}

pub async fn search(_req: Request) -> Response {
    // Redirect with query parameters
    redirect!("users.index")
        .query("page", "1")
        .query("sort", "name")
        .into()
}
```

### Permanent Redirects

Use `.permanent()` for 301 redirects (default is 302):

```rust
use suprnova::{Redirect, Response};

pub async fn old_route(_req: Request) -> Response {
    Redirect::to("/new-route")
        .permanent()
        .into()
}
```

## Error Handling

suprnova automatically converts errors to appropriate HTTP responses when using the `?` operator.

### Using the `?` Operator

Errors are automatically converted to JSON error responses:

```rust
use suprnova::{Request, Response};

pub async fn show(req: Request) -> Response {
    // This returns a 400 error if the parameter is missing
    let id = req.param("id")?;

    json_response!({
        "user_id": id
    })
}
```

### AppError for Custom Errors

Use `AppError` for domain-specific errors with custom status codes:

```rust
use suprnova::{AppError, FrameworkError, Response};

pub async fn find_user(id: i32) -> Result<User, FrameworkError> {
    let user = db.find(id);

    if user.is_none() {
        return Err(AppError::not_found("User not found").into());
    }

    Ok(user.unwrap())
}
```

### AppError Helper Methods

| Method | Status Code | Use Case |
|--------|-------------|----------|
| `AppError::new(msg)` | 500 | Generic server error |
| `AppError::not_found(msg)` | 404 | Resource not found |
| `AppError::bad_request(msg)` | 400 | Invalid request |
| `AppError::unauthorized(msg)` | 401 | Authentication required |
| `AppError::forbidden(msg)` | 403 | Access denied |
| `AppError::unprocessable(msg)` | 422 | Validation failed |
| `AppError::conflict(msg)` | 409 | Resource conflict |

### Custom Status Codes

Set any status code with `.status()`:

```rust
use suprnova::AppError;

let error = AppError::new("Rate limited")
    .status(429);
```

### FrameworkError Types

suprnova's `FrameworkError` handles common error scenarios:

```rust
use suprnova::FrameworkError;

// Service not found (500)
FrameworkError::service_not_found::<MyService>();

// Missing parameter (400)
FrameworkError::param("user_id");

// Validation error (422)
FrameworkError::validation("email", "Invalid email format");

// Database error (500)
FrameworkError::database("Connection failed");

// Internal error (500)
FrameworkError::internal("Unexpected error");

// Custom domain error
FrameworkError::domain("Custom error", 418);
```

### Error Response Format

Errors are returned as JSON in Laravel's canonical envelope: `message`, an optional `errors` map (for validation-style per-field detail), and a `request_id` correlating to the structured logs.

```json
// Parameter error (400)
{
  "message": "Missing required parameter: user_id",
  "request_id": "01J3T7..."
}

// Validation error (422)
{
  "message": "The given data was invalid.",
  "errors": {
    "email": ["Invalid email format"],
    "password": ["Password must be at least 8 characters"]
  },
  "request_id": "01J3T7..."
}

// Generic 4xx
{
  "message": "Error message here",
  "request_id": "01J3T7..."
}

// 5xx (production)
{
  "message": "Internal Server Error",
  "request_id": "01J3T7..."
}

// 5xx (APP_DEBUG=true)
{
  "message": "Internal Server Error",
  "debug_message": "actual error detail",
  "request_id": "01J3T7..."
}
```

In production (`APP_DEBUG=false`), 5xx responses always emit the generic `Internal Server Error` message; the underlying detail flows to logs and the `ErrorOccurred` event but never to the wire. With `APP_DEBUG=true`, a `debug_message` field is added for development visibility — frontends MUST NOT key on this field, because `message` stays generic in both modes.

### Aborting from a handler

`abort_with` / `abort_if` / `abort_unless` (re-exported from the crate root) produce a `FrameworkError` that renders through the same envelope above. They're the idiomatic Rust equivalent of Laravel's `abort($code, $message)`:

```rust
use suprnova::{abort_if, abort_unless};

abort_if(id == "0", 404, "User not found")?;
abort_unless(authorized, 403, "Forbidden")?;
```

### Implementing HttpError Trait

For custom error types, implement the `HttpError` trait:

```rust
use suprnova::HttpError;

#[derive(Debug)]
struct UserNotFoundError {
    user_id: i32,
}

impl std::fmt::Display for UserNotFoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "User {} not found", self.user_id)
    }
}

impl std::error::Error for UserNotFoundError {}

impl HttpError for UserNotFoundError {
    fn status_code(&self) -> u16 {
        404
    }
}
```

## Response Macros

suprnova provides convenient macros for creating responses:

| Macro | Description | Example |
|-------|-------------|---------|
| `json_response!` | Create a JSON response | `json_response!({"key": "value"})` |
| `text_response!` | Create a text response | `text_response!("Hello")` |
| `redirect!` | Redirect to named route | `redirect!("users.index").into()` |

## Complete Example

```rust
use suprnova::{
    json_response, redirect, text_response,
    AppError, FrameworkError, HttpResponse,
    Request, Response, ResponseExt,
};

// List users
pub async fn index(_req: Request) -> Response {
    json_response!({
        "users": [
            {"id": 1, "name": "Alice"},
            {"id": 2, "name": "Bob"}
        ]
    })
}

// Show a specific user
pub async fn show(req: Request) -> Response {
    let id = req.param("id")?;

    // Simulate user lookup
    if id == "999" {
        return Err(HttpResponse::json(serde_json::json!({
            "error": "User not found"
        })).status(404));
    }

    json_response!({
        "id": id,
        "name": format!("User {}", id)
    })
}

// Create a user
pub async fn store(_req: Request) -> Response {
    // ... create user logic ...

    // Return 201 Created
    json_response!({"id": 1, "created": true})
        .status(201)
}

// Delete and redirect
pub async fn destroy(_req: Request) -> Response {
    // ... delete logic ...

    redirect!("users.index").into()
}

// Custom error example
pub async fn process(_req: Request) -> Response {
    let result = do_something()?;

    if !result.is_valid() {
        return Err(AppError::unprocessable("Invalid data").into());
    }

    json_response!({"success": true})
}
```

## Bulk headers and cookies

Both `HttpResponse` and the `Response` (`Result`) chain expose Laravel-style bulk-header and cookie builders.

```rust
use suprnova::{Cookie, HttpResponse, Response, ResponseExt};

// On HttpResponse directly:
let r = HttpResponse::text("ok")
    .with_headers([
        ("X-A", "1"),
        ("X-B", "2"),
        ("X-Rate-Limit", "60"),
    ])
    .with_cookies([
        Cookie::new("session", "abc"),
        Cookie::new("user_id", "42"),
    ])
    .without_header("X-Internal-Debug")
    .without_cookie("legacy_session");

// Same methods are available through the ResponseExt chain on `Response`:
let r: Response = Ok(HttpResponse::text("ok"))
    .with_headers([("X-A", "1"), ("X-B", "2")])
    .cookie(Cookie::new("session", "abc"));
```

| Method | Behavior |
|--------|---------|
| `.header(name, value)` | Append a header (allows duplicates for `Set-Cookie`). |
| `.replace_header(name, value)` | Collapse any prior values and set one. |
| `.with_headers([(k, v), ...])` | Append many at once. Accepts `HashMap`, `Vec`, or array literal. |
| `.without_header(name)` | Remove all occurrences of a header (case-insensitive). |
| `.header_value(name)` | Read back the first-set header (useful in tests). |
| `.cookie(Cookie)` | Attach one cookie. |
| `.with_cookies([Cookie, ...])` | Attach many. |
| `.without_cookie(name)` | Schedule deletion (`Cookie::forget`-equivalent). |

## Redirect builders

`Redirect` covers Laravel's full redirector surface — including session-aware flows.

### Targets

```rust
use suprnova::Redirect;

Redirect::to("/dashboard");           // explicit URL or path
Redirect::route("users.show").with("id", "42"); // named route + params
Redirect::away("https://external.example.com");  // explicit external URL
Redirect::refresh("/current/path");   // re-display current path
Redirect::back("/login");             // session.previous_url, fallback to "/login"
Redirect::intended("/home");          // session pull("url.intended"), one-shot
```

`Redirect::back` and `Redirect::intended` read the session if one is available; without a session scope they cleanly fall through to the supplied default.

To pre-populate the intended target (typically from auth middleware before redirecting to `/login`):

```rust
Redirect::set_intended_url("/admin/users");
```

### Status

```rust
Redirect::to("/x").permanent();          // 301
Redirect::to("/x").status(303);          // 303 (See Other), or 307 / 308
```

### Session flashes

Redirect builders carry their own flash bag that's drained into the session on conversion to `Response`. This lets handlers attach status messages, repopulate form input, and surface validation errors all in one chain:

```rust
Redirect::back("/users/new")
    .with("status", "User created")        // single key/value
    .with_input([                          // repopulate form
        ("email", "shawn@example.com"),
        ("name", "Shawn"),
    ])
    .with_errors([                         // default error bag
        ("email", "Must be unique"),
    ])
    .with_errors_bag("login", [            // named error bag
        ("password", "Required"),
    ])
```

The receiving page reads these back through `session.get(...)` (for `with(...)`), `session.get_old_input(...)` (for `with_input(...)`), and the bag map drained by `session.pull_errors_flash()` (for `with_errors(...)` / `with_errors_bag(...)`). The Inertia response layer consumes the errors-flash automatically — the `errors` prop on every Inertia response is seeded from the session, so a `Redirect::back().with_errors(...)` flow surfaces messages on the destination page without any extra handler wiring. The `X-Inertia-Error-Bag` request header still scopes the prop under a named bag for multi-form pages.

### Cookies, headers, fragments

```rust
use suprnova::Cookie;

Redirect::route("billing.show")
    .with_cookies([Cookie::new("welcome", "yes")])
    .with_headers([("X-Trace", "abc")])
    .with_fragment("invoices")    // append #invoices
    .without_fragment()           // OR strip any prior #fragment
```

`with_fragment` accepts the fragment with or without a leading `#`. Calling `with_fragment` after `without_fragment` re-attaches one.

### Preserve fragment across the redirect

For Inertia apps where the destination should preserve the *originating* URL hash (`#section`), use `preserve_fragment`:

```rust
Redirect::route("dashboard.index").preserve_fragment().into()
```

This flashes `_inertia.preserve_fragment = true` into the session; the next Inertia response reads it and emits `preserveFragment: true` in its page object.

## Summary

| Feature | Usage |
|---------|-------|
| JSON response | `HttpResponse::json(value)` or `json_response!({...})` |
| Text response | `HttpResponse::text(str)` or `text_response!(str)` |
| HTML response | `HttpResponse::html(str)` |
| SSE stream | `HttpResponse::sse(stream)` |
| Set status | `.status(code)` |
| Add header | `.header(name, value)` |
| Bulk headers | `.with_headers([(k, v), ...])` |
| Remove header | `.without_header(name)` |
| Attach cookie | `.cookie(Cookie)` |
| Bulk cookies | `.with_cookies([Cookie, ...])` |
| Forget cookie | `.without_cookie(name)` |
| Simple redirect | `Redirect::to(path).into()` |
| Named redirect | `redirect!("route.name").into()` or `Redirect::route("name")` |
| Back redirect | `Redirect::back(fallback)` |
| Intended redirect | `Redirect::intended(default)` |
| Set intended target | `Redirect::set_intended_url(url)` |
| External URL | `Redirect::away(url)` |
| Refresh | `Redirect::refresh(path)` |
| With route params | `.with("key", "value")` |
| With query params | `.query("key", "value")` |
| Flash data | `.with(key, value)` |
| Flash input | `.with_input([(k, v), ...])` |
| Flash errors | `.with_errors([(k, msg), ...])` |
| Named error bag | `.with_errors_bag(bag, [(k, msg)])` |
| Cookies on redirect | `.cookie(c)`, `.with_cookies([...])` |
| Headers on redirect | `.header(k, v)`, `.with_headers([...])` |
| Append fragment | `.with_fragment("section")` |
| Strip fragment | `.without_fragment()` |
| Permanent redirect | `.permanent()` |
| Custom status | `.status(303)` |
| Abort early | `abort_with(code, msg)`, `abort_if(cond, code, msg)`, `abort_unless(cond, code, msg)` |
| Not found error | `AppError::not_found(msg)` |
| Bad request | `AppError::bad_request(msg)` |
| Unauthorized | `AppError::unauthorized(msg)` |
| Custom status (error) | `AppError::new(msg).status(code)` |
