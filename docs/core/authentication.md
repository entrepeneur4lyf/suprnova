---
title: Authentication
description: Session-based authentication in suprnova
icon: lock
---

suprnova provides Laravel-style session-based authentication out of the box. When you create a new project wsuprnova `suprnova new`, it includes a complete authentication system with login, registration, and protected routes.

## Overview

The authentication system includes:

- **Session-based auth** with database-backed sessions
- **Secure password hashing** using bcrypt
- **CSRF protection** on all state-changing requests
- **Auth middleware** for protecting routes
- **Guest middleware** for login/register pages
- **Remember me** functionality

## Auth Facade

The `Auth` struct provides a simple API for authentication operations:

```rust
use suprnova::Auth;

// Check if user is authenticated
if Auth::check() {
    // User is logged in
}

// Check if user is a guest
if Auth::guest() {
    // User is not logged in
}

// Get current user ID
if let Some(user_id) = Auth::id() {
    println!("User ID: {}", user_id);
}

// Get the currently authenticated user
if let Some(user) = Auth::user().await? {
    println!("User ID: {}", user.auth_identifier());
}

// Get user as concrete type (e.g., your User model)
if let Some(user) = Auth::user_as::<User>().await? {
    println!("Welcome, {}!", user.name);
}

// Log in a user
Auth::login(user_id);

// Log out the current user
Auth::logout();
```

## Getting the Current User

suprnova provides two methods to retrieve the currently authenticated user:

### Auth::user()

Returns the user as a trait object (`Arc<dyn Authenticatable>`):

```rust
use suprnova::Auth;

#[handler]
pub async fn profile(_req: Request) -> Response {
    if let Some(user) = Auth::user().await? {
        println!("User ID: {}", user.auth_identifier());
    }
    // ...
}
```

### Auth::user_as\<T\>()

Returns the user cast to your concrete User type:

```rust
use suprnova::Auth;
use crate::models::users::User;

#[handler]
pub async fn profile(_req: Request) -> Response {
    if let Some(user) = Auth::user_as::<User>().await? {
        // Access User model fields directly
        println!("Welcome, {}!", user.name);
    }
    // ...
}
```

## Authenticatable Trait

Your User model must implement the `Authenticatable` trait to enable `Auth::user()`. This is already set up for you when you create a new suprnova project:

```rust
use suprnova::Authenticatable;
use std::any::Any;

impl Authenticatable for Model {
    fn auth_identifier(&self) -> i64 {
        self.id as i64
    }

    fn auth_identifier_name(&self) -> &'static str {
        "id"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
```

## User Provider

The `UserProvider` trait tells suprnova how to fetch users from your database. A default `DatabaseUserProvider` is registered in `bootstrap.rs`:

```rust
// In app/src/providers/auth_provider.rs
use async_trait::async_trait;
use suprnova::auth::{Authenticatable, UserProvider};
use suprnova::FrameworkError;
use std::sync::Arc;

#[derive(Default)]
pub struct DatabaseUserProvider;

#[async_trait]
impl UserProvider for DatabaseUserProvider {
    async fn retrieve_by_id(
        &self,
        id: i64,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        let user = User::query()
            .filter(Column::Id.eq(id as i32))
            .first()
            .await?;
        Ok(user.map(|u| Arc::new(u) as Arc<dyn Authenticatable>))
    }
}
```

Register the provider in `bootstrap.rs`:

```rust
use suprnova::{bind, UserProvider};
use crate::providers::DatabaseUserProvider;

pub async fn register() {
    // ...
    bind!(dyn UserProvider, DatabaseUserProvider);
}
```

## Protecting Routes

### Auth Middleware

Use `AuthMiddleware` to protect routes that require authentication:

```rust
use suprnova::{Router, AuthMiddleware};

pub fn routes() -> Router {
    Router::new()
        // Protected routes
        .get("/dashboard", controllers::dashboard::index)
        .post("/logout", controllers::auth::logout)
        .middleware(AuthMiddleware::redirect_to("/login"))
}
```

The `redirect_to` method specifies where unauthenticated users should be redirected. For API routes, use `AuthMiddleware::new()` which returns a 401 status instead.

### Guest Middleware

Use `GuestMiddleware` to protect routes that should only be accessible to guests (like login and register pages):

```rust
use suprnova::{Router, GuestMiddleware};

pub fn routes() -> Router {
    Router::new()
        // Guest-only routes
        .get("/login", controllers::auth::show_login)
        .post("/login", controllers::auth::login)
        .get("/register", controllers::auth::show_register)
        .post("/register", controllers::auth::register)
        .middleware(GuestMiddleware::redirect_to("/dashboard"))
}
```

## Authentication Controller

Here's a typical authentication controller:

```rust
use suprnova::{handler, Auth, Request, Response};
use suprnova::hashing;
use crate::models::user::User;

#[handler]
pub async fn show_login(_req: Request) -> Response {
    inertia!("auth/Login")
}

#[handler]
pub async fn login(req: Request) -> Response {
    let email: String = req.input("email").unwrap_or_default();
    let password: String = req.input("password").unwrap_or_default();
    let remember: bool = req.input("remember").unwrap_or(false);

    // Find user by email
    let user = match User::find_by_email(&email).await {
        Ok(Some(u)) => u,
        _ => return inertia!("auth/Login", { "errors": { "email": ["Invalid credentials"] } }),
    };

    // Verify password
    if !hashing::verify(&password, &user.password).unwrap_or(false) {
        return inertia!("auth/Login", { "errors": { "email": ["Invalid credentials"] } });
    }

    // Log in the user
    Auth::login(user.id);

    redirect!("/dashboard")
}

#[handler]
pub async fn logout(_req: Request) -> Response {
    Auth::logout();
    redirect!("/")
}
```

## User Model

The generated User model includes helper methods for authentication:

```rust
use suprnova::hashing;

impl User {
    /// Find a user by email
    pub async fn find_by_email(email: &str) -> Result<Option<Self>, suprnova::FrameworkError> {
        Self::query()
            .filter(Column::Email.eq(email))
            .first()
            .await
    }

    /// Create a new user with hashed password
    pub async fn create_with_password(
        name: &str,
        email: &str,
        password: &str,
    ) -> Result<Self, suprnova::FrameworkError> {
        let hashed = hashing::hash(password)?;

        Self::create()
            .set_name(name)
            .set_email(email)
            .set_password(&hashed)
            .insert()
            .await
    }

    /// Verify a password against the stored hash
    pub fn verify_password(&self, password: &str) -> bool {
        hashing::verify(password, &self.password).unwrap_or(false)
    }
}
```

## Frontend Pages

suprnova generates React/Inertia pages for authentication:

### Login Page

```tsx
// frontend/src/pages/auth/Login.tsx
import { useForm } from '@inertiajs/react';
import { LoginProps } from '../types/inertia-props';

export default function Login({ errors }: LoginProps) {
  const { data, setData, post, processing } = useForm({
    email: '',
    password: '',
    remember: false,
  });

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    post('/login');
  };

  return (
    <form onSubmit={handleSubmit}>
      <input
        type="email"
        value={data.email}
        onChange={e => setData('email', e.target.value)}
      />
      {errors?.email && <span>{errors.email[0]}</span>}

      <input
        type="password"
        value={data.password}
        onChange={e => setData('password', e.target.value)}
      />

      <label>
        <input
          type="checkbox"
          checked={data.remember}
          onChange={e => setData('remember', e.target.checked)}
        />
        Remember me
      </label>

      <button type="submit" disabled={processing}>
        Login
      </button>
    </form>
  );
}
```

## Sessions

Sessions are automatically managed by the framework. See the [Sessions](/core/sessions) documentation for details on how to work with session data directly.

## CSRF Protection

All POST, PUT, PATCH, and DELETE requests are automatically protected against CSRF attacks. See the [CSRF Protection](/core/csrf) documentation for details.

## Security Features

suprnova's authentication system includes several security measures:

- **bcrypt password hashing** with secure cost factor
- **HttpOnly session cookies** to prevent XSS attacks
- **SameSite=Lax cookies** to prevent CSRF attacks
- **Secure cookies** in production (when `SESSION_SECURE=true`)
- **CSRF tokens** validated on all state-changing requests
- **Constant-time token comparison** to prevent timing attacks
- **Session regeneration** on logout to prevent session fixation

## Environment Configuration

Configure authentication behavior in your `.env` file:

```env
# Session Configuration
SESSION_LIFETIME=120       # Session lifetime in minutes
SESSION_COOKIE=suprnova_session # Cookie name
SESSION_SECURE=false       # Set to true in production (requires HTTPS)
SESSION_PATH=/
SESSION_SAME_SITE=Lax      # Lax, Strict, or None
```

## Database Tables

Authentication requires two database tables, which are automatically created when you run migrations:

### Users Table

| Column | Type | Description |
|--------|------|-------------|
| id | BIGINT | Primary key |
| name | VARCHAR | User's name |
| email | VARCHAR | Unique email |
| password | VARCHAR | Hashed password |
| remember_token | VARCHAR | Remember me token |
| created_at | TIMESTAMP | Creation time |
| updated_at | TIMESTAMP | Last update time |

### Sessions Table

| Column | Type | Description |
|--------|------|-------------|
| id | VARCHAR | Session ID (primary key) |
| user_id | BIGINT | Associated user (nullable) |
| payload | TEXT | Session data (JSON) |
| csrf_token | VARCHAR | CSRF token |
| last_activity | TIMESTAMP | Last activity time |
