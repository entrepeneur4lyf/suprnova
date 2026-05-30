# Inertia Responses

suprnova provides the `inertia_response!` macro for returning Inertia responses from your controllers. This macro handles both initial page loads (HTML) and subsequent XHR requests (JSON).

## The inertia_response! Macro

The `inertia_response!` macro takes a component name and props, and returns the appropriate response format:

```rust
use suprnova::{Request, Response, inertia_response, InertiaProps};

#[derive(InertiaProps)]
pub struct HomeProps {
    pub title: String,
    pub message: String,
}

pub async fn index(_req: Request) -> Response {
    inertia_response!("Home", HomeProps {
        title: "Welcome".to_string(),
        message: "Hello from suprnova!".to_string(),
    })
}
```

## InertiaProps Derive Macro

The `InertiaProps` derive macro automatically implements `Serialize` for your props struct:

```rust
use suprnova::InertiaProps;

#[derive(InertiaProps)]
pub struct UserProps {
    pub name: String,
    pub email: String,
    pub role: String,
    pub is_active: bool,
}
```

This is equivalent to:

```rust
use serde::Serialize;

#[derive(Serialize)]
pub struct UserProps {
    pub name: String,
    pub email: String,
    pub role: String,
    pub is_active: bool,
}
```

## Compile-Time Component Validation

The `inertia_response!` macro validates at compile time that your component exists:

```rust
// This will compile - assuming frontend/src/pages/Home.tsx exists
inertia_response!("Home", HomeProps { ... })

// This will fail to compile if Dashboard.tsx doesn't exist
inertia_response!("Dashboard", DashboardProps { ... })
```

Components are resolved from `frontend/src/pages/{component}.tsx`.

## Nested Components

For nested page components, use the full path:

```rust
// Looks for: frontend/src/pages/Users/Index.tsx
inertia_response!("Users/Index", props)

// Looks for: frontend/src/pages/Admin/Dashboard.tsx
inertia_response!("Admin/Dashboard", props)
```

## JSON-Style Props

You can also use JSON-style syntax for simple cases:

```rust
pub async fn index(_req: Request) -> Response {
    inertia_response!("Home", {
        "title": "Welcome",
        "count": 42,
        "items": ["one", "two", "three"]
    })
}
```

However, typed props are recommended for type safety and TypeScript generation.

## Complex Props

Props can contain nested structs, vectors, and optional values:

```rust
use suprnova::InertiaProps;
use serde::Serialize;

#[derive(Serialize)]
pub struct User {
    pub id: i32,
    pub name: String,
    pub email: String,
}

#[derive(Serialize)]
pub struct Stats {
    pub total_users: i32,
    pub active_sessions: i32,
}

#[derive(InertiaProps)]
pub struct DashboardProps {
    pub user: User,
    pub stats: Stats,
    pub recent_activity: Vec<String>,
    pub notification: Option<String>,
}

pub async fn dashboard(_req: Request) -> Response {
    inertia_response!("Dashboard", DashboardProps {
        user: User {
            id: 1,
            name: "John Doe".to_string(),
            email: "john@example.com".to_string(),
        },
        stats: Stats {
            total_users: 150,
            active_sessions: 42,
        },
        recent_activity: vec![
            "Logged in".to_string(),
            "Updated profile".to_string(),
        ],
        notification: Some("Welcome back!".to_string()),
    })
}
```

## Fetching Data from Database

Combine Inertia responses with database queries:

```rust
use suprnova::{Request, Response, inertia_response, InertiaProps};
use suprnova::database::Model;
use crate::models::posts::{Entity as Posts, Model as Post};

#[derive(InertiaProps)]
pub struct PostsIndexProps {
    pub posts: Vec<Post>,
    pub total: u64,
}

pub async fn index(_req: Request) -> Response {
    let posts = Posts::all().await.unwrap_or_default();
    let total = Posts::count_all().await.unwrap_or(0);

    inertia_response!("Posts/Index", PostsIndexProps {
        posts,
        total,
    })
}
```

## How Response Format Works

The `inertia_response!` macro automatically detects whether to return HTML or JSON:

### Initial Page Load

When a user navigates directly to a URL:

```
GET /dashboard
Accept: text/html

Response: Full HTML document with embedded page data
```

### XHR Navigation

When Inertia makes an XHR request:

```
GET /dashboard
X-Inertia: true
X-Inertia-Version: 1.0

Response: JSON with component and props
{
  "component": "Dashboard",
  "props": { ... },
  "url": "/dashboard",
  "version": "1.0"
}
```

## Configuration

Inertia behavior is configured via environment variables:

```env
# .env
INERTIA_DEVELOPMENT=true          # Enable dev mode
VITE_DEV_SERVER=http://localhost:5173
INERTIA_ENTRY_POINT=src/main.tsx
INERTIA_VERSION=1.0
```

### InertiaConfig

You can also configure programmatically:

```rust
use suprnova::InertiaConfig;

let config = InertiaConfig::builder()
    .vite_dev_server("http://localhost:5173")
    .entry_point("src/main.tsx")
    .version("1.0")
    .development(true)
    .build();
```

## Best Practices

### Keep Props Flat When Possible

```rust
// Good: Flat structure
#[derive(InertiaProps)]
pub struct UserProfileProps {
    pub user_id: i32,
    pub user_name: String,
    pub user_email: String,
}

// Also good: Nested when it makes sense
#[derive(InertiaProps)]
pub struct UserProfileProps {
    pub user: User,
    pub permissions: Vec<String>,
}
```

### Use Option for Nullable Values

```rust
#[derive(InertiaProps)]
pub struct ArticleProps {
    pub title: String,
    pub content: String,
    pub published_at: Option<String>,  // null if not published
    pub author: Option<User>,          // null if anonymous
}
```

### Avoid Sending Sensitive Data

```rust
// Bad: Sending password hash to frontend
#[derive(InertiaProps)]
pub struct UserProps {
    pub email: String,
    pub password_hash: String,  // Never do this!
}

// Good: Only send what's needed
#[derive(InertiaProps)]
pub struct UserProps {
    pub email: String,
    pub name: String,
}
```

## Summary

| Feature | Description |
|---------|-------------|
| `inertia_response!` | Macro to return Inertia responses |
| `InertiaProps` | Derive macro for props serialization |
| Compile-time validation | Checks component exists at build time |
| Automatic format | Returns HTML or JSON based on request |
| Nested props | Support for complex data structures |
