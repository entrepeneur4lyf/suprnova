# Authentication

suprnova provides Laravel-style session-based authentication out of the box. When you create a new project wsuprnova `suprnova new`, it includes a complete authentication system with login, registration, and protected routes.

## Overview

The authentication system includes:

- **Named guards** — a default session guard (`web`) and a stateless token guard (`api`), each with its own user provider, resolved through `Auth::guard("name")`
- **A Laravel-shaped `Auth` facade** — `attempt`, `login`, `once`, `login_using_id`, `validate`, `logout`, … all delegating to the default guard
- **Built-in user providers** — `EloquentUserProvider<User>` (typed model) and `DatabaseUserProvider` (table-backed) so the common case needs no custom provider
- **Session-based auth** with database-backed sessions
- **Secure password hashing** using bcrypt
- **CSRF protection** on all state-changing requests
- **Auth + Guest middleware** for protecting routes (with per-guard `for_guard("api")`)
- **HTTP Basic** authentication middleware
- **Remember me** functionality
- **Lifecycle events** — `Attempting`, `Authenticated`, `Login`, `Logout`, `Failed`

## Auth Facade

The `Auth` struct exposes Laravel's facade methods. The credential- and
user-based methods delegate to the **default guard** (resolved from the
container `AuthManager`); the sync `check`/`guest`/`id` are the session-backed
fast path.

```rust
use suprnova::{Auth, Credentials};

// Validate credentials and log the user in (fires Login + Authenticated,
// honours remember-me). Returns the resolved user, or None on bad credentials.
if let Some(user) = Auth::attempt(&Credentials::password(&email, &password), remember).await? {
    println!("Welcome, user {}", user.get_auth_identifier());
}

// Log a known user in directly.
Auth::login(user, remember).await?;

// Validate without logging in (e.g. a password-confirmation dialog).
let ok: bool = Auth::validate(&Credentials::password(&email, &password)).await?;

// Authenticate for this request only — no session (Laravel's `once`).
Auth::once(&Credentials::password(&email, &password)).await?;

// Sync, session-backed checks (no AuthManager required).
if Auth::check() { /* authenticated */ }
if Auth::guest() { /* not authenticated */ }
if let Some(user_id) = Auth::id() { println!("User ID: {user_id}"); }

// Resolve the current user (via the registered provider).
if let Some(user) = Auth::user().await? {
    println!("User ID: {}", user.get_auth_identifier());
}
if let Some(user) = Auth::user_as::<User>().await? {
    println!("Welcome, {}!", user.name);
}

// Log out — clears the session + request user, revokes remember-me,
// fires Logout. Always `.await` it.
Auth::logout().await?;
```

If you have already verified a user's identity yourself and only need to
establish the session, use the synchronous primitive `Auth::login_id(id)` — it
writes the session id without a provider, an `AuthManager`, or events.
Returns `Err` when called outside a request scope (no `SessionMiddleware`
installed) so a silently-dropped login can never look like success:

```rust
Auth::login_id(user.id.to_string())?;
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
    /// The identifier as a string — the canonical value stored in the session
    /// and used as the guard key (Laravel's `getAuthIdentifier`). Numeric
    /// primary keys stringify trivially; UUIDs / ULIDs / opaque
    /// external-provider ids flow through unchanged.
    fn get_auth_identifier(&self) -> String {
        self.id.to_string()
    }

    /// The hashed password the built-in providers verify against. Return
    /// `None` for users that authenticate by other means (OAuth, passkey).
    fn get_auth_password(&self) -> Option<&str> {
        Some(&self.password)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
```

The optional `auth_identifier() -> i64` method is a convenience for apps whose
id is a signed integer primary key — Suprnova itself never calls it. The
default parses `get_auth_identifier`, falling back to `0` for non-numeric ids.
Override it for free when your model already holds an `i64`.

`get_auth_identifier` (a `String`) is the surface the guard and session use;
`auth_identifier` (an `i64`) is a Suprnova convenience that defaults the string
form for integer-keyed models.

## User Provider

The `UserProvider` trait tells suprnova how to fetch and validate users. Two
providers ship built in, so the common case needs **no** custom implementation:

- **`EloquentUserProvider<User>`** — resolves through a typed
  `#[suprnova::model]` user that is also `Authenticatable`. Looks up by primary
  key for ids and by `email` for credentials.
- **`DatabaseUserProvider`** — resolves a table by name into a `GenericUser`
  (id + attribute map); use it when you don't have (or want) a typed model.

Register a provider and the guard configuration on the container `AuthManager`
in `bootstrap.rs` (the Rust analogue of `config/auth.php`):

```rust
use std::sync::Arc;
use suprnova::{App, Auth, AuthConfig, AuthManager, EloquentUserProvider};
use crate::models::user::User;

pub async fn register() {
    // ...
    App::singleton(AuthManager::new(AuthConfig::from_env()));
    Auth::register_provider("users", Arc::new(EloquentUserProvider::<User>::new()))
        .expect("register users provider");
}
```

Both built-in providers filter credential lookups against an allowlist (default
`["email"]`) — a hostile credential map cannot inject extra `WHERE` predicates.

To plug in a custom source (LDAP, an external API), implement `UserProvider`
yourself. Note `retrieve_by_id` takes the identifier as a **string**:

```rust
use async_trait::async_trait;
use std::sync::Arc;
use suprnova::{Authenticatable, FrameworkError, UserProvider};

struct MyProvider;

#[async_trait]
impl UserProvider for MyProvider {
    async fn retrieve_by_id(
        &self,
        id: &str,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        // fetch your user by `id`, return it as `Arc<dyn Authenticatable>`
        # let _ = id; Ok(None)
    }
    // retrieve_by_credentials + validate_credentials have provider defaults;
    // override them to support `Auth::attempt`/`validate`.
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

To check a **named guard** other than the default, chain `for_guard`:

```rust
// 401 unless the `api` guard is authenticated.
.middleware(AuthMiddleware::new().for_guard("api"))
```

`GuestMiddleware::for_guard("name")` works the same way. A token guard
(`for_guard("api")`) relies on the bearer-token middleware running earlier in
the chain to populate the request's auth id.

## Named Guards

`AuthConfig` declares the guards and which provider each uses — the default
config wires `web` (session) and `api` (token), both backed by the `users`
provider. Resolve a guard by name to act against it explicitly:

```rust
use suprnova::Auth;

// Read-only check against the API (token) guard.
if Auth::guard("api")?.check().await? { /* ... */ }

// Login/logout/attempt against a session guard.
let user = Auth::stateful_guard("web")?
    .attempt(&credentials, false)
    .await?;
```

The bare `Auth::attempt`/`login`/`logout`/… methods are sugar over the
**default** guard (`AUTH_GUARD`, default `web`). Set the default with the
`AUTH_GUARD` environment variable.

## HTTP Basic Authentication

`BasicAuthMiddleware` authenticates from the `Authorization: Basic` header
against a guard's provider:

```rust
use suprnova::BasicAuthMiddleware;

// Stateful — logs the user into the session on success.
.middleware(BasicAuthMiddleware::new())

// Stateless — authenticates for this request only.
.middleware(BasicAuthMiddleware::once())
```

The decoded username is matched against the `field` credential (default
`email`); a missing, malformed, or invalid header returns `401` with a
`WWW-Authenticate: Basic` challenge. Configure with `.field(...)`, `.realm(...)`,
and `.for_guard(...)`.

## Auth Events

The guards dispatch lifecycle events you can listen for (see [Events &
Listeners](events.md)):

| Event | When |
|-------|------|
| `Attempting` | a credential attempt begins |
| `Authenticated` | a user is resolved as the current user (login or `once`) |
| `Login` | a user is persisted to the session |
| `Logout` | a user is logged out |
| `Failed` | a credential attempt fails |

Events carry the guard name and a string user id — never the credentials.

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

`suprnova new` generates a controller that verifies the password by hand and
establishes the session with the `Auth::login_id` primitive:

```rust
use suprnova::{handler, Auth, Request, Response};
use crate::models::user::User;

#[handler]
pub async fn show_login(_req: Request) -> Response {
    inertia!("auth/Login")
}

#[handler]
pub async fn login(req: Request) -> Response {
    let email: String = req.input("email").unwrap_or_default();
    let password: String = req.input("password").unwrap_or_default();

    // Find user by email and verify the password ourselves.
    let user = match User::find_by_email(&email).await {
        Ok(Some(u)) => u,
        _ => return inertia!("auth/Login", { "errors": { "email": ["Invalid credentials"] } }),
    };
    if !user.verify_password(&password).unwrap_or(false) {
        return inertia!("auth/Login", { "errors": { "email": ["Invalid credentials"] } });
    }

    // Establish the session for this id (sync primitive — no provider needed).
    // The `?` propagates an `Err` if the request is missing its
    // `SessionMiddleware` scope, so a silently-dropped login never reaches
    // the redirect.
    Auth::login_id(user.id.to_string())?;

    redirect!("/dashboard")
}

#[handler]
pub async fn logout(_req: Request) -> Response {
    Auth::logout().await?;
    redirect!("/")
}
```

Once you've registered an `AuthManager` and a provider (see [User
Provider](#user-provider)), the whole `login` body collapses to one
guard-backed call that also fires the `Attempting`/`Login`/`Authenticated`
events and handles remember-me:

```rust
use suprnova::Credentials;

#[handler]
pub async fn login(req: Request) -> Response {
    let email: String = req.input("email").unwrap_or_default();
    let password: String = req.input("password").unwrap_or_default();
    let remember: bool = req.input("remember").unwrap_or(false);

    match Auth::attempt(&Credentials::password(&email, &password), remember).await? {
        Some(_user) => redirect!("/dashboard"),
        None => inertia!("auth/Login", { "errors": { "email": ["Invalid credentials"] } }),
    }
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

Sessions are automatically managed by the framework. See the [Sessions](session.md) documentation for details on how to work with session data directly.

## CSRF Protection

All POST, PUT, PATCH, and DELETE requests are automatically protected against CSRF attacks. See the [CSRF Protection](csrf.md) documentation for details.

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
