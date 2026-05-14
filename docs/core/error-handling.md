---
title: 'Error Handling'
description: 'Handle errors elegantly with automatic HTTP response conversion'
icon: 'circle-exclamation'
---

suprnova provides a comprehensive error handling system that integrates seamlessly with Rust's `Result` type and the `?` operator. Errors are automatically converted to appropriate HTTP responses, making error handling clean and consistent throughout your application.

## The Response Type

suprnova's controller handlers return `Response`, which is an alias for `Result<HttpResponse, HttpResponse>`:

```rust
pub type Response = Result<HttpResponse, HttpResponse>;
```

This design enables:
- Using the `?` operator for automatic error conversion
- Returning successful responses with `Ok(HttpResponse::...)`
- Returning error responses with `Err(HttpResponse::...)`

## Quick Error Handling with `?`

The most common pattern is using the `?` operator, which automatically converts errors to HTTP responses:

```rust
use suprnova::{Request, Response, json_response};

pub async fn show(req: Request) -> Response {
    // Returns 400 if param missing
    let id = req.param("id")?;

    // Returns 500 if database fails
    let user = find_user(id).await?;

    json_response!({
        "user": user
    })
}
```

## Error Types

### AppError

`AppError` is a simple wrapper for creating inline/ad-hoc errors with custom status codes:

```rust
use suprnova::AppError;

// Basic error (500 Internal Server Error)
let error = AppError::new("Something went wrong");

// With custom status code
let error = AppError::new("Rate limited").status(429);
```

#### AppError Helper Methods

| Method | Status Code | Description |
|--------|-------------|-------------|
| `AppError::new(msg)` | 500 | Internal Server Error |
| `AppError::not_found(msg)` | 404 | Resource Not Found |
| `AppError::bad_request(msg)` | 400 | Bad Request |
| `AppError::unauthorized(msg)` | 401 | Unauthorized |
| `AppError::forbidden(msg)` | 403 | Forbidden |
| `AppError::unprocessable(msg)` | 422 | Unprocessable Entity |
| `AppError::conflict(msg)` | 409 | Conflict |

```rust
use suprnova::{AppError, Request, Response, json_response};

pub async fn show(req: Request) -> Response {
    let id = req.param("id")?;

    let user = find_user(id).await
        .ok_or_else(|| AppError::not_found("User not found"))?;

    json_response!({
        "user": user
    })
}
```

### FrameworkError

`FrameworkError` is suprnova's comprehensive error enum that handles all framework-level errors:

```rust
use suprnova::FrameworkError;

pub enum FrameworkError {
    ServiceNotFound { type_name: &'static str },
    ParamError { param_name: String },
    ValidationError { field: String, message: String },
    Database(String),
    Internal { message: String },
    Domain { message: String, status_code: u16 },
}
```

#### FrameworkError Factory Methods

| Method | Status Code | Use Case |
|--------|-------------|----------|
| `FrameworkError::service_not_found::<T>()` | 500 | DI container resolution failed |
| `FrameworkError::param(name)` | 400 | Missing route parameter |
| `FrameworkError::validation(field, msg)` | 422 | Validation failed |
| `FrameworkError::database(msg)` | 500 | Database operation failed |
| `FrameworkError::internal(msg)` | 500 | Generic internal error |
| `FrameworkError::domain(msg, status)` | Custom | Domain-specific error |

```rust
use suprnova::FrameworkError;

pub async fn process() -> Result<Data, FrameworkError> {
    if !is_valid {
        return Err(FrameworkError::validation("email", "Invalid email format"));
    }

    if quota_exceeded {
        return Err(FrameworkError::domain("Quota exceeded", 429));
    }

    Ok(data)
}
```

## Automatic Error Conversion

suprnova automatically converts common error types to `FrameworkError`:

### Database Errors

SeaORM's `DbErr` converts automatically:

```rust
use suprnova::{DB, FrameworkError};
use sea_orm::ActiveModelTrait;

pub async fn create_user(user: User) -> Result<User, FrameworkError> {
    // DbErr automatically converts to FrameworkError::Database
    let saved = user.insert(&*DB::get()?).await?;
    Ok(saved)
}
```

### Parameter Errors

Route parameter extraction returns errors usable with `?`:

```rust
pub async fn show(req: Request) -> Response {
    // Returns 400 with JSON error if "id" param is missing
    let id = req.param("id")?;

    json_response!({
        "id": id
    })
}
```

### AppError to FrameworkError

`AppError` converts to `FrameworkError::Domain`:

```rust
use suprnova::{AppError, FrameworkError};

let app_error = AppError::not_found("User not found");
let framework_error: FrameworkError = app_error.into();
// Results in: FrameworkError::Domain { message: "User not found", status_code: 404 }
```

## Error Response Format

Errors are automatically converted to JSON responses:

### Parameter Error (400)
```json
{
  "error": "Missing required parameter: user_id"
}
```

### Validation Error (422)
```json
{
  "error": "Validation failed",
  "field": "email",
  "message": "Invalid email format"
}
```

### Generic Error
```json
{
  "error": "Error message here"
}
```

## Creating Custom Domain Errors

### Generating Errors with CLI

The fastest way to create a custom domain error is using the suprnova CLI:

```bash
suprnova make:error UserNotFound
```

This command will:
1. Create `src/errors/user_not_found.rs` with a domain error struct
2. Create or update `src/errors/mod.rs` to export the new error

```bash Examples
# Creates user_not_found.rs in src/errors/
suprnova make:error UserNotFound

# Creates payment_failed.rs in src/errors/
suprnova make:error PaymentFailed

# Error name is converted to snake_case for the file
suprnova make:error InsufficientStock  # Creates insufficient_stock.rs
```

```rust Generated File
//! UserNotFound error

use suprnova::domain_error;

#[domain_error(status = 500, message = "User not found")]
pub struct UserNotFound;
```

### The `#[domain_error]` Macro

The `#[domain_error]` macro automatically implements all necessary traits for HTTP error handling:

- Derives `Debug` and `Clone`
- Implements `Display`, `Error`, and `HttpError` traits
- Implements `From<T> for FrameworkError` for seamless `?` usage

```rust
use suprnova::domain_error;

// Basic usage - defaults to 500 status code
#[domain_error(status = 404, message = "User not found")]
pub struct UserNotFoundError;

// With custom status code
#[domain_error(status = 429, message = "Rate limit exceeded")]
pub struct RateLimitError;

// With fields for additional context
#[domain_error(status = 404, message = "Resource not found")]
pub struct ResourceNotFoundError {
    pub resource_type: String,
    pub resource_id: i32,
}
```

Use generated errors in controllers with the `?` operator:

```rust
use crate::errors::user_not_found::UserNotFound;

pub async fn show(req: Request) -> Response {
    let id = req.param("id")?;

    let user = find_user(id).await
        .ok_or(UserNotFound)?;  // Automatically converts to 404 response

    json_response!({ "user": user })
}
```

### Using AppError

For simple, inline errors:

```rust
use suprnova::{AppError, Request, Response, json_response};

pub async fn transfer(req: Request) -> Response {
    let amount: f64 = req.param("amount")?.parse()
        .map_err(|_| AppError::bad_request("Invalid amount"))?;

    if amount <= 0.0 {
        return Err(AppError::bad_request("Amount must be positive").into());
    }

    if amount > balance {
        return Err(AppError::unprocessable("Insufficient funds").into());
    }

    // Process transfer...
    json_response!({ "success": true })
}
```

### Implementing HttpError Trait

For reusable domain errors, implement the `HttpError` trait:

```rust
use suprnova::HttpError;

#[derive(Debug)]
pub struct UserNotFoundError {
    pub user_id: i32,
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

    // Optional: customize error message
    fn error_message(&self) -> String {
        format!("The requested user with ID {} does not exist", self.user_id)
    }
}
```

### Creating Error Enums

For complex applications, create dedicated error enums:

```rust
use suprnova::{AppError, FrameworkError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OrderError {
    #[error("Order {0} not found")]
    NotFound(i32),

    #[error("Insufficient stock for product {product_id}")]
    InsufficientStock { product_id: i32 },

    #[error("Payment failed: {0}")]
    PaymentFailed(String),

    #[error("Order already shipped")]
    AlreadyShipped,
}

impl From<OrderError> for FrameworkError {
    fn from(err: OrderError) -> Self {
        let (message, status) = match &err {
            OrderError::NotFound(_) => (err.to_string(), 404),
            OrderError::InsufficientStock { .. } => (err.to_string(), 422),
            OrderError::PaymentFailed(_) => (err.to_string(), 402),
            OrderError::AlreadyShipped => (err.to_string(), 409),
        };
        FrameworkError::Domain {
            message,
            status_code: status,
        }
    }
}
```

Use in controllers:

```rust
pub async fn cancel_order(req: Request) -> Response {
    let order_id: i32 = req.param("id")?.parse()
        .map_err(|_| AppError::bad_request("Invalid order ID"))?;

    let order = find_order(order_id).await
        .ok_or(OrderError::NotFound(order_id))?;

    if order.shipped {
        return Err(OrderError::AlreadyShipped.into());
    }

    // Cancel order...
    json_response!({ "cancelled": true })
}
```

## Common Error Patterns

### Early Returns with `?`

```rust
pub async fn create_post(req: Request) -> Response {
    let user_id = req.param("user_id")?;
    let user = App::resolve::<UserService>()?.find(user_id).await?;

    if !user.can_post() {
        return Err(AppError::forbidden("User cannot create posts").into());
    }

    let post = App::resolve::<PostService>()?.create(&user).await?;

    json_response!({ "post": post }).status(201)
}
```

### Validation Errors

```rust
use suprnova::{FrameworkError, Request, Response, json_response};

pub async fn register(req: Request) -> Response {
    let email = req.param("email")?;
    let password = req.param("password")?;

    // Validate email
    if !email.contains('@') {
        return Err(FrameworkError::validation("email", "Invalid email format").into());
    }

    // Validate password
    if password.len() < 8 {
        return Err(FrameworkError::validation(
            "password",
            "Password must be at least 8 characters"
        ).into());
    }

    // Create user...
    json_response!({ "success": true }).status(201)
}
```

### Resource Not Found

```rust
use suprnova::{AppError, Request, Response, json_response};

pub async fn show(req: Request) -> Response {
    let id: i32 = req.param("id")?.parse()
        .map_err(|_| AppError::bad_request("Invalid ID format"))?;

    let item = repository.find(id).await
        .ok_or_else(|| AppError::not_found(format!("Item {} not found", id)))?;

    json_response!({ "item": item })
}
```

### Chaining Operations

```rust
pub async fn process_payment(req: Request) -> Response {
    let order_id = req.param("order_id")?;

    // Chain multiple fallible operations
    let order = find_order(order_id).await?;
    let payment = validate_payment(&order).await?;
    let receipt = process_payment(payment).await?;
    let confirmation = send_confirmation(&receipt).await?;

    json_response!({ "confirmation": confirmation })
}
```

### Conditional Errors

```rust
pub async fn update(req: Request) -> Response {
    let id = req.param("id")?;
    let user = get_current_user(&req)?;
    let resource = find_resource(id).await?;

    // Authorization check
    if resource.owner_id != user.id && !user.is_admin {
        return Err(AppError::forbidden("You don't have permission to update this resource").into());
    }

    // Update resource...
    json_response!({ "updated": true })
}
```

## Error Handling in Actions

Actions can return `Result<T, FrameworkError>` for clean error propagation:

```rust
use suprnova::{injectable, FrameworkError};
use suprnova::database::{Model, ModelMut};

#[injectable]
pub struct UserService;

impl UserService {
    pub async fn find_by_email(&self, email: &str) -> Result<User, FrameworkError> {
        users::Entity::find()
            .filter(users::Column::Email.eq(email))
            .one(&*DB::get()?)
            .await?
            .ok_or_else(|| FrameworkError::domain("User not found", 404))
    }

    pub async fn create(&self, data: CreateUser) -> Result<User, FrameworkError> {
        // Validation
        if data.email.is_empty() {
            return Err(FrameworkError::validation("email", "Email is required"));
        }

        // Check for duplicates
        if self.find_by_email(&data.email).await.is_ok() {
            return Err(FrameworkError::conflict("Email already exists").into());
        }

        // Create user
        let user = users::ActiveModel {
            email: Set(data.email),
            ..Default::default()
        };

        users::Entity::insert_one(user).await
    }
}
```

Use in controllers:

```rust
pub async fn store(req: Request) -> Response {
    let service = App::resolve::<UserService>()?;
    let user = service.create(data).await?;

    json_response!({ "user": user }).status(201)
}
```

## Summary

| Feature | Usage |
|---------|-------|
| Generate error | `suprnova make:error ErrorName` |
| Domain error macro | `#[domain_error(status = 404, message = "...")]` |
| Quick error | `AppError::new("message")` |
| Not found | `AppError::not_found("message")` |
| Bad request | `AppError::bad_request("message")` |
| Unauthorized | `AppError::unauthorized("message")` |
| Forbidden | `AppError::forbidden("message")` |
| Validation | `FrameworkError::validation("field", "message")` |
| Custom status | `AppError::new("msg").status(code)` |
| Domain error | `FrameworkError::domain("msg", status)` |
| Convert to Response | `.into()` or `?` operator |
| Custom error type | Implement `HttpError` trait |
| Error propagation | Use `?` operator |
| File location | `src/errors/` |
