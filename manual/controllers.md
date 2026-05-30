# Controllers

suprnova controllers are async functions that handle HTTP requests and return responses. Following Laravel's conventions, controllers organize your application's request handling logic into dedicated modules, making your codebase clean and maintainable.

## Generating Controllers

The fastest way to create a new controller is using the suprnova CLI:

```bash
suprnova make:controller User
```

This command will:
1. Create `src/controllers/user.rs` with a controller stub
2. Update `src/controllers/mod.rs` to export the new controller

```bash Examples
# Creates user.rs in src/controllers/
suprnova make:controller User

# Creates product.rs in src/controllers/
suprnova make:controller Product

# Controller name is converted to snake_case for the file
suprnova make:controller OrderItem  # Creates order_item.rs
```

```rust Generated File
//! User controller

use suprnova::{handler, json_response, Request, Response};

#[handler]
pub async fn invoke(_req: Request) -> Response {
    json_response!({
        "controller": "User"
    })
}
```

## Controller Structure

Controllers are async functions decorated with `#[handler]` that take a `Request` and return a `Response`:

```rust
use suprnova::{handler, Request, Response, json_response};

#[handler]
pub async fn index(_req: Request) -> Response {
    json_response!({
        "message": "Hello from controller"
    })
}
```

### The Handler Attribute

All controller methods use the `#[handler]` attribute. This enables:
- Automatic extraction and validation of request data
- Clean integration with FormRequests for POST data validation
- Future DX improvements like dependency injection

```rust
#[handler]
pub async fn handler_name(request: Request) -> Response
```

- **`#[handler]`**: Required attribute that enables automatic parameter extraction
- **`async fn`**: Controllers are asynchronous, allowing non-blocking I/O operations
- **`Request`**: Contains all information about the incoming HTTP request
- **`Response`**: An alias for `Result<HttpResponse, HttpResponse>`, enabling the `?` operator

> **Note:**
>
> For handling POST data with validation, see [Requests](requests.md).


## Path Parameter Injection

The `#[handler]` macro supports automatic extraction of path parameters directly as function arguments. This eliminates the need to manually call `req.param()`.

### Primitive Path Parameters

Declare path parameters as function arguments with primitive types:

```rust
use suprnova::{handler, json_response, Response};

// Route: get!("/users/{id}", controllers::user::show)
#[handler]
pub async fn show(id: i32) -> Response {
    json_response!({
        "user_id": id
    })
}

// Route: get!("/posts/{slug}", controllers::post::show)
#[handler]
pub async fn show_by_slug(slug: String) -> Response {
    json_response!({
        "slug": slug
    })
}
```

### Multiple Path Parameters

Extract multiple parameters in a single handler:

```rust
// Route: get!("/posts/{post_id}/comments/{comment_id}", handler)
#[handler]
pub async fn show_comment(post_id: i32, comment_id: i32) -> Response {
    json_response!({
        "post_id": post_id,
        "comment_id": comment_id
    })
}
```

### Supported Types

| Type | Description |
|------|-------------|
| `i32`, `i64` | Signed integers |
| `u32`, `u64`, `usize` | Unsigned integers |
| `String` | String values |

> **Note:**
>
> The parameter name in your function must match the route parameter name (e.g., `{id}` requires `id: i32`).


## Route Model Binding

Route model binding automatically resolves database models from route parameters. If the model is not found, suprnova automatically returns a 404 response.

### Setting Up Route Binding

First, enable route binding for your model using the `route_binding!` macro:

```rust
// src/models/user.rs
use sea_orm::entity::prelude::*;
use suprnova::{route_binding, Model, ModelMut};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub email: String,
}

impl suprnova::Model for Entity {}
impl suprnova::ModelMut for Entity {}

// Enable route model binding - "user" matches {user} in routes
route_binding!(Entity, Model, "user");
```

### Using Route Model Binding in Handlers

Now you can directly accept the model as a handler parameter:

```rust
use suprnova::{handler, json_response, Response};
use crate::models::user;

// Route: get!("/users/{user}", controllers::user::show)
#[handler]
pub async fn show(user: user::Model) -> Response {
    // user is automatically fetched from the database
    // Returns 404 if not found
    json_response!({
        "id": user.id,
        "name": user.name,
        "email": user.email
    })
}
```

### Combining Models with Form Requests

Mix route model binding with form requests for update operations:

```rust
use suprnova::{handler, json_response, Response};
use crate::models::user;
use crate::requests::UpdateUserRequest;

// Route: put!("/users/{user}", controllers::user::update)
#[handler]
pub async fn update(user: user::Model, form: UpdateUserRequest) -> Response {
    // user: auto-fetched from database (404 if not found)
    // form: auto-validated from request body

    json_response!({
        "updated_user_id": user.id,
        "new_name": form.name
    })
}
```

### Error Responses

| Error | Status Code | Description |
|-------|-------------|-------------|
| Model not found | 404 | The requested resource doesn't exist |
| Invalid parameter | 400 | Parameter couldn't be parsed (e.g., "abc" as i32) |
| Missing parameter | 400 | Required route parameter is missing |

### Flexibility

You have full control over how you access route parameters:

```rust
// Option 1: Request passthrough (manual extraction)
#[handler]
pub async fn show(req: Request) -> Response {
    let id = req.param("id")?;
    // Manual database query...
}

// Option 2: Primitive path params (no model binding)
#[handler]
pub async fn show(id: i32) -> Response {
    // Just the ID, no automatic model fetch
}

// Option 3: Route model binding (auto-fetch with 404)
#[handler]
pub async fn show(user: user::Model) -> Response {
    // Model automatically fetched from database
}

// Option 4: Mix primitives and models
#[handler]
pub async fn nested(category_id: i32, product: product::Model) -> Response {
    // category_id: just extracted as i32
    // product: auto-fetched from database
}
```

## The Request Object

The `Request` struct provides Laravel-like access to request data:

### Getting Route Parameters

Access dynamic URL segments defined in your routes:

```rust
use suprnova::{Request, Response, json_response};

// Route: get!("/users/{id}", controllers::user::show)
pub async fn show(req: Request) -> Response {
    // Using ? operator - returns 400 error if param missing
    let id = req.param("id")?;

    json_response!({
        "user_id": id
    })
}
```

For routes with multiple parameters:

```rust
// Route: get!("/posts/{post_id}/comments/{comment_id}", handler)
pub async fn show_comment(req: Request) -> Response {
    let post_id = req.param("post_id")?;
    let comment_id = req.param("comment_id")?;

    json_response!({
        "post_id": post_id,
        "comment_id": comment_id
    })
}
```

### Getting Headers

Access HTTP headers from the request:

```rust
pub async fn index(req: Request) -> Response {
    // Get a specific header (returns Option<&str>)
    let auth = req.header("Authorization");
    let content_type = req.header("Content-Type");

    if let Some(token) = auth {
        // Process authenticated request
    }

    json_response!({"status": "ok"})
}
```

### Request Methods

| Method | Return Type | Description |
|--------|-------------|-------------|
| `method()` | `&Method` | HTTP method (GET, POST, etc.) |
| `path()` | `&str` | Request path (e.g., `/users/123`) |
| `param(name)` | `Result<&str, ParamError>` | Get a route parameter |
| `params()` | `&HashMap<String, String>` | Get all route parameters |
| `header(name)` | `Option<&str>` | Get a header value |
| `is_inertia()` | `bool` | Check if Inertia.js request |
| `inertia_version()` | `Option<&str>` | Get Inertia version |

## Creating Responses

Controllers return responses using helper methods and macros:

### JSON Responses

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

```rust
use suprnova::{text_response, Request, Response};

pub async fn health(_req: Request) -> Response {
    text_response!("OK")
}
```

### Setting Status Codes

```rust
use suprnova::{json_response, Request, Response, ResponseExt};

pub async fn store(_req: Request) -> Response {
    // Create user...

    json_response!({"id": 1, "created": true})
        .status(201)
}
```

> **Note:**
>
> For more response options, see the [Responses documentation](responses.md).


## RESTful Controllers

suprnova encourages organizing controllers following REST conventions:

```rust
// src/controllers/user.rs
use suprnova::{json_response, redirect, Request, Response, ResponseExt};

/// GET /users - List all users
pub async fn index(_req: Request) -> Response {
    json_response!({
        "users": [
            {"id": 1, "name": "John"},
            {"id": 2, "name": "Jane"}
        ]
    })
}

/// GET /users/{id} - Show a specific user
pub async fn show(req: Request) -> Response {
    let id = req.param("id")?;

    json_response!({
        "id": id,
        "name": format!("User {}", id)
    })
}

/// POST /users - Create a new user
pub async fn store(_req: Request) -> Response {
    // Create user logic...

    json_response!({"id": 1, "created": true})
        .status(201)
}

/// PUT /users/{id} - Update a user
pub async fn update(req: Request) -> Response {
    let id = req.param("id")?;

    // Update user logic...

    json_response!({
        "id": id,
        "updated": true
    })
}

/// DELETE /users/{id} - Delete a user
pub async fn destroy(req: Request) -> Response {
    let _id = req.param("id")?;

    // Delete user logic...

    redirect!("users.index").into()
}
```

Register these in your routes:

```rust
// src/routes.rs
use suprnova::{get, post, put, delete, routes};
use crate::controllers;

routes! {
    get!("/users", controllers::user::index).name("users.index"),
    get!("/users/{id}", controllers::user::show).name("users.show"),
    post!("/users", controllers::user::store).name("users.store"),
    put!("/users/{id}", controllers::user::update).name("users.update"),
    delete!("/users/{id}", controllers::user::destroy).name("users.destroy"),
}
```

## Error Handling in Controllers

Use the `?` operator for clean error propagation:

```rust
use suprnova::{AppError, Request, Response, json_response};

pub async fn show(req: Request) -> Response {
    // Returns 400 if param is missing
    let id = req.param("id")?;

    // Simulate database lookup
    let user = find_user(id).await?;

    json_response!({
        "user": user
    })
}

async fn find_user(id: &str) -> Result<User, AppError> {
    // Database query...
    if id == "999" {
        return Err(AppError::not_found("User not found"));
    }
    Ok(User { id: id.to_string(), name: "John".to_string() })
}
```

> **Note:**
>
> For more error handling options, see the [Responses documentation](responses.md).


## Dependency Injection

Use `App::resolve()` to inject dependencies from the container:

```rust
use suprnova::{App, Request, Response, json_response};
use crate::actions::UserService;

pub async fn index(_req: Request) -> Response {
    // Resolve a service from the container
    let user_service = App::resolve::<UserService>();

    let users = user_service.list_all();

    json_response!({
        "users": users
    })
}
```

## File Organization

The standard file structure for controllers:

```
src/
├── controllers/
│   ├── mod.rs          # Re-export all controllers
│   ├── home.rs         # Home controller
│   ├── user.rs         # User controller
│   ├── product.rs      # Product controller
│   └── api/            # Nested API controllers
│       ├── mod.rs
│       └── user.rs     # API user controller
├── routes.rs           # Route definitions
└── main.rs
```

**src/controllers/mod.rs:**
```rust
pub mod home;
pub mod user;
pub mod product;
pub mod api;
```

**src/controllers/user.rs:**
```rust
use suprnova::{json_response, Request, Response};

pub async fn index(_req: Request) -> Response {
    json_response!({"controller": "user"})
}

pub async fn show(req: Request) -> Response {
    let id = req.param("id")?;
    json_response!({"id": id})
}
```

## Practical Examples

### API Controller with Validation

```rust
use suprnova::{AppError, Request, Response, json_response, ResponseExt};

pub async fn store(req: Request) -> Response {
    // Get required header
    let content_type = req.header("Content-Type")
        .ok_or_else(|| AppError::bad_request("Content-Type header required"))?;

    if !content_type.contains("application/json") {
        return Err(AppError::bad_request("Content-Type must be application/json").into());
    }

    // Process the request...

    json_response!({"created": true})
        .status(201)
}
```

### Controller with Redirects

```rust
use suprnova::{redirect, route, Request, Response};

pub async fn store(_req: Request) -> Response {
    // Create resource...

    // Redirect to named route
    redirect!("users.index").into()
}

pub async fn update(req: Request) -> Response {
    let id = req.param("id")?;

    // Update resource...

    // Redirect with route parameter
    redirect!("users.show")
        .with("id", id)
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

## Summary

| Feature | Usage |
|---------|-------|
| Generate controller | `suprnova make:controller Name` |
| Handler attribute | `#[handler]` on all controller methods |
| Handler signature | `pub async fn name(req: Request) -> Response` |
| Path param handler | `pub async fn name(id: i32) -> Response` |
| Route model binding | `pub async fn name(user: user::Model) -> Response` |
| FormRequest handler | `pub async fn name(form: FormRequest) -> Response` |
| Mixed params | `pub async fn name(user: user::Model, form: UpdateRequest) -> Response` |
| Enable route binding | `route_binding!(Entity, Model, "param_name")` |
| Get route param | `req.param("id")?` |
| Get all params | `req.params()` |
| Get header | `req.header("Authorization")` |
| Get HTTP method | `req.method()` |
| Get path | `req.path()` |
| JSON response | `json_response!({...})` |
| Text response | `text_response!("...")` |
| Set status | `.status(201)` |
| Redirect | `redirect!("route.name").into()` |
