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

Errors are returned as JSON with appropriate structure:

```json
// Parameter error (400)
{
  "error": "Missing required parameter: user_id"
}

// Validation error (422)
{
  "error": "Validation failed",
  "field": "email",
  "message": "Invalid email format"
}

// Generic error
{
  "error": "Error message here"
}
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

## Summary

| Feature | Usage |
|---------|-------|
| JSON response | `HttpResponse::json(value)` or `json_response!({...})` |
| Text response | `HttpResponse::text(str)` or `text_response!(str)` |
| Set status | `.status(code)` |
| Add header | `.header(name, value)` |
| Simple redirect | `Redirect::to(path).into()` |
| Named redirect | `redirect!("route.name").into()` |
| With route params | `.with("key", "value")` |
| With query params | `.query("key", "value")` |
| Permanent redirect | `.permanent()` |
| Not found error | `AppError::not_found(msg)` |
| Bad request | `AppError::bad_request(msg)` |
| Unauthorized | `AppError::unauthorized(msg)` |
| Custom status | `AppError::new(msg).status(code)` |
