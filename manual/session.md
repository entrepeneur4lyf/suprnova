# Sessions

suprnova provides database-backed session management similar to Laravel. Sessions are automatically initialized and managed through middleware.

## Overview

Sessions in suprnova are:

- **Database-backed** for horizontal scalability
- **Automatically initialized** via `SessionMiddleware`
- **Request-scoped** with thread-local storage
- **Secure** with HttpOnly, SameSite cookies

## Accessing Session Data

Use the `session()` and `session_mut()` functions to access session data:

```rust
use suprnova::session::{session, session_mut};

// Read session data
if let Some(session) = session() {
    // Get a value
    if let Some(user_id) = session.get::<i64>("user_id") {
        println!("User ID: {}", user_id);
    }

    // Check if key exists
    if session.has("cart") {
        // ...
    }
}

// Write session data
if let Some(mut session) = session_mut() {
    // Set a value
    session.put("locale", "en");

    // Remove a value
    session.forget("temp_data");

    // Flash data (available only for next request)
    session.flash("message", "Welcome back!");
}
```

## Session API

### Reading Data

```rust
use suprnova::session::session;

if let Some(session) = session() {
    // Get a typed value
    let count: Option<i32> = session.get("view_count");

    // Get with default
    let theme = session.get::<String>("theme").unwrap_or("light".to_string());

    // Check existence
    if session.has("user_id") {
        // User is logged in
    }
}
```

### Writing Data

```rust
use suprnova::session::session_mut;

if let Some(mut session) = session_mut() {
    // Store a value
    session.put("user_id", 123i64);
    session.put("username", "john");

    // Store complex types (must be serializable)
    session.put("preferences", serde_json::json!({
        "theme": "dark",
        "notifications": true
    }));

    // Remove a value
    session.forget("temp_key");
}
```

### Flash Messages

Flash data is available only for the next request, perfect for success/error messages:

```rust
use suprnova::session::session_mut;

// In your controller action
if let Some(mut session) = session_mut() {
    session.flash("success", "Your profile has been updated!");
}

// Redirect to another page
redirect!("/profile")
```

```rust
// On the next request, the flash data is automatically available
if let Some(session) = session() {
    if let Some(success) = session.get::<String>("success") {
        // Display success message
    }
}
```

## Session Configuration

Configure sessions in your `.env` file:

```env
# Session lifetime in minutes (default: 120)
SESSION_LIFETIME=120

# Cookie name (default: suprnova_session)
SESSION_COOKIE=suprnova_session

# Enable secure cookies (requires HTTPS, default: false)
SESSION_SECURE=false

# Cookie path (default: /)
SESSION_PATH=/

# SameSite policy: Lax, Strict, or None (default: Lax)
SESSION_SAME_SITE=Lax
```

## Session Middleware

The `SessionMiddleware` is automatically registered in `bootstrap.rs`:

```rust
use suprnova::{Router, SessionMiddleware, CsrfMiddleware};
use suprnova::session::{SessionConfig, DatabaseSessionDriver};

pub fn boot(router: Router) -> Router {
    // Initialize session store
    let session_config = SessionConfig::from_env();
    let session_store = DatabaseSessionDriver::new();

    router
        // Session middleware (must come before CSRF)
        .middleware(SessionMiddleware::new(session_store, session_config))
        // CSRF protection (requires session)
        .middleware(CsrfMiddleware::new())
}
```

## Sessions Table

Sessions are stored in the `sessions` database table:

```sql
CREATE TABLE sessions (
    id VARCHAR(40) PRIMARY KEY,
    user_id BIGINT NULL,
    payload TEXT NOT NULL,
    csrf_token VARCHAR(80) NOT NULL,
    last_activity TIMESTAMP NOT NULL
);

CREATE INDEX sessions_user_id_index ON sessions(user_id);
CREATE INDEX sessions_last_activity_index ON sessions(last_activity);
```

This table is automatically created when you run migrations on a new suprnova project.

## Session Garbage Collection

Expired sessions are automatically cleaned up. The session lifetime is determined by `SESSION_LIFETIME` in your `.env` file.

## Working with the Auth System

Sessions integrate seamlessly with suprnova's authentication system:

```rust
use suprnova::{Auth, session::session};

// The Auth facade uses sessions internally
Auth::login(user_id);  // Stores user_id in session

// You can access the user_id directly if needed
if let Some(session) = session() {
    let user_id = session.user_id();  // Returns Option<i64>
}
```

## Thread Safety

Sessions use thread-local storage to ensure each request has its own isolated session data. This means:

- Session data is automatically scoped to the current request
- No race conditions between concurrent requests
- No need for explicit locking or synchronization

```rust
// Safe to call from anywhere in your request handling code
if let Some(session) = session() {
    // This session is isolated to the current request
}
```
