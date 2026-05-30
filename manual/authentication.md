# Authentication

Suprnova ships a Laravel-shaped authentication system: a static `Auth`
facade, named guards resolved through an `AuthManager`, pluggable user
providers, an `Authenticatable` trait on your User model, and middleware
to gate routes. A scaffolded project boots with a session guard (`web`)
and a token guard (`api`) already wired against your typed `User`, so
login, registration, and protected routes work the day you run
`suprnova new`.

## The pieces

| Type | Role |
|---|---|
| `Auth` | Static facade — `Auth::user()`, `Auth::attempt()`, `Auth::login()`, `Auth::logout()`, `Auth::guard("name")` |
| `Authenticatable` | Trait your User model implements; surfaces `get_auth_identifier() -> String` and the password hash |
| `UserProvider` | Trait that fetches users from storage; `EloquentUserProvider<M>` and `DatabaseUserProvider` ship built in |
| `AuthManager` | Holds the [`AuthConfig`] + registered providers; resolves named guards on demand |
| `SessionGuard` / `TokenGuard` | Session-backed (stateful) and bearer-token (stateless) guards |
| `AuthMiddleware` / `GuestMiddleware` / `BasicAuthMiddleware` | Route guards |
| `Credentials` | JSON-shaped credential map, typically `{ "email", "password" }` |

The trail in source is short: `framework/src/auth/{guard,manager,contract,
authenticatable,middleware,session_guard,token_guard,eloquent_provider,
database_provider}.rs`. Higher-level flows — email verification, password
reset, brute-force throttling, TOTP 2FA — live alongside in
`framework/src/auth_flows/` and have their own chapter:
[Auth Flows](auth-flows.md).

## Identifier model

The authenticated user's id flows through Suprnova as a `String`
end-to-end — session storage, [`UserProvider::retrieve_by_id`], the
remember-me table, every auth event. The canonical surface is
`Authenticatable::get_auth_identifier() -> String` (Laravel's
`getAuthIdentifier`). Numeric primary keys stringify trivially; UUIDs,
ULIDs, and opaque OAuth provider ids flow through unchanged.

```rust
use std::any::Any;
use suprnova::Authenticatable;

impl Authenticatable for User {
    fn get_auth_identifier(&self) -> String {
        self.id.to_string()
    }

    fn get_auth_password(&self) -> Option<&str> {
        Some(&self.password)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
```

`get_auth_password` is what the built-in providers verify a plaintext
password against via `hashing::verify_async`. Return `None` for users
that authenticate by other means (OAuth, passkey, magic link). The
`auth_identifier_name() -> &'static str` method (default `"id"`) names
the column the id lives in. The convenience `auth_identifier() -> i64`
default-parses the string id and falls back to `0` for non-numeric ids —
Suprnova itself never calls it; override only for integer-keyed models
that want to skip the parse.

### Why Suprnova diverges

Laravel's `getAuthIdentifier()` returns `mixed`. PHP doesn't care
whether the id is an int, a UUID string, or a stringly-typed primary
key from a legacy table. Rust needs a single concrete type the session,
the provider, and the events all agree on. `String` is the only choice
that accommodates every id shape without forcing the framework to know
which one your app uses. The `auth_identifier()` integer convenience
exists for the common case where your column is a `BIGINT`, but the
framework never depends on it — switch your `User` to a ULID tomorrow
and nothing in the auth stack notices.

## Wiring auth at boot

The Rust analogue of `config/auth.php` is an `AuthConfig` registered as
an `AuthManager` singleton on the container, plus a `UserProvider`
registered under a name. `bootstrap.rs` typically does both in two
lines:

```rust
use std::sync::Arc;
use suprnova::{App, Auth, AuthConfig, AuthManager, EloquentUserProvider};

use crate::models::user::User;

pub async fn bootstrap() -> Result<(), suprnova::FrameworkError> {
    // ... DB::init, SessionMiddleware install, etc.

    App::singleton(AuthManager::new(AuthConfig::from_env()));
    Auth::register_provider("users", Arc::new(EloquentUserProvider::<User>::new()))
        .expect("register users provider");

    Ok(())
}
```

`AuthConfig::from_env()` reads the default guard from `AUTH_GUARD`
(default `"web"`) and ships with two named guards out of the box: a
`web` session guard and an `api` token guard, both backed by the
`"users"` provider. Apps that need more guards (separate `admins`
provider, distinct stateful and stateless guards) build the config
explicitly:

```rust
use suprnova::{AuthConfig, GuardConfig};

let config = AuthConfig::new("web")
    .guard("web", GuardConfig::session("users"))
    .guard("admin", GuardConfig::session("admins"))
    .guard("api", GuardConfig::token("users"));
```

## The `Auth` facade

The static `Auth` facade is the Laravel-shaped surface you call from
controllers and middleware. The credential- and user-based methods
delegate to the **default guard** (whatever `AuthConfig::default_guard`
points at, default `"web"`); the synchronous `check`/`guest`/`id` reads
are the session-backed fast path and need no manager.

```rust
use suprnova::{Auth, Credentials};

// Validate credentials and log the user in. Fires Attempting → (Login +
// Authenticated), honours remember-me. Returns the resolved user, or
// None on bad credentials.
if let Some(user) = Auth::attempt(&Credentials::password(&email, &password), remember).await? {
    println!("Welcome, user {}", user.get_auth_identifier());
}

// Log a known user in directly.
Auth::login(user, remember).await?;

// Log in by id without re-checking credentials (e.g. just-finished registration).
Auth::login_using_id(&id, remember).await?;

// Validate credentials without persisting a session (password-confirmation dialogs).
let ok: bool = Auth::validate(&Credentials::password(&email, &password)).await?;

// Authenticate for this request only — no session write. Laravel's `once`.
let ok: bool = Auth::once(&Credentials::password(&email, &password)).await?;
Auth::once_using_id(&id).await?;

// Session-backed fast path (no AuthManager required).
if Auth::check()    { /* authenticated */ }
if Auth::guest()    { /* not authenticated */ }
if let Some(id) = Auth::id() { /* string id */ }

// Whether the current user was authenticated by remember-me cookie this
// request. Laravel's `viaRemember()`.
if Auth::via_remember() { /* … */ }

// Resolve the current user (via the registered provider).
if let Some(user) = Auth::user().await? {
    println!("user id: {}", user.get_auth_identifier());
}
if let Some(user) = Auth::user_as::<User>().await? {
    println!("Welcome, {}!", user.name);
}

// Tear down auth + revoke remember-me + rotate CSRF + fire Logout.
Auth::logout().await?;

// Full session destruction (regenerate id + flush + revoke remember-me + fire Logout).
Auth::logout_and_invalidate().await?;
```

`Auth::attempt` returns the resolved user on success rather than a bare
`bool` — richer than Laravel's API, and saves the follow-up `Auth::user()`
call. `Ok(None)` means the credentials did not resolve a user; `Err`
means a database / hashing / configuration failure that needs to bubble.

If you have already verified a user's identity yourself and only want
to establish the session — say after an OAuth callback completes —
reach for the synchronous primitive:

```rust
// Sync, no provider, no AuthManager, no events. Returns Err when called
// outside a request scope (no SessionMiddleware installed) so a
// silently-dropped login can never look like success.
Auth::login_id(user.id.to_string())?;
```

`login_id` regenerates the session id (preventing session fixation) and
rotates the CSRF token, then writes the id into the session. It's
deliberately failure-loud: previous versions silently no-op'd outside a
session scope, and the audit fixed that — a "successful login" that
never landed is the kind of bug nothing else catches.

## `Auth::user()` and `user_as<T>`

`Auth::user()` returns the user behind the trait:

```rust
if let Some(user) = Auth::user().await? {
    println!("user id: {}", user.get_auth_identifier());
}
```

That trait object covers anyone who implements `Authenticatable`. To get
your concrete `User` back, downcast through `user_as::<T>()`:

```rust
use suprnova::Auth;
use crate::models::user::User;

if let Some(user) = Auth::user_as::<User>().await? {
    // Field access on the model directly.
    println!("Welcome, {}!", user.name);
}
```

`user_as` returns `Ok(None)` both when no user is authenticated *and*
when the resolved user isn't a `T` (e.g. an `Auth::set_user(...)` of
a different type elsewhere in the stack). Inside a request the user is
cached per-request, so calling `Auth::user()` repeatedly only hits the
provider once.

## Named guards

The bare `Auth::*` methods talk to the default guard. To act against a
specific guard, resolve it by name:

```rust
use suprnova::Auth;

// Read-only operations work on every driver.
if Auth::guard("api")?.check().await? { /* … */ }

// Login/logout/attempt need a stateful guard. Token guards fail loud here.
let user = Auth::stateful_guard("web")?
    .attempt(&credentials, false)
    .await?;
```

`Auth::guard("name")` returns `Arc<dyn Guard>` (the read contract) and
`Auth::stateful_guard("name")` returns `Arc<dyn StatefulGuard>` (adds
`attempt`/`login`/`logout`). Asking for the stateful contract on a token
guard returns an error with a remediation message rather than silently
limiting the API.

## User providers

A `UserProvider` tells the auth stack how to fetch and validate users.
Two providers ship built in, so the common case needs no custom
implementation:

- **`EloquentUserProvider<M>`** — resolves through a typed
  `#[suprnova::model]` `User` that is also `Authenticatable`. Looks up
  by primary key for ids, by `email` (default) for credentials.
- **`DatabaseUserProvider`** — resolves a raw table by name into a
  `GenericUser` (id + attribute map). Use it when you don't have or
  want a typed model.

Both filter credential lookups against an allowlist (default
`["email"]`) — a hostile credential map cannot inject extra `WHERE`
predicates. Customise the allowlist with `.credential_columns([...])`,
the lookup column with `.identifier_column("uuid")`, or the id-binding
strategy with `.with_id_parser(...)`.

To plug in a custom source (LDAP, an external API), implement
`UserProvider` directly. `retrieve_by_id` takes the identifier as
a `&str`:

```rust
use async_trait::async_trait;
use std::sync::Arc;
use suprnova::{Authenticatable, FrameworkError, UserProvider};

struct LdapProvider;

#[async_trait]
impl UserProvider for LdapProvider {
    async fn retrieve_by_id(
        &self,
        id: &str,
    ) -> Result<Option<Arc<dyn Authenticatable>>, FrameworkError> {
        // … fetch from LDAP, return as Arc<dyn Authenticatable>
        Ok(None)
    }

    // retrieve_by_credentials + validate_credentials have trait defaults
    // that return None / false. Override them to support `Auth::attempt`
    // and `Auth::validate` against your source.
}
```

Register it on the manager:

```rust
Auth::register_provider("ldap", Arc::new(LdapProvider))?;
```

## Protecting routes

### `AuthMiddleware`

Gate authenticated-only routes. Unauthenticated requests are redirected
to a login page or receive `401`:

```rust
use suprnova::{AuthMiddleware, Router};

pub fn routes() -> Router {
    Router::new()
        .get("/dashboard", controllers::dashboard::index)
        .post("/logout", controllers::auth::logout)
        .middleware(AuthMiddleware::redirect_to("/login"))
}
```

`AuthMiddleware::new()` returns `401 Unauthorized` instead — best for
JSON APIs. `AuthMiddleware::redirect_to("/login")` issues a `302` for
regular requests and a `409 X-Inertia-Location` for Inertia requests
(which the Inertia client turns into a full-page visit). To gate on a
specific guard, chain `for_guard`:

```rust
// 401 unless the api guard is authenticated.
.middleware(AuthMiddleware::new().for_guard("api"))
```

A token guard (`for_guard("api")`) relies on whatever bearer-token
middleware runs earlier in the chain to populate the request's auth id;
without it the guard always reports unauthenticated.

### `GuestMiddleware`

The inverse — for login and registration pages that authenticated users
shouldn't see:

```rust
use suprnova::{GuestMiddleware, Router};

pub fn routes() -> Router {
    Router::new()
        .get("/login", controllers::auth::show_login)
        .post("/login", controllers::auth::login)
        .get("/register", controllers::auth::show_register)
        .post("/register", controllers::auth::register)
        .middleware(GuestMiddleware::redirect_to("/dashboard"))
}
```

`GuestMiddleware::for_guard("name")` works the same way as
`AuthMiddleware::for_guard`.

### `BasicAuthMiddleware`

HTTP Basic auth from the `Authorization: Basic` header against a
guard's provider:

```rust
use suprnova::BasicAuthMiddleware;

// Stateful — logs the user into the session on success (Laravel's `basic`).
.middleware(BasicAuthMiddleware::new())

// Stateless — authenticates for this request only (Laravel's `onceBasic`).
.middleware(BasicAuthMiddleware::once())
```

The decoded username is matched against the `field` credential (default
`"email"`); a missing, malformed, or invalid header returns `401` with
a `WWW-Authenticate: Basic realm="..."` challenge. Configure with
`.field(...)`, `.realm(...)`, and `.for_guard(...)`.

## Lifecycle events

The guards dispatch five lifecycle events. Listen for them via the
[`EventFacade`](events.md):

| Event | When |
|---|---|
| `Attempting` | a credential attempt begins (`attempt`/`once`) |
| `Authenticated` | a user is actively authenticated this request (`login`/`once`/`once_using_id`) |
| `Login` | a user is persisted to the session (`login`/successful `attempt`) |
| `Logout` | a user is logged out |
| `Failed` | a credential attempt fails (bad password or unknown id) |

Every event carries the guard name and a string user id — never the
plaintext password and never the raw credential map. `Authenticated`
fires only when a user is actively established, not on a passive
`Auth::user()` resolution off an existing session, so listeners don't
get a stream of duplicates on every authenticated request.

## The scaffolded login flow

`suprnova new` generates an authentication controller that uses
`Auth::attempt` against the registered provider. The framework's
`FormRequest` and `Validate` derives handle per-field validation; the
Inertia client surfaces a `422` with `{ message, errors }` automatically
on the originating page:

```rust
use serde::Deserialize;
use suprnova::{
    handler, inertia_response, redirect, serde_json, Auth, Credentials,
    FormRequest, InertiaProps, Request, Response, Validate, ValidationErrors,
};

#[derive(InertiaProps)]
pub struct LoginProps {
    pub errors: Option<serde_json::Value>,
}

#[handler]
pub async fn show_login(req: Request) -> Response {
    inertia_response!(&req, "auth/Login", LoginProps { errors: None })
}

#[derive(Deserialize, Validate)]
pub struct LoginRequest {
    #[validate(email(message = "Please enter a valid email address"))]
    pub email: String,
    #[validate(length(min = 1, message = "Password is required"))]
    pub password: String,
    #[serde(default)]
    pub remember: bool,
}

impl FormRequest for LoginRequest {}

fn invalid_credentials() -> suprnova::FrameworkError {
    let mut errs = ValidationErrors::new();
    errs.add("email", "These credentials do not match our records.");
    suprnova::FrameworkError::Validation(errs)
}

#[handler]
pub async fn login(form: LoginRequest) -> Response {
    match Auth::attempt(
        &Credentials::password(&form.email, &form.password),
        form.remember,
    )
    .await?
    {
        Some(_user) => redirect!("/dashboard").into(),
        None => Err(invalid_credentials().into()),
    }
}

#[handler]
pub async fn logout(_req: Request) -> Response {
    Auth::logout().await?;
    redirect!("/").into()
}
```

Registration follows the same shape: validate the form, create the
user, then `Auth::login(Arc::new(user), false).await?` logs the freshly
created user into the session and fires the `Login` event.

## The scaffolded `User` model

The generated `User` is a `#[suprnova::model]` that also implements
`Authenticatable`. Password handling lives in two helpers backed by
the [`hashing`](hashing.md) module:

```rust
use chrono::{DateTime, Utc};
use suprnova::{attrs, hashing, model, Authenticatable, FrameworkError};

#[model(
    table = "users",
    fillable = ["name", "email", "password"],
    hidden = ["password", "remember_token"],
    timestamps,
)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub password: String,
    pub remember_token: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl User {
    pub async fn find_by_email(email: &str) -> Result<Option<Self>, FrameworkError> {
        <Self as suprnova::eloquent::Model>::query()
            .filter("email", email)
            .first()
            .await
    }

    pub fn verify_password(&self, password: &str) -> Result<bool, FrameworkError> {
        hashing::verify(password, &self.password)
    }

    pub async fn create(
        name: impl Into<String>,
        email: impl Into<String>,
        password: &str,
    ) -> Result<Self, FrameworkError> {
        let hashed = hashing::hash(password)?;
        <Self as suprnova::eloquent::Model>::create(attrs! {
            name: name.into(),
            email: email.into(),
            password: hashed,
        })
        .await
    }
}
```

The `hidden = ["password", "remember_token"]` attribute makes the model
skip those columns when serialising to JSON for the wire — they exist
on the struct but never leak through an Inertia response.

## Remember-me

`Auth::attempt(credentials, remember)` with `remember = true` issues a
remember-me token alongside the session login. The token lives in the
`remember_tokens` table (bcrypt-hashed, single-use rotating) and a
matching encrypted cookie. On a future request where the session is
gone, `SessionMiddleware` verifies the cookie against the hashed row,
rotates the token, and hydrates the session — the user is logged back
in transparently.

Apps that have already established a session and want to issue the
remember-me half separately (the 2FA challenge flow does this) reach
for `Auth::issue_remember_cookie(&user_id, ttl_minutes).await?`.
`Auth::revoke_remember_tokens()` invalidates every remember-me token
for the current user — the right hook for a "log me out everywhere"
account-security button.

## Security guarantees

A short list of invariants the auth stack establishes:

- **`Auth::login_id` fails loud outside a request scope.** Previous
  versions silently dropped the session write; a "successful login"
  that never landed is the kind of bug nothing else catches.
- **Session id and CSRF token regenerate on every login.** Both
  `login_id` and the guard-backed `login`/`attempt` rotate them to
  prevent session fixation.
- **Logout clears auth state before revoking remember-me.** If the DB
  revoke fails, the session is already in a logged-out state, so a
  stale auth slot cannot survive a partial logout. The remember-me
  clear cookie is queued *before* the DB delete, so the browser drops
  the cookie even when the row delete fails (the prune sweep cleans up
  later).
- **Credential allowlists block injection.** Both built-in providers
  filter `retrieve_by_credentials` against `credential_columns`, so
  extra keys in an attacker-influenced credential map cannot become
  extra `WHERE` predicates.
- **Auth events never carry plaintext.** Guard name + string user id,
  nothing else. Failed-attempt tracking (email-keyed lockouts) belongs
  to `BruteForce` in [Auth Flows](auth-flows.md), not the lifecycle
  events.

The [Session](session.md) chapter covers the cookie configuration
(`SESSION_LIFETIME`, `SESSION_COOKIE`, `SESSION_SECURE`,
`SESSION_SAME_SITE`) that the session-backed guards inherit.

## Next

- [Auth Flows](auth-flows.md) — email verification, password reset,
  brute-force throttling with `LoginThrottleMiddleware`,
  TOTP 2FA, the `auth_flows` event suite
- [Authorization](authorization.md) — `Gate`, policies, `Authorizable`
  for "what is this user allowed to do"
- [Session](session.md) — the cookie + storage that backs `web`-style
  guards
- [CSRF Protection](csrf.md) — how state-changing requests are gated
- [Hashing](hashing.md) — bcrypt + argon2 helpers behind
  `verify_password`
