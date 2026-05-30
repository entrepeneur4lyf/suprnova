# Requests

suprnova provides a simple pattern for handling incoming request data with automatic validation. The `#[request]` attribute works with both JSON and form-urlencoded data, making it suitable for REST APIs and HTML forms alike.

## Overview

Requests combine three concerns into a single, declarative struct:
1. **Data parsing** - Automatically parse JSON or form-urlencoded data
2. **Validation** - Validate fields using the `validator` crate
3. **Authorization** - Optionally check if the request is authorized

## The `#[handler]` Attribute

All controller methods in suprnova use the `#[handler]` attribute. This enables automatic extraction and validation of request data:

```rust
use suprnova::{handler, json_response, Request, Response};

// Simple handler with Request
#[handler]
pub async fn index(req: Request) -> Response {
    json_response!({ "message": "Hello" })
}
```

When combined with validated request types, validation happens automatically:

```rust
use suprnova::{handler, json_response, Response};
use crate::requests::CreateUserRequest;

// Request handler - automatically validates incoming data
#[handler]
pub async fn store(form: CreateUserRequest) -> Response {
    // `form` is already validated - this code only runs if validation passes
    json_response!({
        "email": form.email,
        "name": form.name
    })
}
```

## Defining a Request

The `#[request]` attribute automatically adds `Deserialize` and `Validate` derives:

```rust
use suprnova::request;

#[request]
pub struct CreateUserRequest {
    #[validate(email(message = "Please provide a valid email address"))]
    pub email: String,

    #[validate(length(min = 8, message = "Password must be at least 8 characters"))]
    pub password: String,

    #[validate(length(min = 1, max = 100, message = "Name is required"))]
    pub name: String,
}
```

## Validation Rules

suprnova uses the `validator` crate for validation. Here are common validation rules:

### String Validations

```rust
#[request]
pub struct ExampleRequest {
    // Required (non-empty)
    #[validate(length(min = 1, message = "This field is required"))]
    pub name: String,

    // Email format
    #[validate(email(message = "Invalid email address"))]
    pub email: String,

    // URL format
    #[validate(url(message = "Invalid URL"))]
    pub website: String,

    // Length constraints
    #[validate(length(min = 8, max = 100))]
    pub password: String,

    // Regex pattern
    #[validate(regex(path = "PHONE_REGEX", message = "Invalid phone number"))]
    pub phone: String,
}
```

### Numeric Validations

```rust
#[request]
pub struct ProductRequest {
    // Range validation
    #[validate(range(min = 0, max = 10000, message = "Price must be between 0 and 10000"))]
    pub price: f64,

    // Minimum value
    #[validate(range(min = 1))]
    pub quantity: i32,

    // Maximum value
    #[validate(range(max = 100))]
    pub discount_percent: i32,
}
```

### Nested and Collection Validations

```rust
use serde::Deserialize;

#[derive(Deserialize, Validate)]
pub struct Address {
    #[validate(length(min = 1))]
    pub street: String,

    #[validate(length(min = 1))]
    pub city: String,
}

#[request]
pub struct OrderRequest {
    // Nested struct validation
    #[validate(nested)]
    pub shipping_address: Address,

    // Collection length
    #[validate(length(min = 1, message = "At least one item required"))]
    pub items: Vec<String>,
}
```

### Common Validation Attributes

| Attribute | Description | Example |
|-----------|-------------|---------|
| `email` | Valid email format | `#[validate(email)]` |
| `url` | Valid URL format | `#[validate(url)]` |
| `length` | String/collection length | `#[validate(length(min = 1, max = 100))]` |
| `range` | Numeric range | `#[validate(range(min = 0, max = 100))]` |
| `regex` | Regex pattern match | `#[validate(regex(path = "PATTERN"))]` |
| `contains` | String contains substring | `#[validate(contains(pattern = "@"))]` |
| `does_not_contain` | String doesn't contain | `#[validate(does_not_contain(pattern = "admin"))]` |
| `nested` | Validate nested struct | `#[validate(nested)]` |

## Validation Error Response

When validation fails, suprnova automatically returns a 422 response with Laravel/Inertia-compatible error format:

```json
HTTP 422 Unprocessable Entity

{
    "message": "The given data was invalid.",
    "errors": {
        "email": ["Please provide a valid email address"],
        "password": ["Password must be at least 8 characters"]
    }
}
```

This format integrates seamlessly with Inertia.js form handling on the frontend.

## Complete Example

Here's a complete example of a user registration endpoint:

**Define the request:**

```rust
// src/requests/create_user.rs
use suprnova::request;

#[request]
pub struct CreateUserRequest {
    #[validate(email(message = "Please provide a valid email address"))]
    pub email: String,

    #[validate(length(min = 8, message = "Password must be at least 8 characters"))]
    pub password: String,

    #[validate(length(min = 2, max = 50, message = "Name must be between 2 and 50 characters"))]
    pub name: String,
}
```

**Create the controller:**

```rust
// src/controllers/user.rs
use suprnova::{handler, json_response, Request, Response, ResponseExt};
use crate::requests::CreateUserRequest;

#[handler]
pub async fn index(_req: Request) -> Response {
    json_response!({ "users": [] })
}

#[handler]
pub async fn store(form: CreateUserRequest) -> Response {
    // Validation passed - create the user
    // In a real app, you'd save to database here

    json_response!({
        "user": {
            "email": form.email,
            "name": form.name
        },
        "message": "User created successfully"
    })
    .status(201)
}
```

**Register the routes:**

```rust
// src/routes.rs
use suprnova::{get, post, routes};
use crate::controllers;

routes! {
    get!("/users", controllers::user::index).name("users.index"),
    post!("/users", controllers::user::store).name("users.store"),
}
```

## Authorization

You can override the `authorize` method to add authorization checks:

```rust
use suprnova::request;

#[request]
pub struct DeleteUserRequest {
    pub user_id: i64,
}

impl DeleteUserRequest {
    fn authorize(req: &suprnova::Request) -> bool {
        // Check if user has admin role
        // Return false to reject with 403 Forbidden
        req.header("X-Admin-Token").is_some()
    }
}
```

If `authorize` returns `false`, the request is rejected with a 403 Forbidden response:

```json
HTTP 403 Forbidden

{
    "message": "This action is unauthorized."
}
```

## Request Content Types

Requests automatically detect and parse the content type:

- `application/json` - Parsed as JSON
- `application/x-www-form-urlencoded` - Parsed as form data

The parsing is handled automatically based on the `Content-Type` header.

## Using Request with Validated Data

If you need access to both the validated data and the original request (for headers, params, etc.), you can still access request information in your controller:

```rust
use suprnova::{handler, json_response, Response, App};
use crate::requests::CreateUserRequest;
use crate::services::UserService;

#[handler]
pub async fn store(form: CreateUserRequest) -> Response {
    // Access services via dependency injection
    let user_service = App::resolve::<UserService>();

    // Use the validated form data
    let user = user_service.create_user(&form.email, &form.name);

    json_response!({ "user": user })
}
```

## File Organization

The standard structure for requests:

```
src/
├── requests/
│   ├── mod.rs                 # Re-exports all requests
│   ├── create_user.rs         # CreateUserRequest
│   ├── update_user.rs         # UpdateUserRequest
│   └── create_post.rs         # CreatePostRequest
├── controllers/
│   └── user.rs                # Uses CreateUserRequest
└── routes.rs
```

**src/requests/mod.rs:**
```rust
pub mod create_user;
pub mod update_user;

pub use create_user::CreateUserRequest;
pub use update_user::UpdateUserRequest;
```

## End-to-End Type Safety with Inertia

Requests can also derive `InertiaProps` to generate TypeScript types, enabling end-to-end type safety from your Rust backend to your React frontend.

### Generating TypeScript Types for Requests

Add `InertiaProps` derive alongside `#[request]`:

```rust
use suprnova::{request, InertiaProps};

#[request]
#[derive(InertiaProps)]
pub struct CreateTodoRequest {
    #[validate(length(min = 1, message = "Title is required"))]
    pub title: String,

    #[validate(length(max = 500))]
    pub description: Option<String>,
}
```

Run type generation:

```bash
suprnova generate-types
```

This generates TypeScript types in `frontend/src/types/inertia-props.ts`:

```typescript
export interface CreateTodoRequest {
  title: string
  description: string | null
}
```

### Type-Safe Forms with Inertia

Use Inertia's `<Form>` component for the cleanest form handling:

```tsx
import { Form, usePage } from '@inertiajs/react'

export default function CreateTodo() {
  const { errors } = usePage().props

  return (
    <Form action="/todos" method="post">
      <input
        type="text"
        name="title"
        placeholder="Todo title"
      />
      {errors?.title && <span className="error">{errors.title}</span>}

      <textarea
        name="description"
        placeholder="Description (optional)"
      />

      <button type="submit">Create Todo</button>
    </Form>
  )
}
```

For more control, combine `<Form>` with the `useForm` hook and your generated types:

```tsx
import { Form, useForm } from '@inertiajs/react'
import type { CreateTodoRequest } from '../types/inertia-props'

export default function CreateTodo() {
  const { data, setData, errors, processing } = useForm<CreateTodoRequest>({
    title: '',
    description: null,
  })

  return (
    <Form action="/todos" method="post">
      {({ processing }) => (
        <>
          <input
            type="text"
            name="title"
            value={data.title}
            onChange={(e) => setData('title', e.target.value)}
            placeholder="Todo title"
          />
          {errors.title && <span className="error">{errors.title}</span>}

          <textarea
            name="description"
            value={data.description || ''}
            onChange={(e) => setData('description', e.target.value || null)}
            placeholder="Description (optional)"
          />

          <button type="submit" disabled={processing}>
            Create Todo
          </button>
        </>
      )}
    </Form>
  )
}
```

### Benefits of End-to-End Type Safety

1. **Compile-time checks**: TypeScript catches field name typos and type mismatches
2. **IDE autocomplete**: Full IntelliSense for form fields in your editor
3. **Validation alignment**: Your TypeScript types match your Rust validation rules
4. **Refactoring safety**: Rename a field in Rust, TypeScript errors show where to update

### Workflow

1. Define request with validation in Rust
2. Add `#[derive(InertiaProps)]` to the struct
3. Run `suprnova generate-types` to generate TypeScript
4. Use the generated type with `useForm<RequestType>`
5. Get full type safety and validation error handling

> **Note:**
>
> For more information on TypeScript type generation, see [TypeScript Types](frontend-typescript-types.md).


## Request Accessors

Beyond the validated-form pattern above, the `Request` type carries Laravel-style accessors for inspecting the wire-level request — URL, headers, query string, content negotiation, route metadata, and client IP. These are useful in middleware, in handlers that want raw access alongside a `FormRequest`, and in any place where validated parsing isn't the right tool.

### URL and path

| Method | Returns | Notes |
|--------|---------|-------|
| `req.path()` | `&str` | Raw URI path. |
| `req.decoded_path()` | `String` | Path with percent-escapes resolved. |
| `req.segments()` | `Vec<String>` | Path split on `/`, empty segments dropped. |
| `req.segment(index, default)` | `Option<String>` | 1-based segment access. |
| `req.url()` | `String` | Scheme + host + path (no query string). |
| `req.full_url()` | `String` | URL + query string. |
| `req.full_url_with_query(&[("k","v")])` | `String` | Append or override query keys. |
| `req.full_url_without_query(&["k"])` | `String` | Strip query keys. |

```rust
use suprnova::{handler, json_response, Request, Response};

#[handler]
pub async fn show(req: Request) -> Response {
    if req.is(&["admin/*"]) {
        // path matches the admin/* wildcard
    }
    json_response!({ "url": req.full_url() })
}
```

### Host, scheme, IP

| Method | Returns | Source order |
|--------|---------|--------------|
| `req.host()` | `Option<String>` | `X-Forwarded-Host` → `Host` → URI authority. |
| `req.http_host()` | `Option<String>` | Host plus port when non-default. |
| `req.scheme_and_http_host()` | `Option<String>` | `scheme://host:port`. |
| `req.scheme()` | `&'static str` | `"https"` when [`secure`] is true, else `"http"`. |
| `req.secure()` | `bool` | URI scheme → `X-Forwarded-Proto` → `X-Forwarded-Ssl: on`. |
| `req.ip()` | `Option<String>` | `X-Forwarded-For[0]` → `X-Real-IP` → peer addr. |
| `req.ips()` | `Vec<String>` | Full chain: proxy headers, then peer addr. |
| `req.user_agent()` | `Option<&str>` | `User-Agent` header. |
| `req.port()` | `Option<u16>` | Host header port → `X-Forwarded-Port` → URI port. |

### Headers and method

| Method | Returns |
|--------|---------|
| `req.has_header("X-Foo")` | `bool` |
| `req.bearer_token()` | `Option<String>` (last `Bearer ` substring, comma-trimmed) |
| `req.is_method("POST")` | `bool` (case-insensitive) |
| `req.ajax()` | `X-Requested-With: XMLHttpRequest` |
| `req.pjax()` | Truthy `X-PJAX` header |
| `req.prefetch()` | `X-Moz`, `Purpose`, or `Sec-Purpose` = `prefetch` |

### Content negotiation

```rust
if req.is_json() { /* Content-Type carries /json or +json */ }
if req.expects_json() { /* AJAX without Accept narrowing, or Accept prefers JSON */ }
if req.wants_json() { /* Accept header tops with JSON */ }
if req.accepts_html() { /* Accept allows text/html */ }

let preferred = req.prefers(&["application/json", "text/html"]);
let acceptable = req.acceptable_content_types();
```

`accepts(&[ty])` matches both bare types and `application/<vendor>+json`-style suffixes. `accepts_any_content_type()` returns true when there is no Accept header or the top preference is `*/*`.

### Query string

```rust
let id: Option<String> = req.query_param("id");
let map = req.query_params(); // HashMap<String, String>

// Typed query parse via serde
#[derive(serde::Deserialize)]
struct SearchQuery { page: u32, q: String }
let q: SearchQuery = req.query_into()?;
```

### Route metadata

After the router dispatches a request, the matched pattern is recorded on the request:

```rust
if req.route_is(&["users.show", "users.*"]) {
    // we're inside the users.show or users.* route
}

let pattern = req.route_pattern(); // Some("/users/{id}")
let name = req.route_name();       // Some("users.show")
```

`route_is(&[...])` accepts `*` wildcards (Laravel's `Str::is` semantics).

## Aborting early

For early-exit error handling without the full `Response` envelope, the `abort_with` / `abort_if` / `abort_unless` helpers return a `FrameworkError` that renders through the standard `From<FrameworkError> for HttpResponse` pipeline. They compose with `?` directly:

```rust
use suprnova::{abort_if, abort_unless, abort_with, handler, json_response, Request, Response};

#[handler]
pub async fn show(req: Request) -> Response {
    let id = req.param("id")?;

    // 404 when the resource is missing.
    abort_if(id == "0", 404, "User not found")?;

    // 403 when the caller is unauthenticated.
    abort_unless(req.has_header("Authorization"), 403, "Login required")?;

    // Or raise a status unconditionally:
    if some_condition() {
        return Err(abort_with(418, "I'm a teapot").unwrap_err().into());
    }

    json_response!({ "id": id })
}
```

`abort_if` / `abort_unless` return `Ok(())` when the condition is false, so the `?` continues normally.

## Summary

| Feature | Description |
|---------|-------------|
| Define request | `#[request]` attribute |
| Handler attribute | `#[handler]` on all controller methods |
| Validation | Use `#[validate(...)]` attributes |
| Error format | Laravel/Inertia-compatible 422 JSON |
| Authorization | Override `authorize()` method |
| Auto content-type | Detects JSON vs form-urlencoded |
| URL accessors | `url`, `full_url`, `segments`, `decoded_path` |
| Host/IP | `host`, `http_host`, `ip`, `ips`, `secure` |
| Headers | `has_header`, `bearer_token`, `user_agent` |
| Content negotiation | `accepts`, `prefers`, `is_json`, `wants_json` |
| Query string | `query_param`, `query_params`, `query_into` |
| Route metadata | `route_pattern`, `route_name`, `route_is` |
| Abort helpers | `abort_with`, `abort_if`, `abort_unless` |
| TypeScript types | Add `#[derive(InertiaProps)]` for type generation |
| Type-safe forms | Use generated types with `useForm<T>` |

## Untyped input bag — divergence from Laravel

Laravel exposes a synchronous, merged input bag — `$req->input('field')`, `$req->all()`, `$req->only(['a','b'])`, `$req->boolean('flag')` — pulled from query string and parsed body together. Suprnova **does not** ship that surface. The reason:

- Suprnova's body is consume-once, async, and typed via `FormRequest`. Forcing a synchronous `all()` would require reading the body up front, which has memory and DoS implications very different from PHP's per-request lifecycle.
- The typed alternative (`#[request]` + `FormRequest`) gives compile-time field names, validation, and content-type-aware parsing — exactly the safety net PHP's untyped bag lacks.

The accessors above (`query_param`, `query_into`, `bearer_token`, header readers) cover the cases where Laravel users reach for the bag against query / header / route state. For body-side access, define a `#[request]` struct.
