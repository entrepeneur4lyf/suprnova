# Requests

Suprnova handlers receive a `Request` — the wire-level HTTP request — or
a typed form-request struct that parses, validates, and authorizes the
body before your code runs. Both paths live on the same `#[handler]`
macro; you pick the shape per route. This chapter covers both, plus the
multipart upload extractor and the raw accessors you reach for in
middleware.

## Typed form requests

The `#[request]` attribute marks a struct as a `FormRequest`. The macro
adds `serde::Deserialize` and `validator::Validate` derives and emits an
`impl FormRequest` so the `#[handler]` macro knows to extract and
validate it on the way in:

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

A handler that names this type as its parameter is handed an
already-validated value:

```rust
use suprnova::{handler, json_response, Response};
use crate::requests::CreateUserRequest;

#[handler]
pub async fn store(form: CreateUserRequest) -> Response {
    // `form` is validated — this code only runs if every rule passed.
    json_response!({ "email": form.email, "name": form.name })
}
```

A handler that names `Request` instead gets the raw request through
unchanged:

```rust
use suprnova::{handler, json_response, Request, Response};

#[handler]
pub async fn index(req: Request) -> Response {
    json_response!({ "path": req.path() })
}
```

Both are extractors — the `#[handler]` macro looks up
`FromRequest::from_request` for every parameter type, and any struct
that implements `FormRequest` gets a blanket `FromRequest` impl for
free.

## Validation rules

Validation runs through the `validator` crate. Common rules:

### String validations

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

### Numeric validations

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

### Nested and collection validations

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

### Common validation attributes

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

## Validation error responses

When validation fails, Suprnova returns a 422 response with the
Laravel / Inertia-compatible error bag:

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

The `errors` shape matches what `@inertiajs/*` clients read from
`usePage().props.errors` directly.

## Complete example

A user registration endpoint, end to end.

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

## Authorization and cross-field hooks

The `FormRequest` trait exposes three lifecycle hooks: `authorize`,
`after_validation`, and `after_validation_async`. The `#[request]`
attribute emits the `impl FormRequest` block for you, so overriding any
of them means dropping to the derive form (`FormRequestDerive`) and
writing the impl yourself — you cannot add a second `impl FormRequest`
alongside one the attribute already wrote.

```rust
use suprnova::{FormRequest, FormRequestDerive, Request};
use serde::Deserialize;
use validator::Validate;

#[derive(Deserialize, Validate, FormRequestDerive)]
pub struct DeleteUserRequest {
    pub user_id: i64,
}

impl FormRequest for DeleteUserRequest {
    fn authorize(req: &Request) -> bool {
        // Return false to short-circuit with 403 Forbidden before the
        // body is read.
        req.header("X-Admin-Token").is_some()
    }
}
```

When `authorize` returns `false`, extraction returns
`FrameworkError::Unauthorized` and renders:

```json
HTTP 403 Forbidden

{ "message": "This action is unauthorized." }
```

`after_validation` is the synchronous cross-field hook — use it for
rules like "password and confirmation must match". `after_validation_async`
is the asynchronous counterpart and is where database-backed rules
(e.g. the built-in `Unique`) participate in automatic validation. Both
fire after the per-field `validator` rules pass; `extract` bails at the
first failing stage.

```rust
use suprnova::{FormRequest, FormRequestDerive, ValidationErrors};
use serde::Deserialize;
use validator::Validate;

#[derive(Deserialize, Validate, FormRequestDerive)]
pub struct UpdatePasswordRequest {
    #[validate(length(min = 8))]
    pub new_password: String,
    pub confirmation: String,
}

impl FormRequest for UpdatePasswordRequest {
    fn after_validation(&self) -> Result<(), ValidationErrors> {
        if self.new_password != self.confirmation {
            let mut errs = ValidationErrors::new();
            errs.add("confirmation", "passwords do not match");
            return Err(errs);
        }
        Ok(())
    }
}
```

### Body size caps

The per-struct `#[form_request(max_body_bytes = N)]` attribute
overrides the process-global 8 MiB cap on a single FormRequest:

```rust
use suprnova::FormRequestDerive;
use serde::Deserialize;
use validator::Validate;

#[derive(Deserialize, Validate, FormRequestDerive)]
#[form_request(max_body_bytes = 64 * 1024 * 1024)] // 64 MiB
pub struct ImportPayload {
    pub rows: Vec<Row>,
}

#[derive(Deserialize, Validate)]
pub struct Row { /* ... */ }
```

`Content-Length` is parsed up front and the request is rejected with
HTTP 413 *before* a body byte is read when the declared size exceeds
the cap; clients that lie about `Content-Length` still trip the
streaming byte counter during read.

## Content type detection

`FormRequest::extract` looks only at the `Content-Type` header:

- `application/x-www-form-urlencoded` → parsed via `serde_urlencoded`
- `application/json` or any `application/*+json` suffix → parsed via `serde_json`
- Anything else (including a missing header) → rejected with HTTP 415
  Unsupported Media Type, before the body is read

For multipart bodies (`multipart/form-data`), see
[file uploads](#file-uploads-multipartrequest) below.

## Reading the body directly

For one-off endpoints or middleware that doesn't want a full
`FormRequest`, the `Request` type itself reads the body in three flavors
— each consumes `self` because the body can be read at most once:

```rust
use serde::Deserialize;
use suprnova::{handler, json_response, Request, Response};

#[derive(Deserialize)]
struct LoginForm { username: String, password: String }

#[handler]
pub async fn login(req: Request) -> Response {
    // Pick the parser explicitly.
    let form: LoginForm = req.form().await?;
    json_response!({ "user": form.username })
}

#[handler]
pub async fn webhook(req: Request) -> Response {
    // Same shape, JSON wire.
    let payload: serde_json::Value = req.json().await?;
    json_response!({ "received": payload })
}

#[handler]
pub async fn ingest(req: Request) -> Response {
    // Auto-pick based on Content-Type — JSON unless
    // `application/x-www-form-urlencoded` is explicit.
    let value: serde_json::Value = req.input().await?;
    json_response!({ "value": value })
}
```

For raw access, `req.body_bytes().await` returns the buffered `Bytes`
plus the `RequestParts` metadata (route params and content type). Use
`body_bytes_with_cap(n)` to override the global 8 MiB cap on a
case-by-case basis.

## Resolving services alongside the form

Validated form requests compose with the [service container](container.md).
Use `App::resolve::<T>()` (or `App::get::<T>()`) inside the handler:

```rust
use suprnova::{handler, json_response, Response, App};
use crate::requests::CreateUserRequest;
use crate::services::UserService;

#[handler]
pub async fn store(form: CreateUserRequest) -> Response {
    let user_service = App::resolve::<UserService>()?;
    let user = user_service.create_user(&form.email, &form.name).await?;
    json_response!({ "user": user })
}
```

## File uploads (`MultipartRequest`)

`multipart/form-data` is its own extractor — `#[derive(MultipartRequest)]`
streams the body part by part, spilling large file parts to a temp file
above the configured threshold so a 200 MiB upload never sits fully in
RAM. Each field carries a `#[field("name")]` annotation that names the
wire field; file fields use `UploadedFile<V>` where `V` is a validator
(or a tuple of validators) from `suprnova::http::upload::validators`.

```rust
use suprnova::{handler, json_response, MultipartRequest, Response};
use suprnova::http::upload::UploadedFile;
use suprnova::http::upload::validators::{Image, MaxSize};

#[derive(MultipartRequest)]
pub struct AvatarUpload {
    #[field("avatar")]
    pub avatar: UploadedFile<(Image, MaxSize<5_242_880>)>, // 5 MiB cap
    #[field("caption")]
    pub caption: Option<String>,
}

#[handler]
pub async fn upload_avatar(form: AvatarUpload) -> Response {
    // `avatar` is in memory or in a temp file depending on size.
    // `.bytes()` reads either; `.store_as(...)` streams to a disk.
    let bytes = form.avatar.bytes().await?;
    json_response!({ "size": bytes.len(), "caption": form.caption })
}
```

Field shapes:

| Declaration | Wire shape |
|---|---|
| `UploadedFile<V>` | required file |
| `Option<UploadedFile<V>>` | optional file |
| `Vec<UploadedFile<V>>` | array uploads (`photos[]`) |
| `String` / `u32` / any `FromStr` | text field (required) |
| `Option<String>` / `Option<T: FromStr>` | optional text field |
| `Vec<String>` / `Vec<T: FromStr>` | repeated text fields |

Built-in validators in `suprnova::http::upload::validators`:

- `MaxSize<N>` — short-circuits at the byte boundary when the running
  total exceeds `N` bytes (HTTP 413).
- `Image` — rejects parts whose magic bytes don't claim `image/*`.
- `MimeType<L>` — accepts a fixed allowlist provided by your own
  `MimeAllowlist` type.
- `()` — no-op; `UploadedFile<()>` accepts any bytes.

Validators compose as tuples: `(Image, MaxSize<5_242_880>)` runs both,
short-circuiting on the first failure.

### Per-field caps and array bounds

The byte cap on the total body is global (8 MiB by default for
multipart, configurable via
`suprnova::http::upload::set_global_max_multipart_body_bytes`). Per-field
caps prevent abuse where a body of many small parts grows
`Vec<UploadedFile<_>>` unbounded within the byte budget:

```rust
#[derive(MultipartRequest)]
pub struct Gallery {
    #[field("photos", max_count = 8)]
    pub photos: Vec<UploadedFile<MaxSize<1_048_576>>>,
}
```

The (`max_count` + 1)-th part with that name returns HTTP 422 before
allocating, so the extra part never reaches `Vec` growth.

### Authorize and after-validation hooks

`MultipartRequest` mirrors `FormRequest`'s hooks via the
`MultipartRequestHooks` trait. By default the derive emits an empty
impl; opt in to your own with `#[multipart(custom_hooks)]`:

```rust
use suprnova::{MultipartRequest, Request, ValidationErrors};
use suprnova::http::upload::{MultipartRequestHooks, UploadedFile};

#[derive(MultipartRequest)]
#[multipart(custom_hooks)]
pub struct GuardedUpload {
    #[field("file")]
    pub file: UploadedFile,
}

impl MultipartRequestHooks for GuardedUpload {
    fn authorize(req: &Request) -> bool {
        req.header("X-Admin-Token").is_some()
    }

    fn after_validation(&self) -> Result<(), ValidationErrors> {
        if self.file.size == 0 {
            let mut errs = ValidationErrors::new();
            errs.add("file", "empty file");
            return Err(errs);
        }
        Ok(())
    }
}
```

### Streaming to storage

`UploadedFile::store_as` writes the part to a registered storage disk.
For disk-backed parts the path is fully streaming (64 KiB chunks via
`opendal::Operator::writer`); in-memory parts use a single write call.
Use the content-derived extension when the storage path is
content-addressed — the filename header is untrusted:

```rust
use suprnova::Storage;

let disk = Storage::disk("avatars")?;
let path = format!("{}.{}", user.id, form.avatar.extension_from_magic());
form.avatar.store_as(&disk, &path).await?;
```

See [Filesystem](filesystem.md) for the storage disk registry.

## File organization

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

## End-to-end type safety with Inertia

Requests can also derive `InertiaProps` to generate TypeScript types, enabling end-to-end type safety from your Rust backend to your React frontend.

### Generating TypeScript types for requests

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

### Type-safe forms with Inertia

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

### What the derive buys you

- TypeScript catches field-name typos and type mismatches at compile
  time.
- IDE autocomplete reads the generated `.ts` directly.
- Rename a field in Rust, rerun `suprnova generate-types`, and the
  TypeScript surface follows.

See [TypeScript types](frontend-typescript-types.md) for the full
generation pipeline.

## Request accessors

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
let present: bool = req.has_query("id");
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

## Why Suprnova diverges

Laravel exposes a synchronous, merged input bag — `$req->input('field')`,
`$req->all()`, `$req->only(['a','b'])`, `$req->boolean('flag')` — pulled
from the query string and the parsed body together. Suprnova does not
ship that surface. The reason:

- Suprnova's body is consume-once and async. A synchronous `all()`
  would require buffering every body up front to satisfy a method that
  most handlers never call — the memory and DoS surface differs from
  PHP's per-request-process lifecycle.
- The typed alternative (`#[request]` + `FormRequest`) gives
  compile-time field names, validation, and content-type-aware parsing
  — exactly the safety net the untyped bag lacks.

For query / header / route inspection, reach for `query_param`,
`query_into`, `has_query`, `bearer_token`, and the header readers
above. For body-side access, define a `#[request]` struct or a
`#[derive(MultipartRequest)]` extractor.

## Next

- [Validation](validation.md) — the rule library behind `#[validate(...)]`
  and the shape of the 422 error bag
- [Responses](responses.md) — building `HttpResponse` values back from
  your handler, including streaming and redirects
- [Errors](errors.md) — handler patterns built on top of `Response`
  being `Result<HttpResponse, HttpResponse>`
- [Routing](routing.md) — registering routes and the `{id}` parameters
  `req.param("id")` reads
- [Authentication](authentication.md) — `Auth::user_as`, `Auth::attempt`,
  and the guards that resolve the current user from the request
- [Filesystem](filesystem.md) — registering the storage disks that
  `UploadedFile::store_as` writes to
